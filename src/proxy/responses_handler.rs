use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    body::Bytes,
};
use crate::app::AppState;
use crate::classification::chain::IntentClassify;

pub(crate) async fn responses_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return crate::proxy::util::json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            crate::protocol::responses::wrap_error_as_responses(
                415,
                "expected application/json",
            )
            .to_string(),
        );
    }

    let body_str: String = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => {
            return crate::proxy::util::json_response(
                StatusCode::BAD_REQUEST,
                crate::protocol::responses::wrap_error_as_responses(
                    400,
                    "invalid UTF-8 body",
                )
                .to_string(),
            );
        }
    };

    let parsed_body: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(e) => {
            return crate::proxy::util::json_response(
                StatusCode::BAD_REQUEST,
                crate::protocol::responses::wrap_error_as_responses(
                    400,
                    &format!("invalid JSON body: {e}"),
                )
                .to_string(),
            );
        }
    };

    let stream = parsed_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // ── Translate Responses → Chat ──
    let (chat_body, extras) = match crate::protocol::responses::request_to_chat(&parsed_body) {
        Ok(result) => result,
        Err(rejection) => {
            return crate::proxy::util::json_response(
                StatusCode::from_u16(rejection.status).unwrap_or(StatusCode::BAD_REQUEST),
                crate::protocol::responses::wrap_error_as_responses(
                    rejection.status,
                    &rejection.message,
                )
                .to_string(),
            );
        }
    };

    let chat_body_str = serde_json::to_string(&chat_body).unwrap_or_default();
    let chat_body_bytes = Bytes::from(chat_body_str.clone());

    // ── Forward headers ──
    let forward_headers = crate::proxy::util::collect_forward_headers(&headers);
    let session_id = crate::proxy::util::session_id_from_forward(&forward_headers);

    // ── Cache check ──
    let mut cache_key: Option<String> = None;
    if let Some(cache) = &state.response_cache {
        let no_cache = headers
            .get("x-frugalis-no-cache")
            .and_then(|v| v.to_str().ok())
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !no_cache {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&body);
            let key = format!("{:x}", hasher.finalize());
            if let Some(entry) = cache.get(&key) {
                // The cached value is already in Chat format. Re-parse and
                // re-wrap in Responses envelope so the cache is opaque.
                if let Ok(cached_json) = serde_json::from_str::<serde_json::Value>(&entry.body) {
                    if let Ok(resp) = crate::protocol::responses::response_from_chat(
                        &cached_json,
                        &extras,
                    ) {
                        return crate::proxy::util::json_response(
                            StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK),
                            resp.to_string(),
                        );
                    }
                }
            }
            cache_key = Some(key);
        }
    }

    // ── Prompt extraction ──
    let prompt =
        crate::persistence::extract_last_user_message_responses(&body_str, &parsed_body);

    // ── Classification ──
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

    let classification = if let (Some(category), Some(model)) = (x_category.as_ref(), x_model.as_ref()) {
        let routing = state.routing.read().await;
        match routing.get(category) {
            Some(entry) => crate::classification::types::ClassificationResult {
                category: category.clone(),
                model: model.clone(),
                tier: crate::classification::types::ClassificationTier::Fallback,
                providers: entry.providers.clone(),
            },
            None => {
                let fallback = match state.classifier.as_ref() {
                    Some(c) => c.classify("").await,
                    None => crate::classification::types::ClassificationResult::fallback(),
                };
                crate::proxy::util::log_classification(
                    &state, &fallback, &body_str, "", std::time::Instant::now(),
                    "ok", 1, "",
                );
                return crate::proxy::util::json_response(
                    StatusCode::OK,
                    crate::protocol::responses::wrap_error_as_responses(
                        400,
                        &format!("X-Frugalis-Category '{category}' not found in routing"),
                    )
                    .to_string(),
                );
            }
        }
    } else {
        match state.classifier.as_ref() {
            Some(c) => c.classify(&prompt).await,
            None => crate::classification::types::ClassificationResult::fallback(),
        }
    };

    let client = match &state.http_client {
        Some(c) => c,
        None => {
            crate::proxy::util::log_classification(
                &state,
                &classification,
                &body_str,
                &prompt,
                std::time::Instant::now(),
                "ok",
                1,
                "",
            );
            return crate::proxy::util::json_response(
                StatusCode::OK,
                crate::protocol::responses::wrap_error_as_responses(
                    502,
                    "no HTTP client configured",
                )
                .to_string(),
            );
        }
    };

    // ── Cascade loop ──
    let mut last_error_response: Option<Response> = None;
    let total_providers = classification.providers.len();
    let providers_clone = classification.providers.clone();

    for (idx, provider) in providers_clone.iter().enumerate() {
        let is_last = idx + 1 >= total_providers;

        let api_key = match &provider.api_key_env {
            Some(env_name) => match std::env::var(env_name) {
                Ok(key) if !key.is_empty() => key,
                _ => {
                    if is_last {
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            std::time::Instant::now(),
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
                if is_last {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        std::time::Instant::now(),
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
            if is_last {
                crate::proxy::util::log_classification(
                    &state,
                    &classification,
                    &body_str,
                    &prompt,
                    std::time::Instant::now(),
                    "upstream_error",
                    idx as u8 + 1,
                    &provider.model,
                );
                return crate::proxy::util::json_response(
                    StatusCode::BAD_GATEWAY,
                    crate::protocol::responses::wrap_error_as_responses(502, "no endpoint configured")
                        .to_string(),
                );
            }
            last_error_response = Some(crate::proxy::util::json_response(
                StatusCode::BAD_GATEWAY,
                crate::protocol::responses::wrap_error_as_responses(502, "no endpoint configured")
                    .to_string(),
            ));
            continue;
        }

        // All providers are reached via Chat Completions
        let (_client_wants_stream, upstream_req) =
            match crate::proxy::upstream::build_upstream_request(
                client,
                provider,
                &chat_body_bytes,
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
                            std::time::Instant::now(),
                            "bad_request",
                            idx as u8 + 1,
                            &provider.model,
                        );
                        return crate::proxy::util::json_response(
                            StatusCode::BAD_REQUEST,
                            crate::protocol::responses::wrap_error_as_responses(400, &msg)
                                .to_string(),
                        );
                    }
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::protocol::responses::wrap_error_as_responses(400, &msg).to_string(),
                    ));
                    continue;
                }
                Ok(r) => r,
            };

        let upstream_result = upstream_req.send().await;

        if is_last || !crate::proxy::upstream::is_retryable_error(&upstream_result) {
            match upstream_result {
                Ok(upstream_response) => {
                    // Non-2xx handling
                    if !upstream_response.status().is_success() {
                        if stream {
                            let resp =
                                crate::proxy::streaming::handle_streaming_error(upstream_response)
                                    .await;
                            crate::proxy::util::log_classification(
                                &state,
                                &classification,
                                &body_str,
                                &prompt,
                                std::time::Instant::now(),
                                "upstream_error",
                                idx as u8 + 1,
                                &provider.model,
                            );
                            return resp;
                        }
                        let max_upstream_body_bytes =
                            *state.max_upstream_body_bytes.read().await;
                        let (upstream_status, resp_body) =
                            crate::proxy::upstream::handle_buffered_response(
                                upstream_response,
                                max_upstream_body_bytes,
                                false,
                            )
                            .await;
                        let err_json = crate::protocol::responses::map_upstream_error_to_responses(
                            upstream_status.as_u16(),
                            &resp_body,
                        );
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            std::time::Instant::now(),
                            "upstream_error",
                            idx as u8 + 1,
                            &provider.model,
                        );
                        return crate::proxy::util::json_response(
                            upstream_status,
                            err_json.to_string(),
                        );
                    }

                    // ── Streaming path (Phase 2) ──
                    if stream {
                        let keepalive_interval_secs =
                            *state.keepalive_interval_secs.read().await;
                        return crate::proxy::streaming::handle_streaming_response(
                            state,
                            classification,
                            body_str,
                            prompt,
                            std::time::Instant::now(),
                            upstream_response.bytes_stream(),
                            keepalive_interval_secs,
                            idx as u8 + 1,
                            provider.model.clone(),
                            session_id.map(|s| s.to_string()),
                        );
                    }

                    // ── Non-streaming path ──
                    let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
                    let (upstream_status, resp_body) =
                        crate::proxy::upstream::handle_buffered_response(
                            upstream_response,
                            max_upstream_body_bytes,
                            false,
                        )
                        .await;

                    let log_status = if upstream_status.is_success() {
                        "ok"
                    } else {
                        "upstream_error"
                    };
                    let usage = if upstream_status.is_success() {
                        crate::proxy::util::parse_usage_from_body(&resp_body, false)
                    } else {
                        None
                    };
                    crate::proxy::util::log_classification_with_usage(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        std::time::Instant::now(),
                        log_status,
                        idx as u8 + 1,
                        &provider.model,
                        usage.as_ref(),
                        session_id,
                    );

                    if !upstream_status.is_success() {
                        let err_json =
                            crate::protocol::responses::map_upstream_error_to_responses(
                                upstream_status.as_u16(),
                                &resp_body,
                            );
                        return crate::proxy::util::json_response(
                            upstream_status,
                            err_json.to_string(),
                        );
                    }

                    // Wrap Chat response in Responses envelope
                    let chat_resp: serde_json::Value =
                        match serde_json::from_str(&resp_body) {
                            Ok(v) => v,
                            Err(e) => {
                                return crate::proxy::util::json_response(
                                    StatusCode::BAD_GATEWAY,
                                    crate::protocol::responses::wrap_error_as_responses(
                                        502,
                                        &format!("invalid upstream response JSON: {e}"),
                                    )
                                    .to_string(),
                                );
                            }
                        };

                    let responses_json = match crate::protocol::responses::response_from_chat(
                        &chat_resp,
                        &extras,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            return crate::proxy::util::json_response(
                                StatusCode::BAD_GATEWAY,
                                crate::protocol::responses::wrap_error_as_responses(
                                    502,
                                    &format!("response translation error: {e}"),
                                )
                                .to_string(),
                            );
                        }
                    };

                    // Cache the translated Responses body (via original key)
                    if upstream_status.is_success() {
                        if let Some(ref key) = cache_key {
                            if let Some(ref cache) = state.response_cache {
                                cache.put(
                                    key.clone(),
                                    crate::cache::CachedEntry {
                                        body: resp_body,
                                        status: upstream_status.as_u16(),
                                    },
                                );
                            }
                        }
                    }

                    return crate::proxy::util::json_response(
                        upstream_status,
                        responses_json.to_string(),
                    );
                }
                Err(e) => {
                    crate::proxy::util::log_classification(
                        &state,
                        &classification,
                        &body_str,
                        &prompt,
                        std::time::Instant::now(),
                        "upstream_error",
                        idx as u8 + 1,
                        &provider.model,
                    );
                    return crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::protocol::responses::wrap_error_as_responses(
                            502,
                            &e.to_string(),
                        )
                        .to_string(),
                    );
                }
            }
        } else {
            match &upstream_result {
                Ok(upstream_response) => {
                    last_error_response = Some(crate::proxy::util::json_response(
                        upstream_response.status(),
                        crate::protocol::responses::wrap_error_as_responses(
                            upstream_response.status().as_u16(),
                            "upstream error",
                        )
                        .to_string(),
                    ));
                }
                Err(e) => {
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::BAD_GATEWAY,
                        crate::protocol::responses::wrap_error_as_responses(
                            502,
                            &e.to_string(),
                        )
                        .to_string(),
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
        std::time::Instant::now(),
        "upstream_error",
        total_providers as u8,
        &final_provider,
    );
    crate::proxy::util::json_response(
        StatusCode::BAD_GATEWAY,
        crate::protocol::responses::wrap_error_as_responses(502, "all providers failed")
            .to_string(),
    )
}
