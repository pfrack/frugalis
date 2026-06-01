use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use askama::Template;
use askama_web::WebTemplate;
use std::collections::HashMap;

mod auth;
mod persistence;
mod intent_classificator;

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/index.html")]
struct DashboardIndex {
    summary: Option<persistence::LatencySummary>,
    error: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/inferences.html")]
struct InferencesTemplate {
    records: Vec<persistence::InferenceLog>,
    page: u32,
    total_pages: u32,
    error: Option<String>,
    filter_category: Option<String>,
    filter_model: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/latency.html")]
struct LatencyTemplate {
    summary: Option<persistence::LatencySummary>,
    hours: u32,
    error: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/savings.html")]
struct SavingsTemplate {
    estimate: Option<persistence::SavingsEstimate>,
    error: Option<String>,
    baseline_model: String,
}

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classificator::IntentClassifier>>,
    classify_db_log: bool,
    http_client: Option<reqwest::Client>,
}

#[tokio::main]
async fn main() {
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
) -> (StatusCode, String) {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json".to_string());
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

    (StatusCode::OK, response_body)
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
) -> impl IntoResponse {
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
            return json_response(StatusCode::BAD_REQUEST, r#"{"error":"bad_request","message":"invalid UTF-8 body"}"#.to_string());
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

    if state.http_client.is_none() {
        let response_body = serde_json::json!({
            "status": "classified",
            "category": classification.category,
            "model": classification.model,
            "tier": format!("{:?}", classification.tier),
        }).to_string();
        log_classification(&state, &classification, body_str, start, "ok");
        return json_response(StatusCode::OK, response_body);
    }

    let api_key = match &classification.api_key_env {
        Some(env_name) => match std::env::var(env_name) {
            Ok(key) if !key.is_empty() => key,
            _ => {
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
    req_body["model"] = serde_json::Value::String(classification.model.clone());
    let modified_body = serde_json::to_vec(&req_body).unwrap_or_else(|_| body.to_vec());

    let auth_headers = intent_classificator::auth_headers_for(&classification.provider_type, &api_key);
    let client = state.http_client.as_ref().unwrap();

    let mut upstream_req = client.post(&classification.endpoint).body(modified_body);
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

    let upstream_status = upstream_response.status();
    const MAX_UPSTREAM_BODY: usize = 10 * 1024 * 1024; // 10 MB
    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    loop {
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
            Ok(None) => break,
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
    }
    let upstream_body = String::from_utf8_lossy(&upstream_body_bytes).into_owned();

    if !upstream_status.is_success() {
        let truncated = upstream_body.chars().take(512).collect::<String>();
        if upstream_body.len() > truncated.len() {
            eprintln!(
                "WARN completion_handler: upstream error body truncated from {} to 512 chars; \
                 full body: {}",
                upstream_body.len(),
                upstream_body
            );
        }
        let error_body = serde_json::json!({
            "error": "upstream_error",
            "status": upstream_status.as_u16(),
            "message": truncated,
        }).to_string();
        log_classification(&state, &classification, body_str, start, "upstream_error");
        return json_response(
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            error_body,
        );
    }

    log_classification(&state, &classification, body_str, start, "ok");
    json_response(StatusCode::OK, upstream_body)
}

/// Classify handler: extracts prompt, classifies intent, optionally logs a
/// lightweight classification record with status "classified", and returns
/// classification JSON. Logging is controlled by `CLASSIFY_DB_LOG` env var.
async fn classify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let start = std::time::Instant::now();
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    let log_status = if state.classify_db_log { Some("classified") } else { None };
    classify_and_log(&headers, body_str, start, &state, log_status)
}

async fn dashboard(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return DashboardIndex {
                summary: None,
                error: None,
            };
        }
    };

    match persistence.fetch_latency_summary(24).await {
        Ok(s) => DashboardIndex {
            summary: Some(s),
            error: None,
        },
        Err(e) => DashboardIndex {
            summary: None,
            error: Some(e.to_string()),
        },
    }
}

async fn inferences(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let offset = params
        .get("offset")
        .and_then(|o| o.parse::<u32>().ok())
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .map(|l| l.min(100))
        .unwrap_or(20);
    let filter_category = params.get("filter_category").cloned();
    let filter_model = params.get("filter_model").cloned();

    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return InferencesTemplate {
                records: vec![],
                page: 0,
                total_pages: 0,
                error: Some("Database not configured".to_string()),
                filter_category,
                filter_model,
            };
        }
    };

    match persistence
        .fetch_inferences(
            offset,
            limit,
            filter_category.as_deref(),
            filter_model.as_deref(),
        )
        .await
    {
        Ok((records, total_count)) => {
            let page = if limit > 0 { offset / limit } else { 0 };
            let total_pages = if limit > 0 {
                ((total_count as u32).saturating_add(limit - 1)) / limit
            } else {
                0
            };
            InferencesTemplate {
                records,
                page,
                total_pages,
                error: None,
                filter_category,
                filter_model,
            }
        }
        Err(e) => InferencesTemplate {
            records: vec![],
            page: 0,
            total_pages: 0,
            error: Some(e.to_string()),
            filter_category,
            filter_model,
        },
    }
}

async fn latency(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let hours: u32 = params
        .get("hours")
        .and_then(|h| h.parse::<u32>().ok())
        .map(|h| h.clamp(1, 720))
        .unwrap_or(24);

    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return LatencyTemplate {
                summary: None,
                hours,
                error: Some("Database not configured".to_string()),
            };
        }
    };

    match persistence.fetch_latency_summary(hours).await {
        Ok(s) => LatencyTemplate {
            summary: Some(s),
            hours,
            error: None,
        },
        Err(e) => LatencyTemplate {
            summary: None,
            hours,
            error: Some(e.to_string()),
        },
    }
}

async fn savings(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return SavingsTemplate {
                estimate: None,
                error: Some("Database not configured".to_string()),
                baseline_model: "unknown".to_string(),
            };
        }
    };

    let (model_costs, baseline_model) = match &state.classifier {
        Some(c) => (c.model_costs().clone(), c.baseline_model.clone()),
        None => (intent_classificator::ModelCosts::empty(), "unknown".to_string()),
    };

    match persistence.fetch_savings_estimate(24, &model_costs, &baseline_model).await {
        Ok(est) => SavingsTemplate {
            estimate: Some(est),
            error: None,
            baseline_model: baseline_model.clone(),
        },
        Err(e) => SavingsTemplate {
            estimate: None,
            error: Some(e.to_string()),
            baseline_model: baseline_model.clone(),
        },
    }
}

fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let proxy_routes = Router::new()
        .route("/chat/completions", post(completion_handler))
        .route("/classify", post(classify_handler))
        .layer(middleware::from_fn_with_state(
            auth_config.clone(),
            auth::require_proxy_bearer,
        ));

    let dashboard_routes =
        Router::new()
            .route("/", get(dashboard))
            .route("/inferences", get(inferences))
            .route("/latency", get(latency))
            .route("/savings", get(savings))
            .layer(middleware::from_fn_with_state(
                auth_config,
                auth::require_dashboard_basic,
            ));

    Router::new()
        .route("/health", get(health))
        .nest("/v1", proxy_routes)
        .nest("/dashboard", dashboard_routes)
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

    fn test_app_with_http_client(env_var_name: &str) -> (Router, httpmock::MockServer) {
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
        // With both persistence and classifier None, DB-not-configured fires first.
        assert!(
            body.contains("Database not configured") || body.contains("Cost configuration not available"),
            "expected error state, got: {body}"
        );
    }
}
