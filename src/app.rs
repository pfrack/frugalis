use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};

use crate::{auth, cache, classification, config, dashboard, persistence, proxy};

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub(crate) struct AppState {
    pub persistence: Option<persistence::PersistenceConfig>,
    pub classifier: Option<Arc<classification::chain::ClassifierChain>>,
    pub fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>>,
    pub routing:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, config::routing::RouteEntry>>>,
    pub model_costs: Arc<tokio::sync::RwLock<config::routing::ModelCosts>>,
    pub baseline_model: Arc<tokio::sync::RwLock<String>>,
    pub classify_db_log: Arc<std::sync::atomic::AtomicBool>,
    pub http_client: Option<reqwest::Client>,
    pub max_upstream_body_bytes: Arc<tokio::sync::RwLock<usize>>,
    pub keepalive_interval_secs: Arc<tokio::sync::RwLock<u64>>,
    pub request_body_limit_bytes: usize,
    pub streaming_channel_capacity: usize,
    pub dashboard_config: config::types::DashboardConfig,
    pub auth_providers: Arc<Vec<config::types::AuthProviderConfig>>,
    pub allowed_origins: Arc<RwLock<Vec<String>>>,
    pub response_cache: Option<Arc<cache::ResponseCache>>,
    #[cfg(feature = "otel")]
    pub metrics: Option<crate::telemetry::Metrics>,
}

pub(crate) fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let unauth_v1_routes = Router::new().route("/models", get(proxy::handlers::models_handler));

    let proxy_routes = Router::new()
        .route("/chat/completions", post(proxy::handlers::completion_handler))
        .route("/messages", post(proxy::handlers::messages_handler))
        .route("/messages/count_tokens", post(proxy::handlers::count_tokens_handler))
        .route("/classify", post(proxy::handlers::classify_handler))
        .route("/feedback", post(proxy::handlers::feedback_handler))
        .route_layer(auth::proxy_auth_layer(auth_config.clone()))
        .merge(unauth_v1_routes);

    let dashboard_routes = dashboard::routes(auth_config);

    let allowed_origin_headers: Vec<HeaderValue> = app_state
        .allowed_origins
        .try_read()
        .expect("allowed_origins RwLock written at init; poisoning impossible")
        .iter()
        .filter_map(|s| header::HeaderValue::from_str(s).ok())
        .collect();

    let cors_layer = if allowed_origin_headers.is_empty() {
        CorsLayer::new()
    } else {
        let mut cors = CorsLayer::new();
        for origin in allowed_origin_headers {
            cors = cors.allow_origin(origin);
        }
        cors.allow_methods([Method::GET, Method::POST])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT])
    };

    Router::new()
        .route("/health", get(proxy::handlers::health))
        .nest("/v1", proxy_routes)
        .nest("/dashboard", dashboard_routes)
        .layer(cors_layer)
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(
            app_state.request_body_limit_bytes,
        ))
        .with_state(app_state)
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use axum::Router;
    use crate::classification;
    use crate::config;
    use crate::auth;
    use crate::app::{AppState, build_app};

    pub fn test_categories() -> Vec<classification::types::CategoryConfig> {
        vec![
            classification::types::CategoryConfig {
                name: "FILE_READING".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 1,
                patterns: vec![
                    classification::types::PatternEntry {
                        regex: r"(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            classification::types::CategoryConfig {
                name: "SYNTAX_FIX".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 2,
                patterns: vec![
                    classification::types::PatternEntry {
                        regex: r"(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            classification::types::CategoryConfig {
                name: "COMPLEX_REASONING".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 3,
                patterns: vec![
                    classification::types::PatternEntry {
                        regex: r"(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            classification::types::CategoryConfig {
                name: "CASUAL".to_string(),
                description: String::new(),
                threshold: 1,
                priority: 4,
                patterns: vec![
                    classification::types::PatternEntry {
                        regex: r"(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
        ]
    }

    pub fn test_negative_patterns() -> Vec<classification::types::NegativePatternConfig> {
        vec![]
    }

    pub fn make_test_app_state(
        classifier: classification::regex::RegexClassifier,
        http_client: Option<reqwest::Client>,
        model_costs: config::routing::ModelCosts,
        baseline_model: String,
        max_upstream_body_bytes: usize,
    ) -> Arc<AppState> {
        let classifier_chain = classification::chain::ClassifierChain::new(vec![Arc::new(classifier)]);
        let classifier_arc = Some(Arc::new(classifier_chain));
        let mut merged_routing = std::collections::HashMap::new();
        if let Some(cls) = classifier_arc.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        Arc::new(AppState {
            persistence: None,
            classifier: classifier_arc,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
            baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client,
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(max_upstream_body_bytes)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::types::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            response_cache: None,
            #[cfg(feature = "otel")]
            metrics: None,
        })
    }

    pub fn test_app() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier: None,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            model_costs: Arc::new(tokio::sync::RwLock::new(
                config::routing::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: None,
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::types::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            response_cache: None,
            #[cfg(feature = "otel")]
            metrics: None,
        });
        build_app(auth_config, app_state)
    }

    pub fn test_app_with_classifier() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            cats[3].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "ca-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = config::routing::RouteEntry {
            providers: vec![config::routing::ProviderEntry {
                model: "fallback-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = classification::regex::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            None,
            config::routing::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        build_app(auth_config, app_state)
    }

    pub async fn parse_json_body(response: axum::response::Response) -> serde_json::Value {
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        serde_json::from_slice(&body_bytes)
            .unwrap_or_else(|e| panic!("response body should be JSON: {e}; body={:?}", body_bytes))
    }
}
