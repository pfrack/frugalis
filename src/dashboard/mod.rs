use std::sync::Arc;

use axum::{routing::get, Router};
use tower_http::services::ServeDir;

use crate::{app::AppState, routing};

pub(crate) mod handlers;
pub(crate) mod nav;
pub(crate) mod templates;

pub fn routes(auth_config: Arc<routing::AuthConfig>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(handlers::dashboard_handler))
        .route("/inferences", get(handlers::inferences_handler))
        .route("/latency", get(handlers::latency_handler))
        .route("/savings", get(handlers::savings_handler))
        .route("/cache", get(handlers::cache_handler))
        .nest_service("/static", ServeDir::new("static"))
        .route_layer(routing::dashboard_auth_layer(auth_config))
}
