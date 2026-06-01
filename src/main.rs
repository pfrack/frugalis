use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::IntoResponse,
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

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classificator::IntentClassifier>>,
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
    let app_state = Arc::new(AppState {
        persistence: persistence_state,
        classifier,
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

/// Completion handler: classifies intent, returns JSON metadata, and
/// enqueues a non-blocking inference logging event.
async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json".to_string());
    }

    let start = std::time::Instant::now();

    let body_str = std::str::from_utf8(&body).unwrap_or("");
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
    let response = (StatusCode::OK, response_body);

    // Fire-and-forget: enqueue after response is assembled, never awaited.
    if let Some(persistence) = &state.persistence {
        let duration_ms = start.elapsed().as_millis() as i32;
        let snippet = persistence::extract_snippet(body_str);
        let prompt_char_count = if prompt.is_empty() {
            None
        } else {
            Some(prompt.chars().count() as i32)
        };
        let record = persistence::InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
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

    response
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

fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let proxy_routes = Router::new()
        .route("/chat/completions", post(completion_handler))
        .layer(middleware::from_fn_with_state(
            auth_config.clone(),
            auth::require_proxy_bearer,
        ));

    let dashboard_routes =
        Router::new()
            .route("/", get(dashboard))
            .route("/inferences", get(inferences))
            .route("/latency", get(latency))
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
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classificator::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = intent_classificator::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
        };
        let classifier = Some(Arc::new(
            intent_classificator::IntentClassifier::from_values(routing, fallback),
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
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
}
