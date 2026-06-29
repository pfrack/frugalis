use askama::Template;
use askama_web::WebTemplate;

use crate::persistence;
use super::nav::NavContext;

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
