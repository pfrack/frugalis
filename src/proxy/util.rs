use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use tracing::debug;

use crate::app::AppState;
use crate::classification::chain::IntentClassify;

#[cfg(feature = "otel")]
use crate::proxy::handlers::RequestMetrics;

#[cfg(feature = "otel")]
use opentelemetry::KeyValue;

/// Captured token usage from an upstream response, used to populate the token
/// fields of an InferenceRecord. Fields hold the upstream's raw values; the
/// extraction helpers normalize OpenAI's `cached_tokens` into the Anthropic
/// `cache_read` shape so the record is protocol-agnostic.
#[derive(Clone, Default)]
pub(crate) struct UsageBreakdown {
    pub(crate) input_tokens: i32,
    pub(crate) output_tokens: i32,
    pub(crate) cache_read_tokens: i32,
    pub(crate) cache_creation_tokens: i32,
}

/// Strip fields that NVIDIA NIM rejects before forwarding translated requests.
/// Called after protocol translation but before `client.post()`.
pub(crate) fn sanitize_for_nim(body: &mut serde_json::Value) {
    if let Some(obj) = body.as_object_mut() {
        for key in &["top_k", "metadata", "thinking"] {
            if obj.remove(*key).is_some() {
                debug!("NIM sanitization: stripped '{}' field", key);
            }
        }
    }
}

/// Check if the request matches a known trivial probe pattern and return
/// a canned response, avoiding the full classification + upstream round-trip.
/// Returns `None` if the request should proceed normally.
pub(crate) fn try_optimize_request(body: &[u8], is_anthropic: bool) -> Option<Response> {
    // Skip deserialization entirely for large bodies — probe patterns
    // only match when body <512 bytes, so this avoids wasted parse work.
    if body.len() >= 512 {
        return None;
    }
    let val: serde_json::Value = serde_json::from_slice(body).ok()?;
    let messages = val.get("messages")?.as_array()?;

    // Empty messages array → return empty assistant response
    if messages.is_empty() {
        debug!("Request optimization: empty messages array, returning canned response");
        let resp_body = if is_anthropic {
            serde_json::json!({
                "id": "msg_optimized",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "frugalis-optimized",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 0, "output_tokens": 0}
            })
        } else {
            serde_json::json!({
                "id": "chatcmpl-optimized",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": ""},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
            })
        };
        return Some(json_response(StatusCode::OK, resp_body.to_string()));
    }

    // Single-message known probe patterns — only match when the entire
    // Single-message probe patterns. Skip streaming requests — real probes never stream.
    if messages.len() == 1 && val.get("stream") != Some(&serde_json::Value::Bool(true)) {
        let content = messages[0].get("content")?;
        let text = if let Some(s) = content.as_str() {
            s.trim().to_lowercase()
        } else if let Some(arr) = content.as_array() {
            if arr.len() == 1 {
                arr[0].get("text")?.as_str()?.trim().to_lowercase()
            } else {
                return None;
            }
        } else {
            return None;
        };

        if matches!(text.as_str(), "hello" | "hi" | "test" | "hey") {
            debug!(
                "Request optimization: matched probe pattern '{}', returning canned response",
                text
            );
            let resp_body = if is_anthropic {
                serde_json::json!({
                    "id": "msg_optimized",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Hello! How can I help you today?"}],
                    "model": "frugalis-optimized",
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 1, "output_tokens": 8}
                })
            } else {
                serde_json::json!({
                    "id": "chatcmpl-optimized",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Hello! How can I help you today?"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 8, "total_tokens": 9}
                })
            };
            return Some(json_response(StatusCode::OK, resp_body.to_string()));
        }
    }

    None
}

/// Extract a `UsageBreakdown` from an Anthropic-shaped response body's
/// `usage` object. Returns `None` when the body carries no usage (e.g.
/// non-success or untranslated error bodies). Cache fields default to 0 when
/// the upstream omits them (no caching active).
pub(crate) fn extract_anthropic_usage(body: &serde_json::Value) -> Option<UsageBreakdown> {
    let usage = body.get("usage")?;
    Some(UsageBreakdown {
        input_tokens: usage
            .get("input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        output_tokens: usage
            .get("output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        cache_read_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        cache_creation_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
    })
}

/// Extract a `UsageBreakdown` from an OpenAI-shaped response body's `usage`
/// object. OpenAI reports cache hits as
/// `usage.prompt_tokens_details.cached_tokens` (a subset of `prompt_tokens`);
/// we map that to `cache_read_tokens` and derive the non-cached `input_tokens`
/// so the record matches Anthropic semantics. `cache_creation_tokens` is 0
/// (OpenAI has no creation concept).
pub(crate) fn extract_openai_usage(body: &serde_json::Value) -> Option<UsageBreakdown> {
    let usage = body.get("usage")?;
    let prompt = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some(UsageBreakdown {
        input_tokens: (prompt - cached) as i32,
        output_tokens: usage
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        cache_read_tokens: cached as i32,
        cache_creation_tokens: 0,
    })
}

/// Parse a response body string and extract usage in the protocol the client
/// sees: `anthropic_shape = true` for `/v1/messages` traffic (Anthropic usage),
/// `false` for `/v1/chat/completions` traffic (OpenAI usage). Returns `None`
/// when the body is not valid JSON or carries no usage object — callers log
/// without usage in that case.
pub(crate) fn parse_usage_from_body(body: &str, anthropic_shape: bool) -> Option<UsageBreakdown> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if anthropic_shape {
        extract_anthropic_usage(&v)
    } else {
        extract_openai_usage(&v)
    }
}

/// Extract the Claude Code session id (`x-claude-code-session-id`) from the
/// forwarded-header set collected at the handler entry. Returns `None` when
/// the client did not send it.
pub(crate) fn session_id_from_forward(forward_headers: &[(String, String)]) -> Option<&str> {
    forward_headers
        .iter()
        .find(|(n, _)| n == "x-claude-code-session-id")
        .map(|(_, v)| v.as_str())
}

/// Shared logging helper. Builds the inference record from the pre-extracted
/// `prompt` and enqueues a fire-and-forget DB write. Callers must extract the
/// prompt themselves (via `crate::persistence::extract_last_user_message` for OpenAI
/// traffic or `crate::persistence::extract_last_user_message_anthropic` for Anthropic
/// traffic) so the persistence log records the same prompt the classifier saw.
///
/// This 8-argument variant records NO token usage and NO session attribution
/// (the new InferenceRecord fields are left `None`). It is the right call for
/// error / boundary paths where no upstream usage is available. Success paths
/// that have parsed the response body should call `log_classification_with_usage`
/// instead so the token counts and Claude Code session id are captured.
#[allow(clippy::too_many_arguments)]
pub(crate) fn log_classification(
    state: &AppState,
    classification: &crate::classification::types::ClassificationResult,
    _body_str: &str,
    prompt: &str,
    start: std::time::Instant,
    log_status: &str,
    provider_attempts: u8,
    final_provider: &str,
) {
    enqueue_inference_record(
        state,
        classification,
        prompt,
        start,
        log_status,
        provider_attempts,
        final_provider,
        None,
        None,
    );
}

/// Success-path logging variant that captures token usage and the Claude Code
/// session id into the InferenceRecord. Use this once the upstream response
/// body has been parsed (non-streaming) or the stream has closed with a
/// terminal usage chunk (streaming). `usage` / `session_id` may be `None`
/// when that datum was not available for this request.
#[allow(clippy::too_many_arguments)]
pub(crate) fn log_classification_with_usage(
    state: &AppState,
    classification: &crate::classification::types::ClassificationResult,
    _body_str: &str,
    prompt: &str,
    start: std::time::Instant,
    log_status: &str,
    provider_attempts: u8,
    final_provider: &str,
    usage: Option<&UsageBreakdown>,
    session_id: Option<&str>,
) {
    enqueue_inference_record(
        state,
        classification,
        prompt,
        start,
        log_status,
        provider_attempts,
        final_provider,
        usage,
        session_id,
    );
}

/// Build the InferenceRecord (with optional token usage + session id) and
/// enqueue the fire-and-forget DB write. Shared by `log_classification` and
/// `log_classification_with_usage` so the two public entry points cannot drift.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_inference_record(
    state: &AppState,
    classification: &crate::classification::types::ClassificationResult,
    prompt: &str,
    start: std::time::Instant,
    log_status: &str,
    provider_attempts: u8,
    final_provider: &str,
    usage: Option<&UsageBreakdown>,
    session_id: Option<&str>,
) {
    if let Some(persistence) = &state.persistence {
        let duration_ms = start.elapsed().as_millis() as i32;
        // Snippet is the 200-char privacy-safe truncation of the FULL prompt,
        // not the body — bodies may contain system prompts, tool calls, etc.
        let snippet: String = prompt.chars().take(200).collect();
        let prompt_char_count = if prompt.is_empty() {
            None
        } else {
            Some(prompt.chars().count() as i32)
        };
        let (input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens) = match usage {
            Some(u) => (
                Some(u.input_tokens),
                Some(u.output_tokens),
                Some(u.cache_read_tokens),
                Some(u.cache_creation_tokens),
            ),
            None => (None, None, None, None),
        };
        let record = crate::persistence::InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: log_status.to_string(),
            category: Some(classification.category.clone()),
            upstream_model: Some(classification.model.clone()),
            duration_ms: Some(duration_ms),
            prompt_snippet: snippet,
            prompt_char_count,
            created_at: chrono::Utc::now(),
            provider_attempts,
            final_provider: final_provider.to_string(),
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            client_session_id: session_id.map(|s| s.to_string()),
        };
        crate::persistence::log_inference(
            persistence.backend.clone(),
            persistence.task_semaphore.clone(),
            record,
        );
    }
}

/// Shared classify-and-log logic. Validates Content-Type, extracts the prompt,
/// classifies intent, builds the JSON response, and optionally enqueues a
/// fire-and-forget inference record with the given `log_status`.
pub(crate) async fn classify_and_log(
    headers: &HeaderMap,
    body_str: &str,
    start: std::time::Instant,
    state: &AppState,
    log_status: Option<&str>,
) -> Response {
    #[cfg(feature = "otel")]
    let mut rm = RequestMetrics::new(state.metrics.clone(), "POST", "/v1/classify");

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        return json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"error":"bad_request","status":415,"message":"expected application/json"}"#
                .to_string(),
        );
    }

    let prompt = crate::persistence::extract_last_user_message(body_str);

    let classification = match state.classifier.as_ref() {
        Some(c) => c.classify(&prompt).await,
        None => crate::classification::types::ClassificationResult::fallback(),
    };

    #[cfg(feature = "otel")]
    if let Some(ref metrics) = state.metrics {
        metrics.classification_total.add(
            1,
            &[
                KeyValue::new("category", classification.category.clone()),
                KeyValue::new("tier", format!("{:?}", classification.tier)),
            ],
        );
    }

    let response_body = serde_json::json!({
        "status": "classified",
        "category": classification.category,
        "model": classification.model,
        "tier": format!("{:?}", classification.tier),
    })
    .to_string();
    if let Some(log_status) = log_status {
        log_classification(
            state,
            &classification,
            body_str,
            &prompt,
            start,
            log_status,
            1,
            "",
        );
    }

    json_response(StatusCode::OK, response_body)
}

pub(crate) fn json_response(status: StatusCode, body: String) -> Response<Body> {
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::header::HeaderValue::from_static("application/json"),
    );
    resp
}

pub(crate) fn upstream_error_json(status: u16, message: &str) -> String {
    serde_json::json!({
        "error": "upstream_error",
        "status": status,
        "message": message,
    })
    .to_string()
}

/// Anthropic-shaped error body for the proxy's own errors (auth failure,
/// bad request, no endpoint). Anthropic-speaking clients expect
/// `{"type": "error", "error": {"type": "...", "message": "..."}}` so we
/// match that envelope rather than the OpenAI-shaped `upstream_error_json`.
/// `error_type` is the Anthropic error type, e.g. `"authentication_error"`,
/// `"invalid_request_error"`, `"api_error"`. Status passthrough happens at
/// the HTTP layer (this body is wrapped in a `StatusCode` from the caller).
pub(crate) fn anthropic_error_json(error_type: &str, message: &str) -> String {
    serde_json::json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message,
        }
    })
    .to_string()
}

pub(crate) fn classification_only_json(result: &crate::classification::types::ClassificationResult) -> String {
    serde_json::json!({
        "status": "classified",
        "category": result.category,
        "model": result.model,
        "tier": format!("{:?}", result.tier),
    })
    .to_string()
}

/// Collect inbound headers that must be forwarded to Anthropic upstreams as an
/// open list: any header whose lower-cased name begins with `anthropic-` or
/// `x-claude-code-`. This is the single extraction point both proxy handlers
/// reuse before threading the result into `auth_headers_for` / upstream
/// construction.
///
/// SECURITY INVARIANT: never include `authorization` or `x-api-key`. Those
/// carry the proxy's own inbound credential (consumed by the auth middleware)
/// and forwarding them would let a client overwrite the resolved upstream key.
/// The prefix allowlist already excludes them — keep the prefixes restrictive
/// (do not broaden to a blind copy of all inbound headers). When the same name
/// appears multiple times, the first value wins so downstream emission stays
/// deterministic.
pub(crate) fn collect_forward_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in headers.iter() {
        let name_lower = name.as_str();
        if (name_lower.starts_with("anthropic-") || name_lower.starts_with("x-claude-code-"))
            && !out.iter().any(|(n, _)| *n == name_lower)
        {
            if let Ok(v) = value.to_str() {
                out.push((name_lower.to_string(), v.to_string()));
            }
        }
    }
    out
}

pub(crate) fn format_sse_error_event(error_msg: &str) -> String {
    let mut escaped = String::with_capacity(error_msg.len() * 2);
    for c in error_msg.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            c if (c as u32) < 0x20 => escaped.push(' '),
            _ => escaped.push(c),
        }
    }
    format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", escaped)
}
