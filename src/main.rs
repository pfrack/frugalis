use std::collections::HashMap;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn, Subscriber};
use tracing_subscriber::{fmt, layer::Layer, prelude::*, EnvFilter, Registry};

#[cfg(feature = "otel")]
mod telemetry;

mod app;
mod auth;
mod cache;
mod classification;
mod cli;
mod config;
mod dashboard;
mod persistence;
mod protocol;
mod proxy;
mod quickstart;

#[cfg(test)]
mod test_util;

use app::{build_app, AppState};
use cli::CliMode;



#[tokio::main]
async fn main() {
    let cli::CliResult { mode, force } = cli::parse_args();

    // Early-exit commands (before config loading or tracing init)
    if let CliMode::Help = mode {
        cli::print_help();
        std::process::exit(0);
    }

    if let CliMode::Init(path_opt) = &mode {
        match cli::run_init(path_opt.as_deref(), force) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }

    if let CliMode::Quickstart = mode {
        match quickstart::run_quickstart() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }

    let config_path_option = std::env::var("CONFIG_PATH").ok();
    let config_path_was_set = config_path_option.is_some();

    // ── Validation mode ──
    if let CliMode::Validate = mode {
        let result = config::run_validation(config_path_option.as_deref());
        match result {
            Ok(()) => {
                println!("Configuration valid");
                std::process::exit(0);
            }
            Err(errors) => {
                for err in &errors {
                    eprintln!("{}", err);
                }
                std::process::exit(1);
            }
        }
    }

    // Parse config before tracing init to get server settings
    const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
    let mut config_root: config::ConfigRoot = match toml::from_str(DEFAULT_CONFIG_TOML) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("Embedded config.toml is invalid: {e}; using hardcoded defaults");
            config::ConfigRoot::default()
        }
    };

    if let Some(config_path) = config_path_option {
        match config::load_config_from_path(&config_path) {
            Ok(overlay) => {
                config::merge_configs(&mut config_root, overlay);
            }
            Err(e) => {
                eprintln!(
                    "failed to parse config file at {}: {}; using embedded defaults",
                    config_path, e
                );
            }
        }
    }

    let server_config = config::loader::load_server_config_from_value(&config_root);

    // Initialize OpenTelemetry providers before tracing (layers reference the providers)
    #[cfg(feature = "otel")]
    let otel: Option<(telemetry::OtelGuard, telemetry::Metrics)> = telemetry::init("frugalis");

    // Initialize tracing using server_config with RUST_LOG override
    let log_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&server_config.log_level));

    let make_fmt_layer = |filter: EnvFilter| -> Box<dyn Layer<Registry> + Send + Sync> {
        match server_config.log_format.as_str() {
            "json" => fmt::layer().json().with_filter(filter).boxed(),
            _ => fmt::layer().compact().with_filter(filter).boxed(),
        }
    };

    // Type-erase the subscriber so #[cfg(feature = "otel")] can produce differing layer stacks.
    let subscriber: Box<dyn Subscriber + Send + Sync> = {
        #[cfg(feature = "otel")]
        match otel.as_ref() {
            Some((guard, _)) => Box::new(
                tracing_subscriber::registry()
                    .with(make_fmt_layer(log_filter))
                    .with(guard.trace_layer())
                    .with(guard.log_layer()),
            ),
            None => Box::new(tracing_subscriber::registry().with(make_fmt_layer(log_filter))),
        }
        #[cfg(not(feature = "otel"))]
        Box::new(tracing_subscriber::registry().with(make_fmt_layer(log_filter)))
    };

    tracing::subscriber::set_global_default(subscriber)
        .expect("global default subscriber should be set");

    // Ensure any panic is logged through the active tracing subscriber so it
    // reaches both the fmt layer and the OTel log bridge (when the otel
    // feature is enabled).
    panic::set_hook(Box::new(|info| {
        tracing::error!("Panic in Frugalis: {info}");
    }));

    let auth_config = auth::AuthConfig::from_env().unwrap_or_else(|err| {
        panic!("Auth configuration error: {err}");
    });
    let auth_config = Arc::new(auth_config);

    if !config_path_was_set {
        info!("No CONFIG_PATH set — using embedded defaults. Run `frugalis --init` to generate a starter config.");
    }

    let regex_config = config::loader::load_regex_classifier_config_from_value(&config_root);

    // Load global classifiers config
    let classifiers_config = config::loader::load_classifiers_config_from_value(&config_root);

    let negative_patterns = config::loader::load_negative_patterns_from_value(&config_root);

    let http_config = config::loader::load_http_config_from_value(&config_root);
    let max_upstream_body_bytes = http_config.max_upstream_body_bytes;
    let keepalive_interval_secs = http_config.keepalive_interval_secs;

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            http_config.client_timeout_secs,
        ))
        .connect_timeout(std::time::Duration::from_secs(
            http_config.client_connect_timeout_secs,
        ))
        .build()
        .expect("reqwest client should build");

    let classify_db_log = config_root.classify_db_log.unwrap_or(false);
    let auth_providers = Arc::new(config::loader::load_auth_providers_from_value(&config_root));
    let (classifier, routing, model_costs, baseline_model, fewshot_classifier) = {
        let categories_res = config::loader::load_categories_from_value(&config_root);
        let categories_ok = categories_res.is_ok();
        let mut categories = categories_res.unwrap_or_default();

        // Resolve external pattern files for each category
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

        let (mut routing_map, mut fallback_entry) = match config::loader::routing_from_value(&config_root) {
            Ok((map, fallback)) => (map, fallback),
            Err(e) => {
                warn!(
                    "routing config parsing failed: {}; using hardcoded routing defaults",
                    e
                );
                config::loader::hardcoded_routing(&categories)
            }
        };

        // Validate that all custom categories have corresponding routing entries.
        // If any category missing, fall back to hardcoded categories and matching routing.
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

        // Log each active route with its resolved model and endpoint so
        // operators can verify their CONFIG_PATH overlay took effect (per
        // the plan: "log which routes are active so users can verify their
        // overlay took effect"). DEFAULT is logged from `fallback_entry`
        // since `routing_from_value` strips it from the routing map; the
        // `hardcoded_routing` fallback path does not strip it, so we dedupe
        // to avoid printing DEFAULT twice.
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

        let model_costs = config::loader::build_model_costs(&config_root, &routing_map);
        let baseline_model = config_root
            .baseline_model
            .clone()
            .unwrap_or_else(|| config::routing::DEFAULT_MODEL_COMPLEX.to_string());
        let mut fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>> = None;
        if !classifiers_config.enabled {
            info!("All classifiers disabled via config");
            (None, HashMap::new(), model_costs, baseline_model, None)
        } else {
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
                                &negative_patterns,
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
                        if let Some(config) = config::loader::load_fewshot_config_from_value(&config_root) {
                            let fewshot = Arc::new(classification::fewshot::FewShotClassifier::new(
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
                            config::loader::load_llm_classifier_config_from_value(&config_root)
                        {
                            let llm = classification::llm::LLMClassifier::new(
                                llm_config,
                                http_client.clone(),
                                categories.clone(),
                                auth_providers.clone(),
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
                (None, HashMap::new(), model_costs, baseline_model, None)
            } else {
                let chain = classification::chain::ClassifierChain::new(backends);
                let mut merged_routing = HashMap::new();
                for backend in chain.backends().iter() {
                    if let Some(r) = backend.get_routing() {
                        merged_routing.extend(r.clone());
                    }
                }
                (
                    Some(Arc::new(chain)),
                    merged_routing,
                    model_costs,
                    baseline_model,
                    fewshot_classifier,
                )
            }
        }
    };

    let db_config = config::loader::load_database_config_from_value(&config_root);
    let persistence_settings = config::loader::load_persistence_config_from_value(&config_root);
    let semaphore_limit = db_config.log_concurrency_limit as usize;

    let persistence_state = {
        let db_url = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty());

        // Priority 1: DATABASE_URL env var forces Postgres.
        if let Some(_url) = db_url {
            let backend = persistence::postgres::PostgresBackend::from_env(&db_config).await;
            match backend {
                Ok(b) => {
                    info!("Persistence backend: postgres (via DATABASE_URL)");
                    Some(persistence::PersistenceConfig {
                        backend: Arc::new(persistence::DbBackend::Postgres(b)),
                        task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                    })
                }
                Err(e) => {
                    panic!("{e}");
                }
            }
        } else {
            // Priority 2: Read backend from config.
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
                    match persistence::sqlite::SqliteBackend::from_path(&persistence_settings.sqlite_path)
                        .await
                    {
                        Ok(backend) => {
                            info!(
                                "Persistence backend: sqlite (path={})",
                                persistence_settings.sqlite_path
                            );
                            Some(persistence::PersistenceConfig {
                                backend: Arc::new(persistence::DbBackend::Sqlite(backend)),
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
                    // Default: memory.
                    let backend = persistence::memory::MemoryBackend::new();
                    info!("Persistence backend: memory");
                    Some(persistence::PersistenceConfig {
                        backend: Arc::new(persistence::DbBackend::Memory(backend)),
                        task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                    })
                }
            }
        }
    };

    let cors_config = config::loader::load_cors_config_from_value(&config_root);
    let allowed_origins = Arc::new(RwLock::new(cors_config.allowed_origins));

    let response_cache: Option<Arc<cache::ResponseCache>> =
        config::loader::load_cache_config_from_value(&config_root).map(|cfg| {
            info!(
                "Response cache enabled: ttl={}s max_entries={}",
                cfg.ttl_secs, cfg.max_entries
            );
            Arc::new(cache::ResponseCache::new(cfg.ttl_secs, cfg.max_entries))
        });

    let app_state = Arc::new(AppState {
        persistence: persistence_state,
        classifier,
        fewshot_classifier,
        routing: Arc::new(tokio::sync::RwLock::new(routing)),
        model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
        baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
        classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(classify_db_log)),
        http_client: Some(http_client),
        max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(
            max_upstream_body_bytes as usize,
        )),
        keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(keepalive_interval_secs as u64)),
        request_body_limit_bytes: http_config.request_body_limit_bytes,
        streaming_channel_capacity: http_config.streaming_channel_capacity,
        dashboard_config: config::loader::load_dashboard_config_from_value(&config_root),
        auth_providers,
        allowed_origins,
        response_cache,
        #[cfg(feature = "otel")]
        metrics: otel.as_ref().map(|(_, m)| m.clone()),
    });

    let port = server_config.port;

    let app = build_app(auth_config, app_state);
    let bind_addr = format!("0.0.0.0:{port}");
    info!("Starting frugalis on {bind_addr}");

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("Axum server exited unexpectedly");

    #[cfg(feature = "otel")]
    if let Some((guard, _)) = otel {
        let shutdown_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || guard.shutdown()),
        )
        .await;
        match shutdown_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("OTel shutdown task panicked: {e}"),
            Err(_) => {
                warn!("OTel shutdown timed out after 5s; exiting with telemetry possibly unflushed")
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    tokio::select! {
        _ = ctrl_c => {
            info!("Shutdown signal received (SIGINT), flushing telemetry");
        }
        _ = term.recv() => {
            info!("Shutdown signal received (SIGTERM), flushing telemetry");
        }
    }
}



