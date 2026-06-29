use axum::body::Bytes;
use axum::http::StatusCode;
use tracing::warn;

pub(crate) fn build_upstream_request(
    client: &reqwest::Client,
    provider: &crate::routing::ProviderEntry,
    body: &Bytes,
    api_key: &str,
    auth_providers: &[crate::config::types::AuthProviderConfig],
    forward_headers: &[(String, String)],
) -> Result<(bool, reqwest::RequestBuilder), String> {
    let mut req_body: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;

    let client_wants_stream = req_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let serde_json::Value::Object(map) = &mut req_body {
        map.insert(
            "model".to_string(),
            serde_json::Value::String(provider.model.clone()),
        );
    } else {
        return Err("request body must be a JSON object".to_string());
    }

    let modified_body = serde_json::to_vec(&req_body).unwrap_or_else(|_| body.to_vec());

    // `auth_headers_for` is the single emission point for the full upstream
    // header set: it resolves the credential AND, for anthropic providers,
    // appends the client-forwarded `anthropic-*` / `x-claude-code-*` headers
    // (with a client `anthropic-version` preferred over the default). Applying
    // only its return value here — instead of also forwarding client headers
    // directly — avoids duplicate headers and keeps one decision point keyed
    // on provider_type. The auth credential is always applied, and
    // `collect_forward_headers` excludes `authorization` / `x-api-key`, so a
    // client can never overwrite the resolved upstream key.
    let auth_headers = crate::classification::llm::auth_headers_for(
        auth_providers,
        &provider.provider_type,
        api_key,
        forward_headers,
    );

    let mut req = client
        .post(&provider.endpoint)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(modified_body);
    for (name, value) in &auth_headers {
        req = req.header(name.as_str(), value.as_str());
    }
    if let Some(ms) = provider.timeout_ms {
        req = req.timeout(std::time::Duration::from_millis(ms));
    }

    Ok((client_wants_stream, req))
}

/// Buffer an upstream response. For OpenAI traffic, non-2xx responses are
/// wrapped in the `upstream_error_json` envelope so the client always sees a
/// consistent JSON shape. For Anthropic traffic (`anthropic_errors = true`),
/// non-2xx responses pass through verbatim — the Anthropic upstream already
/// produces an Anthropic-format error body, and re-wrapping it would
/// double-encode the message and break the client's error contract.
pub(crate) async fn handle_buffered_response(
    mut upstream_response: reqwest::Response,
    max_upstream_body_bytes: usize,
    anthropic_errors: bool,
) -> (StatusCode, String) {
    let upstream_status = upstream_response.status();
    if !upstream_status.is_success() {
        // Cap the upstream error body to 2 KB to bound latency and memory on
        // large error payloads (lesson: "Handle upstream error bodies without
        // full buffering where possible").
        const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
        let mut error_bytes = Vec::new();
        let error_body = loop {
            match upstream_response.chunk().await {
                Ok(Some(chunk)) => {
                    if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                        break String::from_utf8_lossy(&error_bytes).into_owned();
                    }
                    error_bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break String::from_utf8_lossy(&error_bytes).into_owned(),
                Err(e) => break e.to_string(),
            }
        };
        let status =
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        if anthropic_errors {
            // Pass through verbatim — the upstream already speaks Anthropic.
            return (status, error_body);
        }
        // OpenAI: re-encode the upstream error text in our envelope so the
        // client sees a consistent shape regardless of upstream quirks.
        let error_text = error_body
            .chars()
            .take(512)
            .collect::<String>()
            .replace(['\n', '\r'], " ");
        return (
            status,
            crate::proxy::util::upstream_error_json(upstream_status.as_u16(), &error_text),
        );
    }

    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    let upstream_body = loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if upstream_body_bytes.len() + chunk.len() > max_upstream_body_bytes {
                    return (
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::upstream_error_json(502, "upstream response too large"),
                    );
                }
                upstream_body_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break String::from_utf8_lossy(&upstream_body_bytes).into_owned(),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    crate::proxy::util::upstream_error_json(502, &e.to_string()),
                );
            }
        }
    };

    let response_body = match serde_json::from_str::<serde_json::Value>(&upstream_body) {
        Ok(value) => serde_json::to_string(&value).unwrap_or(upstream_body),
        Err(_) => upstream_body,
    };
    (StatusCode::OK, response_body)
}

/// Buffer an Anthropic upstream response and translate it to OpenAI format.
/// Non-2xx errors are translated from Anthropic error shape to OpenAI error envelope.
pub(crate) async fn translate_anthropic_buffered_response(
    mut upstream_response: reqwest::Response,
    max_upstream_body_bytes: usize,
) -> (StatusCode, String) {
    let upstream_status = upstream_response.status();
    if !upstream_status.is_success() {
        const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
        let mut error_bytes = Vec::new();
        let error_body = loop {
            match upstream_response.chunk().await {
                Ok(Some(chunk)) => {
                    if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                        break String::from_utf8_lossy(&error_bytes).into_owned();
                    }
                    error_bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break String::from_utf8_lossy(&error_bytes).into_owned(),
                Err(e) => break e.to_string(),
            }
        };
        let status =
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let translated =
            crate::protocol::response::translate_error(&error_body, upstream_status.as_u16());
        return (status, translated);
    }

    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    let upstream_body = loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if upstream_body_bytes.len() + chunk.len() > max_upstream_body_bytes {
                    return (
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::upstream_error_json(502, "upstream response too large"),
                    );
                }
                upstream_body_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break String::from_utf8_lossy(&upstream_body_bytes).into_owned(),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    crate::proxy::util::upstream_error_json(502, &e.to_string()),
                );
            }
        }
    };

    match serde_json::from_str::<serde_json::Value>(&upstream_body) {
        Ok(parsed) => match crate::protocol::response::translate_response(&parsed) {
            Ok(translated) => {
                let body_str = serde_json::to_string(&translated).unwrap_or(upstream_body);
                (StatusCode::OK, body_str)
            }
            Err(_) => (StatusCode::OK, upstream_body),
        },
        Err(_) => (StatusCode::OK, upstream_body),
    }
}

/// Buffer an OpenAI upstream response and translate it to Anthropic Messages format.
/// Used by messages_handler when the upstream speaks OpenAI protocol.
pub(crate) async fn translate_openai_buffered_to_anthropic(
    mut upstream_response: reqwest::Response,
    max_upstream_body_bytes: usize,
) -> (StatusCode, String) {
    let upstream_status = upstream_response.status();
    if !upstream_status.is_success() {
        const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
        let mut error_bytes = Vec::new();
        let error_body = loop {
            match upstream_response.chunk().await {
                Ok(Some(chunk)) => {
                    if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                        break String::from_utf8_lossy(&error_bytes).into_owned();
                    }
                    error_bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break String::from_utf8_lossy(&error_bytes).into_owned(),
                Err(e) => break e.to_string(),
            }
        };
        let status =
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let translated = crate::protocol::response::openai_to_anthropic_error(
            &error_body,
            upstream_status.as_u16(),
        );
        return (status, translated);
    }

    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    let upstream_body = loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if upstream_body_bytes.len() + chunk.len() > max_upstream_body_bytes {
                    return (
                        StatusCode::BAD_GATEWAY,
                        crate::proxy::util::anthropic_error_json(
                            "api_error",
                            "upstream response too large",
                        ),
                    );
                }
                upstream_body_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break String::from_utf8_lossy(&upstream_body_bytes).into_owned(),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    crate::proxy::util::anthropic_error_json("api_error", &e.to_string()),
                );
            }
        }
    };

    match serde_json::from_str::<serde_json::Value>(&upstream_body) {
        Ok(parsed) => match crate::protocol::response::openai_to_anthropic_response(&parsed) {
            Ok(translated) => {
                let body_str = serde_json::to_string(&translated).unwrap_or(upstream_body);
                (StatusCode::OK, body_str)
            }
            Err(e) => {
                warn!("OAI→Anthropic response translation failed: {e}");
                (StatusCode::OK, upstream_body)
            }
        },
        Err(e) => {
            warn!("OAI→Anthropic response JSON parse failed: {e}");
            (StatusCode::OK, upstream_body)
        }
    }
}

/// Returns true when a send error or upstream response indicates the request
/// can be retried on another provider: connection errors, timeouts, 5xx, 429.
pub(crate) fn is_retryable_error(result: &Result<reqwest::Response, reqwest::Error>) -> bool {
    match result {
        Err(e) => e.is_connect() || e.is_timeout(),
        Ok(response) => {
            let status = response.status().as_u16();
            status == 429 || (500..=599).contains(&status)
        }
    }
}
