use axum::body::Body;
use axum::response::Response;
use axum::body::Bytes;
use futures::StreamExt;
use std::convert::Infallible;
use std::sync::Arc;

use crate::app::AppState;

/// Set up SSE streaming response with keepalive and logging.
/// The `Unpin` bound is required because the byte_stream is moved into a spawned task.
/// Spawned tasks must own all captured data (trait objects require `Unpin` for safe pinning).
/// `prompt` is the pre-extracted user prompt for the persistence log (passed
/// explicitly so callers can use protocol-specific extractors — OpenAI vs.
/// Anthropic — without re-parsing the body).
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_streaming_response(
    state: Arc<AppState>,
    classification: crate::classification::types::ClassificationResult,
    body_str: String,
    prompt: String,
    start: std::time::Instant,
    byte_stream: impl futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    keepalive_interval_secs: u64,
    provider_attempts: u8,
    final_provider: String,
    session_id: Option<String>,
) -> Response {
    let channel_capacity = state.streaming_channel_capacity;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(channel_capacity);

    crate::proxy::util::log_classification(
        &state,
        &classification,
        &body_str,
        &prompt,
        start,
        "streaming",
        provider_attempts,
        &final_provider,
    );

    tokio::spawn(async move {
        let keepalive_secs = keepalive_interval_secs;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(keepalive_secs));
        let mut stream = byte_stream;
        let mut stream_status = "ok";
        interval.tick().await;
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => { if tx.send(bytes).await.is_err() { break; } }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            // Use the same SSE error event format as
                            // `handle_streaming_error` (non-2xx upstream) so
                            // the two error paths produce byte-compatible
                            // frames — a single SSE error contract. Apply the
                            // same 512-char truncate to bound the SSE event
                            // size (the inline branch's `_e` is a
                            // `reqwest::Error`; while typically < 1 KB, a
                            // pathological upstream could produce a longer
                            // string).
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = super::util::format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                        break;
                    }
                }
            }
        }
        crate::proxy::util::log_classification_with_usage(
            &state,
            &classification,
            &body_str,
            &prompt,
            start,
            stream_status,
            provider_attempts,
            &final_provider,
            None::<&crate::proxy::util::UsageBreakdown>,
            session_id.as_deref(),
        );
    });

    let body =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>));

    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

/// Convert a non-2xx upstream response into an SSE error event for the client.
///
/// 5 invariants protect this code path (the prior-review-fix lessons in
/// `context/foundation/lessons.md`, specifically "Re-run review after a
/// follow-up change touches the same handler" — the F1–F4 review fixes
/// were lost twice across follow-up commits; this function is the
/// regression guard that catches any future re-loss):
/// 1. **Body cap (2 KB)** — upstream error bodies are bounded to 2 KB.
///    Large upstream bodies would amplify latency and memory pressure
///    on the proxy, and SSE clients don't need the full body to surface
///    an error.
/// 2. **JSON escape** — `\`, `"`, and all C0 control chars
///    (`\0x00`-`\0x1F`, including `\n`, `\r`, `\t`, `\b`, `\f`, and
///    other non-printable bytes) in the upstream error text are
///    replaced with safe equivalents before serialization. Without
///    this, a malicious upstream could inject SSE frames or break the
///    JSON parse that downstream consumers use to detect error events.
///    See `format_sse_error_event` for the escape rule.
/// 3. **SSE event format** — the body is `event: error\ndata: {"error":"…"}\n\n`.
///    A valid SSE event with the `error` event name lets clients using
///    `EventSource`-style subscribe to error events distinctly from data
///    events.
/// 4. **Status passthrough** — the upstream's status code is forwarded
///    to the client (e.g., 503 → 503). This preserves the upstream's
///    classification of the failure (rate limit vs. server error vs.
///    auth failure) so clients can react correctly.
/// 5. **`Content-Type: text/event-stream` + `Cache-Control: no-cache`**
///    — the client must parse the body as SSE and must not cache error
///    events (caching would replay a transient error long after it has
///    been resolved).
pub(crate) async fn handle_streaming_error(upstream_response: reqwest::Response) -> Response {
    handle_streaming_error_with_transform(upstream_response, |body, _status| body).await
}

/// Handle a non-2xx upstream response for an Anthropic upstream.
/// Translates the Anthropic error body to OpenAI error envelope,
/// returns an SSE error event (matching the format used by
/// `handle_streaming_error`) with the upstream's status code.
pub(crate) async fn handle_anthropic_streaming_error(upstream_response: reqwest::Response) -> Response {
    handle_streaming_error_with_transform(upstream_response, |body, status| {
        crate::protocol::response::translate_error(&body, status)
    })
    .await
}

/// Shared implementation for streaming error handling. Takes a transform
/// closure that converts the raw error body into the desired SSE error text.
pub(crate) async fn handle_streaming_error_with_transform(
    mut upstream_response: reqwest::Response,
    transform: impl FnOnce(String, u16) -> String,
) -> Response {
    // Bound the upstream error body to 2 KB to cap latency and memory on
    // large error payloads.
    const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
    let mut error_bytes = Vec::new();
    loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                    break;
                }
                error_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    // Truncate to 512 chars before passing to the helper. The helper
    // applies the JSON-escape rule and emits the SSE event body.
    let error_text = String::from_utf8_lossy(&error_bytes)
        .chars()
        .take(512)
        .collect::<String>();
    let status = upstream_response.status().as_u16();
    let transformed = transform(error_text, status);
    let sse_error = super::util::format_sse_error_event(&transformed);
    let mut resp = Response::new(Body::from(sse_error));
    // Forward the upstream's status code to the client so it can react
    // to the specific failure class.
    *resp.status_mut() = upstream_response.status();
    // Mark the response as an uncacheable SSE stream.
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

/// Handle a streaming response from an Anthropic upstream by translating
/// each Anthropic SSE event into one or more OpenAI SSE chunks before
/// forwarding to the client.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_anthropic_streaming_response(
    state: Arc<AppState>,
    classification: crate::classification::types::ClassificationResult,
    body_str: String,
    prompt: String,
    start: std::time::Instant,
    byte_stream: impl futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    keepalive_interval_secs: u64,
    provider_attempts: u8,
    final_provider: String,
    session_id: Option<String>,
) -> Response {
    let channel_capacity = state.streaming_channel_capacity;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(channel_capacity);

    crate::proxy::util::log_classification(
        &state,
        &classification,
        &body_str,
        &prompt,
        start,
        "streaming",
        provider_attempts,
        &final_provider,
    );

    tokio::spawn(async move {
        let keepalive_secs = keepalive_interval_secs;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(keepalive_secs));
        let mut stream = byte_stream;
        let mut stream_status = "ok";
        let mut translate_state = crate::protocol::stream::StreamTranslateState::default();
        let mut buffer = Vec::new();
        interval.tick().await;
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            buffer.extend_from_slice(&bytes);
                            const MAX_SSE_BUFFER: usize = 1024 * 1024; // 1 MB
                            if buffer.len() > MAX_SSE_BUFFER {
                                let sse_error = super::util::format_sse_error_event("SSE buffer exceeded 1 MB limit");
                                let _ = tx.send(Bytes::from(sse_error)).await;
                                stream_status = "buffer_overflow";
                                break;
                            }
                            let events = crate::protocol::stream::parse_sse_events(&buffer);
                            if !events.is_empty() {
                                // Drain only up to last complete event boundary; keep partial tail.
                                if let Some(last_boundary) = buffer.windows(2).rposition(|w| w == b"\n\n") {
                                    buffer.drain(..last_boundary + 2);
                                } else {
                                    buffer.clear();
                                }
                                for (event_type, data) in &events {
                                    if let Some(openai_chunk) =
                                        crate::protocol::stream::translate_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        if tx.send(Bytes::from(openai_chunk)).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = super::util::format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => {
                            // Stream ended — flush remaining buffer.
                            if !buffer.is_empty() {
                                let events = crate::protocol::stream::parse_sse_events(&buffer);
                                for (event_type, data) in &events {
                                    if let Some(openai_chunk) =
                                        crate::protocol::stream::translate_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        let _ = tx.send(Bytes::from(openai_chunk)).await;
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                        break;
                    }
                }
            }
        }
        // Finalize the inference record at stream close with the usage
        // accumulated across message_start/message_delta (Anthropic splits
        // usage across both events). Only emit token fields when a
        // message_start was actually seen — otherwise the stream errored
        // before the upstream produced any usage and a zero-usage row would be
        // misleading (None is the correct "unknown" signal).
        let usage = if translate_state.started {
            let (inp, out, cr, cc) = translate_state.collected_usage();
            Some(crate::proxy::util::UsageBreakdown {
                input_tokens: inp as i32,
                output_tokens: out as i32,
                cache_read_tokens: cr as i32,
                cache_creation_tokens: cc as i32,
            })
        } else {
            None
        };
        crate::proxy::util::log_classification_with_usage(
            &state,
            &classification,
            &body_str,
            &prompt,
            start,
            stream_status,
            provider_attempts,
            &final_provider,
            usage.as_ref(),
            session_id.as_deref(),
        );
    });

    let body =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>));

    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

/// Stream handler that translates OpenAI SSE chunks to Anthropic SSE events.
/// Used by messages_handler when the upstream speaks OpenAI protocol and the client requested streaming.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_translating_anthropic_stream(
    state: Arc<AppState>,
    classification: crate::classification::types::ClassificationResult,
    body_str: String,
    prompt: String,
    start: std::time::Instant,
    byte_stream: impl futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    keepalive_interval_secs: u64,
    provider_attempts: u8,
    final_provider: String,
    session_id: Option<String>,
) -> Response {
    let channel_capacity = state.streaming_channel_capacity;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(channel_capacity);

    crate::proxy::util::log_classification(
        &state,
        &classification,
        &body_str,
        &prompt,
        start,
        "streaming",
        provider_attempts,
        &final_provider,
    );

    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(keepalive_interval_secs));
        let mut stream = byte_stream;
        let mut stream_status = "ok";
        let mut translate_state = crate::protocol::stream::AnthropicStreamState::default();
        let mut buffer = Vec::new();
        interval.tick().await;
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            buffer.extend_from_slice(&bytes);
                            const MAX_SSE_BUFFER: usize = 1024 * 1024;
                            if buffer.len() > MAX_SSE_BUFFER {
                                let sse_error = super::util::format_sse_error_event("SSE buffer exceeded 1 MB limit");
                                let _ = tx.send(Bytes::from(sse_error)).await;
                                stream_status = "buffer_overflow";
                                break;
                            }
                            let events = crate::protocol::stream::parse_sse_events(&buffer);
                            if !events.is_empty() {
                                if let Some(last_boundary) = buffer.windows(2).rposition(|w| w == b"\n\n") {
                                    buffer.drain(..last_boundary + 2);
                                } else {
                                    buffer.clear();
                                }
                                for (event_type, data) in &events {
                                    if let Some(anthropic_events) =
                                        crate::protocol::stream::openai_to_anthropic_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        if tx.send(Bytes::from(anthropic_events)).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = super::util::format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => {
                            if !buffer.is_empty() {
                                let events = crate::protocol::stream::parse_sse_events(&buffer);
                                for (event_type, data) in &events {
                                    if let Some(anthropic_events) =
                                        crate::protocol::stream::openai_to_anthropic_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        let _ = tx.send(Bytes::from(anthropic_events)).await;
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                        break;
                    }
                }
            }
        }
        // Finalize the inference record at stream close with the usage
        // accumulated from the terminal OpenAI usage chunk. Only emit token
        // fields when the stream actually started — otherwise it errored
        // before the upstream produced any usage.
        let usage = if translate_state.message_started {
            let (inp, out, cr, cc) = translate_state.collected_usage();
            Some(crate::proxy::util::UsageBreakdown {
                input_tokens: inp as i32,
                output_tokens: out as i32,
                cache_read_tokens: cr as i32,
                cache_creation_tokens: cc as i32,
            })
        } else {
            None
        };
        crate::proxy::util::log_classification_with_usage(
            &state,
            &classification,
            &body_str,
            &prompt,
            start,
            stream_status,
            provider_attempts,
            &final_provider,
            usage.as_ref(),
            session_id.as_deref(),
        );
    });

    let body =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>));

    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use crate::{auth, classification, config};
    use crate::app::build_app;
    use std::sync::Arc;
    
    use crate::app::test_helpers::{test_categories, test_negative_patterns, make_test_app_state};
    use crate::proxy::util::format_sse_error_event;
    use crate::proxy::handlers::tests::test_app_with_http_client;
    use crate::test_util::EnvGuard;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_returns_sse_content_type() {
        let env = "TEST_STREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let sse_body = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "text/event-stream").body(sse_body);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).expect("response should have Content-Type");
        assert_eq!(content_type, "text/event-stream");
        let cache_control = response.headers().get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).expect("response should have Cache-Control");
        assert_eq!(cache_control, "no-cache");
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains("data:"));
        assert!(body.contains("[DONE]"));
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_forwards_upstream_bytes() {
        let env = "TEST_STREAM_FWD";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let sse_chunks = "data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "text/event-stream").body(sse_chunks);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#"content":"A""#));
        assert!(body.contains(r#"content":"B""#));
        assert!(body.contains("[DONE]"));
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_non_2xx_returns_sse_error_event() {
        let env = "TEST_STREAM_ERR";
        let _env_guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503).header("content-type", "application/json").body(r#"{"error":"overloaded"}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.starts_with("event: error"));
        mock.assert();
    }

    #[test]
    fn test_format_sse_error_event_plain_text() {
        let s = format_sse_error_event("hello");
        assert_eq!(s, "event: error\ndata: {\"error\":\"hello\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_escapes_backslash() {
        let s = format_sse_error_event(r"a\b");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a\\\\b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_escapes_double_quote() {
        let s = format_sse_error_event("a\"b");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a\\\"b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_newline_with_space() {
        let s = format_sse_error_event("a\nb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_carriage_return_with_space() {
        let s = format_sse_error_event("a\rb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_combined_injection_produces_valid_json() {
        let s = format_sse_error_event("\";\n}\nattack\n\r{");
        let json_str = s.strip_prefix("event: error\ndata: ").and_then(|s| s.strip_suffix("\n\n")).expect("SSE event should have correct framing");
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("data: payload should be valid JSON");
        assert_eq!(parsed, serde_json::json!({"error": "\"; } attack  {"}));
    }

    #[test]
    fn test_format_sse_error_event_replaces_tab_with_space() {
        let s = format_sse_error_event("a\tb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_backspace_with_space() {
        let s = format_sse_error_event("a\x08\x08");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a  \"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_form_feed_with_space() {
        let s = format_sse_error_event("a\x0Cb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_other_control_chars_with_space() {
        let s = format_sse_error_event("a\x01b\x1Fc");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b c\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_preserves_printable_ascii() {
        let s = format_sse_error_event("Hello, World! 123 ~`@#$%^&*()");
        assert_eq!(s, "event: error\ndata: {\"error\":\"Hello, World! 123 ~`@#$%^&*()\"}\n\n");
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_truncates_oversized_body() {
        let env = "TEST_STREAM_TRUNC";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let large_body = "x".repeat(3_000);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503).header("content-type", "application/json").body(large_body);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.len() <= 2 * 1024 + 64);
        assert!(body.starts_with("event: error"));
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_escapes_json_injection() {
        let env = "TEST_STREAM_ESC";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503).header("content-type", "application/json").body(r#"{"error":"a\"b\\c\nd"}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        let json_str = body.strip_prefix("event: error\ndata: ").and_then(|s| s.strip_suffix("\n\n")).expect("SSE framing");
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("data: payload should be valid JSON");
        let error_value = parsed.get("error").and_then(|v| v.as_str()).expect("error field should be a string");
        assert_eq!(error_value, r#"{"error":"a\"b\\c\nd"}"#);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_content_type_and_cache_control() {
        let env = "TEST_STREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503).header("content-type", "application/json").body(r#"{"error":"overloaded"}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(content_type, "text/event-stream");
        let cache_control = response.headers().get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(cache_control, "no-cache");
        mock.assert();
    }

    async fn assert_status_passthrough(status: u16) {
        let env = "TEST_STREAM_ST";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(status).header("content-type", "application/json").body(r#"{"error":"upstream"}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status().as_u16(), status);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_429() { assert_status_passthrough(429).await; }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_500() { assert_status_passthrough(500).await; }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_502() { assert_status_passthrough(502).await; }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_503() { assert_status_passthrough(503).await; }

    #[tokio::test]
    #[serial]
    async fn test_inline_mid_stream_error_uses_same_format() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let env = "TEST_INLINE_ERR";
        let _env_guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test listener should bind");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");

        let server_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 1000\r\n\r\n";
            sock.write_all(headers.as_bytes()).await.expect("headers");
            sock.flush().await.expect("flush headers");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            sock.write_all(b"data: he").await.expect("first chunk");
            sock.flush().await.expect("flush first chunk");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(sock);
        });

        let cats = test_categories();
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().expect("test reqwest client should build");
        let mut routing = std::collections::HashMap::new();
        routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "sf-model".to_string(), endpoint: url, provider_type: "openai_compatible".to_string(), api_key_env: Some(env.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
        let app_state = make_test_app_state(regex_classifier, Some(client), config::routing::ModelCosts::empty(), String::new(), 10_485_760);
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let app = build_app(auth_config, app_state);

        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.starts_with("data: he"));
        assert!(body.contains("event: error\ndata: {\"error\":"));
        let data_line = body.split('\n').find(|line| line.starts_with("data: ") && line.contains("\"error\"")).expect("expected an SSE data: line with the error event");
        let json_str = data_line.trim_start_matches("data: ");
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("SSE error data: must be valid JSON");
        assert!(parsed.get("error").and_then(|v| v.as_str()).is_some());

        match tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => panic!("server task panicked: {e:?}"),
            Err(_) => panic!("server task did not complete within 2s"),
        }
    }

    mod slow_tests {
        use crate::{auth, classification, config};
        use crate::app::{AppState, build_app};
        use std::sync::Arc;
        use std::collections::HashMap;
        use tokio::sync::RwLock;
        use crate::app::test_helpers::{test_categories, test_negative_patterns};
        use axum::{
            body::Body,
            http::{header, Request, StatusCode},
            routing::get,
            Router,
        };
        use serial_test::serial;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tower::util::ServiceExt;
        use crate::test_util::EnvGuard;

        async fn spawn_slow_sse_server() -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}/v1/chat/completions");
            let handle = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let body = "data: hello\n\n";
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            });
            (url, handle)
        }

        fn build_keepalive_app(url: String, env_var: &'static str) -> Router {
            let _ = tracing_subscriber::fmt().with_test_writer().try_init();
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().unwrap();
            let cats = test_categories();
            let mut routing = std::collections::HashMap::new();
            routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry { model: "sf-model".to_string(), endpoint: url, provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var.to_string()), timeout_ms: None }],
                cost_per_1m_input_tokens: None,
            });
            let fallback = config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
                cost_per_1m_input_tokens: None,
            };
            let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
            let model_costs = config::routing::ModelCosts::empty();
            let baseline_model = String::new();
            let classifier_chain = classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
            let classifier = Some(Arc::new(classifier_chain));
            let mut merged_routing = HashMap::new();
            if let Some(cls) = classifier.as_ref() {
                for backend in cls.backends().iter() {
                    if let Some(r) = backend.get_routing() { merged_routing.extend(r.clone()); }
                }
            }
            let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
            let app_state = Arc::new(AppState {
                persistence: None, classifier, fewshot_classifier: None,
                routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
                model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
                baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
                classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                http_client: Some(client),
                max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
                keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(1)),
                request_body_limit_bytes: 10_485_760,
                streaming_channel_capacity: 32,
                dashboard_config: config::types::DashboardConfig::default(),
                auth_providers: Arc::new(vec![]),
                allowed_origins: Arc::new(RwLock::new(vec![])),
                response_cache: None,
                #[cfg(feature = "otel")]
                metrics: None,
            });
            build_app(auth_config, app_state)
        }

        fn count_anchored_keepalives(body: &str) -> usize {
            body.split('\n').filter(|line| *line == ": keepalive").count()
        }

        async fn spawn_fast_sse_server() -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}/v1/chat/completions");
            let handle = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let body = "data: hello\n\n";
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
            (url, handle)
        }

        async fn spawn_chunk_then_idle_sse_server() -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}/v1/chat/completions");
            let handle = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.write_all(b"data: chunk1\n\n").await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let _ = sock.write_all(b"data: chunk2\n\n").await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            });
            (url, handle)
        }

        async fn spawn_long_stall_sse_server() -> (String, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}/v1/chat/completions");
            let handle = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
                let body = "data: hello\n\n";
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
            (url, handle)
        }

        #[tokio::test]
        #[serial]
        async fn test_streaming_keepalive_injected() {
            let (url, server_handle) = spawn_slow_sse_server().await;
            let env = "TEST_STREAM_KA_SLOW";
            let _guard = EnvGuard(env);
            std::env::set_var(env, "sk-test");
            let app = build_keepalive_app(url, env);
            let response = app.oneshot(
                Request::builder().method("POST").uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
            ).await.expect("request should succeed");
            assert_eq!(response.status(), StatusCode::OK);
            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
            let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
            assert!(count_anchored_keepalives(body) >= 1);
            assert!(body.contains("data: hello"));
            let _ = server_handle.await;
        }

        #[tokio::test]
        #[serial]
        async fn test_streaming_keepalive_not_injected_when_upstream_fast() {
            let (url, server_handle) = spawn_fast_sse_server().await;
            let env = "TEST_STREAM_KA_FAST";
            let _guard = EnvGuard(env);
            std::env::set_var(env, "sk-test");
            let app = build_keepalive_app(url, env);
            let response = app.oneshot(
                Request::builder().method("POST").uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
            ).await.expect("request should succeed");
            assert_eq!(response.status(), StatusCode::OK);
            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
            let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
            assert_eq!(count_anchored_keepalives(body), 0);
            assert!(body.contains("data: hello"));
            let _ = server_handle.await;
        }

        #[tokio::test]
        #[serial]
        async fn test_streaming_keepalive_injected_alongside_chunk() {
            let (url, server_handle) = spawn_chunk_then_idle_sse_server().await;
            let env = "TEST_STREAM_KA_CHUNK";
            let _guard = EnvGuard(env);
            std::env::set_var(env, "sk-test");
            let app = build_keepalive_app(url, env);
            let response = app.oneshot(
                Request::builder().method("POST").uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
            ).await.expect("request should succeed");
            assert_eq!(response.status(), StatusCode::OK);
            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
            let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
            assert!(body.contains("data: chunk1"));
            assert!(body.contains("data: chunk2"));
            assert!(count_anchored_keepalives(body) >= 1);
            let _ = server_handle.await;
        }

        #[tokio::test]
        #[serial]
        async fn test_streaming_keepalive_multiple_consecutive() {
            let (url, server_handle) = spawn_long_stall_sse_server().await;
            let env = "TEST_STREAM_KA_LONG";
            let _guard = EnvGuard(env);
            std::env::set_var(env, "sk-test");
            let app = build_keepalive_app(url, env);
            let response = app.oneshot(
                Request::builder().method("POST").uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
            ).await.expect("request should succeed");
            assert_eq!(response.status(), StatusCode::OK);
            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
            let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
            assert!(count_anchored_keepalives(body) >= 3);
            assert!(body.contains("data: hello"));
            let _ = server_handle.await;
        }

        #[tokio::test]
        #[serial]
        async fn test_graceful_shutdown() {
            use std::time::Duration;
            use tokio::sync::oneshot;
            let app = Router::new().route("/slow", get(|| async { tokio::time::sleep(Duration::from_secs(2)).await; "OK" }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let server = axum::serve(listener, app).with_graceful_shutdown(async move { shutdown_rx.await.ok(); });
            let server_task = tokio::spawn(async move { server.await.expect("server task"); });
            tokio::time::sleep(Duration::from_millis(100)).await;
            let client = reqwest::Client::new();
            let resp = client.get(format!("http://{}/slow", addr)).send().await.unwrap();
            shutdown_tx.send(()).unwrap();
            let body = resp.text().await.unwrap();
            assert_eq!(body, "OK");
            server_task.await.unwrap();
        }
    }
}
