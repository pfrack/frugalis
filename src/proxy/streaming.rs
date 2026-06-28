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
