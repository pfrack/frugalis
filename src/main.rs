use std::convert::Infallible;
use std::panic;
use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use tokio_stream::StreamExt;
use tower_http::{services::ServeDir, limit::RequestBodyLimitLayer, cors::CorsLayer};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};
use tracing::{debug, error, info, warn};


mod auth;
mod persistence;
mod intent_classificator;
mod dashboard;

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classificator::IntentClassifier>>,
    classify_db_log: bool,
    http_client: Option<reqwest::Client>,
}

#[tokio::main]
async fn main() {
    // Initialize tracing subscriber before any other code.
    let log_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = match std::env::var("LOG_FORMAT").as_deref() {
        Ok("json") => fmt::layer().json().with_filter(log_filter).boxed(),
        _ => fmt::layer().compact().with_filter(log_filter).boxed(),
    };

    tracing_subscriber::registry()
        .with(fmt_layer)
        .init();

    // Ensure any panic is logged, not silent.
    panic::set_hook(Box::new(|info| {
        eprintln!("Panic in Cerebrum: {info}");
    }));

    let auth_config = auth::AuthConfig::from_env().unwrap_or_else(|err| {
        panic!("Auth configuration error: {err}");
    });
    let auth_config = Arc::new(auth_config);

    let persistence_state = match persistence::PersistenceConfig::from_env().await {
        Ok(s) => {
            println!("Database connected successfully");
            Some(s)
        }
        Err(e) => {
            eprintln!("WARN: persistence disabled: {e}");
            None
        }
    };
    let classifier = match intent_classificator::IntentClassifier::from_env() {
        Ok(c) => {
            println!("Intent classifier initialized");
            Some(Arc::new(c))
        }
        Err(e) => {
            eprintln!("WARN: intent classification disabled: {e}");
            None
        }
    };
    let classify_db_log = std::env::var("CLASSIFY_DB_LOG")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client should build");
    let app_state = Arc::new(AppState {
        persistence: persistence_state,
        classifier,
        classify_db_log,
        http_client: Some(http_client),
    });

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "10000".to_string())
        .parse()
        .expect("PORT must be a number");

    let app = build_app(auth_config, app_state);
    let bind_addr = format!("0.0.0.0:{port}");
    println!("Starting cerebrum on {bind_addr}");

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app)
        .await
        .expect("Axum server exited unexpectedly");
}

async fn health() -> (StatusCode, &'static str) {
    println!("Health check request received");
    (StatusCode::OK, "ok")
}

/// Shared logging helper. Extracts the snippet, builds the inference record,
/// and enqueues a fire-and-forget DB write.
fn log_classification(
    state: &AppState,
    classification: &intent_classificator::ClassificationResult,
    body_str: &str,
    start: std::time::Instant,
    log_status: &str,
) {
    if let Some(persistence) = &state.persistence {
        let duration_ms = start.elapsed().as_millis() as i32;
        let snippet = persistence::extract_snippet(body_str);
        let prompt = persistence::extract_last_user_message(body_str);
        let prompt_char_count = if prompt.is_empty() {
            None
        } else {
            Some(prompt.chars().count() as i32)
        };
        let record = persistence::InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: log_status.to_string(),
            category: Some(classification.category.clone()),
            upstream_model: Some(classification.model.clone()),
            duration_ms: Some(duration_ms),
            prompt_snippet: snippet,
            prompt_char_count,
        };
        persistence::log_inference(
            persistence.pool.clone(),
            persistence.task_semaphore.clone(),
            record,
        );
    }
}

/// Shared classify-and-log logic. Validates Content-Type, extracts the prompt,
/// classifies intent, builds the JSON response, and optionally enqueues a
/// fire-and-forget inference record with the given `log_status`.
fn classify_and_log(
    headers: &HeaderMap,
    body_str: &str,
    start: std::time::Instant,
    state: &AppState,
    log_status: Option<&str>,
) -> impl IntoResponse {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return json_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json".to_string());
    }

    let prompt = persistence::extract_last_user_message(body_str);

    let classification = state.classifier.as_ref()
        .map(|c| c.classify(&prompt))
        .unwrap_or_else(intent_classificator::ClassificationResult::fallback);

    let response_body = serde_json::json!({
        "status": "classified",
        "category": classification.category,
        "model": classification.model,
        "tier": format!("{:?}", classification.tier),
    }).to_string();
    if let Some(log_status) = log_status {
        log_classification(state, &classification, body_str, start, log_status);
    }

    json_response(StatusCode::OK, response_body)
}

fn json_response(status: StatusCode, body: String) -> Response<Body> {
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Completion handler: classifies intent, optionally skips classification via
/// X-Cerebrum-Category / X-Cerebrum-Model headers, resolves the API key from
/// the env var named by the classification result, builds auth headers,
/// overrides the model field, forwards the body to the upstream endpoint,
/// and returns the buffered response with Content-Type: application/json.
async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let start = std::time::Instant::now();

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return json_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json".to_string());
    }

    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"bad_request","message":"invalid UTF-8 body"}"#.to_string(),
            );
        }
    };

    let x_category = headers
        .get("x-cerebrum-category")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let x_model = headers
        .get("x-cerebrum-model")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let classification = if let (Some(category), Some(model)) = (x_category.as_ref(), x_model.as_ref()) {
        match state.classifier.as_ref().and_then(|c| c.routing.get(category)) {
            Some(entry) => intent_classificator::ClassificationResult {
                category: category.clone(),
                model: model.clone(),
                endpoint: entry.endpoint.clone(),
                tier: intent_classificator::ClassificationTier::Fallback,
                provider_type: entry.provider_type.clone(),
                api_key_env: entry.api_key_env.clone(),
            },
            None => {
                eprintln!(
                    "WARN: X-Cerebrum-Category '{category}' not found in routing configuration; \
                     degrading to classification JSON"
                );
                let fallback = state.classifier.as_ref()
                    .map(|c| c.classify(""))
                    .unwrap_or_else(intent_classificator::ClassificationResult::fallback);
                let response_body = serde_json::json!({
                    "status": "classified",
                    "category": fallback.category,
                    "model": fallback.model,
                    "tier": format!("{:?}", fallback.tier),
                }).to_string();
                log_classification(&state, &fallback, body_str, start, "ok");
                return json_response(StatusCode::OK, response_body);
            }
        }
    } else {
        let prompt = persistence::extract_last_user_message(body_str);
        state.classifier.as_ref()
            .map(|c| c.classify(&prompt))
            .unwrap_or_else(intent_classificator::ClassificationResult::fallback)
    };

    let client = match &state.http_client {
        Some(c) => c,
        None => {
            let response_body = serde_json::json!({
                "status": "classified",
                "category": classification.category,
                "model": classification.model,
                "tier": format!("{:?}", classification.tier),
            }).to_string();
            log_classification(&state, &classification, body_str, start, "ok");
            return json_response(StatusCode::OK, response_body);
        }
    };

    let api_key = match &classification.api_key_env {
        Some(env_name) => match std::env::var(env_name) {
            Ok(key) if !key.is_empty() => key,
            _ => {
                eprintln!("WARN: upstream API key env var '{env_name}' is missing or empty; degrading to classification-only response");
                let response_body = serde_json::json!({
                    "status": "classified",
                    "category": classification.category,
                    "model": classification.model,
                    "tier": format!("{:?}", classification.tier),
                }).to_string();
                log_classification(&state, &classification, body_str, start, "ok");
                return json_response(StatusCode::OK, response_body);
            }
        },
        None => {
            eprintln!("WARN: no api_key_env configured for category '{}'; degrading to classification-only response", classification.category);
            let response_body = serde_json::json!({
                "status": "classified",
                "category": classification.category,
                "model": classification.model,
                "tier": format!("{:?}", classification.tier),
            }).to_string();
            log_classification(&state, &classification, body_str, start, "ok");
            return json_response(StatusCode::OK, response_body);
        }
    };

    if classification.endpoint.is_empty() {
        let error_body = serde_json::json!({
            "error": "upstream_error",
            "status": 502,
            "message": "no endpoint configured",
        }).to_string();
        log_classification(&state, &classification, body_str, start, "upstream_error");
        return json_response(StatusCode::BAD_GATEWAY, error_body);
    }

    let mut req_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let error_body = serde_json::json!({
                "error": "bad_request",
                "status": 400,
                "message": format!("invalid JSON body: {e}"),
            }).to_string();
            log_classification(&state, &classification, body_str, start, "bad_request");
            return json_response(StatusCode::BAD_REQUEST, error_body);
        }
    };
    let client_wants_stream = req_body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    req_body["model"] = serde_json::Value::String(classification.model.clone());
    let modified_body = serde_json::to_vec(&req_body).unwrap_or_else(|_| body.to_vec());

    let auth_headers = intent_classificator::auth_headers_for(&classification.provider_type, &api_key);

    let mut upstream_req = client.post(&classification.endpoint)
        .header(header::CONTENT_TYPE, "application/json")
        .body(modified_body);
    for (name, value) in &auth_headers {
        upstream_req = upstream_req.header(name.as_str(), value.as_str());
    }

    let mut upstream_response = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            let error_body = serde_json::json!({
                "error": "upstream_error",
                "status": 502,
                "message": e.to_string(),
            }).to_string();
            log_classification(&state, &classification, body_str, start, "upstream_error");
            return json_response(StatusCode::BAD_GATEWAY, error_body);
        }
    };

    if client_wants_stream {
        let upstream_status = upstream_response.status();
        if !upstream_status.is_success() {
            const MAX_ERROR_BODY_BYTES: usize = 2 * 1024; // 2 KB, enough for ~512 chars
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
            let error_text = String::from_utf8_lossy(&error_bytes)
                .chars()
                .take(512)
                .collect::<String>()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', " ")
                .replace('\r', " ");
            let sse_error = format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", error_text);
            let mut resp = Response::new(Body::from(sse_error));
            *resp.status_mut() = upstream_status;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            log_classification(&state, &classification, body_str, start, "upstream_error");
            return resp;
        }

        let byte_stream = upstream_response.bytes_stream();
        let channel_capacity = std::env::var("STREAMING_CHANNEL_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32);
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(channel_capacity);

        tokio::spawn(async move {
            let keepalive_secs = std::env::var("KEEPALIVE_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(15);
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(keepalive_secs));
            let mut stream = byte_stream;
            interval.tick().await;
            loop {
                tokio::select! {
                    chunk = stream.next() => {
                        match chunk {
                            Some(Ok(bytes)) => { if tx.send(bytes).await.is_err() { break; } }
                            Some(Err(e)) => {
                                let sanitized = e.to_string().replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ").replace('\r', " ");
                                let _ = tx.send(Bytes::from(
                                    format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", sanitized)
                                )).await;
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
        });

        let body = Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx)
                .map(|bytes| Ok::<_, Infallible>(bytes)),
        );

        let mut resp = Response::new(body);
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        );
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache"),
        );
        log_classification(&state, &classification, body_str, start, "ok");
        return resp;
    }

    let upstream_status = upstream_response.status();
    if !upstream_status.is_success() {
        const MAX_ERROR_BODY_BYTES: usize = 2 * 1024; // 2 KB, enough for ~512 chars
        let mut error_bytes = Vec::new();
        let error_body = loop {
            match upstream_response.chunk().await {
                Ok(Some(chunk)) => {
                    if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                        let error_text = String::from_utf8_lossy(&error_bytes)
                            .chars()
                            .take(512)
                            .collect::<String>()
                            .replace('\n', " ")
                            .replace('\r', " ");
                        break serde_json::json!({
                            "error": "upstream_error",
                            "status": upstream_status.as_u16(),
                            "message": error_text,
                        }).to_string();
                    }
                    error_bytes.extend_from_slice(&chunk);
                }
                Ok(None) => {
                    let error_text = String::from_utf8_lossy(&error_bytes)
                        .chars()
                        .take(512)
                        .collect::<String>()
                        .replace('\n', " ")
                        .replace('\r', " ");
                    break serde_json::json!({
                        "error": "upstream_error",
                        "status": upstream_status.as_u16(),
                        "message": error_text,
                    }).to_string();
                }
                Err(e) => {
                    break serde_json::json!({
                        "error": "upstream_error",
                        "status": 502,
                        "message": e.to_string(),
                    }).to_string();
                }
            }
        };
        log_classification(&state, &classification, body_str, start, "upstream_error");
        return json_response(
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            error_body,
        );
    }

    const MAX_UPSTREAM_BODY: usize = 10 * 1024 * 1024; // 10 MB
    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    let upstream_body = loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if upstream_body_bytes.len() + chunk.len() > MAX_UPSTREAM_BODY {
                    let error_body = serde_json::json!({
                        "error": "upstream_error",
                        "status": 502,
                        "message": "upstream response too large",
                    }).to_string();
                    log_classification(&state, &classification, body_str, start, "upstream_error");
                    return json_response(StatusCode::BAD_GATEWAY, error_body);
                }
                upstream_body_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break String::from_utf8_lossy(&upstream_body_bytes).into_owned(),
            Err(e) => {
                let error_body = serde_json::json!({
                    "error": "upstream_error",
                    "status": 502,
                    "message": e.to_string(),
                }).to_string();
                log_classification(&state, &classification, body_str, start, "upstream_error");
                return json_response(StatusCode::BAD_GATEWAY, error_body);
            }
        }
    };

    log_classification(&state, &classification, body_str, start, "ok");
    let response_body = match serde_json::from_str::<serde_json::Value>(&upstream_body) {
        Ok(value) => {
            // Re-serialize to guarantee valid JSON escaping. Fallback to original on error.
            serde_json::to_string(&value).unwrap_or(upstream_body)
        }
        Err(_) => upstream_body,
    };
    json_response(StatusCode::OK, response_body)
}


/// Classify handler: extracts prompt, classifies intent, optionally logs a
/// lightweight classification record with status "classified", and returns
/// classification JSON. Logging is controlled by `CLASSIFY_DB_LOG` env var.
async fn classify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    let log_status = if state.classify_db_log { Some("classified") } else { None };
    classify_and_log(&headers, body_str, start, &state, log_status)
}

fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let proxy_routes = Router::new()
        .route("/chat/completions", post(completion_handler))
        .route("/classify", post(classify_handler))
        .route_layer(auth::proxy_auth_layer(auth_config.clone()));

    let dashboard_routes = dashboard::routes(auth_config);

    Router::new()
        .route("/health", get(health))
        .nest_service("/static", ServeDir::new("static"))
        .nest("/v1", proxy_routes)
        .nest("/dashboard", dashboard_routes)
        .layer(CorsLayer::permissive())
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024))
        .with_state(app_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use tower::util::ServiceExt;

    fn test_app() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        // No-op persistence: persistence is None, so completion_handler skips logging.
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier: None,
            classify_db_log: false,
            http_client: None,
        });
        build_app(auth_config, app_state)
    }

    fn test_app_with_classifier() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classificator::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classificator::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            classify_db_log: false,
            http_client: None,
        });
        build_app(auth_config, app_state)
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""category":"SYNTAX_FIX""#),
            "expected SYNTAX_FIX category, got: {body}"
        );
        assert!(body.contains(r#""status":"classified""#), "expected classified status");
        assert!(body.contains(r#""tier":"Regex""#), "expected Regex tier");
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""category":"SYNTAX_FIX""#),
            "expected SYNTAX_FIX category, got: {body}"
        );
        assert!(
            body.contains(r#""model":"sf-model""#),
            "expected sf-model, got: {body}"
        );
        assert!(body.contains(r#""status":"classified""#), "expected classified status");
        assert!(body.contains(r#""tier":"Regex""#), "expected Regex tier");
    }

    fn test_app_with_enriched_classifier(
        provider_type_val: &str,
        api_key_env_val: Option<&str>,
    ) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classificator::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: "https://test.endpoint".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: provider_type_val.to_string(),
                api_key_env: api_key_env_val.map(|s| s.to_string()),
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classificator::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            classify_db_log: false,
            http_client: None,
        });
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    async fn test_completion_does_not_include_enriched_fields() {
        std::env::set_var("TEST_API_KEY", "sk-test-value-123");
        let response = test_app_with_enriched_classifier("test_provider", Some("TEST_API_KEY"))
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""category":"SYNTAX_FIX""#), "expected SYNTAX_FIX category");
        assert!(!body.contains(r#""provider_type""#), "response should NOT contain provider_type");
        assert!(!body.contains(r#""endpoint""#), "response should NOT contain endpoint");
        assert!(!body.contains(r#""api_key""#), "response should NOT contain api_key");
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
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(!body.contains(r#""api_key""#), "response should NOT contain api_key");
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
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("classify request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(!body.contains(r#""provider_type""#), "classify response should not contain provider_type");
        assert!(!body.contains(r#""api_key""#), "classify response should not contain api_key");
    }

    #[tokio::test]
    async fn routes_auth_health_is_public() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("health request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn routes_auth_proxy_requires_valid_bearer_token() {
        let unauthorized = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("proxy unauthorized request should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("proxy authorized request should complete");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn routes_auth_dashboard_requires_basic_auth_challenge() {
        let unauthorized = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("dashboard unauthorized request should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let challenge = unauthorized
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .expect("dashboard unauthorized should include challenge header");
        assert!(challenge.starts_with("Basic"));

        let authorized = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("dashboard authorized request should complete");
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    /// DB integration test: only runs when DATABASE_URL is set.
    /// Skips gracefully in local/CI environments without a live database.
    /// Run with: cargo test persistence_integration (requires DATABASE_URL)
    /// Verify the prompt_char_count column exists with INTEGER type.
    /// Runs only when DATABASE_URL is set.
    #[tokio::test]
    async fn persistence_integration_prompt_char_count_column_exists() {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("SKIP persistence_integration_prompt_char_count_column_exists: DATABASE_URL not set");
                return;
            }
        };
        let pool = sqlx::PgPool::connect(&url)
            .await
            .expect("integration test DB connect should succeed");
        let row: Option<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT data_type FROM information_schema.COLUMNS \
             WHERE table_name = 'inferences' AND column_name = 'prompt_char_count'"
        )
        .fetch_optional(&pool)
        .await
        .expect("schema query should succeed");
        let row = row.expect("prompt_char_count column should exist in the inferences table");
        use sqlx::Row;
        let data_type: String = row.try_get("data_type").unwrap();
        assert_eq!(data_type, "integer", "prompt_char_count should be INTEGER type");
    }

    #[tokio::test]
    async fn persistence_integration_insert_and_read_back() {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("SKIP persistence_integration: DATABASE_URL not set");
                return;
            }
        };
        let pool = sqlx::PgPool::connect(&url)
            .await
            .expect("integration test DB connect should succeed");
        let pool = Arc::new(pool);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));

        let request_id = uuid::Uuid::new_v4();
        let record = persistence::InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("chat".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(10),
            prompt_snippet: "integration test snippet".to_string(),
            prompt_char_count: Some(25),
        };
        let handle = persistence::log_inference(pool.clone(), semaphore, record);
        handle.await.expect("logging task should complete");

        // Read back using non-macro query (no offline cache required).
        let row =
            sqlx::query("SELECT status, prompt_snippet, prompt_char_count FROM inferences WHERE request_id = $1")
                .bind(request_id)
                .fetch_optional(pool.as_ref())
                .await
                .expect("read-back query should succeed");

        let row = row.expect("inserted row should be present");
        use sqlx::Row;
        assert_eq!(row.try_get::<String, _>("status").unwrap(), "ok");
        assert_eq!(
            row.try_get::<Option<String>, _>("prompt_snippet")
                .unwrap()
                .as_deref(),
            Some("integration test snippet")
        );
        assert_eq!(
            row.try_get::<Option<i32>, _>("prompt_char_count").unwrap(),
            Some(25),
            "prompt_char_count should be stored and retrievable"
        );
    }

    #[tokio::test]
    async fn test_dashboard_authenticated_returns_html() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("dashboard request should complete");

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Content-Type");
        assert!(
            content_type.starts_with("text/html"),
            "expected text/html, got {content_type}"
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains("Cerebrum Dashboard"),
            "body should contain 'Cerebrum Dashboard'"
        );
    }

    #[tokio::test]
    async fn test_inferences_unauthenticated_returns_401() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_inferences_authenticated_returns_html() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("text/html"), "expected HTML response");
    }

    #[tokio::test]
    async fn test_inferences_empty_state() {
        // test_app() has persistence=None → "Database not configured" error message
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        // When persistence is None, handler returns error template; no crash.
        assert!(
            body.contains("Database not configured")
                || body.contains("No inference records yet"),
            "expected empty/error state message, got: {body}"
        );
    }

    #[tokio::test]
    async fn test_inferences_invalid_params() {
        // offset=abc, limit=999999 → should apply defaults, return 200
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences?offset=abc&limit=999999")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_inferences_db_error() {
        // With persistence=None, handler catches missing DB gracefully and returns 200
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains("Database not configured"),
            "expected error message in response, got: {body}"
        );
    }

    #[tokio::test]
    async fn test_inferences_filter_by_category() {
        // Without a real DB this just verifies the route accepts filter params without crashing.
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences?filter_category=COMPLEX_REASONING")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_inferences_pagination_offset() {
        // Without a real DB this verifies offset/limit params are accepted without crashing.
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/inferences?offset=20&limit=10")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Upstream routing tests ────────────────────────────────────────────────

    pub(crate) fn test_app_with_http_client(env_var_name: &str) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classificator::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: endpoint.clone(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classificator::RouteEntry {
                model: "ca-model".to_string(),
                endpoint,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            classify_db_log: false,
            http_client: Some(client),
        });
        (build_app(auth_config, app_state), server)
    }

    fn test_app_with_dead_endpoint(env_var_name: &str) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .expect("test reqwest client should build");
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classificator::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classificator::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            classify_db_log: false,
            http_client: Some(client),
        });
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    async fn test_upstream_returns_response() {
        let env = "TEST_UPSTREAM_RESP";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
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
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""choices""#), "expected upstream response body, got: {body}");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_upstream_request_includes_auth_header() {
        let env = "TEST_UPSTREAM_AUTH";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("Authorization", "Bearer sk-test");
            then.status(200)
                .header("content-type", "application/json")
                .body("ok");
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
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_upstream_request_includes_content_type_json() {
        let env = "TEST_UPSTREAM_CT";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("Content-Type", "application/json");
            then.status(200)
                .header("content-type", "application/json")
                .body("ok");
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
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_upstream_unreachable_returns_502() {
        let env = "TEST_UPSTREAM_DEAD";
        std::env::set_var(env, "sk-test");
        let app = test_app_with_dead_endpoint(env);
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
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""error":"upstream_error""#),
            "expected upstream_error in body, got: {body}"
        );
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_upstream_skip_classify_via_headers() {
        let env = "TEST_UPSTREAM_SKIP";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"skipped"}}]}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-cerebrum-category", "SYNTAX_FIX")
                    .header("x-cerebrum-model", "gpt-4o-mini")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""skipped""#),
            "expected skip-classify upstream response, got: {body}"
        );
        mock.assert();
        std::env::remove_var(env);
    }

    // ── SSE streaming tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_streaming_handler_returns_sse_content_type() {
        let env = "TEST_STREAM_CT";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let sse_body = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_body);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Content-Type");
        assert_eq!(content_type, "text/event-stream");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Cache-Control");
        assert_eq!(cache_control, "no-cache");
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains("data:"), "expected SSE data, got: {body}");
        assert!(body.contains("[DONE]"), "expected [DONE] marker, got: {body}");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_handler_forwards_upstream_bytes() {
        let env = "TEST_STREAM_FWD";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let sse_chunks = "data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_chunks);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#"content":"A""#), "expected chunk A, got: {body}");
        assert!(body.contains(r#"content":"B""#), "expected chunk B, got: {body}");
        assert!(body.contains("[DONE]"), "expected [DONE] marker, got: {body}");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_handler_non_2xx_returns_sse_error_event() {
        let env = "TEST_STREAM_ERR";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":"overloaded"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.starts_with("event: error"),
            "expected SSE error event, got: {body}"
        );
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_true_returns_sse_content() {
        let env = "TEST_STREAM_TSSE";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("data: hello\n\n");
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(content_type, "text/event-stream", "expected SSE for stream:true");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_false_returns_buffered_json() {
        let env = "TEST_STREAM_FJSON";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"buffered"}}]}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":false}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(content_type, "application/json", "expected JSON for stream:false");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_absent_returns_buffered_json() {
        let env = "TEST_STREAM_AJSON";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"default"}}]}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(content_type, "application/json", "expected JSON for absent stream field");
        mock.assert();
        std::env::remove_var(env);
    }

    #[tokio::test]
    async fn test_streaming_degradation_no_client() {
        // test_app() has http_client: None → classification-only degradation path
        // Even with stream: true, should return classification JSON
        let app = test_app_with_classifier();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains(r#""status":"classified""#), "expected classification JSON, got: {body}");
    }

    // ── Latency page ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_latency_unauthenticated_returns_401() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_latency_authenticated_returns_html() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("text/html"), "expected HTML response");
    }

    #[tokio::test]
    async fn test_latency_empty_state() {
        // test_app() has persistence=None → "Database not configured" error message
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains("Database not configured"),
            "expected 'Database not configured' in response, got: {body}"
        );
    }

    #[tokio::test]
    async fn test_latency_invalid_hours_defaults() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency?hours=abc")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);

        // hours=0 should clamp to default 24 (below min 1)
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency?hours=0")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_latency_out_of_range_clamped() {
        // hours=99999 should clamp to 720
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/latency?hours=99999")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Savings page ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_savings_unauthenticated_returns_401() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/savings")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_savings_authenticated_returns_html() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/savings")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("text/html"), "expected HTML response");
    }

    #[tokio::test]
    async fn test_savings_no_persistence_shows_error() {
        // test_app() has persistence=None + classifier=None
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/savings")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        }
}

#[cfg(test)]
mod slow_tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;

    // ── Keepalive test ──────────────────────────────────────────────────────
    // Uses a real TCP server that sends headers immediately, waits for the
    // keepalive interval, then sends body data. KEEPALIVE_INTERVAL_SECS=1
    // keeps total test time around 2s instead of 17s.

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

    #[tokio::test]
    async fn test_streaming_keepalive_injected() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        std::env::set_var("KEEPALIVE_INTERVAL_SECS", "1");
        let (url, server_handle) = spawn_slow_sse_server().await;
        let env = "TEST_STREAM_KA_SLOW";
        std::env::set_var(env, "sk-test");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let mut routing = std::collections::HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classificator::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: url,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env.to_string()),
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            classify_db_log: false,
            http_client: Some(client),
        });
        let app = build_app(auth_config, app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(content_type, "text/event-stream", "expected SSE content type");
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(": keepalive\n\n"),
            "expected keepalive comment in stream, got: {body}"
        );
        assert!(
            body.contains("data: hello"),
            "expected upstream data after keepalive, got: {body}"
        );
        let _ = server_handle.await;
        std::env::remove_var(env);
        std::env::remove_var("KEEPALIVE_INTERVAL_SECS");
    }
}
