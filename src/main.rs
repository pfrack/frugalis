use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use askama::Template;
use askama_web::WebTemplate;

mod auth;
mod persistence;

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/index.html")]
struct DashboardIndex {}

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
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
    let app_state = Arc::new(AppState {
        persistence: persistence_state,
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

/// Completion handler: assembles response then enqueues a non-blocking inference
/// logging event. DB latency and retries are fully isolated to a background task.
async fn completion_handler(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> (StatusCode, &'static str) {
    let start = std::time::Instant::now();

    // Placeholder response — future: proxy to upstream model.
    let response = (StatusCode::OK, "proxy route is protected");

    println!(
        "DEBUG: wpadło zapytanie. persistence is Some? {}",
        state.persistence.is_some()
    );

    // Fire-and-forget: enqueue after response is assembled, never awaited.
    if let Some(persistence) = &state.persistence {
        let duration_ms = start.elapsed().as_millis() as i32;
        let snippet = persistence::extract_snippet(std::str::from_utf8(&body).unwrap_or(""));
        let record = persistence::InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: None,
            upstream_model: None,
            duration_ms: Some(duration_ms),
            prompt_snippet: snippet,
        };
        persistence::log_inference(
            persistence.pool.clone(),
            persistence.task_semaphore.clone(),
            record,
        );
    }

    response
}

async fn dashboard() -> impl IntoResponse {
    DashboardIndex {}
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
        let app_state = Arc::new(AppState { persistence: None });
        build_app(auth_config, app_state)
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
        };
        let handle = persistence::log_inference(pool.clone(), semaphore, record);
        handle.await.expect("logging task should complete");

        // Read back using non-macro query (no offline cache required).
        let row =
            sqlx::query("SELECT status, prompt_snippet FROM inferences WHERE request_id = $1")
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
}
