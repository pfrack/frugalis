use std::sync::Arc;

use axum::body::Body;
use axum::body::Bytes;
use axum::response::Response;
use futures::StreamExt;
use std::convert::Infallible;

use crate::app::AppState;
use crate::protocol::responses_stream::ResponsesStreamState;

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_responses_streaming_response(
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
    codex_installation_id: Option<String>,
    codex_turn_state: Option<String>,
    codex_window_id: Option<String>,
    codex_turn_metadata: Option<String>,
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

        let mut responses_state = ResponsesStreamState::new();

        // Emit response.created first
        let created_event = responses_state.emit_created();
        let _ = tx.send(Bytes::from(created_event.to_sse_bytes())).await;

        // Buffer for SSE event accumulation (handle split chunks)
        let mut buffer = String::new();
        const MAX_BUFFER_SIZE: usize = 1_048_576; // 1 MB cap

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            // Append chunk to buffer
                            buffer.push_str(&String::from_utf8_lossy(&bytes));

                            // Process complete events up to last \n\n
                            while let Some(pos) = buffer.find("\n\n") {
                                let event_line = buffer[..pos].to_string();
                                buffer.drain(..pos + 2); // remove event + both \n\n bytes

                                if event_line.starts_with("data: ") || event_line == "data: [DONE]" || event_line == "[DONE]" {
                                    let events = crate::protocol::responses_stream::translate_chat_chunk_to_responses_events(
                                        &mut responses_state,
                                        &event_line,
                                    );
                                    for event in events {
                                        let _ = tx.send(Bytes::from(event.to_sse_bytes())).await;
                                    }
                                } else if event_line.starts_with(":") {
                                    // Keepalive comment — forward as-is with \n\n suffix
                                    let _ = tx.send(Bytes::from(event_line + "\n\n")).await;
                                }
                            }

                            // Enforce buffer size cap
                            if buffer.len() > MAX_BUFFER_SIZE {
                                stream_status = "stream_error";
                                let err_msg = format!("SSE buffer exceeded {} bytes", MAX_BUFFER_SIZE);
                                let sse_error = crate::proxy::util::format_sse_error_event(&err_msg);
                                let _ = tx.send(Bytes::from(sse_error)).await;
                                break;
                            }
                        }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = crate::proxy::util::format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => {
                            // Stream ended — emit response.completed if not already
                            if !responses_state.finished {
                                let final_event = crate::protocol::responses_stream::finalize_stream(&responses_state);
                                let _ = tx.send(Bytes::from(final_event.to_sse_bytes())).await;
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

        crate::proxy::util::log_classification_with_usage_and_prev(
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
            None, // previous_response_id
            codex_installation_id.as_deref(),
            codex_turn_state.as_deref(),
            codex_window_id.as_deref(),
            codex_turn_metadata.as_deref(),
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

/// Streaming handler for Anthropic upstream → Responses SSE output.
///
/// Receives Anthropic SSE (content_block_delta / thinking_delta / message_delta)
/// and applies two translation stages in a single task:
///   1. Anthropic SSE → Chat SSE chunk  (via `protocol::stream::translate_stream_event`)
///   2. Chat SSE chunk → Responses SSE events  (via `protocol::responses_stream`)
///
/// This is the R2 streaming path: Responses → Chat → Anthropic → Chat → Responses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_responses_anthropic_streaming_response(
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
    codex_installation_id: Option<String>,
    codex_turn_state: Option<String>,
    codex_window_id: Option<String>,
    codex_turn_metadata: Option<String>,
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

        // Stage 1 state: Anthropic SSE → Chat SSE
        let mut translate_state = crate::protocol::stream::StreamTranslateState::default();
        // Stage 2 state: Chat SSE → Responses SSE
        let mut responses_state = ResponsesStreamState::new();

        // Emit response.created before any upstream bytes reach the client
        let created_event = responses_state.emit_created();
        let _ = tx.send(Bytes::from(created_event.to_sse_bytes())).await;

        let mut raw_buffer: Vec<u8> = Vec::new();
        const MAX_BUFFER_SIZE: usize = 1_048_576; // 1 MB cap

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            raw_buffer.extend_from_slice(&bytes);
                            if raw_buffer.len() > MAX_BUFFER_SIZE {
                                stream_status = "stream_error";
                                let sse_error = crate::proxy::util::format_sse_error_event("SSE buffer exceeded 1 MB limit");
                                let _ = tx.send(Bytes::from(sse_error)).await;
                                break;
                            }

                            // Stage 1: parse and translate Anthropic SSE events to Chat chunks
                            let events = crate::protocol::stream::parse_sse_events(&raw_buffer);
                            if !events.is_empty() {
                                // Drain only up to last complete event boundary
                                if let Some(last_boundary) = raw_buffer.windows(2).rposition(|w| w == b"\n\n") {
                                    raw_buffer.drain(..last_boundary + 2);
                                } else {
                                    raw_buffer.clear();
                                }
                                for (event_type, data) in &events {
                                    if let Some(chat_chunk) =
                                        crate::protocol::stream::translate_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        // Stage 2: translate Chat chunk to Responses events
                                        let resp_events = crate::protocol::responses_stream::translate_chat_chunk_to_responses_events(
                                            &mut responses_state,
                                            chat_chunk.trim_end(),
                                        );
                                        for event in resp_events {
                                            let _ = tx.send(Bytes::from(event.to_sse_bytes())).await;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = crate::proxy::util::format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => {
                            // Stream ended — flush remaining buffer
                            if !raw_buffer.is_empty() {
                                let events = crate::protocol::stream::parse_sse_events(&raw_buffer);
                                for (event_type, data) in &events {
                                    if let Some(chat_chunk) =
                                        crate::protocol::stream::translate_stream_event(
                                            event_type,
                                            data,
                                            &mut translate_state,
                                        )
                                    {
                                        let resp_events = crate::protocol::responses_stream::translate_chat_chunk_to_responses_events(
                                            &mut responses_state,
                                            chat_chunk.trim_end(),
                                        );
                                        for event in resp_events {
                                            let _ = tx.send(Bytes::from(event.to_sse_bytes())).await;
                                        }
                                    }
                                }
                            }
                            if !responses_state.finished {
                                let final_event = crate::protocol::responses_stream::finalize_stream(&responses_state);
                                let _ = tx.send(Bytes::from(final_event.to_sse_bytes())).await;
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

        crate::proxy::util::log_classification_with_usage_and_prev(
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
            None, // previous_response_id
            codex_installation_id.as_deref(),
            codex_turn_state.as_deref(),
            codex_window_id.as_deref(),
            codex_turn_metadata.as_deref(),
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
