use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    body::Bytes,
};
use tracing::warn;
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

    // Extract Codex headers for persistence
    let codex_installation_id = headers
        .get("x-codex-installation-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let codex_turn_state = headers
        .get("x-codex-turn-state")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let codex_window_id = headers
        .get("x-codex-window-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let codex_turn_metadata = headers
        .get("x-codex-turn-metadata")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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
                // If entry.response_id is non-empty, the cached body is already a
                // synthesized Responses JSON envelope; return it directly.
                if !entry.response_id.is_empty() {
                    return crate::proxy::util::json_response(
                        StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK),
                        entry.body,
                    );
                }
                // Otherwise the cached body is in Chat format (from other handlers).
                // Re-parse and re-wrap in Responses envelope so the cache is opaque.
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
                    warn!("API key env var '{:?}' is missing or empty for provider {}; cascading", env_name, provider.model);
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
                warn!("no api_key_env configured for provider {}; cascading", provider.model);
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

        // ── Build and send upstream request ──
        //
        // Three provider paths:
        //   "anthropic"        — translate Chat body → Anthropic body (R2)
        //   "openai_responses" — forward original Responses body verbatim (R5)
        //   everything else    — send translated Chat body as-is (R1, default)
        let (upstream_req, is_anthropic, is_passthrough) = if provider.provider_type == "anthropic" {
            // R2: Chat → Anthropic request translation
            let anthropic_body = match crate::protocol::request::translate_request(&chat_body) {
                Ok(b) => b,
                Err(e) => {
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
                            crate::protocol::responses::wrap_error_as_responses(400, &e).to_string(),
                        );
                    }
                    last_error_response = Some(crate::proxy::util::json_response(
                        StatusCode::BAD_REQUEST,
                        crate::protocol::responses::wrap_error_as_responses(400, &e).to_string(),
                    ));
                    continue;
                }
            };
            let anthropic_bytes = Bytes::from(serde_json::to_vec(&anthropic_body).unwrap_or_default());
            let auth_headers = crate::classification::llm::auth_headers_for(
                &state.auth_providers,
                &provider.provider_type,
                &api_key,
                &forward_headers,
            );
            let mut req = client
                .post(&provider.endpoint)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(anthropic_bytes);
            for (name, value) in &auth_headers {
                req = req.header(name.as_str(), value.as_str());
            }
            if let Some(ms) = provider.timeout_ms {
                req = req.timeout(std::time::Duration::from_millis(ms));
            }
            (req, true, false)
        } else if provider.provider_type == "openai_responses" {
            // R5: forward original Responses body verbatim to native Responses upstream
            let (_client_wants_stream, req) =
                match crate::proxy::upstream::build_upstream_request(
                    client,
                    provider,
                    &body,
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
                                crate::protocol::responses::wrap_error_as_responses(400, &msg).to_string(),
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
            (req, false, true)
        } else {
            // R1 / default: send translated Chat body
            let (_client_wants_stream, req) =
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
            (req, false, false)
        };

        let upstream_result = upstream_req.send().await;

        if is_last || !crate::proxy::upstream::is_retryable_error(&upstream_result) {
            match upstream_result {
                Ok(upstream_response) => {
                    // Non-2xx handling
                    if !upstream_response.status().is_success() {
                        if stream {
                            let resp =
                                crate::proxy::streaming::handle_streaming_error_with_transform(
                                    upstream_response,
                                    |body, status| {
                                        crate::protocol::responses::map_upstream_error_to_responses(
                                            status, &body,
                                        )
                                        .to_string()
                                    },
                                )
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

                    // ── Streaming path ──
                    if stream {
                        let keepalive_interval_secs =
                            *state.keepalive_interval_secs.read().await;
                        if is_anthropic {
                            return crate::proxy::responses_streaming::handle_responses_anthropic_streaming_response(
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
                                codex_installation_id.clone(),
                                codex_turn_state.clone(),
                                codex_window_id.clone(),
                                codex_turn_metadata.clone(),
                            );
                        }
                    return crate::proxy::responses_streaming::handle_responses_streaming_response(
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
                        codex_installation_id.clone(),
                        codex_turn_state.clone(),
                        codex_window_id.clone(),
                        codex_turn_metadata.clone(),
                    );
                    }

                    // ── Non-streaming path ──
                    let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;

                    // R5 passthrough: return upstream Responses body directly without re-wrapping
                    if is_passthrough {
                        let (upstream_status, resp_body) =
                            crate::proxy::upstream::handle_buffered_response(
                                upstream_response,
                                max_upstream_body_bytes,
                                false,
                            )
                            .await;
                        crate::proxy::util::log_classification(
                            &state,
                            &classification,
                            &body_str,
                            &prompt,
                            std::time::Instant::now(),
                            if upstream_status.is_success() { "ok" } else { "upstream_error" },
                            idx as u8 + 1,
                            &provider.model,
                        );
                        return crate::proxy::util::json_response(upstream_status, resp_body);
                    }

                    // R2 Anthropic: translate Anthropic response → Chat, then Chat → Responses
                    let (upstream_status, resp_body) = if is_anthropic {
                        crate::proxy::upstream::translate_anthropic_buffered_response(
                            upstream_response,
                            max_upstream_body_bytes,
                        )
                        .await
                    } else {
                        crate::proxy::upstream::handle_buffered_response(
                            upstream_response,
                            max_upstream_body_bytes,
                            false,
                        )
                        .await
                    };

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
                    crate::proxy::util::log_classification_with_usage_and_prev(
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
                        extras.previous_response_id.as_deref(),
                        codex_installation_id.as_deref(),
                        codex_turn_state.as_deref(),
                        codex_window_id.as_deref(),
                        codex_turn_metadata.as_deref(),
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

                    // Extract response_id for cache entry to preserve across hits
                    let response_id = responses_json
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string();

                    // Cache the synthesized Responses body (via original key)
                    if upstream_status.is_success() {
                        if let Some(ref key) = cache_key {
                            if let Some(ref cache) = state.response_cache {
                                cache.put(
                                    key.clone(),
                                    crate::cache::CachedEntry {
                                        body: responses_json.to_string(),
                                        status: upstream_status.as_u16(),
                                        response_id,
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
#[cfg(test)]
mod tests {
    use crate::app::test_helpers::{test_app_with_anthropic_http_client, test_app_with_cache, test_app_with_openai_responses_http_client};
    use crate::test_util::EnvGuard;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_openai_non_streaming() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server, _cache) = test_app_with_cache(60, 10);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"chatcmpl-123","object":"chat.completion","created":1700000000,"model":"gpt-4o","choices":[{"message":{"content":"Hello world","role":"assistant"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#);
        });

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["object"], "response");
        assert!(json["id"].as_str().unwrap().starts_with("resp_"));
        let status = json["status"].as_str().expect("status field");
        assert!(status == "completed" || status == "incomplete", "status should be completed or incomplete, got {status}");
        let output = json["output"].as_array().expect("output array");
        assert!(!output.is_empty(), "output should be non-empty");
        assert_eq!(output[0]["type"].as_str(), Some("message"));
        assert_eq!(output[0]["role"].as_str(), Some("assistant"));
        let content = output[0]["content"].as_array().expect("content array");
        assert!(!content.is_empty(), "content should be non-empty");
        assert_eq!(content[0]["type"].as_str(), Some("output_text"));
        assert_eq!(
            content[0]["text"].as_str(),
            Some("Hello world")
        );
        let usage = json["usage"].as_object().expect("usage object");
        assert!(usage.contains_key("input_tokens"), "usage should have input_tokens");
        assert!(usage.contains_key("output_tokens"), "usage should have output_tokens");

        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_openai_streaming() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server, _cache) = test_app_with_cache(60, 10);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\ndata: [DONE]\n\n");
        });

        let body = r#"{"model":"gpt-4o","input":"hello","stream":true}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let sse = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(sse.contains("event: response.created"));
        assert!(sse.contains(r#""sequence_number""#), "response.created should have sequence_number");
        assert!(sse.contains("event: response.output_item.added"));
        assert!(sse.contains(r#""type":"message""#), "output_item.added should have type:message");
        assert!(sse.contains("event: response.content_part.added"));
        assert!(sse.contains(r#""type":"output_text""#), "content_part.added should have type:output_text");
        assert!(sse.contains("event: response.output_text.delta"));
        assert!(sse.contains(r#""delta":"#), "output_text.delta should have delta field");
        assert!(sse.contains("event: response.completed"));
        assert!(sse.contains(r#""status":"completed""#), "response.completed should have status:completed");
        assert!(sse.contains(r#""usage""#), "response.completed should have usage");
        let delta_count = sse.matches("event: response.output_text.delta").count();
        assert_eq!(delta_count, 2);
        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_anthropic_non_streaming() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server) = test_app_with_anthropic_http_client("TEST_CACHE_PROXY", 10_485_760);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"msg_123","type":"message","content":[{"type":"text","text":"Hello from Anthropic"}],"role":"assistant","model":"claude-3-5-sonnet","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":10}}"#);
        });

        let body = r#"{"model":"claude-3-5-sonnet","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["object"], "response");
        assert!(json["id"].as_str().unwrap().starts_with("resp_"));
        let status = json["status"].as_str().expect("status field");
        assert!(status == "completed" || status == "incomplete", "status should be completed or incomplete, got {status}");
        let output = json["output"].as_array().expect("output array");
        assert!(!output.is_empty(), "output should be non-empty");
        assert_eq!(output[0]["type"].as_str(), Some("message"));
        assert_eq!(output[0]["role"].as_str(), Some("assistant"));
        let content = output[0]["content"].as_array().expect("content array");
        assert!(!content.is_empty(), "content should be non-empty");
        assert_eq!(content[0]["type"].as_str(), Some("output_text"));
        assert_eq!(
            content[0]["text"].as_str(),
            Some("Hello from Anthropic")
        );
        let usage = json["usage"].as_object().expect("usage object");
        assert!(usage.contains_key("input_tokens"), "usage should have input_tokens");
        assert!(usage.contains_key("output_tokens"), "usage should have output_tokens");

        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_anthropic_streaming() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server) = test_app_with_anthropic_http_client("TEST_CACHE_PROXY", 10_485_760);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"content\":[],\"role\":\"assistant\",\"model\":\"claude-3-5-sonnet\",\"stop_reason\":null}}\n\n\
                     event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Step1\"}}\n\n\
                     event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n\
                     event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
                     event: message_stop\ndata: {}\n\n",
                );
        });

        let body = r#"{"model":"claude-3-5-sonnet","input":"hello","stream":true}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let sse = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(sse.contains("event: response.created"));
        assert!(sse.contains(r#""sequence_number""#), "response.created should have sequence_number");
        assert!(sse.contains("event: response.output_item.added"));
        assert!(sse.contains(r#""type":"message""#), "output_item.added should have type:message");
        assert!(sse.contains("event: response.content_part.added"));
        assert!(sse.contains(r#""type":"output_text""#), "content_part.added should have type:output_text");
        assert!(sse.contains("event: response.reasoning_summary_text.delta"));
        assert!(sse.contains("Step1"));
        assert!(sse.contains("event: response.output_text.delta"));
        assert!(sse.contains(r#""delta":"#), "output_text.delta should have delta field");
        assert!(sse.contains("world"));
        assert!(sse.contains("event: response.completed"));
        assert!(sse.contains(r#""status":"completed""#), "response.completed should have status:completed");
        assert!(sse.contains(r#""usage""#), "response.completed should have usage");

        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_passthrough() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server) = test_app_with_openai_responses_http_client("TEST_CACHE_PROXY");

        // R5 passthrough: original Responses body forwarded verbatim to openai_responses provider.
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/responses");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"resp_123","object":"response","created":1700000000,"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"passthrough"}]}],"status":"completed"}"#);
        });

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        // Verify that the response body matches the upstream passthrough
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["object"], "response");
        assert_eq!(json["output"][0]["content"][0]["text"].as_str().unwrap(), "passthrough");

        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_requires_auth() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, _server, _cache) = test_app_with_cache(60, 10);

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should complete");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_upstream_error_forwards_body() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server, _cache) = test_app_with_cache(60, 10);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"error":"rate limit exceeded"}"#);
        });

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should complete");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["error"]["code"], "rate_limit_exceeded");

        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_cache_hit_returns_cached_response() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server, cache) = test_app_with_cache(60, 10);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"test","choices":[{"message":{"content":"hello"}}]}"#);
        });

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let _resp1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(mock.hits(), 1);

        let _resp2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(mock.hits(), 1);

        let stats = cache.stats();
        // moka entry_count() is approximate; at most one entry was inserted
        assert!(stats.entry_count <= 1, "entry_count={} should be <= 1", stats.entry_count);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_forwards_openai_headers() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server, _cache) = test_app_with_cache(60, 10);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions")
                .header("openai-beta", "test-beta");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"test","choices":[{"message":{"content":"hello"}}]}"#);
        });

        let body = r#"{"model":"gpt-4o","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("openai-beta", "test-beta")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_anthropic_two_stage_streaming() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server) = test_app_with_anthropic_http_client("TEST_CACHE_PROXY", 10_485_760);

        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"content\":[],\"role\":\"assistant\",\"model\":\"sf-model\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse);
        });

        let body = r#"{"model":"claude-3-5-sonnet","input":"hello","stream":true}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let sse = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(sse.contains("event: response.created"), "output should start with response.created");
        assert!(sse.contains(r#""sequence_number":1"#), "first event should have sequence_number 1");
        assert!(sse.contains("event: response.output_item.added"), "should have output_item.added");
        assert!(sse.contains(r#""type":"message""#), "output_item should have type:message");
        assert!(sse.contains("event: response.content_part.added"), "should have content_part.added");
        assert!(sse.contains(r#""type":"output_text""#), "content_part should have type:output_text");
        assert!(sse.contains("event: response.output_text.delta"), "should have output_text.delta");
        assert!(sse.contains(r#""delta""#), "delta should contain delta field");
        assert!(sse.contains("Hello"), "delta should contain translated text");
        assert!(sse.contains("world"), "delta should contain translated text");
        assert!(sse.contains("event: response.completed"), "should end with response.completed");
        assert!(sse.contains(r#""status":"completed""#), "completed should have status:completed");
        assert!(!sse.contains("event: message_start"), "raw Anthropic event types should not leak");
        assert!(!sse.contains("event: content_block_delta"), "raw Anthropic event types should not leak");

        let seq_numbers: Vec<u64> = sse.lines()
            .filter(|l| l.contains(r#""sequence_number""#))
            .filter_map(|l| {
                let json: serde_json::Value = serde_json::from_str(l.trim_start_matches("data: ")).ok()?;
                json.get("sequence_number")?.as_u64()
            })
            .collect();
        for i in 1..seq_numbers.len() {
            assert!(seq_numbers[i] > seq_numbers[i - 1],
                "sequence_number should be monotonically increasing: idx {} ({} -> {})", i, seq_numbers[i-1], seq_numbers[i]);
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_responses_handler_anthropic_buffered_tool_use() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test");
        let (app, server) = test_app_with_anthropic_http_client("TEST_CACHE_PROXY", 10_485_760);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"msg_t1","type":"message","role":"assistant","model":"sf-model","content":[{"type":"tool_use","id":"toolu_xyz","name":"get_weather","input":{"city":"London"}}],"stop_reason":"tool_use","usage":{"input_tokens":15,"output_tokens":10}}"#);
        });

        let body = r#"{"model":"claude-3-5-sonnet","input":"hello"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");

        assert_eq!(json["object"], "response");
        assert_eq!(json["output"][0]["type"], "function_call");
        assert!(json["output"][0]["name"].as_str().is_some(), "function_call should have name");
        assert!(json["output"][0]["arguments"].as_str().is_some(), "function_call should have arguments");
        assert_eq!(json["output"][0]["name"].as_str(), Some("get_weather"));
        assert!(json["output"][0]["arguments"].as_str().unwrap().contains("London"),
            "arguments should contain the tool input data");
    }
}
