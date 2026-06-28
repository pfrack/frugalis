use crate::app::AppState;
use crate::classification::chain::IntentClassify;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::body::Bytes;
use std::sync::Arc;
use tracing::{debug, warn};

#[cfg(feature = "otel")]
use opentelemetry::KeyValue;

#[cfg(feature = "otel")]
pub(crate) struct RequestMetrics {
    metrics: Option<crate::telemetry::Metrics>,
    method: &'static str,
    route: &'static str,
    start: std::time::Instant,
    status: StatusCode,
}

#[cfg(feature = "otel")]
impl RequestMetrics {
    pub(crate) fn new(metrics: Option<crate::telemetry::Metrics>, method: &'static str, route: &'static str) -> Self {
        Self {
            metrics,
            method,
            route,
            start: std::time::Instant::now(),
            status: StatusCode::OK,
        }
    }
    pub(crate) fn set_status(&mut self, status: StatusCode) {
        self.status = status;
    }
}

#[cfg(feature = "otel")]
impl Drop for RequestMetrics {
    fn drop(&mut self) {
        if let Some(ref m) = self.metrics {
            let attrs = [
                KeyValue::new("method", self.method),
                KeyValue::new("route", self.route),
                KeyValue::new("status", self.status.as_u16().to_string()),
            ];
            m.requests_total.add(1, &attrs);
            m.request_duration_seconds
                .record(self.start.elapsed().as_secs_f64(), &attrs);
        }
    }
}

pub(crate) async fn health() -> (StatusCode, &'static str) {
    debug!("Health check request received");
    (StatusCode::OK, "ok")
}

/// POST /v1/messages/count_tokens — local token count approximation.
/// Extracts text content from the Anthropic messages array and returns
/// `total_chars / 4` as a cheap token estimate. Avoids upstream round-trips
/// for Claude Code's context window management.
pub(crate) async fn count_tokens_handler(body: Bytes) -> impl IntoResponse {
    debug!("POST /v1/messages/count_tokens request received");
    let total_chars: usize = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(val) => val
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|msgs| {
                msgs.iter()
                    .flat_map(|msg| msg.get("content"))
                    .flat_map(|content| {
                        if let Some(s) = content.as_str() {
                            // string content
                            Box::new(std::iter::once(s.len())) as Box<dyn Iterator<Item = usize>>
                        } else if let Some(arr) = content.as_array() {
                            // array of content blocks
                            Box::new(arr.iter().filter_map(|block| {
                                block.get("text").and_then(|t| t.as_str()).map(|s| s.len())
                            })) as Box<dyn Iterator<Item = usize>>
                        } else {
                            Box::new(std::iter::empty()) as Box<dyn Iterator<Item = usize>>
                        }
                    })
                    .sum::<usize>()
            })
            .unwrap_or(0),
        Err(_) => 0,
    };
    let estimated_tokens = total_chars / 4;
    crate::proxy::util::json_response(
        StatusCode::OK,
        serde_json::json!({"input_tokens": estimated_tokens}).to_string(),
    )
}

/// GET /v1/models — model list for Claude Code gateway discovery.
///
/// Returns Anthropic-shape entries (each carrying `display_name` and
/// `type: "model"`) so Claude Code's model picker — gated behind
/// `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` — shows friendly names
/// instead of raw IDs. Each entry also retains the OpenAI fields
/// (`object: "model"`, `owned_by`, `created`) so OpenAI clients hitting the
/// same endpoint are unaffected; a superset avoids content-negotiation
/// branching. IDs MUST begin with `claude` or `anthropic` to pass Claude
/// Code's discovery filter. Placed outside the auth layer so Claude Code can
/// probe before authenticating.
pub(crate) async fn models_handler() -> impl IntoResponse {
    debug!("GET /v1/models request received");
    let body = serde_json::json!({
        "object": "list",
        "has_more": false,
        "data": [
            {
                "type": "model",
                "object": "model",
                "id": "claude-sonnet-4-6-20250514",
                "display_name": "Claude Sonnet 4.6",
                "created_at": "2025-05-14T00:00:00Z",
                "created": 1700000000,
                "owned_by": "anthropic"
            },
            {
                "type": "model",
                "object": "model",
                "id": "claude-haiku-4-5-20250514",
                "display_name": "Claude Haiku 4.5",
                "created_at": "2025-05-14T00:00:00Z",
                "created": 1700000000,
                "owned_by": "anthropic"
            },
            {
                "type": "model",
                "object": "model",
                "id": "claude-opus-4-20250514",
                "display_name": "Claude Opus 4",
                "created_at": "2025-05-14T00:00:00Z",
                "created": 1700000000,
                "owned_by": "anthropic"
            }
        ]
    });
    crate::proxy::util::json_response(StatusCode::OK, body.to_string())
}

/// Completion handler: classifies intent, optionally skips classification via
/// X-Frugalis-Category / X-Frugalis-Model headers, resolves the API key from
/// the env var named by the classification result, builds auth headers,
/// overrides the model field, forwards the body to the upstream endpoint,
/// and returns the buffered response with Content-Type: application/json.
pub(crate) async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let start = std::time::Instant::now();

    #[cfg(feature = "otel")]
    let mut rm = RequestMetrics::new(state.metrics.clone(), "POST", "/v1/chat/completions");

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        return crate::proxy::util::json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"error":"bad_request","status":415,"message":"expected application/json"}"#
                .to_string(),
        );
    }

    let body_str: String = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => {
            #[cfg(feature = "otel")]
            rm.set_status(StatusCode::BAD_REQUEST);
            return crate::proxy::util::json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"bad_request","message":"invalid UTF-8 body"}"#.to_string(),
            );
        }
    };

    // Capture Claude Code / Anthropic client headers once; threaded into every
    // upstream attempt so beta-gated features and session attribution reach the
    // upstream. See `collect_forward_headers` for the security invariant.
    let forward_headers = crate::proxy::util::collect_forward_headers(&headers);
    // Claude Code session id for per-request attribution in the inference log.
    let session_id = crate::proxy::util::session_id_from_forward(&forward_headers);

    // Request optimization: skip if explicit routing headers are present —
    // explicit directives should take precedence over probe optimization.
    if headers.get("x-frugalis-category").is_none() && headers.get("x-frugalis-model").is_none() {
        if let Some(response) = crate::proxy::util::try_optimize_request(&body, false) {
            return response;
        }
    }

    // Cache check: after probe optimization, before classification.
    // Bypass via X-Frugalis-No-Cache header for client freshness control.
    let mut cache_key: Option<String> = None;
    if let Some(cache) = &state.response_cache {
        let no_cache = headers
            .get("x-frugalis-no-cache")
            .and_then(|v| v.to_str().ok())
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if no_cache {
            debug!("Cache bypass via X-Frugalis-No-Cache header");
        } else {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&body);
            let key = format!("{:x}", hasher.finalize());
            if let Some(entry) = cache.get(&key) {
                debug!("Cache hit for completion request");
                return crate::proxy::util::json_response(
                    StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK),
                    entry.body,
                );
            }
            debug!("Cache miss for completion request");
            cache_key = Some(key);
        }
    }

    let x_category = headers
        .get("x-frugalis-category")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let x_model = headers
        .get("x-frugalis-model")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Extract the prompt once and reuse it for both classification and the
    // persistence log. When X-Frugalis-Category/Model headers bypass the
    // classifier, we still log an empty prompt rather than re-extracting
    // — the classifier never ran, so there is nothing meaningful to log.
    let prompt = crate::persistence::extract_last_user_message(&body_str);

    let classification = if let (Some(category), Some(model)) =
        (x_category.as_ref(), x_model.as_ref())
    {
        let routing = state.routing.read().await;
        match routing.get(category) {
            Some(entry) => crate::classification::types::ClassificationResult {
                category: category.clone(),
                model: model.clone(),
                tier: crate::classification::types::ClassificationTier::Fallback,
                providers: entry.providers.clone(),
            },
            None => {
                warn!("X-Frugalis-Category '{category}' not found in routing configuration; degrading to classification JSON");
                let fallback = match state.classifier.as_ref() {
                    Some(c) => c.classify("").await,
                    None => crate::classification::types::ClassificationResult::fallback(),
                };
                let response_body = crate::proxy::util::classification_only_json(&fallback);
                crate::proxy::util::log_classification(&state, &fallback, &body_str, "", start, "ok", 1, "");
                return crate::proxy::util::json_response(StatusCode::OK, response_body);
            }
        }
    } else {
        match state.classifier.as_ref() {
            Some(c) => c.classify(&prompt).await,
            None => crate::classification::types::ClassificationResult::fallback(),
        }
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

    let client = match &state.http_client {
        Some(c) => c,
        None => {
            let response_body = crate::proxy::util::classification_only_json(&classification);
            crate::proxy::util::log_classification(
                &state,
                &classification,
                &body_str,
                &prompt,
                start,
                "ok",
                1,
                "",
            );
            return crate::proxy::util::json_response(StatusCode::OK, response_body);
        }
    };

    let mut last_error_response: Option<Response> = None;
    let total_providers = classification.providers.len();

    let providers_clone = classification.providers.clone();
    for (idx, provider) in providers_clone.iter().enumerate() {
        let is_last = idx + 1 >= total_providers;

        // Resolve API key for this provider
        let api_key = match &provider.api_key_env {
            Some(env_name) => match std::env::var(env_name) {
                Ok(key) if !key.is_empty() => key,
                _ => {
                    warn!(
                        "API key env var '{:?}' is missing or empty for provider {}; cascading",
                        provider.api_key_env, provider.model
                    );
                    if is_last {
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            "ok",
                            idx as u8 + 1,
                            &provider.model,
                        );
                        return crate::proxy::util::json_response(
                            StatusCode::OK,
                            crate::proxy::util::classification_only_json(&classification),
                        );
                    }
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::OK,
                        crate::proxy::util::classification_only_json(&classification),
                    ));
                    continue;
                }
            },
            None => {
                warn!(
                    "no api_key_env configured for provider {}; cascading",
                    provider.model
                );
                if is_last {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "ok",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    return crate::proxy::util::json_response(
                        StatusCode::OK,
                        crate::proxy::util::classification_only_json(&classification),
                    );
                }
                last_error_response = Some(crate::proxy::util::json_response(
                    StatusCode::OK,
                    crate::proxy::util::classification_only_json(&classification),
                ));
                continue;
            }
        };

        if provider.endpoint.is_empty() {
            warn!("empty endpoint for provider {}; cascading", provider.model);
            if is_last {
                crate::proxy::util::log_classification(
                    &state,
                    &classification,
                    &body_str,
                    &prompt,
                    start,
                    "upstream_error",
                    idx as u8 + 1,
                    &provider.model,
                );
                #[cfg(feature = "otel")]
                rm.set_status(StatusCode::BAD_GATEWAY);
                return crate::proxy::util::json_response(
                    StatusCode::BAD_GATEWAY,
                    crate::proxy::util::upstream_error_json(502, "no endpoint configured"),
                );
            }
            last_error_response = Some(crate::proxy::util::json_response(
                StatusCode::BAD_GATEWAY,
                crate::proxy::util::upstream_error_json(502, "no endpoint configured"),
            ));
            continue;
        }

        // ── Anthropic upstream: translate OpenAI → Anthropic ──────────
        if provider.provider_type == "anthropic" {
            let parsed_body: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::upstream_error_json(400, &format!("invalid JSON body: {e}")),
                    );
                }
            };

            let anthropic_body = match crate::protocol::request::translate_request(&parsed_body) {
                Ok(b) => b,
                Err(e) => {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::upstream_error_json(400, &e),
                    );
                }
            };

            let client_wants_stream = parsed_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let anthropic_bytes =
                Bytes::from(serde_json::to_vec(&anthropic_body).unwrap_or_default());

            let auth_headers = crate::classification::llm::auth_headers_for(
                &state.auth_providers,
                &provider.provider_type,
                &api_key,
                &forward_headers,
            );
            let mut upstream_req = client
                .post(&provider.endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .body(anthropic_bytes);
            for (name, value) in &auth_headers {
                upstream_req = upstream_req.header(name.as_str(), value.as_str());
            }
            if let Some(ms) = provider.timeout_ms {
                upstream_req = upstream_req.timeout(std::time::Duration::from_millis(ms));
            }

            #[cfg_attr(not(feature = "otel"), allow(unused_variables))]
            let upstream_start = std::time::Instant::now();
            let upstream_result = upstream_req.send().await;

            if is_last || !crate::proxy::upstream::is_retryable_error(&upstream_result) {
                // Last provider or non-retryable — handle or error
                match upstream_result {
                    Ok(upstream_response) => {
                        #[cfg(feature = "otel")]
                        if let Some(ref metrics) = state.metrics {
                            metrics.upstream_duration_seconds.record(
                                upstream_start.elapsed().as_secs_f64(),
                                &[
                                    KeyValue::new("provider", provider.provider_type.clone()),
                                    KeyValue::new(
                                        "status",
                                        upstream_response.status().as_u16().to_string(),
                                    ),
                                ],
                            );
                        }
                        if !upstream_response.status().is_success() {
                            if client_wants_stream {
                                let resp =
                                    crate::proxy::streaming::handle_anthropic_streaming_error(upstream_response)
                                        .await;
                                crate::proxy::util::log_classification(
                                    &state,
                                    &classification,
                                    &body_str,
                                    &prompt,
                                    start,
                                    "upstream_error",
                                    idx as u8 + 1,
                                    &provider.model,
                                );
                                #[cfg(feature = "otel")]
                                rm.set_status(resp.status());
                                return resp;
                            } else {
                                let max_upstream_body_bytes =
                                    *state.max_upstream_body_bytes.read().await;
                                let (status, response_body) =
                                    crate::proxy::upstream::translate_anthropic_buffered_response(
                                        upstream_response,
                                        max_upstream_body_bytes,
                                    )
                                    .await;
                                let log_status = if status == StatusCode::OK {
                                    "ok"
                                } else {
                                    "upstream_error"
                                };
                                let usage = if status == StatusCode::OK {
                                    crate::proxy::util::parse_usage_from_body(&response_body, false)
                                } else {
                                    None
                                };
                                crate::proxy::util::log_classification_with_usage(
                                    &state,
                                    &classification,
                                    &body_str,
                                    &prompt,
                                    start,
                                    log_status,
                                    idx as u8 + 1,
                                    &provider.model,
                                    usage.as_ref(),
                                    session_id,
                                );
                                #[cfg(feature = "otel")]
                                rm.set_status(status);
                                return crate::proxy::util::json_response(status, response_body);
                            }
                        }
                        if client_wants_stream {
                            let keepalive_interval_secs =
                                *state.keepalive_interval_secs.read().await;
                            return crate::proxy::streaming::handle_anthropic_streaming_response(
                                state,
                                classification,
                                body_str,
                                prompt,
                                start,
                                upstream_response.bytes_stream(),
                                keepalive_interval_secs,
                                idx as u8 + 1,
                                provider.model.clone(),
                                session_id.map(|s| s.to_string()),
                            );
                        }
                        let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                        let (status, response_body) =
                            crate::proxy::upstream::translate_anthropic_buffered_response(
                                upstream_response,
                                max_upstream_body_bytes,
                            )
                            .await;
                        let log_status = if status == StatusCode::OK {
                            "ok"
                        } else {
                            "upstream_error"
                        };
                        let usage = if status == StatusCode::OK {
                            crate::proxy::util::parse_usage_from_body(&response_body, false)
                        } else {
                            None
                        };
                        crate::proxy::util::log_classification_with_usage(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            log_status,
                            idx as u8 + 1,
                            &provider.model,
                            usage.as_ref(),
                            session_id,
                        );
                        #[cfg(feature = "otel")]
                        rm.set_status(status);
                        if status == StatusCode::OK {
                            if let Some(ref key) = cache_key {
                                if let Some(ref cache) = state.response_cache {
                                    cache.put(
                                        key.clone(),
                                        crate::cache::CachedEntry {
                                            body: response_body.clone(),
                                            content_type: "application/json".to_string(),
                                            status: 200,
                                        },
                                    );
                                }
                            }
                        }
                        return crate::proxy::util::json_response(status, response_body);
                    }
                    Err(e) => {
                        #[cfg(feature = "otel")]
                        if let Some(ref metrics) = state.metrics {
                            metrics.upstream_duration_seconds.record(
                                upstream_start.elapsed().as_secs_f64(),
                                &[
                                    KeyValue::new("provider", provider.provider_type.clone()),
                                    KeyValue::new("status", "502"),
                                ],
                            );
                        }
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            "upstream_error",
                            idx as u8 + 1,
                            &provider.model,
                        );
                        #[cfg(feature = "otel")]
                        rm.set_status(StatusCode::BAD_GATEWAY);
                        return crate::proxy::util::json_response(
                            StatusCode::BAD_GATEWAY,
                            crate::proxy::util::upstream_error_json(502, &e.to_string()),
                        );
                    }
                }
            } else {
                match &upstream_result {
                    Ok(upstream_response) => {
                        warn!(
                            "Provider {} returned {}; cascading to next",
                            provider.model,
                            upstream_response.status()
                        );
                        last_error_response = Some(crate::proxy::util::json_response(
                            upstream_response.status(),
                            crate::proxy::util::upstream_error_json(
                                upstream_response.status().as_u16(),
                                "upstream error",
                            ),
                        ));
                    }
                    Err(e) => {
                        warn!(
                            "Provider {} connection failed: {}; cascading to next",
                            provider.model, e
                        );
                        last_error_response = Some(crate::proxy::util::json_response(
                            StatusCode::BAD_GATEWAY,
                            crate::proxy::util::upstream_error_json(502, &e.to_string()),
                        ));
                    }
                }
                continue;
            }
        }

        // ── OpenAI-compatible upstream: pass through ──────────────────
        let provider_body = if provider.provider_type == "nvidia_nim" {
            match serde_json::from_slice::<serde_json::Value>(&body) {
                Ok(mut v) => {
                    crate::proxy::util::sanitize_for_nim(&mut v);
                    Bytes::from(serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec()))
                }
                Err(_) => body.clone(),
            }
        } else {
            body.clone()
        };

        let (client_wants_stream, upstream_req) = match crate::proxy::upstream::build_upstream_request(
            client,
            provider,
            &provider_body,
            &api_key,
            &state.auth_providers,
            &forward_headers,
        ) {
            Err(msg) => {
                if is_last {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::upstream_error_json(400, &msg),
                    );
                }
                last_error_response = Some(crate::proxy::util::json_response(
                    StatusCode::BAD_REQUEST,
                    crate::proxy::util::upstream_error_json(400, &msg),
                ));
                continue;
            }
            Ok(r) => r,
        };

        #[cfg_attr(not(feature = "otel"), allow(unused_variables))]
        let upstream_start = std::time::Instant::now();
        let upstream_result = upstream_req.send().await;

        if is_last || !crate::proxy::upstream::is_retryable_error(&upstream_result) {
            match upstream_result {
                Ok(upstream_response) => {
                    #[cfg(feature = "otel")]
                    if let Some(ref metrics) = state.metrics {
                        metrics.upstream_duration_seconds.record(
                            upstream_start.elapsed().as_secs_f64(),
                            &[
                                KeyValue::new("provider", provider.provider_type.clone()),
                                KeyValue::new(
                                    "status",
                                    upstream_response.status().as_u16().to_string(),
                                ),
                            ],
                        );
                    }
                    if !upstream_response.status().is_success() {
                        if client_wants_stream {
                            let resp =
                                crate::proxy::streaming::handle_streaming_error(upstream_response)
                                    .await;
                            crate::proxy::util::log_classification(
                                &state,
                                &classification,
                                &body_str,
                                &prompt,
                                start,
                                "upstream_error",
                                idx as u8 + 1,
                                &provider.model,
                            );
                            #[cfg(feature = "otel")]
                            rm.set_status(resp.status());
                            return resp;
                        }
                        let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                        let (status, resp_body) =
                            crate::proxy::upstream::handle_buffered_response(
                                upstream_response,
                                max_upstream_body_bytes,
                                false,
                            )
                            .await;
                        let log_status = if status == StatusCode::OK {
                            "ok"
                        } else {
                            "upstream_error"
                        };
                        let usage = if status == StatusCode::OK {
                            crate::proxy::util::parse_usage_from_body(&resp_body, false)
                        } else {
                            None
                        };
                        crate::proxy::util::log_classification_with_usage(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            log_status,
                            idx as u8 + 1,
                            &provider.model,
                            usage.as_ref(),
                            session_id,
                        );
                        #[cfg(feature = "otel")]
                        rm.set_status(status);
                        return crate::proxy::util::json_response(status, resp_body);
                    }
                    if client_wants_stream {
                        let keepalive_interval_secs = *state.keepalive_interval_secs.read().await;
                        return crate::proxy::streaming::handle_streaming_response(
                            state,
                            classification,
                            body_str,
                            prompt,
                            start,
                            upstream_response.bytes_stream(),
                            keepalive_interval_secs,
                            idx as u8 + 1,
                            provider.model.clone(),
                            session_id.map(|s| s.to_string()),
                        );
                    }
                    let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                    let (status, resp_body) =
                        crate::proxy::upstream::handle_buffered_response(
                            upstream_response,
                            max_upstream_body_bytes,
                            false,
                        )
                        .await;
                    let log_status = if status == StatusCode::OK {
                        "ok"
                    } else {
                        "upstream_error"
                    };
                    let usage = if status == StatusCode::OK {
                        crate::proxy::util::parse_usage_from_body(&resp_body, false)
                    } else {
                        None
                    };
                    crate::proxy::util::log_classification_with_usage(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        log_status,
                        idx as u8 + 1,
                        &provider.model,
                        usage.as_ref(),
                        session_id,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(status);
                    if status == StatusCode::OK {
                        if let Some(ref key) = cache_key {
                            if let Some(ref cache) = state.response_cache {
                                cache.put(
                                    key.clone(),
                                    crate::cache::CachedEntry {
                                        body: resp_body.clone(),
                                        content_type: "application/json".to_string(),
                                        status: 200,
                                    },
                                );
                            }
                        }
                    }
                    return crate::proxy::util::json_response(status, resp_body);
                }
                Err(e) => {
                    #[cfg(feature = "otel")]
                    if let Some(ref metrics) = state.metrics {
                        metrics.upstream_duration_seconds.record(
                            upstream_start.elapsed().as_secs_f64(),
                            &[
                                KeyValue::new("provider", provider.provider_type.clone()),
                                KeyValue::new("status", "502"),
                            ],
                        );
                    }
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "upstream_error",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_GATEWAY);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::upstream_error_json(502, &e.to_string()),
                    );
                }
            }
        } else {
            match &upstream_result {
                Ok(upstream_response) => {
                    warn!(
                        "Provider {} returned {}; cascading to next",
                        provider.model,
                        upstream_response.status()
                    );
                    last_error_response = Some(crate::proxy::util::json_response(
                        upstream_response.status(),
                        crate::proxy::util::upstream_error_json(upstream_response.status().as_u16(), "upstream error"),
                    ));
                }
                Err(e) => {
                    warn!(
                        "Provider {} connection failed: {}; cascading to next",
                        provider.model, e
                    );
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::upstream_error_json(502, &e.to_string()),
                    ));
                }
            }
            continue;
        }
    }

    // All providers exhausted
    if let Some(resp) = last_error_response {
        return resp;
    }

    let final_provider = classification
        .providers
        .last()
        .map(|p| p.model.clone())
        .unwrap_or_default();
    crate::proxy::util::log_classification(
        &state,
        &classification,
        &body_str,
        &prompt,
        start,
        "upstream_error",
        total_providers as u8,
        &final_provider,
    );
    #[cfg(feature = "otel")]
    rm.set_status(StatusCode::BAD_GATEWAY);
    crate::proxy::util::json_response(
        StatusCode::BAD_GATEWAY,
        crate::proxy::util::upstream_error_json(502, "all providers failed"),
    )
}

/// Anthropic Messages API pass-through handler.
///
/// Mirrors `completion_handler` but for the Anthropic protocol:
/// - `extract_last_user_message_anthropic` handles string-or-array `content`
/// - Auth headers flow through `auth_headers_for` which now emits
///   `x-api-key` + `anthropic-version: 2023-06-01` for `provider_type: "anthropic"`
/// - Upstream streaming is byte-forwarded verbatim (Anthropic SSE format passes
///   through unchanged — both client and upstream speak Anthropic)
/// - Proxy's own errors use the Anthropic envelope
///   (`{"type":"error","error":{"type":"...","message":"..."}}`)
///
/// Pass-through is intentional: protocol translation (Anthropic ↔ OpenAI) is
/// a separate concern handled by sibling changes.
pub(crate) async fn messages_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let start = std::time::Instant::now();

    #[cfg(feature = "otel")]
    let mut rm = RequestMetrics::new(state.metrics.clone(), "POST", "/v1/messages");

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        return crate::proxy::util::json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            crate::proxy::util::anthropic_error_json("invalid_request_error", "expected application/json"),
        );
    }

    let body_str: String = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => {
            #[cfg(feature = "otel")]
            rm.set_status(StatusCode::BAD_REQUEST);
            return crate::proxy::util::json_response(
                StatusCode::BAD_REQUEST,
                crate::proxy::util::anthropic_error_json("invalid_request_error", "invalid UTF-8 body"),
            );
        }
    };

    // Capture Claude Code / Anthropic client headers once; threaded into every
    // upstream attempt so beta-gated features and session attribution reach the
    // upstream. See `collect_forward_headers` for the security invariant.
    let forward_headers = crate::proxy::util::collect_forward_headers(&headers);
    // Claude Code session id for per-request attribution in the inference log.
    let session_id = crate::proxy::util::session_id_from_forward(&forward_headers);

    // Request optimization: skip if explicit routing headers are present —
    // explicit directives should take precedence over probe optimization.
    if headers.get("x-frugalis-category").is_none() && headers.get("x-frugalis-model").is_none() {
        if let Some(response) = crate::proxy::util::try_optimize_request(&body, true) {
            return response;
        }
    }

    // Cache check: after probe optimization, before classification.
    let mut cache_key: Option<String> = None;
    if let Some(cache) = &state.response_cache {
        let no_cache = headers
            .get("x-frugalis-no-cache")
            .and_then(|v| v.to_str().ok())
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if no_cache {
            debug!("Cache bypass via X-Frugalis-No-Cache header");
        } else {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&body);
            let key = format!("{:x}", hasher.finalize());
            if let Some(entry) = cache.get(&key) {
                debug!("Cache hit for messages request");
                return crate::proxy::util::json_response(
                    StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK),
                    entry.body,
                );
            }
            debug!("Cache miss for messages request");
            cache_key = Some(key);
        }
    }

    let x_category = headers
        .get("x-frugalis-category")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let x_model = headers
        .get("x-frugalis-model")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Extract the prompt with the Anthropic extractor (handles string OR
    // array-of-blocks content). Reuse it for both classification and the
    // persistence log.
    let prompt = crate::persistence::extract_last_user_message_anthropic(&body_str);

    let classification = if let (Some(category), Some(model)) =
        (x_category.as_ref(), x_model.as_ref())
    {
        let routing = state.routing.read().await;
        match routing.get(category) {
            Some(entry) => crate::classification::types::ClassificationResult {
                category: category.clone(),
                model: model.clone(),
                tier: crate::classification::types::ClassificationTier::Fallback,
                providers: entry.providers.clone(),
            },
            None => {
                warn!("X-Frugalis-Category '{category}' not found in routing configuration; degrading to classification JSON");
                let fallback = match state.classifier.as_ref() {
                    Some(c) => c.classify("").await,
                    None => crate::classification::types::ClassificationResult::fallback(),
                };
                let response_body = crate::proxy::util::classification_only_json(&fallback);
                crate::proxy::util::log_classification(&state, &fallback, &body_str, "", start, "ok", 1, "");
                return crate::proxy::util::json_response(StatusCode::OK, response_body);
            }
        }
    } else {
        match state.classifier.as_ref() {
            Some(c) => c.classify(&prompt).await,
            None => crate::classification::types::ClassificationResult::fallback(),
        }
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

    let client = match &state.http_client {
        Some(c) => c,
        None => {
            let response_body = crate::proxy::util::classification_only_json(&classification);
            crate::proxy::util::log_classification(
                &state,
                &classification,
                &body_str,
                &prompt,
                start,
                "ok",
                1,
                "",
            );
            return crate::proxy::util::json_response(StatusCode::OK, response_body);
        }
    };

    let mut last_error_response: Option<Response> = None;
    let total_providers = classification.providers.len();

    let providers_clone = classification.providers.clone();
    for (idx, provider) in providers_clone.iter().enumerate() {
        let is_last = idx + 1 >= total_providers;

        let api_key = match &provider.api_key_env {
            Some(env_name) => match std::env::var(env_name) {
                Ok(key) if !key.is_empty() => key,
                _ => {
                    warn!(
                        "API key env var '{:?}' is missing or empty for provider {}; cascading",
                        provider.api_key_env, provider.model
                    );
                    if is_last {
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            "ok",
                            idx as u8 + 1,
                            &provider.model,
                        );
                        return crate::proxy::util::json_response(
                            StatusCode::OK,
                            crate::proxy::util::classification_only_json(&classification),
                        );
                    }
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::OK,
                        crate::proxy::util::classification_only_json(&classification),
                    ));
                    continue;
                }
            },
            None => {
                warn!(
                    "no api_key_env configured for provider {}; cascading",
                    provider.model
                );
                if is_last {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "ok",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    return crate::proxy::util::json_response(
                        StatusCode::OK,
                        crate::proxy::util::classification_only_json(&classification),
                    );
                }
                last_error_response = Some(crate::proxy::util::json_response(
                    StatusCode::OK,
                    crate::proxy::util::classification_only_json(&classification),
                ));
                continue;
            }
        };

        if provider.endpoint.is_empty() {
            warn!("empty endpoint for provider {}; cascading", provider.model);
            if is_last {
                crate::proxy::util::log_classification(
                    &state,
                    &classification,
                    &body_str,
                    &prompt,
                    start,
                    "upstream_error",
                    idx as u8 + 1,
                    &provider.model,
                );
                #[cfg(feature = "otel")]
                rm.set_status(StatusCode::BAD_GATEWAY);
                return crate::proxy::util::json_response(
                    StatusCode::BAD_GATEWAY,
                    crate::proxy::util::anthropic_error_json("api_error", "no endpoint configured"),
                );
            }
            last_error_response = Some(crate::proxy::util::json_response(
                StatusCode::BAD_GATEWAY,
                crate::proxy::util::anthropic_error_json("api_error", "no endpoint configured"),
            ));
            continue;
        }

        let needs_translation = provider.provider_type != "anthropic";
        let request_bytes: Bytes = if needs_translation {
            let parsed: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::anthropic_error_json(
                            "invalid_request_error",
                            &format!("invalid JSON: {e}"),
                        ),
                    );
                }
            };
            match crate::protocol::request::anthropic_to_openai_request_with_cache_signal(&parsed) {
                Ok((translated, had_cache_control)) => {
                    if had_cache_control {
                        debug!(
                            "anth→oai translation: stripped client cache_control \
                             breakpoints for provider {}",
                            provider.model
                        );
                    }
                    Bytes::from(serde_json::to_vec(&translated).unwrap_or_else(|_| body.to_vec()))
                }
                Err(e) => {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::anthropic_error_json("invalid_request_error", &e),
                    );
                }
            }
        } else {
            // Same-protocol Anthropic→Anthropic passthrough. We deliberately
            // forward the raw body bytes (`body.clone()`) rather than
            // parse-normalize-reemit: the translator's explicit field
            // allowlist would drop unknown Anthropic fields (including
            // cache_control breakpoints, thinking config, context_management,
            // etc.), so byte passthrough is what actually preserves them. A
            // debug log surfaces cache_control presence for operators without
            // the cost of a full parse.
            if serde_json::from_slice::<serde_json::Value>(&body)
                .map(|v| v.get("cache_control").is_some())
                .unwrap_or(false)
            {
                debug!(
                    "anthropic passthrough: forwarding client cache_control \
                     breakpoints to upstream unchanged for provider {}",
                    provider.model
                );
            }
            body.clone()
        };

        let request_bytes = if provider.provider_type == "nvidia_nim" {
            match serde_json::from_slice::<serde_json::Value>(&request_bytes) {
                Ok(mut v) => {
                    crate::proxy::util::sanitize_for_nim(&mut v);
                    Bytes::from(serde_json::to_vec(&v).unwrap_or_else(|_| request_bytes.to_vec()))
                }
                Err(_) => request_bytes,
            }
        } else {
            request_bytes
        };

        let (client_wants_stream, upstream_req) = match crate::proxy::upstream::build_upstream_request(
            client,
            provider,
            &request_bytes,
            &api_key,
            &state.auth_providers,
            &forward_headers,
        ) {
            Err(msg) => {
                if is_last {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "bad_request",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_REQUEST);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::proxy::util::anthropic_error_json("invalid_request_error", &msg),
                    );
                }
                last_error_response = Some(crate::proxy::util::json_response(
                    StatusCode::BAD_REQUEST,
                    crate::proxy::util::anthropic_error_json("invalid_request_error", &msg),
                ));
                continue;
            }
            Ok(r) => r,
        };

        #[cfg_attr(not(feature = "otel"), allow(unused_variables))]
        let upstream_start = std::time::Instant::now();
        let upstream_result = upstream_req.send().await;

        if is_last || !crate::proxy::upstream::is_retryable_error(&upstream_result) {
            match upstream_result {
                Ok(upstream_response) => {
                    #[cfg(feature = "otel")]
                    if let Some(ref metrics) = state.metrics {
                        metrics.upstream_duration_seconds.record(
                            upstream_start.elapsed().as_secs_f64(),
                            &[
                                KeyValue::new("provider", provider.provider_type.clone()),
                                KeyValue::new(
                                    "status",
                                    upstream_response.status().as_u16().to_string(),
                                ),
                            ],
                        );
                    }
                    if !upstream_response.status().is_success() {
                        if client_wants_stream {
                            let resp = if needs_translation {
                                crate::proxy::streaming::handle_streaming_error_with_transform(
                                    upstream_response,
                                    |body, status| {
                                        crate::protocol::response::openai_to_anthropic_error(
                                            &body, status,
                                        )
                                    },
                                )
                                .await
                            } else {
                                crate::proxy::streaming::handle_streaming_error(upstream_response)
                                    .await
                            };
                            crate::proxy::util::log_classification(
                                &state,
                                &classification,
                                &body_str,
                                &prompt,
                                start,
                                "upstream_error",
                                idx as u8 + 1,
                                &provider.model,
                            );
                            #[cfg(feature = "otel")]
                            rm.set_status(resp.status());
                            return resp;
                        }
                        let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                        let (status, resp_body) = if needs_translation {
                            crate::proxy::upstream::translate_openai_buffered_to_anthropic(
                                upstream_response,
                                max_upstream_body_bytes,
                            )
                            .await
                        } else {
                            crate::proxy::upstream::handle_buffered_response(
                                upstream_response,
                                max_upstream_body_bytes,
                                true,
                            )
                            .await
                        };
                        let log_status = if status == StatusCode::OK {
                            "ok"
                        } else {
                            warn!(
                                upstream_status = status.as_u16(),
                                "upstream returned non-2xx"
                            );
                            "upstream_error"
                        };
                        let usage = if status == StatusCode::OK {
                            crate::proxy::util::parse_usage_from_body(&resp_body, true)
                        } else {
                            None
                        };
                        crate::proxy::util::log_classification_with_usage(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            start,
                            log_status,
                            idx as u8 + 1,
                            &provider.model,
                            usage.as_ref(),
                            session_id,
                        );
                        #[cfg(feature = "otel")]
                        rm.set_status(status);
                        return crate::proxy::util::json_response(status, resp_body);
                    }
                    if client_wants_stream {
                        let keepalive_interval_secs = *state.keepalive_interval_secs.read().await;
                        if needs_translation {
                            return crate::proxy::streaming::handle_translating_anthropic_stream(
                                state,
                                classification,
                                body_str,
                                prompt,
                                start,
                                upstream_response.bytes_stream(),
                                keepalive_interval_secs,
                                idx as u8 + 1,
                                provider.model.clone(),
                                session_id.map(|s| s.to_string()),
                            );
                        }
                        return crate::proxy::streaming::handle_streaming_response(
                            state,
                            classification,
                            body_str,
                            prompt,
                            start,
                            upstream_response.bytes_stream(),
                            keepalive_interval_secs,
                            idx as u8 + 1,
                            provider.model.clone(),
                            session_id.map(|s| s.to_string()),
                        );
                    }
                    let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                    let (status, resp_body) = if needs_translation {
                        crate::proxy::upstream::translate_openai_buffered_to_anthropic(
                            upstream_response,
                            max_upstream_body_bytes,
                        )
                        .await
                    } else {
                        crate::proxy::upstream::handle_buffered_response(
                            upstream_response,
                            max_upstream_body_bytes,
                            true,
                        )
                        .await
                    };
                    let log_status = if status == StatusCode::OK {
                        "ok"
                    } else {
                        warn!(
                            upstream_status = status.as_u16(),
                            "upstream returned non-2xx"
                        );
                        "upstream_error"
                    };
                    let usage = if status == StatusCode::OK {
                        crate::proxy::util::parse_usage_from_body(&resp_body, true)
                    } else {
                        None
                    };
                    crate::proxy::util::log_classification_with_usage(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        log_status,
                        idx as u8 + 1,
                        &provider.model,
                        usage.as_ref(),
                        session_id,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(status);
                    if status == StatusCode::OK {
                        if let Some(ref key) = cache_key {
                            if let Some(ref cache) = state.response_cache {
                                cache.put(
                                    key.clone(),
                                    crate::cache::CachedEntry {
                                        body: resp_body.clone(),
                                        content_type: "application/json".to_string(),
                                        status: 200,
                                    },
                                );
                            }
                        }
                    }
                    return crate::proxy::util::json_response(status, resp_body);
                }
                Err(e) => {
                    #[cfg(feature = "otel")]
                    if let Some(ref metrics) = state.metrics {
                        metrics.upstream_duration_seconds.record(
                            upstream_start.elapsed().as_secs_f64(),
                            &[
                                KeyValue::new("provider", provider.provider_type.clone()),
                                KeyValue::new("status", "502"),
                            ],
                        );
                    }
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        start,
                        "upstream_error",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    #[cfg(feature = "otel")]
                    rm.set_status(StatusCode::BAD_GATEWAY);
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::anthropic_error_json("api_error", &e.to_string()),
                    );
                }
            }
        } else {
            match &upstream_result {
                Ok(upstream_response) => {
                    warn!(
                        "Provider {} returned {}; cascading to next",
                        provider.model,
                        upstream_response.status()
                    );
                    last_error_response = Some(crate::proxy::util::json_response(
                        upstream_response.status(),
                        crate::proxy::util::anthropic_error_json(
                            "api_error",
                            &format!("{}", upstream_response.status()),
                        ),
                    ));
                }
                Err(e) => {
                    warn!(
                        "Provider {} connection failed: {}; cascading to next",
                        provider.model, e
                    );
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::anthropic_error_json("api_error", &e.to_string()),
                    ));
                }
            }
            continue;
        }
    }

    if let Some(resp) = last_error_response {
        return resp;
    }

    let final_provider = classification
        .providers
        .last()
        .map(|p| p.model.clone())
        .unwrap_or_default();
    crate::proxy::util::log_classification(
        &state,
        &classification,
        &body_str,
        &prompt,
        start,
        "upstream_error",
        total_providers as u8,
        &final_provider,
    );
    #[cfg(feature = "otel")]
    rm.set_status(StatusCode::BAD_GATEWAY);
    crate::proxy::util::json_response(
        StatusCode::BAD_GATEWAY,
        crate::proxy::util::anthropic_error_json("api_error", "all providers failed"),
    )
}

/// Classify handler: extracts prompt, classifies intent, optionally logs a
/// lightweight classification record with status "classified", and returns
/// classification JSON. Logging is controlled by `CLASSIFY_DB_LOG` env var.
pub(crate) async fn classify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    let log_status = if state
        .classify_db_log
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        Some("classified")
    } else {
        None
    };
    crate::proxy::util::classify_and_log(&headers, body_str, start, &state, log_status).await
}

#[derive(serde::Deserialize)]
pub(crate) struct FeedbackRequest {
    text: String,
    #[serde(default)]
    predicted_category: Option<String>,
    actual_category: String,
    #[serde(default = "default_satisfaction")]
    satisfaction: f64,
}

pub(crate) fn default_satisfaction() -> f64 {
    1.0
}

pub(crate) async fn feedback_handler(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<FeedbackRequest>,
) -> impl IntoResponse {
    let fewshot = match &state.fewshot_classifier {
        Some(fs) => fs.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "error": "fewshot_classifier_not_configured",
                    "status": 503,
                    "message": "No few-shot classifier backend is configured"
                })),
            );
        }
    };

    // Validate actual_category against known routing keys
    let routing = state.routing.read().await;
    if !routing.contains_key(&body.actual_category.to_uppercase()) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "invalid_category",
                "status": 400,
                "message": format!("Unknown category '{}'", body.actual_category)
            })),
        );
    }
    drop(routing);

    // Clamp satisfaction to [0.0, 1.0] as per OpenAPI spec
    let satisfaction = body.satisfaction.clamp(0.0, 1.0);
    fewshot
        .add_feedback(
            body.text,
            body.predicted_category,
            body.actual_category,
            satisfaction,
        )
        .await;

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "accepted"
        })),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{auth, classification, config};
    use crate::app::build_app;
    use crate::app::test_helpers::{test_categories, test_negative_patterns, make_test_app_state, test_app, test_app_with_classifier, parse_json_body};
    use crate::test_util::EnvGuard;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_feedback_requires_auth() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/feedback")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"hello","actual_category":"CASUAL"}"#))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_feedback_no_fewshot_returns_503() {
        let app = test_app_with_classifier();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/feedback")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"text":"hello","actual_category":"SYNTAX_FIX"}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_completion_handler_returns_classification_json() {
        let response = test_app_with_classifier()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let json = parse_json_body(response).await;
        assert_eq!(json.get("category").and_then(|v| v.as_str()), Some("SYNTAX_FIX"));
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("classified"));
        assert_eq!(json.get("tier").and_then(|v| v.as_str()), Some("Regex"));
    }

    #[tokio::test]
    async fn test_classify_handler_returns_classification_json() {
        let response = test_app_with_classifier()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/classify")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("classify request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let json = parse_json_body(response).await;
        assert_eq!(json.get("category").and_then(|v| v.as_str()), Some("SYNTAX_FIX"));
        assert_eq!(json.get("model").and_then(|v| v.as_str()), Some("sf-model"));
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("classified"));
        assert_eq!(json.get("tier").and_then(|v| v.as_str()), Some("Regex"));
    }

    #[tokio::test]
    #[serial]
    async fn test_max_upstream_body_bytes_truncation() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard2 = EnvGuard("TEST_API_KEY");
        std::env::set_var("TEST_API_KEY", "sk-test");
        let (app, server) = test_app_with_http_client("TEST_API_KEY", 1_100_000);
        let large_content = "x".repeat(2_000_000);
        let body = format!("{{\"choices\":[{{\"message\":{{\"content\":\"{large_content}\"}}}}]}}");
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(body);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let json = parse_json_body(response).await;
        assert_eq!(json.get("error").and_then(|v| v.as_str()), Some("upstream_error"));
        assert_eq!(json.get("message").and_then(|v| v.as_str()), Some("upstream response too large"));
        mock.assert();
    }

    fn test_app_with_enriched_classifier(
        provider_type_val: &str,
        api_key_env_val: Option<&str>,
    ) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: "https://test.endpoint".to_string(),
                    provider_type: provider_type_val.to_string(),
                    api_key_env: api_key_env_val.map(|s| s.to_string()),
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            cats[3].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "ca-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry {
                model: "fallback-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(
            routing, fallback, 30, cats, &test_negative_patterns(),
        );
        let app_state = make_test_app_state(regex_classifier, None, config::routing::ModelCosts::empty(), String::new(), 10_485_760);
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_does_not_include_enriched_fields() {
        let _guard = EnvGuard("TEST_API_KEY");
        std::env::set_var("TEST_API_KEY", "sk-test-value-123");
        let response = test_app_with_enriched_classifier("test_provider", Some("TEST_API_KEY"))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        assert_eq!(json.get("category").and_then(|v| v.as_str()), Some("SYNTAX_FIX"));
        for forbidden in ["provider_type", "endpoint", "api_key"] {
            assert!(json.get(forbidden).is_none(), "response should NOT contain {forbidden}, got: {json}");
        }
    }

    #[tokio::test]
    async fn test_completion_no_enriched_fields_with_missing_env() {
        let response = test_app_with_enriched_classifier("test_provider", Some("MISSING_KEY_XYZ"))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        assert!(json.get("api_key").is_none(), "response should NOT contain api_key, got: {json}");
    }

    #[tokio::test]
    async fn test_classify_no_enriched_fields() {
        let response = test_app_with_enriched_classifier("test_provider", Some("TEST_API_KEY"))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/classify")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#))
                    .expect("request should be valid"),
            )
            .await
            .expect("classify request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        for forbidden in ["provider_type", "api_key"] {
            assert!(json.get(forbidden).is_none(), "classify response should not contain {forbidden}, got: {json}");
        }
    }

    #[tokio::test]
    async fn routes_auth_health_is_public() {
        let response = test_app()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).expect("request should be valid"))
            .await
            .expect("health request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_models_endpoint_returns_valid_json_no_auth() {
        let response = test_app()
            .oneshot(Request::builder().uri("/v1/models").body(Body::empty()).expect("request should be valid"))
            .await
            .expect("models request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).expect("response should have Content-Type");
        assert!(content_type.starts_with("application/json"));
        let json = parse_json_body(response).await;
        assert_eq!(json.get("object").and_then(|v| v.as_str()), Some("list"));
        assert_eq!(json.get("has_more").and_then(|v| v.as_bool()), Some(false));
        let data = json.get("data").and_then(|v| v.as_array()).expect("data should be an array");
        assert_eq!(data.len(), 3);
        let model_ids: Vec<&str> = data.iter().map(|m| m.get("id").and_then(|v| v.as_str()).unwrap_or("")).collect();
        assert!(model_ids.contains(&"claude-sonnet-4-6-20250514"));
        assert!(model_ids.contains(&"claude-haiku-4-5-20250514"));
        assert!(model_ids.contains(&"claude-opus-4-20250514"));
        for model in data {
            assert_eq!(model.get("object").and_then(|v| v.as_str()), Some("model"));
            assert_eq!(model.get("owned_by").and_then(|v| v.as_str()), Some("anthropic"));
        }
    }

    #[tokio::test]
    async fn test_models_endpoint_entries_have_display_name_and_prefixed_id() {
        let response = test_app()
            .oneshot(Request::builder().uri("/v1/models").body(Body::empty()).expect("request should be valid"))
            .await
            .expect("models request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let data = json.get("data").and_then(|v| v.as_array()).expect("data should be an array");
        assert!(!data.is_empty());
        for model in data {
            let id = model.get("id").and_then(|v| v.as_str()).expect("each entry must have an id");
            assert!(id.starts_with("claude") || id.starts_with("anthropic"), "id must be claude/anthropic-prefixed, got {id}");
            let display_name = model.get("display_name").and_then(|v| v.as_str()).expect("each entry must have a display_name");
            assert!(!display_name.is_empty());
            assert_eq!(model.get("type").and_then(|v| v.as_str()), Some("model"));
        }
    }

    #[tokio::test]
    async fn test_count_tokens_returns_estimated_tokens() {
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hello world"}]});
        let response = test_app()
            .oneshot(
                Request::builder().method("POST").uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap())).expect("request should be valid"),
            ).await.expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json.get("input_tokens").and_then(|v| v.as_u64()).expect("input_tokens should be a number");
        assert_eq!(tokens, 2);
    }

    #[tokio::test]
    async fn test_count_tokens_array_content_blocks() {
        let body = serde_json::json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "hello world test"}]}]});
        let response = test_app()
            .oneshot(
                Request::builder().method("POST").uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap())).expect("request should be valid"),
            ).await.expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json.get("input_tokens").and_then(|v| v.as_u64()).expect("input_tokens should be a number");
        assert_eq!(tokens, 4);
    }

    #[tokio::test]
    async fn test_count_tokens_empty_messages() {
        let body = serde_json::json!({"messages": []});
        let response = test_app()
            .oneshot(
                Request::builder().method("POST").uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap())).expect("request should be valid"),
            ).await.expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json.get("input_tokens").and_then(|v| v.as_u64()).expect("input_tokens should be a number");
        assert_eq!(tokens, 0);
    }

    #[tokio::test]
    async fn routes_auth_proxy_requires_valid_bearer_token() {
        let unauthorized = test_app()
            .oneshot(Request::builder().method("POST").uri("/v1/chat/completions").body(Body::empty()).expect("request should be valid"))
            .await.expect("proxy unauthorized request should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = test_app()
            .oneshot(
                Request::builder().method("POST").uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::empty()).expect("request should be valid"),
            ).await.expect("proxy authorized request should complete");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn routes_auth_dashboard_requires_basic_auth_challenge() {
        let unauthorized = test_app()
            .oneshot(Request::builder().uri("/dashboard").body(Body::empty()).expect("request should be valid"))
            .await.expect("dashboard unauthorized request should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let challenge = unauthorized.headers().get(header::WWW_AUTHENTICATE).and_then(|value| value.to_str().ok()).expect("dashboard unauthorized should include challenge header");
        assert!(challenge.starts_with("Basic"));

        let authorized = test_app()
            .oneshot(
                Request::builder().uri("/dashboard")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty()).expect("request should be valid"),
            ).await.expect("dashboard authorized request should complete");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    pub(crate) fn test_app_with_http_client(
        env_var_name: &str,
        max_upstream_body_bytes: usize,
    ) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing = HashMap::new();
        routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "sf-model".to_string(), endpoint: endpoint.clone(), provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        routing.insert(cats[3].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "ca-model".to_string(), endpoint, provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
        let app_state = make_test_app_state(regex_classifier, Some(client), config::routing::ModelCosts::empty(), String::new(), max_upstream_body_bytes);
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    pub(crate) fn test_app_with_anthropic_http_client(
        env_var_name: &str,
        max_upstream_body_bytes: usize,
    ) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let endpoint = server.url("/v1/messages");
        let mut routing = HashMap::new();
        routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "sf-model".to_string(), endpoint: endpoint.clone(), provider_type: "anthropic".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        routing.insert(cats[3].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "ca-model".to_string(), endpoint, provider_type: "anthropic".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
        let app_state = make_test_app_state(regex_classifier, Some(client), config::routing::ModelCosts::empty(), String::new(), max_upstream_body_bytes);
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    fn test_app_with_dead_endpoint(env_var_name: &str) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(1)).build().expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let mut routing = HashMap::new();
        routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "sf-model".to_string(), endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(), provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        routing.insert(cats[3].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "ca-model".to_string(), endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(), provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
        let app_state = make_test_app_state(regex_classifier, Some(client), config::routing::ModelCosts::empty(), String::new(), 10_485_760);
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_returns_response() {
        let env = "TEST_UPSTREAM_RESP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "application/json").body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""choices""#));
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_request_includes_auth_header() {
        let env = "TEST_UPSTREAM_AUTH";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header("Authorization", "Bearer sk-test");
            then.status(200).header("content-type", "application/json").body("ok");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_request_includes_content_type_json() {
        let env = "TEST_UPSTREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header("Content-Type", "application/json");
            then.status(200).header("content-type", "application/json").body("ok");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_unreachable_returns_502() {
        let env = "TEST_UPSTREAM_DEAD";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let app = test_app_with_dead_endpoint(env);
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let json = parse_json_body(response).await;
        assert_eq!(json.get("error").and_then(|v| v.as_str()), Some("upstream_error"));
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_skip_classify_via_headers() {
        let env = "TEST_UPSTREAM_SKIP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "application/json").body(r#"{"choices":[{"message":{"content":"skipped"}}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("x-frugalis-category", "SYNTAX_FIX").header("x-frugalis-model", "gpt-4o-mini")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""skipped""#));
        mock.assert();
    }

    #[tokio::test]
    async fn test_messages_handler_requires_auth() {
        let response = test_app().oneshot(
            Request::builder().method("POST").uri("/v1/messages").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","messages":[{"role":"user","content":"hi"}]}"#)).expect("request should be valid"),
        ).await.expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_non_streaming_passthrough() {
        let env = "TEST_ANTHROPIC_NS";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test").header("anthropic-version", "2023-06-01");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body_str.contains("hello"));
        assert!(body_str.contains("msg_1"));
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_forwards_anthropic_client_headers() {
        let env = "TEST_ANTHROPIC_FWD";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages")
                .header("x-api-key", "sk-ant-test").header("anthropic-version", "2024-10-22")
                .header("anthropic-beta", "context-management-2025-09").header("x-claude-code-session-id", "sess-123");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("anthropic-version", "2024-10-22").header("anthropic-beta", "context-management-2025-09")
                .header("x-claude-code-session-id", "sess-123")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_does_not_forward_anthropic_headers_to_openai() {
        let env = "TEST_OPENAI_NO_FWD";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let beta_canary = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header_exists("anthropic-beta");
            then.status(200).body("canary-beta");
        });
        let version_canary = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header_exists("anthropic-version");
            then.status(200).body("canary-version");
        });
        let positive = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header("Authorization", "Bearer sk-test");
            then.status(200).header("content-type", "application/json").body("ok");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("anthropic-version", "2024-10-22").header("anthropic-beta", "context-management-2025-09")
                .header("x-claude-code-session-id", "sess-123")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(beta_canary.hits(), 0);
        assert_eq!(version_canary.hits(), 0);
        assert_eq!(positive.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_streaming_passthrough() {
        let env = "TEST_ANTHROPIC_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200).header("content-type", "text/event-stream")
                .body("event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"stream":true,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()), Some("text/event-stream"));
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body_str.contains("message_start"));
        assert!(body_str.contains("content_block_delta"));
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_upstream_error_forwards_body() {
        let env = "TEST_ANTHROPIC_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(429).header("content-type", "application/json")
                .body(r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();
        let json = parse_json_body(response).await;
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("error"));
    }

    #[tokio::test]
    async fn test_messages_handler_classification_only_when_no_http_client() {
        let response = test_app_with_classifier().oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("classified"));
        assert_eq!(json.get("category").and_then(|v| v.as_str()), Some("SYNTAX_FIX"));
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_overrides_model_to_classifier_choice() {
        let env = "TEST_ANTHROPIC_MODEL";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").body_contains("\"model\":\"sf-model\"");
            then.status(200).header("content-type", "application/json").body(r#"{"id":"msg_1","type":"message"}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_true_returns_sse_content() {
        let env = "TEST_STREAM_TSSE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "text/event-stream").body("data: hello\n\n");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(content_type, "text/event-stream");
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_false_returns_buffered_json() {
        let env = "TEST_STREAM_FJSON";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "application/json").body(r#"{"choices":[{"message":{"content":"buffered"}}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":false}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(content_type, "application/json");
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_absent_returns_buffered_json() {
        let env = "TEST_STREAM_AJSON";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "application/json").body(r#"{"choices":[{"message":{"content":"default"}}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        assert_eq!(content_type, "application/json");
        mock.assert();
    }

    #[tokio::test]
    async fn test_streaming_degradation_no_client() {
        let app = test_app_with_classifier();
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""status":"classified""#));
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_translation() {
        let env = "TEST_TRANSLATE_O2A";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test").header("anthropic-version", "2023-06-01")
                .body_contains("\"system\"").body_contains("\"max_tokens\"");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message","role":"assistant","model":"sf-model","content":[{"type":"text","text":"translated response"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"fix this bug"}],"max_tokens":100}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let json = parse_json_body(response).await;
        assert_eq!(json.get("object").and_then(|v| v.as_str()), Some("chat.completion"));
        let choices = json.get("choices").and_then(|v| v.as_array()).expect("choices array");
        assert_eq!(choices.len(), 1);
        let message = choices[0].get("message").expect("message field");
        assert_eq!(message.get("content").and_then(|v| v.as_str()), Some("translated response"));
        assert_eq!(choices[0].get("finish_reason").and_then(|v| v.as_str()), Some("stop"));
        let usage = json.get("usage").expect("usage field");
        assert_eq!(usage.get("prompt_tokens").and_then(|v| v.as_u64()), Some(10));
        assert_eq!(usage.get("completion_tokens").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(usage.get("total_tokens").and_then(|v| v.as_u64()), Some(15));
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_translation_inserts_cache_control() {
        let env = "TEST_TRANSLATE_O2A_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test").body_contains("\"cache_control\"");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"max_tokens":100}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_translates_cache_tokens_in_usage() {
        let env = "TEST_USAGE_O2A_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_u","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":5}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"max_tokens":100}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let usage = json.get("usage").expect("usage in client response");
        assert_eq!(usage.get("prompt_tokens").and_then(|v| v.as_u64()), Some(100 + 80 + 5));
        assert_eq!(usage.get("completion_tokens").and_then(|v| v.as_u64()), Some(20));
        let cached = usage.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_u64());
        assert_eq!(cached, Some(80));
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_streaming() {
        let env = "TEST_TRANSLATE_O2A_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200).header("content-type", "text/event-stream")
                .body("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_s1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"sf-model\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()), Some("text/event-stream"));
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body_str.contains("chatcmpl-"));
        assert!(body_str.contains("\"role\":\"assistant\""));
        assert!(body_str.contains("Hello "));
        assert!(body_str.contains("\"finish_reason\":\"stop\""));
        assert!(body_str.contains("[DONE]"));
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_error() {
        let env = "TEST_TRANSLATE_O2A_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(429).header("content-type", "application/json")
                .body(r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();
        let json = parse_json_body(response).await;
        let error = json.get("error").expect("error field");
        assert_eq!(error.get("type").and_then(|v| v.as_str()), Some("rate_limit_error"));
        assert_eq!(error.get("message").and_then(|v| v.as_str()), Some("Too many requests"));
    }

    fn test_app_with_openai_translation(env_var_name: &str) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values("proxy-token", "user", "password"));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing = HashMap::new();
        routing.insert(cats[1].name.clone(), config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "gpt-4o".to_string(), endpoint: endpoint.clone(), provider_type: "openai_compatible".to_string(), api_key_env: Some(env_var_name.to_string()), timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        });
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry { model: "fallback-model".to_string(), endpoint: String::new(), provider_type: String::new(), api_key_env: None, timeout_ms: None }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(routing, fallback, 30, cats, &test_negative_patterns());
        let app_state = make_test_app_state(regex_classifier, Some(client), config::routing::ModelCosts::empty(), String::new(), 10_485_760);
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_strips_cache_control() {
        let env = "TEST_A2O_NO_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);
        let canary = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").body_contains("cache_control");
            then.status(200).body("canary");
        });
        let positive = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header("authorization", "Bearer sk-openai-test");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("x-frugalis-category", "SYNTAX_FIX").header("x-frugalis-model", "gpt-4o")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":[{"type":"text","text":"fix this bug","cache_control":{"type":"ephemeral"}}]}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(canary.hits(), 0);
        assert_eq!(positive.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_anthropic_passthrough_preserves_cache_control() {
        let env = "TEST_ANT_PASSTHROUGH_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test").header("anthropic-version", "2023-06-01").body_contains("\"cache_control\"");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":[{"type":"text","text":"fix this bug","cache_control":{"type":"ephemeral"}}]}]}"#)).expect("request should be valid"),
        ).await.expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_non_streaming() {
        let env = "TEST_A2O_NS";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions").header("authorization", "Bearer sk-openai-test");
            then.status(200).header("content-type", "application/json")
                .body(r#"{"id":"chatcmpl-abc","object":"chat.completion","model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"Hello from OpenAI"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("x-frugalis-category", "SYNTAX_FIX").header("x-frugalis-model", "gpt-4o")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":"fix this bug"}]}"#)).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body.get("type").unwrap().as_str().unwrap(), "message");
        assert_eq!(body.get("role").unwrap().as_str().unwrap(), "assistant");
        assert_eq!(body.get("stop_reason").unwrap().as_str().unwrap(), "end_turn");
        let content = body.get("content").unwrap().as_array().unwrap();
        assert_eq!(content[0].get("text").unwrap().as_str().unwrap(), "Hello from OpenAI");
        let usage = body.get("usage").unwrap();
        assert_eq!(usage.get("input_tokens").unwrap().as_u64().unwrap(), 10);
        assert_eq!(usage.get("output_tokens").unwrap().as_u64().unwrap(), 5);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_streaming() {
        let env = "TEST_A2O_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);
        let sse_body = "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).header("content-type", "text/event-stream").body(sse_body);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("x-frugalis-category", "SYNTAX_FIX").header("x-frugalis-model", "gpt-4o")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":1024,"stream":true,"messages":[{"role":"user","content":"fix this bug"}]}"#)).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();
        assert!(body_str.contains("event: message_start"));
        assert!(body_str.contains("event: content_block_start"));
        assert!(body_str.contains("text_delta"));
        assert!(body_str.contains("Hi"));
        assert!(body_str.contains("event: message_delta"));
        assert!(body_str.contains("end_turn"));
        assert!(body_str.contains("event: message_stop"));
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_error() {
        let env = "TEST_A2O_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(429).header("content-type", "application/json")
                .body(r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit","code":"rate_limit_exceeded"}}"#);
        });
        let response = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header(header::AUTHORIZATION, "Bearer proxy-token").header(header::CONTENT_TYPE, "application/json")
                .header("x-frugalis-category", "SYNTAX_FIX").header("x-frugalis-model", "gpt-4o")
                .body(Body::from(r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":"fix this bug"}]}"#)).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body.get("type").unwrap().as_str().unwrap(), "error");
        let error = body.get("error").unwrap();
        assert_eq!(error.get("type").unwrap().as_str().unwrap(), "rate_limit_error");
        assert_eq!(error.get("message").unwrap().as_str().unwrap(), "Rate limit exceeded");
    }
}
