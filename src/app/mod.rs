use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::{error, info, warn};

use crate::{
    cache, classification, config, dashboard, persistence, proxy, routing,
};

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub(crate) struct AppState {
    pub persistence: Option<persistence::PersistenceConfig>,
    pub classifier: Option<Arc<classification::chain::ClassifierChain>>,
    pub fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>>,
    pub routing:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, routing::RouteEntry>>>,
    pub model_costs: Arc<tokio::sync::RwLock<routing::ModelCosts>>,
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

pub(crate) struct ClassifierBuildResult {
    pub classifier: Option<Arc<classification::chain::ClassifierChain>>,
    pub routing: HashMap<String, routing::RouteEntry>,
    pub model_costs: routing::ModelCosts,
    pub baseline_model: String,
    pub fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>>,
}

pub(crate) fn build_classifiers(
    config_root: &config::ConfigRoot,
    http_client: reqwest::Client,
    auth_providers: Arc<Vec<config::types::AuthProviderConfig>>,
    regex_config: &config::types::RegexClassifierConfig,
    classifiers_config: &config::types::ClassifiersConfig,
    negative_patterns: &[config::types::NegativePatternConfig],
) -> ClassifierBuildResult {
    let categories_res = config::loader::load_categories_from_value(config_root);
    let categories_ok = categories_res.is_ok();
    let mut categories = categories_res.unwrap_or_default();

    let patterns_dir = config_root
        .patterns_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("./patterns"));
    for cat in &mut categories {
        if let Some(ref pf) = cat.patterns_file.take() {
            match config::loader::load_patterns_from_file(pf, &patterns_dir) {
                Ok(entries) => {
                    cat.patterns = entries;
                }
                Err(e) => {
                    warn!("Failed to load pattern file '{}': {}; using empty patterns for category '{}'", pf, e, cat.name);
                    cat.patterns = vec![];
                }
            }
        }
    }

    let (mut routing_map, mut fallback_entry) =
        match config::loader::routing_from_value(config_root) {
            Ok((map, fallback)) => (map, fallback),
            Err(e) => {
                warn!(
                    "routing config parsing failed: {}; using hardcoded routing defaults",
                    e
                );
                config::loader::hardcoded_routing(&categories)
            }
        };

    if categories_ok {
        let mut missing = Vec::new();
        for cat in &categories {
            if !routing_map.contains_key(&cat.name.to_uppercase()) {
                missing.push(cat.name.clone());
            }
        }
        if !missing.is_empty() {
            warn!("Categories {:?} missing routing entries; falling back to empty categories and hardcoded routing", missing);
            categories = vec![];
            let (new_map, new_fallback) = config::loader::hardcoded_routing(&categories);
            routing_map = new_map;
            fallback_entry = new_fallback;
        }
    }

    let mut route_keys: Vec<&String> = routing_map.keys().collect();
    route_keys.sort();
    for key in route_keys {
        let entry = &routing_map[key];
        info!(
            "Route {} -> {} @ {}",
            key,
            entry.primary().model,
            entry.primary().endpoint
        );
    }
    if !routing_map.contains_key("DEFAULT") {
        info!(
            "Route DEFAULT -> {} @ {}",
            fallback_entry.primary().model,
            fallback_entry.primary().endpoint
        );
    }

    let model_costs = config::loader::build_model_costs(config_root, &routing_map);
    let baseline_model = config_root
        .baseline_model
        .clone()
        .unwrap_or_else(|| routing::DEFAULT_MODEL_COMPLEX.to_string());
    let mut fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>> = None;

    if !classifiers_config.enabled {
        info!("All classifiers disabled via config");
        return ClassifierBuildResult {
            classifier: None,
            routing: HashMap::new(),
            model_costs,
            baseline_model,
            fewshot_classifier: None,
        };
    }

    let mut backends: Vec<Arc<dyn classification::chain::IntentClassify + Send + Sync>> =
        Vec::new();

    for name in &classifiers_config.order {
        match name.as_str() {
            "regex" => {
                if regex_config.enabled {
                    match classification::regex::RegexClassifier::from_env(
                        routing_map.clone(),
                        fallback_entry.clone(),
                        regex_config.short_prompt_len,
                        categories.clone(),
                        negative_patterns,
                    ) {
                        Ok(c) => {
                            info!("Regex classifier initialized");
                            backends.push(Arc::new(c));
                        }
                        Err(e) => {
                            warn!("RegexClassifier disabled: {e}");
                        }
                    }
                } else {
                    info!("Regex classifier disabled");
                }
            }
            "fewshot" => {
                if let Some(config) =
                    config::loader::load_fewshot_config_from_value(config_root)
                {
                    let fewshot =
                        Arc::new(classification::fewshot::FewShotClassifier::new(
                            config,
                            routing_map.clone(),
                            fallback_entry.clone(),
                        ));
                    info!("Few-shot classifier enabled");
                    fewshot_classifier = Some(fewshot.clone());
                    backends.push(fewshot);
                }
            }
            "llm" => {
                if let Some(llm_config) =
                    config::loader::load_llm_classifier_config_from_value(config_root)
                {
                    let llm = classification::llm::LLMClassifier::new(
                                llm_config,
                                http_client.clone(),
                                categories.clone(),
                                auth_providers.clone(),
                                routing_map.clone(),
                                fallback_entry.clone(),
                            );
                    info!(
                        "LLM classifier enabled: model={}, endpoint={}",
                        llm.model, llm.endpoint
                    );
                    backends.push(Arc::new(llm));
                }
            }
            unknown => {
                warn!("unknown classifier in order: '{unknown}'");
            }
        }
    }

    if backends.is_empty() {
        warn!("no classifier backends enabled");
        ClassifierBuildResult {
            classifier: None,
            routing: HashMap::new(),
            model_costs,
            baseline_model,
            fewshot_classifier: None,
        }
    } else {
        let chain = classification::chain::ClassifierChain::new(backends);
        let mut merged_routing = HashMap::new();
        for backend in chain.backends().iter() {
            if let Some(r) = backend.get_routing() {
                merged_routing.extend(r.clone());
            }
        }
        ClassifierBuildResult {
            classifier: Some(Arc::new(chain)),
            routing: merged_routing,
            model_costs,
            baseline_model,
            fewshot_classifier,
        }
    }
}

pub(crate) async fn build_persistence(
    config_root: &config::ConfigRoot,
) -> Option<persistence::PersistenceConfig> {
    let db_config = config::loader::load_database_config_from_value(config_root);
    let persistence_settings = config::loader::load_persistence_config_from_value(config_root);
    let semaphore_limit = db_config.log_concurrency_limit as usize;

    let db_url = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty());

    if let Some(url) = db_url {
        match persistence::sql_backend::SqlBackend::connect(&url, &db_config).await {
            Ok(backend) => {
                info!("Persistence backend: sql (unified, via DATABASE_URL)");
                Some(persistence::PersistenceConfig {
                    backend: Arc::new(persistence::DbBackend::Sql(backend)),
                    task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                })
            }
            Err(e) => {
                error!("DATABASE_URL backend failed ({}); falling back to memory", e);
                let backend = persistence::memory::MemoryBackend::new();
                info!("Persistence backend: memory (DATABASE_URL fallback)");
                Some(persistence::PersistenceConfig {
                    backend: Arc::new(persistence::DbBackend::Memory(backend)),
                    task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                })
            }
        }
    } else {
        match persistence_settings.backend.as_str() {
            "postgres" => {
                warn!("[persistence] backend = \"postgres\" but DATABASE_URL is not set; falling through to memory");
                let backend = persistence::memory::MemoryBackend::new();
                info!("Persistence backend: memory (per config fallback)");
                Some(persistence::PersistenceConfig {
                    backend: Arc::new(persistence::DbBackend::Memory(backend)),
                    task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                })
            }
            "sqlite" => {
                let sqlite_url = format!("sqlite:{}?mode=rwc", persistence_settings.sqlite_path);
                match persistence::sql_backend::SqlBackend::connect(&sqlite_url, &db_config).await {
                    Ok(backend) => {
                        info!(
                            "Persistence backend: sql (sqlite, path={})",
                            persistence_settings.sqlite_path
                        );
                        Some(persistence::PersistenceConfig {
                            backend: Arc::new(persistence::DbBackend::Sql(backend)),
                            task_semaphore: Arc::new(tokio::sync::Semaphore::new(
                                semaphore_limit,
                            )),
                        })
                    }
                    Err(e) => {
                        warn!("SQLite backend failed ({}); falling back to memory", e);
                        let backend = persistence::memory::MemoryBackend::new();
                        Some(persistence::PersistenceConfig {
                            backend: Arc::new(persistence::DbBackend::Memory(backend)),
                            task_semaphore: Arc::new(tokio::sync::Semaphore::new(
                                semaphore_limit,
                            )),
                        })
                    }
                }
            }
            _ => {
                let backend = persistence::memory::MemoryBackend::new();
                info!("Persistence backend: memory");
                Some(persistence::PersistenceConfig {
                    backend: Arc::new(persistence::DbBackend::Memory(backend)),
                    task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                })
            }
        }
    }
}

pub(crate) fn build_app(auth_config: Arc<routing::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let unauth_v1_routes = Router::new().route("/models", get(proxy::handlers::models_handler));

    let proxy_routes = Router::new()
        .route(
            "/chat/completions",
            post(proxy::handlers::completion_handler),
        )
        .route("/messages", post(proxy::handlers::messages_handler))
        .route(
            "/messages/count_tokens",
            post(proxy::handlers::count_tokens_handler),
        )
        .route("/classify", post(proxy::handlers::classify_handler))
        .route("/feedback", post(proxy::handlers::feedback_handler))
        .route("/responses", post(proxy::responses_handler::responses_handler))
        .route_layer(routing::proxy_auth_layer(auth_config.clone()))
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

pub(crate) mod cli;
pub(crate) mod quickstart;

#[cfg(test)]
pub(crate) mod test_helpers;
