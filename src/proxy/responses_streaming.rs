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
                                buffer.drain(..=pos); // remove event + trailing \n\n

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
