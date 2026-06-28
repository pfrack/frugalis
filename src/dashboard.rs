use askama::Template;
use askama_web::WebTemplate;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tracing::debug;

use crate::{auth, persistence, AppState};
use persistence::PersistenceBackend;

/// Navigation page entry registered in `PAGES`.
///
/// # Safety
/// The `icon` field must contain a trusted SVG string (compile-time constant).
/// It is rendered with `|safe` in `base.html` — bypassing HTML escaping.
/// Never source `icon` from user input, a database, or any untrusted origin.
pub struct NavPage {
    pub path: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

pub struct NavItem {
    pub path: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub active: bool,
}

pub struct NavContext {
    pub pages: Vec<NavItem>,
}

const ICON_DASHBOARD: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><rect x='3' y='3' width='7' height='7'/><rect x='14' y='3' width='7' height='7'/><rect x='3' y='14' width='7' height='7'/><rect x='14' y='14' width='7' height='7'/></svg>";
const ICON_LIST: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='8' y1='6' x2='21' y2='6'/><line x1='8' y1='12' x2='21' y2='12'/><line x1='8' y1='18' x2='21' y2='18'/><line x1='3' y1='6' x2='3.01' y2='6'/><line x1='3' y1='12' x2='3.01' y2='12'/><line x1='3' y1='18' x2='3.01' y2='18'/></svg>";
const ICON_CLOCK: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><circle cx='12' cy='12' r='10'/><polyline points='12 6 12 12 16 14'/></svg>";
const ICON_DOLLAR: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><line x1='12' y1='1' x2='12' y2='23'/><path d='M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6'/></svg>";
const ICON_CACHE: &str = "<svg width='16' height='16' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='M21 12a9 9 0 0 0-8.17-8.98'/><path d='M3 12a9 9 0 0 0 8.17 8.98'/><polyline points='15 7 21 7 21 1'/><polyline points='9 17 3 17 3 23'/></svg>";

pub static PAGES: &[NavPage] = &[
    NavPage {
        path: "",
        label: "Dashboard",
        icon: ICON_DASHBOARD,
    },
    NavPage {
        path: "inferences",
        label: "Inference Logs",
        icon: ICON_LIST,
    },
    NavPage {
        path: "latency",
        label: "Latency",
        icon: ICON_CLOCK,
    },
    NavPage {
        path: "savings",
        label: "Savings",
        icon: ICON_DOLLAR,
    },
    NavPage {
        path: "cache",
        label: "Cache",
        icon: ICON_CACHE,
    },
];

pub fn nav_for(current: &str) -> NavContext {
    NavContext {
        pages: PAGES
            .iter()
            .map(|p| NavItem {
                path: p.path,
                label: p.label,
                icon: p.icon,
                active: p.path == current,
            })
            .collect(),
    }
}

macro_rules! dashboard_page {
    (
        $(#[$attr:meta])*
        struct $name:ident for $path:literal {
            $($field:ident: $ty:ty),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[derive(Template, WebTemplate)]
        #[template(path = $path)]
        pub struct $name {
            pub nav: NavContext,
            pub error: Option<String>,
            $(
                pub $field: $ty,
            )*
        }
    };
}

dashboard_page! {
    struct DashboardTemplate for "dashboard/index.html" {
        summary: Option<persistence::LatencySummary>,
        savings: Option<persistence::SavingsEstimate>,
        recent: Vec<persistence::InferenceLog>,
        db_connected: bool,
        classifier_active: bool,
        baseline_model: String,
    }
}

dashboard_page! {
    struct InferencesTemplate for "dashboard/inferences.html" {
        records: Vec<persistence::InferenceLog>,
        page: u32,
        total_pages: u32,
        filter_category: Option<String>,
        filter_model: Option<String>,
    }
}

dashboard_page! {
    struct LatencyTemplate for "dashboard/latency.html" {
        summary: Option<persistence::LatencySummary>,
        hours: u32,
    }
}

dashboard_page! {
    struct SavingsTemplate for "dashboard/savings.html" {
        estimate: Option<persistence::SavingsEstimate>,
        baseline_model: String,
    }
}

dashboard_page! {
    struct CacheTemplate for "dashboard/cache.html" {
        enabled: bool,
        hit_count: u64,
        miss_count: u64,
        hit_rate: f64,
        entry_count: u64,
        max_entries: u64,
        ttl_secs: u64,
    }
}

async fn dashboard_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

async fn inferences_handler(
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

async fn latency_handler(
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

async fn savings_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

async fn cache_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

pub fn routes(auth_config: Arc<auth::AuthConfig>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(dashboard_handler))
        .route("/inferences", get(inferences_handler))
        .route("/latency", get(latency_handler))
        .route("/savings", get(savings_handler))
        .route("/cache", get(cache_handler))
        .nest_service("/static", ServeDir::new("static"))
        .route_layer(auth::dashboard_auth_layer(auth_config))
}
