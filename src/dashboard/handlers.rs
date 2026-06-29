use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use tracing::debug;

use crate::app::AppState;
use crate::persistence::PersistenceBackend;
use super::nav::nav_for;
use super::templates::{
    CacheTemplate, DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate,
};

pub(super) async fn dashboard_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db_connected = state.persistence.is_some();
    let classifier_active = state.classifier.is_some();
    let model_costs = state.model_costs.read().await.clone();
    let baseline_model = state.baseline_model.read().await.clone();

    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return DashboardTemplate {
                nav: nav_for(""),
                summary: None,
                savings: None,
                recent: vec![],
                db_connected,
                classifier_active,
                baseline_model,
                error: None,
            };
        }
    };

    let summary_fut = persistence
        .backend
        .fetch_latency_summary(state.dashboard_config.default_hours);
    let savings_fut = persistence.backend.fetch_savings_estimate(
        state.dashboard_config.default_hours,
        &model_costs,
        &baseline_model,
    );
    let recent_fut =
        persistence
            .backend
            .fetch_inferences(0, state.dashboard_config.recent_count, None, None);

    let (summary_res, savings_res, recent_res) = tokio::join!(summary_fut, savings_fut, recent_fut);

    let error = summary_res
        .as_ref()
        .err()
        .or(savings_res.as_ref().err())
        .or(recent_res.as_ref().err())
        .map(|e| e.to_string());

    let summary = summary_res.ok();
    let savings = savings_res.ok();
    let (recent, _) = recent_res.unwrap_or((vec![], 0));

    DashboardTemplate {
        nav: nav_for(""),
        summary,
        savings,
        recent,
        db_connected,
        classifier_active,
        baseline_model,
        error,
    }
}

pub(super) async fn inferences_handler(
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
        .map(|l| l.min(state.dashboard_config.page_limit_max))
        .unwrap_or(state.dashboard_config.page_limit);
    let filter_category = params.get("filter_category").cloned();
    let filter_model = params.get("filter_model").cloned();

    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return InferencesTemplate {
                nav: nav_for("inferences"),
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
        .backend
        .fetch_inferences(
            offset,
            limit,
            filter_category.as_deref(),
            filter_model.as_deref(),
        )
        .await
    {
        Ok((records, total_count)) => {
            let page = offset.checked_div(limit).unwrap_or(0);
            let total_pages = ((total_count as u32).saturating_add(limit.saturating_sub(1)))
                .checked_div(limit)
                .unwrap_or(0);
            InferencesTemplate {
                nav: nav_for("inferences"),
                records,
                page,
                total_pages,
                error: None,
                filter_category,
                filter_model,
            }
        }
        Err(e) => InferencesTemplate {
            nav: nav_for("inferences"),
            records: vec![],
            page: 0,
            total_pages: 0,
            error: Some(e.to_string()),
            filter_category,
            filter_model,
        },
    }
}

pub(super) async fn latency_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let hours: u32 = params
        .get("hours")
        .and_then(|h| h.parse::<u32>().ok())
        .map(|h| {
            h.clamp(
                state.dashboard_config.hours_min,
                state.dashboard_config.hours_max,
            )
        })
        .unwrap_or(state.dashboard_config.default_hours);

    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return LatencyTemplate {
                nav: nav_for("latency"),
                summary: None,
                hours,
                error: Some("Database not configured".to_string()),
            };
        }
    };

    match persistence.backend.fetch_latency_summary(hours).await {
        Ok(s) => LatencyTemplate {
            nav: nav_for("latency"),
            summary: Some(s),
            hours,
            error: None,
        },
        Err(e) => LatencyTemplate {
            nav: nav_for("latency"),
            summary: None,
            hours,
            error: Some(e.to_string()),
        },
    }
}

pub(super) async fn savings_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let persistence = match &state.persistence {
        Some(p) => p,
        None => {
            return SavingsTemplate {
                nav: nav_for("savings"),
                estimate: None,
                error: Some("Database not configured".to_string()),
                baseline_model: "unknown".to_string(),
            };
        }
    };

    let model_costs = state.model_costs.read().await.clone();
    let baseline_model = state.baseline_model.read().await.clone();

    match persistence
        .backend
        .fetch_savings_estimate(
            state.dashboard_config.default_hours,
            &model_costs,
            &baseline_model,
        )
        .await
    {
        Ok(est) => SavingsTemplate {
            nav: nav_for("savings"),
            estimate: Some(est),
            error: None,
            baseline_model: baseline_model.clone(),
        },
        Err(e) => SavingsTemplate {
            nav: nav_for("savings"),
            estimate: None,
            error: Some(e.to_string()),
            baseline_model: baseline_model.clone(),
        },
    }
}

pub(super) async fn cache_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match &state.response_cache {
        Some(cache) => {
            let stats = cache.stats();
            let total = stats.hit_count + stats.miss_count;
            let hit_rate = if total > 0 {
                stats.hit_count as f64 / total as f64
            } else {
                0.0
            };
            debug!(
                "Cache stats: hits={} misses={} entries={}",
                stats.hit_count, stats.miss_count, stats.entry_count
            );
            CacheTemplate {
                nav: nav_for("cache"),
                error: None,
                enabled: true,
                hit_count: stats.hit_count,
                miss_count: stats.miss_count,
                hit_rate,
                entry_count: stats.entry_count,
                max_entries: stats.max_entries,
                ttl_secs: stats.ttl_secs,
            }
        }
        None => CacheTemplate {
            nav: nav_for("cache"),
            error: None,
            enabled: false,
            hit_count: 0,
            miss_count: 0,
            hit_rate: 0.0,
            entry_count: 0,
            max_entries: 0,
            ttl_secs: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_helpers::test_app;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use tower::util::ServiceExt;

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
        assert!(content_type.starts_with("text/html"));
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains("Frugalis Dashboard"));
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
        assert!(content_type.starts_with("text/html"));
    }

    #[tokio::test]
    async fn test_inferences_empty_state() {
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
            body.contains("Database not configured") || body.contains("No inference records yet")
        );
    }

    #[tokio::test]
    async fn test_inferences_invalid_params() {
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
        assert!(body.contains("Database not configured"));
    }

    #[tokio::test]
    async fn test_inferences_filter_by_category() {
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
        assert!(content_type.starts_with("text/html"));
    }

    #[tokio::test]
    async fn test_latency_empty_state() {
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
        assert!(body.contains("Database not configured"));
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
        assert!(content_type.starts_with("text/html"));
    }

    #[tokio::test]
    async fn test_savings_no_persistence_shows_error() {
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
        assert!(body.contains("Database not configured"));
    }
}
