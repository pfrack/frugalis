use std::collections::HashMap;
use std::convert::Infallible;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tokio::sync::RwLock;
use tokio_stream::{Stream, StreamExt};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::{debug, info, warn, Subscriber};
use tracing_subscriber::{fmt, layer::Layer, prelude::*, EnvFilter, Registry};

#[cfg(feature = "otel")]
mod telemetry;
#[cfg(feature = "otel")]
use opentelemetry::KeyValue;

#[cfg(feature = "otel")]
struct RequestMetrics {
    metrics: Option<telemetry::Metrics>,
    method: &'static str,
    route: &'static str,
    start: std::time::Instant,
    status: StatusCode,
}

#[cfg(feature = "otel")]
impl RequestMetrics {
    fn new(metrics: Option<telemetry::Metrics>, method: &'static str, route: &'static str) -> Self {
        Self {
            metrics,
            method,
            route,
            start: std::time::Instant::now(),
            status: StatusCode::OK,
        }
    }
    fn set_status(&mut self, status: StatusCode) {
        self.status = status;
    }
}

#[cfg(feature = "otel")]
impl Drop for RequestMetrics {
    fn drop(&mut self) {
        if let Some(ref m) = self.metrics {
            let attrs = [
                KeyValue::new("method", self.method),
                KeyValue::new("route", self.route),
                KeyValue::new("status", self.status.as_u16().to_string()),
            ];
            m.requests_total.add(1, &attrs);
            m.request_duration_seconds
                .record(self.start.elapsed().as_secs_f64(), &attrs);
        }
    }
}

mod auth;
mod config;
mod dashboard;
mod fewshot_classifier;
mod intent_classifier;
mod persistence;
mod quickstart;
mod routing;

use intent_classifier::IntentClassify;

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classifier::ClassifierChain>>,
    fewshot_classifier: Option<Arc<fewshot_classifier::FewShotClassifier>>,
    routing:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, intent_classifier::RouteEntry>>>,
    model_costs: Arc<tokio::sync::RwLock<intent_classifier::ModelCosts>>,
    baseline_model: Arc<tokio::sync::RwLock<String>>,
    classify_db_log: Arc<std::sync::atomic::AtomicBool>,
    http_client: Option<reqwest::Client>,
    max_upstream_body_bytes: Arc<tokio::sync::RwLock<usize>>,
    keepalive_interval_secs: Arc<tokio::sync::RwLock<u64>>,
    request_body_limit_bytes: usize,
    streaming_channel_capacity: usize,
    dashboard_config: config::DashboardConfig,
    auth_providers: Arc<Vec<config::AuthProviderConfig>>,
    allowed_origins: Arc<RwLock<Vec<String>>>,
    #[cfg(feature = "otel")]
    pub metrics: Option<telemetry::Metrics>,
}

/// Embedded init template loaded at compile time. Used by `--init` to
/// produce a commented starter config the user can fill in.
const INIT_TEMPLATE: &str = include_str!("../init_template.toml");

/// Write the init template to the given path, or print it to stdout if no
/// path is given. Refuses to overwrite an existing file unless `force` is
/// true. Creates parent directories as needed. Returns an error suitable
/// for `eprintln!` on failure (empty on success).
fn run_init(path: Option<&str>, force: bool) -> Result<(), String> {
    match path {
        Some(p) => {
            // Reject flag-shaped paths to avoid silently swallowing unknown
            // flags (e.g. `cerebrum --init --validate` would otherwise drop
            // `--validate` and treat --init as having no path). The check is
            // intentionally narrow — paths that happen to contain `--` in
            // the middle are unaffected.
            if p.starts_with('-') {
                return Err(format!(
                    "refusing path that starts with '-': {p} (looks like a flag, not a path)"
                ));
            }
            let path = std::path::Path::new(p);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("failed to create parent directory for {}: {}", p, e)
                    })?;
                }
            }
            // Atomic create-or-overwrite: avoids the TOCTOU race between
            // `path.exists()` and `std::fs::write` (a symlink could be
            // installed in the gap when --force is used). `create_new` and
            // `truncate` are mutually exclusive — the combination enforces
            // the same external behavior as the old exists/write pair.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .create_new(!force)
                .truncate(force)
                .open(path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        format!("refusing to overwrite existing file: {p} (use --force to overwrite)")
                    } else {
                        format!("failed to write {p}: {e}")
                    }
                })?;
            std::io::Write::write_all(&mut file, INIT_TEMPLATE.as_bytes())
                .map_err(|e| format!("failed to write {p}: {e}"))?;
            eprintln!("Wrote starter config to {p}");
        }
        None => {
            print!("{}", INIT_TEMPLATE);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    // Parse CLI arguments
    enum CliMode {
        Run,
        Validate,
        Help,
        Init(Option<String>),
        Quickstart,
    }

    let args: Vec<String> = std::env::args().collect();
    let mut mode = CliMode::Run;
    let mut force = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--validate" => {
                mode = CliMode::Validate;
                i += 1;
            }
            "--help" => {
                mode = CliMode::Help;
                i += 1;
            }
            "--init" => {
                let mut j = i + 1;
                // --force may appear before or after the path; handle both.
                if args.get(j).map(|a| a.as_str()) == Some("--force") {
                    force = true;
                    j += 1;
                }
                // Resolve the path arg. If the next arg starts with `--`, treat
                // it as an unknown flag (don't silently drop it as we used to)
                // so the user gets a clear error at the point of confusion
                // and `run_init` is never called with `Init(None)` from a typo.
                let path = match args.get(j) {
                    Some(s) if s.starts_with("--") => {
                        eprintln!("unknown argument: {s}");
                        std::process::exit(2);
                    }
                    Some(s) => {
                        j += 1;
                        Some(s.clone())
                    }
                    None => None,
                };
                if args.get(j).map(|a| a.as_str()) == Some("--force") {
                    force = true;
                    j += 1;
                }
                i = j;
                mode = CliMode::Init(path);
            }
            "--quickstart" => {
                mode = CliMode::Quickstart;
                i += 1;
            }
            "--force" => {
                // Standalone --force (outside --init). Consumed for forward
                // compatibility with other commands; ignored if no command
                // acts on it.
                force = true;
                i += 1;
            }
            _ => {
                eprintln!("unknown argument: {}", args[i]);
                std::process::exit(2);
            }
        }
    }

    // Early-exit commands (before config loading or tracing init)
    if let CliMode::Help = mode {
        print!(
            "\
cerebrum — intent-aware routing gateway

USAGE:
    cerebrum [OPTIONS]

OPTIONS:
    --help         Show this help
    --init [PATH]  Generate a starter config (default: stdout)
    --force        With --init, overwrite an existing file at PATH
    --quickstart   Interactive setup wizard
    --validate     Validate configuration and exit

ENVIRONMENT:
    CONFIG_PATH              Path to config overlay (TOML or YAML)
    PROXY_API_BEARER_TOKEN   Required for proxy routes
    DASHBOARD_BASIC_USER     Required for dashboard access
    DASHBOARD_BASIC_PASSWORD Required for dashboard access
"
        );
        std::process::exit(0);
    }

    if let CliMode::Init(path_opt) = &mode {
        match run_init(path_opt.as_deref(), force) {
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

    let server_config = config::load_server_config_from_value(&config_root);

    // Initialize OpenTelemetry providers before tracing (layers reference the providers)
    #[cfg(feature = "otel")]
    let otel: Option<(telemetry::OtelGuard, telemetry::Metrics)> = telemetry::init("cerebrum");

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
        tracing::error!("Panic in Cerebrum: {info}");
    }));

    let auth_config = auth::AuthConfig::from_env().unwrap_or_else(|err| {
        panic!("Auth configuration error: {err}");
    });
    let auth_config = Arc::new(auth_config);

    if !config_path_was_set {
        info!("No CONFIG_PATH set — using embedded defaults. Run `cerebrum --init` to generate a starter config.");
    }

    let regex_config = config::load_regex_classifier_config_from_value(&config_root);

    // Load global classifiers config
    let classifiers_config = config::load_classifiers_config_from_value(&config_root);

    let negative_patterns = config::load_negative_patterns_from_value(&config_root);

    let http_config = config::load_http_config_from_value(&config_root);
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
    let auth_providers = Arc::new(config::load_auth_providers_from_value(&config_root));
    let (classifier, routing, model_costs, baseline_model, fewshot_classifier) = {
        let categories_res = config::load_categories_from_value(&config_root);
        let categories_ok = categories_res.is_ok();
        let mut categories = categories_res.unwrap_or_default();

        // Resolve external pattern files for each category
        let patterns_dir = config_root
            .patterns_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("./patterns"));
        for cat in &mut categories {
            if let Some(ref pf) = cat.patterns_file.take() {
                match config::load_patterns_from_file(pf, &patterns_dir) {
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

        let (mut routing_map, mut fallback_entry) = match config::routing_from_value(&config_root) {
            Ok((map, fallback)) => (map, fallback),
            Err(e) => {
                warn!(
                    "routing config parsing failed: {}; using hardcoded routing defaults",
                    e
                );
                config::hardcoded_routing(&categories)
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
                let (new_map, new_fallback) = config::hardcoded_routing(&categories);
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
            info!("Route {} -> {} @ {}", key, entry.model, entry.endpoint);
        }
        if !routing_map.contains_key("DEFAULT") {
            info!(
                "Route DEFAULT -> {} @ {}",
                fallback_entry.model, fallback_entry.endpoint
            );
        }

        let model_costs = config::build_model_costs(&config_root, &routing_map);
        let baseline_model = config_root
            .baseline_model
            .clone()
            .unwrap_or_else(|| intent_classifier::DEFAULT_MODEL_COMPLEX.to_string());
        let mut fewshot_classifier: Option<Arc<fewshot_classifier::FewShotClassifier>> = None;
        if !classifiers_config.enabled {
            info!("All classifiers disabled via config");
            (None, HashMap::new(), model_costs, baseline_model, None)
        } else {
            let mut backends: Vec<Arc<dyn intent_classifier::IntentClassify + Send + Sync>> =
                Vec::new();

            for name in &classifiers_config.order {
                match name.as_str() {
                    "regex" => {
                        if regex_config.enabled {
                            match intent_classifier::RegexClassifier::from_env(
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
                        if let Some(config) = config::load_fewshot_config_from_value(&config_root) {
                            let fewshot = Arc::new(fewshot_classifier::FewShotClassifier::new(
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
                            config::load_llm_classifier_config_from_value(&config_root)
                        {
                            let llm = intent_classifier::LLMClassifier::new(
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
                let chain = intent_classifier::ClassifierChain::new(backends);
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

    let db_config = config::load_database_config_from_value(&config_root);
    let persistence_settings = config::load_persistence_config_from_value(&config_root);
    let semaphore_limit = db_config.log_concurrency_limit as usize;

    let persistence_state = {
        let db_url = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty());

        // Priority 1: DATABASE_URL env var forces Postgres.
        if let Some(_url) = db_url {
            let backend = persistence::PostgresBackend::from_env(&db_config).await;
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
                    let backend = persistence::MemoryBackend::new();
                    info!("Persistence backend: memory (per config fallback)");
                    Some(persistence::PersistenceConfig {
                        backend: Arc::new(persistence::DbBackend::Memory(backend)),
                        task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                    })
                }
                "sqlite" => {
                    match persistence::SqliteBackend::from_path(&persistence_settings.sqlite_path)
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
                            let backend = persistence::MemoryBackend::new();
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
                    let backend = persistence::MemoryBackend::new();
                    info!("Persistence backend: memory");
                    Some(persistence::PersistenceConfig {
                        backend: Arc::new(persistence::DbBackend::Memory(backend)),
                        task_semaphore: Arc::new(tokio::sync::Semaphore::new(semaphore_limit)),
                    })
                }
            }
        }
    };

    let cors_config = config::load_cors_config_from_value(&config_root);
    let allowed_origins = Arc::new(RwLock::new(cors_config.allowed_origins));

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
        dashboard_config: config::load_dashboard_config_from_value(&config_root),
        auth_providers,
        allowed_origins,
        #[cfg(feature = "otel")]
        metrics: otel.as_ref().map(|(_, m)| m.clone()),
    });

    let port = server_config.port;

    let app = build_app(auth_config, app_state);
    let bind_addr = format!("0.0.0.0:{port}");
    info!("Starting cerebrum on {bind_addr}");

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

async fn health() -> (StatusCode, &'static str) {
    debug!("Health check request received");
    (StatusCode::OK, "ok")
}

/// Shared logging helper. Extracts the snippet, builds the inference record,
/// and enqueues a fire-and-forget DB write.
fn log_classification(
    state: &AppState,
    classification: &intent_classifier::ClassificationResult,
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
            created_at: chrono::Utc::now(),
        };
        persistence::log_inference(
            persistence.backend.clone(),
            persistence.task_semaphore.clone(),
            record,
        );
    }
}

/// Shared classify-and-log logic. Validates Content-Type, extracts the prompt,
/// classifies intent, builds the JSON response, and optionally enqueues a
/// fire-and-forget inference record with the given `log_status`.
async fn classify_and_log(
    headers: &HeaderMap,
    body_str: &str,
    start: std::time::Instant,
    state: &AppState,
    log_status: Option<&str>,
) -> impl IntoResponse {
    #[cfg(feature = "otel")]
    let mut rm = RequestMetrics::new(state.metrics.clone(), "POST", "/v1/classify");

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        return json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"error":"bad_request","status":415,"message":"expected application/json"}"#
                .to_string(),
        );
    }

    let prompt = persistence::extract_last_user_message(body_str);

    let classification = match state.classifier.as_ref() {
        Some(c) => c.classify(&prompt).await,
        None => intent_classifier::ClassificationResult::fallback(),
    };

    #[cfg(feature = "otel")]
    if let Some(ref metrics) = state.metrics {
        metrics.classification_total.add(
            1,
            &[
                KeyValue::new("category", classification.category.clone()),
                KeyValue::new("tier", format!("{:?}", classification.tier)),
            ],
        );
    }

    let response_body = serde_json::json!({
        "status": "classified",
        "category": classification.category,
        "model": classification.model,
        "tier": format!("{:?}", classification.tier),
    })
    .to_string();
    if let Some(log_status) = log_status {
        log_classification(state, &classification, body_str, start, log_status);
    }

    json_response(StatusCode::OK, response_body)
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

fn upstream_error_json(status: u16, message: &str) -> String {
    serde_json::json!({
        "error": "upstream_error",
        "status": status,
        "message": message,
    })
    .to_string()
}

fn classification_only_json(result: &intent_classifier::ClassificationResult) -> String {
    serde_json::json!({
        "status": "classified",
        "category": result.category,
        "model": result.model,
        "tier": format!("{:?}", result.tier),
    })
    .to_string()
}

fn build_upstream_request(
    client: &reqwest::Client,
    classification: &intent_classifier::ClassificationResult,
    body: &Bytes,
    api_key: &str,
    auth_providers: &[config::AuthProviderConfig],
) -> Result<(bool, reqwest::RequestBuilder), String> {
    let mut req_body: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;

    let client_wants_stream = req_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let serde_json::Value::Object(map) = &mut req_body {
        map.insert(
            "model".to_string(),
            serde_json::Value::String(classification.model.clone()),
        );
    } else {
        return Err("request body must be a JSON object".to_string());
    }

    let modified_body = serde_json::to_vec(&req_body).unwrap_or_else(|_| body.to_vec());

    let auth_headers =
        intent_classifier::auth_headers_for(auth_providers, &classification.provider_type, api_key);

    let mut req = client
        .post(&classification.endpoint)
        .header(header::CONTENT_TYPE, "application/json")
        .body(modified_body);
    for (name, value) in &auth_headers {
        req = req.header(name.as_str(), value.as_str());
    }

    Ok((client_wants_stream, req))
}

async fn handle_buffered_response(
    mut upstream_response: reqwest::Response,
    max_upstream_body_bytes: usize,
) -> (StatusCode, String) {
    let upstream_status = upstream_response.status();
    if !upstream_status.is_success() {
        const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
        let mut error_bytes = Vec::new();
        let error_body = loop {
            match upstream_response.chunk().await {
                Ok(Some(chunk)) => {
                    if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                        let error_text = String::from_utf8_lossy(&error_bytes)
                            .chars()
                            .take(512)
                            .collect::<String>()
                            .replace(['\n', '\r'], " ");
                        break upstream_error_json(upstream_status.as_u16(), &error_text);
                    }
                    error_bytes.extend_from_slice(&chunk);
                }
                Ok(None) => {
                    let error_text = String::from_utf8_lossy(&error_bytes)
                        .chars()
                        .take(512)
                        .collect::<String>()
                        .replace(['\n', '\r'], " ");
                    break upstream_error_json(upstream_status.as_u16(), &error_text);
                }
                Err(e) => break upstream_error_json(502, &e.to_string()),
            }
        };
        return (
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            error_body,
        );
    }

    let mut upstream_body_bytes: Vec<u8> = Vec::new();
    let upstream_body = loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if upstream_body_bytes.len() + chunk.len() > max_upstream_body_bytes {
                    return (
                        StatusCode::BAD_GATEWAY,
                        upstream_error_json(502, "upstream response too large"),
                    );
                }
                upstream_body_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break String::from_utf8_lossy(&upstream_body_bytes).into_owned(),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    upstream_error_json(502, &e.to_string()),
                );
            }
        }
    };

    let response_body = match serde_json::from_str::<serde_json::Value>(&upstream_body) {
        Ok(value) => serde_json::to_string(&value).unwrap_or(upstream_body),
        Err(_) => upstream_body,
    };
    (StatusCode::OK, response_body)
}

/// Set up SSE streaming response with keepalive and logging.
/// The `Unpin` bound is required because the byte_stream is moved into a spawned task.
/// Spawned tasks must own all captured data (trait objects require `Unpin` for safe pinning).
fn handle_streaming_response(
    state: Arc<AppState>,
    classification: intent_classifier::ClassificationResult,
    body_str: String,
    start: Instant,
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    keepalive_interval_secs: u64,
) -> Response<Body> {
    let channel_capacity = state.streaming_channel_capacity;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(channel_capacity);

    log_classification(&state, &classification, &body_str, start, "streaming");

    tokio::spawn(async move {
        let keepalive_secs = keepalive_interval_secs;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(keepalive_secs));
        let mut stream = byte_stream;
        let mut stream_status = "ok";
        interval.tick().await;
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => { if tx.send(bytes).await.is_err() { break; } }
                        Some(Err(_e)) => {
                            stream_status = "stream_error";
                            // Use the same SSE error event format as
                            // `handle_streaming_error` (non-2xx upstream) so
                            // the two error paths produce byte-compatible
                            // frames — a single SSE error contract. Apply the
                            // same 512-char truncate to bound the SSE event
                            // size (the inline branch's `_e` is a
                            // `reqwest::Error`; while typically < 1 KB, a
                            // pathological upstream could produce a longer
                            // string).
                            let error_text: String = _e.to_string().chars().take(512).collect();
                            let sse_error = format_sse_error_event(&error_text);
                            let _ = tx.send(Bytes::from(sse_error)).await;
                            break;
                        }
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                        break;
                    }
                }
            }
        }
        log_classification(&state, &classification, &body_str, start, stream_status);
    });

    let body =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, Infallible>));

    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    resp
}

/// Build the SSE error event body for an upstream error message:
/// `event: error\ndata: {"error":"<msg>"}\n\n`.
///
/// Applies the JSON-escape rule to the embedded message:
/// - `\\` → `\\\\` (backslash escape, JSON-required)
/// - `"` → `\\"` (double-quote escape, JSON-required)
/// - All C0 control chars (`\0x00`-`\0x1F`, including `\n`, `\r`,
///   `\t`, `\b`, `\f`, and other non-printable bytes) → ` ` (space)
///
/// The C0 → space replacement ensures the resulting `data:` payload
/// is valid JSON (any literal C0 char would break `serde_json::from_str`
/// or smuggle SSE frames into the event body) and is consistent with
/// the plan's original `\n`/`\r` → space rule — extending it to the
/// full C0 range closes the RFC 8259 §7 gap (tab, backspace, form
/// feed, and other control chars) that was previously a no-escape
/// hole. The 2 KB body cap and 512-char truncate (upstream on
/// `handle_streaming_error`) and the status passthrough +
/// `Content-Type: text/event-stream` + `Cache-Control: no-cache`
/// (downstream on the response) are NOT this helper's concern.
/// Helper invariants: (a) JSON-escape correctness of the embedded
/// message, (b) SSE event format. Both call sites — `handle_streaming_error`
/// (non-2xx upstream) and the inline mid-stream error branch in
/// `handle_streaming_response` (chunk stream error) — call this helper
/// with the raw upstream error string.
pub(crate) fn format_sse_error_event(error_msg: &str) -> String {
    let mut escaped = String::with_capacity(error_msg.len());
    for c in error_msg.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            c if (c as u32) < 0x20 => escaped.push(' '),
            _ => escaped.push(c),
        }
    }
    format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", escaped)
}

/// Convert a non-2xx upstream response into an SSE error event for the client.
///
/// 5 invariants protect this code path (the prior-review-fix lessons in
/// `context/foundation/lessons.md`, specifically "Re-run review after a
/// follow-up change touches the same handler" — the F1–F4 review fixes
/// were lost twice across follow-up commits; this function is the
/// regression guard that catches any future re-loss):
/// 1. **Body cap (2 KB)** — upstream error bodies are bounded to 2 KB.
///    Large upstream bodies would amplify latency and memory pressure
///    on the proxy, and SSE clients don't need the full body to surface
///    an error.
/// 2. **JSON escape** — `\`, `"`, and all C0 control chars
///    (`\0x00`-`\0x1F`, including `\n`, `\r`, `\t`, `\b`, `\f`, and
///    other non-printable bytes) in the upstream error text are
///    replaced with safe equivalents before serialization. Without
///    this, a malicious upstream could inject SSE frames or break the
///    JSON parse that downstream consumers use to detect error events.
///    See `format_sse_error_event` for the escape rule.
/// 3. **SSE event format** — the body is `event: error\ndata: {"error":"…"}\n\n`.
///    A valid SSE event with the `error` event name lets clients using
///    `EventSource`-style subscribe to error events distinctly from data
///    events.
/// 4. **Status passthrough** — the upstream's status code is forwarded
///    to the client (e.g., 503 → 503). This preserves the upstream's
///    classification of the failure (rate limit vs. server error vs.
///    auth failure) so clients can react correctly.
/// 5. **`Content-Type: text/event-stream` + `Cache-Control: no-cache`**
///    — the client must parse the body as SSE and must not cache error
///    events (caching would replay a transient error long after it has
///    been resolved).
async fn handle_streaming_error(mut upstream_response: reqwest::Response) -> Response {
    // Bound the upstream error body to 2 KB to cap latency and memory on
    // large error payloads.
    const MAX_ERROR_BODY_BYTES: usize = 2 * 1024;
    let mut error_bytes = Vec::new();
    loop {
        match upstream_response.chunk().await {
            Ok(Some(chunk)) => {
                if error_bytes.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                    break;
                }
                error_bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    // Truncate to 512 chars before passing to the helper. The helper
    // applies the JSON-escape rule and emits the SSE event body.
    let error_text = String::from_utf8_lossy(&error_bytes)
        .chars()
        .take(512)
        .collect::<String>();
    let sse_error = format_sse_error_event(&error_text);
    let mut resp = Response::new(Body::from(sse_error));
    // Forward the upstream's status code to the client so it can react
    // to the specific failure class.
    *resp.status_mut() = upstream_response.status();
    // Mark the response as an uncacheable SSE stream.
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
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
) -> Response {
    let start = std::time::Instant::now();

    #[cfg(feature = "otel")]
    let mut rm = RequestMetrics::new(state.metrics.clone(), "POST", "/v1/chat/completions");

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        return json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"error":"bad_request","status":415,"message":"expected application/json"}"#
                .to_string(),
        );
    }

    let body_str: String = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => {
            #[cfg(feature = "otel")]
            rm.set_status(StatusCode::BAD_REQUEST);
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"bad_request","message":"invalid UTF-8 body"}"#.to_string(),
            );
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

    let classification = if let (Some(category), Some(model)) =
        (x_category.as_ref(), x_model.as_ref())
    {
        let routing = state.routing.read().await;
        match routing.get(category) {
            Some(entry) => intent_classifier::ClassificationResult {
                category: category.clone(),
                model: model.clone(),
                endpoint: entry.endpoint.clone(),
                tier: intent_classifier::ClassificationTier::Fallback,
                provider_type: entry.provider_type.clone(),
                api_key_env: entry.api_key_env.clone(),
            },
            None => {
                warn!("X-Cerebrum-Category '{category}' not found in routing configuration; degrading to classification JSON");
                let fallback = match state.classifier.as_ref() {
                    Some(c) => c.classify("").await,
                    None => intent_classifier::ClassificationResult::fallback(),
                };
                let response_body = classification_only_json(&fallback);
                log_classification(&state, &fallback, &body_str, start, "ok");
                return json_response(StatusCode::OK, response_body);
            }
        }
    } else {
        let prompt = persistence::extract_last_user_message(&body_str);
        match state.classifier.as_ref() {
            Some(c) => c.classify(&prompt).await,
            None => intent_classifier::ClassificationResult::fallback(),
        }
    };

    #[cfg(feature = "otel")]
    if let Some(ref metrics) = state.metrics {
        metrics.classification_total.add(
            1,
            &[
                KeyValue::new("category", classification.category.clone()),
                KeyValue::new("tier", format!("{:?}", classification.tier)),
            ],
        );
    }

    let client = match &state.http_client {
        Some(c) => c,
        None => {
            let response_body = classification_only_json(&classification);
            log_classification(&state, &classification, &body_str, start, "ok");
            return json_response(StatusCode::OK, response_body);
        }
    };

    let api_key = match &classification.api_key_env {
        Some(env_name) => match std::env::var(env_name) {
            Ok(key) if !key.is_empty() => key,
            _ => {
                warn!("upstream API key env var '{env_name}' is missing or empty; degrading to classification-only response");
                log_classification(&state, &classification, &body_str, start, "ok");
                return json_response(StatusCode::OK, classification_only_json(&classification));
            }
        },
        None => {
            warn!("no api_key_env configured for category '{}'; degrading to classification-only response", classification.category);
            let response_body = classification_only_json(&classification);
            log_classification(&state, &classification, &body_str, start, "ok");
            return json_response(StatusCode::OK, response_body);
        }
    };

    if classification.endpoint.is_empty() {
        log_classification(&state, &classification, &body_str, start, "upstream_error");
        #[cfg(feature = "otel")]
        rm.set_status(StatusCode::BAD_GATEWAY);
        return json_response(
            StatusCode::BAD_GATEWAY,
            upstream_error_json(502, "no endpoint configured"),
        );
    }

    let (client_wants_stream, upstream_req) = match build_upstream_request(
        client,
        &classification,
        &body,
        &api_key,
        &state.auth_providers,
    ) {
        Err(msg) => {
            log_classification(&state, &classification, &body_str, start, "bad_request");
            #[cfg(feature = "otel")]
            rm.set_status(StatusCode::BAD_REQUEST);
            return json_response(StatusCode::BAD_REQUEST, upstream_error_json(400, &msg));
        }
        Ok(r) => r,
    };

    #[cfg_attr(not(feature = "otel"), allow(unused_variables))]
    let upstream_start = std::time::Instant::now();
    let upstream_response = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            #[cfg(feature = "otel")]
            if let Some(ref metrics) = state.metrics {
                metrics.upstream_duration_seconds.record(
                    upstream_start.elapsed().as_secs_f64(),
                    &[
                        KeyValue::new("provider", classification.provider_type.clone()),
                        KeyValue::new("status", "502"),
                    ],
                );
            }
            log_classification(&state, &classification, &body_str, start, "upstream_error");
            #[cfg(feature = "otel")]
            rm.set_status(StatusCode::BAD_GATEWAY);
            return json_response(
                StatusCode::BAD_GATEWAY,
                upstream_error_json(502, &e.to_string()),
            );
        }
    };

    #[cfg(feature = "otel")]
    if let Some(ref metrics) = state.metrics {
        metrics.upstream_duration_seconds.record(
            upstream_start.elapsed().as_secs_f64(),
            &[
                KeyValue::new("provider", classification.provider_type.clone()),
                KeyValue::new("status", upstream_response.status().as_u16().to_string()),
            ],
        );
    }

    if client_wants_stream {
        if !upstream_response.status().is_success() {
            let resp = handle_streaming_error(upstream_response).await;
            log_classification(&state, &classification, &body_str, start, "upstream_error");
            #[cfg(feature = "otel")]
            rm.set_status(resp.status());
            return resp;
        }

        let keepalive_interval_secs = *state.keepalive_interval_secs.read().await;

        return handle_streaming_response(
            state,
            classification,
            body_str,
            start,
            upstream_response.bytes_stream(),
            keepalive_interval_secs,
        );
    }

    let max_upstream_body_bytes = *state.max_upstream_body_bytes.read().await;
    let (status, body) = handle_buffered_response(upstream_response, max_upstream_body_bytes).await;
    let log_status = if status == StatusCode::OK {
        "ok"
    } else {
        "upstream_error"
    };
    log_classification(&state, &classification, &body_str, start, log_status);

    #[cfg(feature = "otel")]
    rm.set_status(status);

    json_response(status, body)
}

/// Classify handler: extracts prompt, classifies intent, optionally logs a
/// lightweight classification record with status "classified", and returns
/// classification JSON. Logging is controlled by `CLASSIFY_DB_LOG` env var.
async fn classify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let body_str = std::str::from_utf8(&body).unwrap_or("");
    let log_status = if state
        .classify_db_log
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        Some("classified")
    } else {
        None
    };
    classify_and_log(&headers, body_str, start, &state, log_status).await
}

#[derive(serde::Deserialize)]
struct FeedbackRequest {
    text: String,
    #[serde(default)]
    predicted_category: Option<String>,
    actual_category: String,
    #[serde(default = "default_satisfaction")]
    satisfaction: f64,
}

fn default_satisfaction() -> f64 {
    1.0
}

async fn feedback_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FeedbackRequest>,
) -> impl IntoResponse {
    let fewshot = match &state.fewshot_classifier {
        Some(fs) => fs.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "fewshot_classifier_not_configured",
                    "status": 503,
                    "message": "No few-shot classifier backend is configured"
                })),
            );
        }
    };

    // Validate actual_category against known routing keys
    let routing = state.routing.read().await;
    if !routing.contains_key(&body.actual_category.to_uppercase()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_category",
                "status": 400,
                "message": format!("Unknown category '{}'", body.actual_category)
            })),
        );
    }
    drop(routing);

    // Clamp satisfaction to [0.0, 1.0] as per OpenAPI spec
    let satisfaction = body.satisfaction.clamp(0.0, 1.0);
    fewshot
        .add_feedback(
            body.text,
            body.predicted_category,
            body.actual_category,
            satisfaction,
        )
        .await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "accepted"
        })),
    )
}

fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let proxy_routes = Router::new()
        .route("/chat/completions", post(completion_handler))
        .route("/classify", post(classify_handler))
        .route("/feedback", post(feedback_handler))
        .route_layer(auth::proxy_auth_layer(auth_config.clone()));

    let dashboard_routes = dashboard::routes(auth_config);

    // Build CORS layer from [cors].allowed_origins in config.toml. If empty, no CORS headers (secure default).
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
        .route("/health", get(health))
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
fn test_categories() -> Vec<intent_classifier::CategoryConfig> {
    vec![
        intent_classifier::CategoryConfig {
            name: "FILE_READING".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 1,
            patterns: vec![
                intent_classifier::PatternEntry {
                    regex: r"(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        intent_classifier::CategoryConfig {
            name: "SYNTAX_FIX".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 2,
            patterns: vec![
                intent_classifier::PatternEntry {
                    regex: r"(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        intent_classifier::CategoryConfig {
            name: "COMPLEX_REASONING".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 3,
            patterns: vec![
                intent_classifier::PatternEntry {
                    regex: r"(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        intent_classifier::CategoryConfig {
            name: "CASUAL".to_string(),
            description: String::new(),
            threshold: 1,
            priority: 4,
            patterns: vec![
                intent_classifier::PatternEntry {
                    regex: r"(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
    ]
}

#[cfg(test)]
fn test_negative_patterns() -> Vec<intent_classifier::NegativePatternConfig> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;

    /// Guard that removes an env var on drop to prevent test pollution.
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    /// Read a response body as a `serde_json::Value` so assertions can target
    /// the parsed structure instead of brittle substring matches. Refusing to
    /// return `Option` here means a non-JSON body fails the test loudly,
    /// which is the right behavior for shape contracts.
    async fn parse_json_body(response: axum::response::Response) -> serde_json::Value {
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        serde_json::from_slice(&body_bytes)
            .unwrap_or_else(|e| panic!("response body should be JSON: {e}; body={:?}", body_bytes))
    }

    /// Build an `AppState` from a `RegexClassifier` and optional HTTP client.
    /// Mergeroutes from all classifier backends.
    fn make_test_app_state(
        classifier: intent_classifier::RegexClassifier,
        http_client: Option<reqwest::Client>,
        model_costs: intent_classifier::ModelCosts,
        baseline_model: String,
        max_upstream_body_bytes: usize,
    ) -> Arc<AppState> {
        let classifier_chain = intent_classifier::ClassifierChain::new(vec![Arc::new(classifier)]);
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
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            #[cfg(feature = "otel")]
            metrics: None,
        })
    }

    fn test_app() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        // No-op persistence: persistence is None, so completion_handler skips logging.
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier: None,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            model_costs: Arc::new(tokio::sync::RwLock::new(
                intent_classifier::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: None,
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            #[cfg(feature = "otel")]
            metrics: None,
        });
        build_app(auth_config, app_state)
    }

    fn test_app_with_classifier() -> Router {
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
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            None,
            intent_classifier::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    async fn test_feedback_requires_auth() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/feedback")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"hello","actual_category":"CASUAL"}"#))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_feedback_no_fewshot_returns_503() {
        // test_app_with_classifier has no fewshot_classifier → 503
        let app = test_app_with_classifier();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/feedback")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"text":"hello","actual_category":"SYNTAX_FIX"}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_chain_with_regex_and_fewshot() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let fewshot_config = config::FewShotConfig {
            enabled: true,
            confidence_threshold: 0.4,
            cold_start_threshold: 0.6,
            cold_start_feedback_count: 5,
            feature_dimensions: 1000,
            retraining_threshold: 5,
            data_path: format!("/tmp/fewshot_int_{}.yaml", nanos),
            max_vocabulary_warn: 5000,
            max_training_examples: 10000,
        };
        let fewshot = fewshot_classifier::FewShotClassifier::new(
            fewshot_config,
            HashMap::new(),
            intent_classifier::RouteEntry {
                model: "fallback-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );

        let chain = intent_classifier::ClassifierChain::new(vec![
            Arc::new(regex_classifier),
            Arc::new(fewshot),
        ]);

        // Regex should catch "fix this bug"
        let result = chain.classify("fix this bug").await;
        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, intent_classifier::ClassificationTier::Regex);

        // Regex returns Fallback on non-matching prompt, few-shot catches bootstrap text
        let result = chain.classify("can you explain what a hash map is").await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, intent_classifier::ClassificationTier::FewShot);
    }

    // ── 3-backend chain integration test (Risk #1 — production data path floor) ──
    // Proves the chain escalates regex → fewshot → LLM when both regex and
    // fewshot return Fallback. Uses CountingClassifier for fewshot side-effect
    // observation (tier inspection cannot distinguish regex-tier from LLM-tier
    // matches because LLMClassifier returns tier: Regex on success) and
    // httpmock to assert the LLM was called exactly once.
    #[tokio::test]
    #[serial]
    async fn test_chain_3_backend_escalates_to_llm() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use httpmock::prelude::*;
        use intent_classifier::test_util::CountingClassifier;
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _guard = EnvGuard("OPENAI_API_KEY");
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let server = MockServer::start();
        let llm_mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "content": "SYNTAX_FIX"
                        }
                    }
                ]
            }));
        });

        let cats = test_categories();
        let cats_for_llm = cats.clone();
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );

        // CountingClassifier for the fewshot tier — always returns Fallback.
        // Forces the chain to escalate past fewshot into the LLM tier.
        let fewshot_counter = Arc::new(AtomicUsize::new(0));
        let fewshot_stub = CountingClassifier {
            counter: fewshot_counter.clone(),
            result: intent_classifier::ClassificationResult::fallback(),
        };

        let llm_config = config::LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };
        let llm = intent_classifier::LLMClassifier::new(
            llm_config,
            reqwest::Client::new(),
            cats_for_llm,
            Arc::new(vec![]),
        );

        let chain = intent_classifier::ClassifierChain::new(vec![
            Arc::new(regex_classifier),
            Arc::new(fewshot_stub),
            Arc::new(llm),
        ]);

        // A prompt that matches no regex pattern (>30 chars to avoid the
        // short-prompt → CASUAL routing) and that the fewshot stub returns
        // Fallback for. Forces escalation to the LLM tier.
        let result = chain
            .classify("this is a long prompt that exercises the chain's escalation path from regex through fewshot to the llm tier")
            .await;

        // LLMClassifier sets tier: Regex on a successful match (architectural
        // detail: ClassificationTier has no Llm variant). The chain sees
        // tier != Fallback and returns this result. We verify the escalation
        // happened via side-effect counters, not via tier inspection.
        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, intent_classifier::ClassificationTier::Regex);
        assert_eq!(
            fewshot_counter.load(Ordering::SeqCst),
            1,
            "fewshot backend should be called exactly once (and return Fallback)"
        );
        llm_mock.assert_hits(1);
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

        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("category").and_then(|v| v.as_str()),
            Some("SYNTAX_FIX"),
            "expected SYNTAX_FIX category, got: {json}"
        );
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("classified"),
            "expected classified status"
        );
        assert_eq!(
            json.get("tier").and_then(|v| v.as_str()),
            Some("Regex"),
            "expected Regex tier"
        );
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

        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("category").and_then(|v| v.as_str()),
            Some("SYNTAX_FIX"),
            "expected SYNTAX_FIX category, got: {json}"
        );
        assert_eq!(
            json.get("model").and_then(|v| v.as_str()),
            Some("sf-model"),
            "expected sf-model, got: {json}"
        );
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("classified"),
            "expected classified status"
        );
        assert_eq!(
            json.get("tier").and_then(|v| v.as_str()),
            Some("Regex"),
            "expected Regex tier"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_max_upstream_body_bytes_truncation() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard2 = EnvGuard("TEST_API_KEY");
        // Set limit to 1.1MB and send response > limit to trigger truncation
        std::env::set_var("TEST_API_KEY", "sk-test");
        let (app, server) = test_app_with_http_client("TEST_API_KEY", 1_100_000);
        let large_content = "x".repeat(2_000_000); // 2MB payload
        let body = format!("{{\"choices\":[{{\"message\":{{\"content\":\"{large_content}\"}}}}]}}");
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(body);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("error").and_then(|v| v.as_str()),
            Some("upstream_error"),
            "expected upstream_error contract, got: {json}"
        );
        assert_eq!(
            json.get("message").and_then(|v| v.as_str()),
            Some("upstream response too large"),
            "expected truncation message, got: {json}"
        );
        mock.assert();
    }

    fn test_app_with_enriched_classifier(
        provider_type_val: &str,
        api_key_env_val: Option<&str>,
    ) -> Router {
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
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: "https://test.endpoint".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: provider_type_val.to_string(),
                api_key_env: api_key_env_val.map(|s| s.to_string()),
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            None,
            intent_classifier::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_does_not_include_enriched_fields() {
        let _guard = EnvGuard("TEST_API_KEY");
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

        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("category").and_then(|v| v.as_str()),
            Some("SYNTAX_FIX"),
            "expected SYNTAX_FIX category"
        );
        for forbidden in ["provider_type", "endpoint", "api_key"] {
            assert!(
                json.get(forbidden).is_none(),
                "response should NOT contain {forbidden}, got: {json}"
            );
        }
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

        let json = parse_json_body(response).await;
        assert!(
            json.get("api_key").is_none(),
            "response should NOT contain api_key, got: {json}"
        );
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

        let json = parse_json_body(response).await;
        for forbidden in ["provider_type", "api_key"] {
            assert!(
                json.get(forbidden).is_none(),
                "classify response should not contain {forbidden}, got: {json}"
            );
        }
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
        let pool = match persistence::test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP persistence_integration_prompt_char_count_column_exists: DATABASE_URL not set or unreachable");
                return;
            }
        };
        let row: Option<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT data_type FROM information_schema.COLUMNS \
             WHERE table_name = 'inferences' AND column_name = 'prompt_char_count'",
        )
        .fetch_optional(pool.as_ref())
        .await
        .expect("schema query should succeed");
        let row = row.expect("prompt_char_count column should exist in the inferences table");
        use sqlx::Row;
        let data_type: String = row.try_get("data_type").unwrap();
        assert_eq!(
            data_type, "integer",
            "prompt_char_count should be INTEGER type"
        );
    }

    #[tokio::test]
    async fn persistence_integration_insert_and_read_back() {
        let pool = match persistence::test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP persistence_integration_insert_and_read_back: DATABASE_URL not set or unreachable");
                return;
            }
        };
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = persistence::PostgresBackend {
            pool: (*pool).clone(),
        };
        let db_backend = Arc::new(persistence::DbBackend::Postgres(backend));

        let request_id = uuid::Uuid::new_v4();
        let record = persistence::InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("chat".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(10),
            prompt_snippet: "integration test snippet".to_string(),
            prompt_char_count: Some(25),
            created_at: chrono::Utc::now(),
        };
        let handle = persistence::log_inference(db_backend, semaphore, record);
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

    /// Integration test: verifies that a successful SSE streaming request
    /// produces exactly two inference records with statuses "streaming" and "ok".
    /// Requires DATABASE_URL to be set; skips gracefully otherwise.
    #[tokio::test]
    #[serial]
    async fn persistence_integration_sse_streaming_success() {
        let pool = match persistence::test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP persistence_integration_sse_streaming_success: DATABASE_URL not set or unreachable");
                return;
            }
        };

        let _mock_api_key_guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));

        let (app, server) = build_app_with_persistence(pool.clone(), semaphore.clone(), None);

        let unique_id = uuid::Uuid::new_v4().to_string();
        let test_message = format!("fix this bug {}", unique_id);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("data: hello\n\n");
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"messages":[{{"role":"user","content":"{}"}}],"stream":true}}"#,
                        test_message
                    )))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        // Wait for the background logging task to complete
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Verify DB records: should have exactly "streaming" and "ok"
        let rows = sqlx::query(&format!("SELECT status FROM inferences WHERE prompt_snippet LIKE '%{}%' ORDER BY created_at ASC", test_message))
            .fetch_all(pool.as_ref())
            .await
            .expect("query should succeed");

        use sqlx::Row;
        let statuses: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("status").unwrap())
            .collect();

        assert_eq!(
            statuses,
            vec!["streaming", "ok"],
            "expected streaming then ok records"
        );
    }

    /// Integration test: verifies that a failed SSE streaming request (upstream error)
    /// produces records with "streaming" and "stream_error".
    /// Requires DATABASE_URL to be set; skips gracefully otherwise.
    #[tokio::test]
    #[serial]
    async fn persistence_integration_sse_streaming_error() {
        let pool = match persistence::test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP persistence_integration_sse_streaming_error: DATABASE_URL not set or unreachable");
                return;
            }
        };

        let _mock_api_key_guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));

        let (app, server) = build_app_with_persistence(pool.clone(), semaphore.clone(), None);

        let unique_id = uuid::Uuid::new_v4().to_string();
        let test_message = format!("fix this error {}", unique_id);

        // Mock upstream that returns error
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":"service unavailable"}"#);
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"messages":[{{"role":"user","content":"{}"}}],"stream":true}}"#,
                        test_message
                    )))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        mock.assert();

        // Wait for the background logging task to complete
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Verify DB records: should have "streaming" and "upstream_error"
        let rows = sqlx::query(&format!("SELECT status FROM inferences WHERE prompt_snippet LIKE '%{}%' ORDER BY created_at ASC", test_message))
            .fetch_all(pool.as_ref())
            .await
            .expect("query should succeed");

        use sqlx::Row;
        let statuses: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("status").unwrap())
            .collect();

        assert_eq!(
            statuses,
            vec!["upstream_error"],
            "expected upstream_error record only"
        );
    }

    // ── In-memory snippet path tests (Risk #2 F1 — runs in default CI) ──
    // The 3 tests below exercise the F1 invariants end-to-end via the real
    // axum stack (proxy → completion_handler → log_classification →
    // log_inference → MemoryBackend::insert_inference) without requiring
    // DATABASE_URL. They read from MemoryBackend::records directly to prove
    // the data flowed through `log_classification` end-to-end.

    #[tokio::test]
    #[serial]
    async fn test_snippet_path_truncates_to_200_chars() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = persistence::MemoryBackend::new();
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(persistence::DbBackend::Memory(memory_backend));
        let (app, server) = build_app_with_persistence_backend(backend, semaphore, None);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });

        let long_message = format!("fix this bug {}", "x".repeat(487)); // 500 chars total
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"messages":[{{"role":"user","content":"{}"}}]}}"#,
                        long_message
                    )))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        // Wait for the fire-and-forget log task to complete.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let records = records_handle.read().await;
        assert_eq!(records.len(), 1, "expected exactly one persisted record");
        let snippet = &records[0].prompt_snippet;
        assert!(
            snippet.chars().count() <= 200,
            "snippet should be <= 200 chars (got {})",
            snippet.chars().count()
        );
        assert_eq!(
            records[0].prompt_char_count,
            Some(500),
            "prompt_char_count should preserve the full message length"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_snippet_path_does_not_contain_full_prompt() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = persistence::MemoryBackend::new();
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(persistence::DbBackend::Memory(memory_backend));
        let (app, server) = build_app_with_persistence_backend(backend, semaphore, None);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });

        // Build a message where the marker sits PAST the 200-char truncation
        // point. The 200-char snippet must contain the prefix but NOT the
        // marker, proving that the full prompt body is not persisted.
        // Prefix = "fix this bug " (13) + 167 'a' = 180 chars. Total message
        // = 180 + 26 (marker) + 100 ('x') = 306 chars. The marker starts at
        // position 180, so the 200-char snippet (positions 0-199) only
        // includes the first 20 chars of the 26-char marker.
        // `snippet.contains(marker)` is therefore false.
        let prefix = format!("fix this bug {}", "a".repeat(167));
        let marker = "UNIQUE_MARKER_XYZ_9876543210";
        let message = format!("{prefix}{marker}{}", "x".repeat(100));
        let full_message_len = message.chars().count();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"messages":[{{"role":"user","content":"{}"}}]}}"#,
                        message
                    )))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let records = records_handle.read().await;
        assert_eq!(records.len(), 1);
        let snippet = &records[0].prompt_snippet;
        assert!(
            snippet.contains(&prefix),
            "snippet should contain the 200-char prefix, got: {snippet}"
        );
        assert!(
            !snippet.contains(marker),
            "snippet should NOT contain the marker (which sits past the 200-char truncation point), got: {snippet}"
        );
        assert!(
            snippet.chars().count() <= 200,
            "snippet should be <= 200 chars (got {})",
            snippet.chars().count()
        );
        assert_eq!(
            records[0].prompt_char_count,
            Some(full_message_len as i32),
            "prompt_char_count should preserve the full message length"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_log_classification_failure_does_not_block_response() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = persistence::MemoryBackend::new();
        // Inject one failure into the next insert. The flag auto-resets to
        // false after the first call (see MemoryBackend::insert_inference).
        memory_backend
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(persistence::DbBackend::Memory(memory_backend));
        let (app, server) = build_app_with_persistence_backend(backend.clone(), semaphore, None);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
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
            .expect("completion request should succeed even when log_inference fails");

        // (a) Response status is 200 — the proxy succeeds even though the
        // background log task will fail.
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        // Wait for the fire-and-forget log task to attempt + fail.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // (b) The failed insert means the record was NOT persisted AND
        // `fail_next` was atomically consumed. The consumption check confirms
        // the log task actually ran within the wait window (otherwise the
        // `records.len() == 0` check above would be a false-positive
        // indistinguishable from "log task never ran").
        // (The flag auto-resets, so a follow-up request would succeed.)
        let records = records_handle.read().await;
        assert_eq!(
            records.len(),
            0,
            "the injected failure should prevent the record from being persisted"
        );
        drop(records);
        if let persistence::DbBackend::Memory(ref mb) = *backend {
            assert!(
                !mb.fail_next.load(std::sync::atomic::Ordering::SeqCst),
                "fail_next must have been consumed by the log task within the wait window; \
                 if this fires, the log task didn't run and the records.len() check above is meaningless"
            );
        } else {
            panic!("test fixture invariant: backend must be DbBackend::Memory");
        }
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
        assert!(
            content_type.starts_with("text/html"),
            "expected HTML response"
        );
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
            body.contains("Database not configured") || body.contains("No inference records yet"),
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

    pub(crate) fn test_app_with_http_client(
        env_var_name: &str,
        max_upstream_body_bytes: usize,
    ) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
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
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: endpoint.clone(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            Some(client),
            intent_classifier::ModelCosts::empty(),
            String::new(),
            max_upstream_body_bytes,
        );
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    /// Build app state and router with an arbitrary `DbBackend` for integration tests.
    /// The in-memory variant (`DbBackend::Memory`) runs in default CI without
    /// `DATABASE_URL`; the Postgres variant requires `DATABASE_URL` and is used
    /// by the existing `persistence_integration_*` tests (which skip cleanly
    /// when the env var is not set).
    pub(crate) fn build_app_with_persistence_backend(
        backend: Arc<persistence::DbBackend>,
        semaphore: Arc<tokio::sync::Semaphore>,
        http_client: Option<reqwest::Client>,
    ) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("test reqwest client should build")
        });
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: endpoint.clone(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some("MOCK_API_KEY".to_string()),
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some("MOCK_API_KEY".to_string()),
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let classifier_chain =
            intent_classifier::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier_arc = Some(Arc::new(classifier_chain));
        let mut merged_routing = std::collections::HashMap::new();
        if let Some(cls) = classifier_arc.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let app_state = Arc::new(AppState {
            persistence: Some(persistence::PersistenceConfig {
                backend,
                task_semaphore: semaphore,
            }),
            classifier: classifier_arc,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(
                intent_classifier::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            #[cfg(feature = "otel")]
            metrics: None,
        });
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    /// Build app state and router with a real Postgres pool for integration tests.
    /// Thin wrapper around `build_app_with_persistence_backend` that constructs
    /// a `PostgresBackend` from the pool. Kept for the 2 existing
    /// `persistence_integration_sse_streaming_*` tests that still want a
    /// real Postgres backend.
    pub(crate) fn build_app_with_persistence(
        pool: Arc<sqlx::PgPool>,
        semaphore: Arc<tokio::sync::Semaphore>,
        http_client: Option<reqwest::Client>,
    ) -> (Router, httpmock::MockServer) {
        let pg_backend = persistence::PostgresBackend {
            pool: (*pool).clone(),
        };
        let backend = Arc::new(persistence::DbBackend::Postgres(pg_backend));
        build_app_with_persistence_backend(backend, semaphore, http_client)
    }

    fn test_app_with_dead_endpoint(env_var_name: &str) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
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
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        routing.insert(
            cats[3].name.clone(),
            intent_classifier::RouteEntry {
                model: "ca-model".to_string(),
                endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            Some(client),
            intent_classifier::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_returns_response() {
        let env = "TEST_UPSTREAM_RESP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
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
        assert!(
            body.contains(r#""choices""#),
            "expected upstream response body, got: {body}"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_request_includes_auth_header() {
        let env = "TEST_UPSTREAM_AUTH";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
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
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_request_includes_content_type_json() {
        let env = "TEST_UPSTREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("Content-Type", "application/json");
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
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_unreachable_returns_502() {
        let env = "TEST_UPSTREAM_DEAD";
        let _guard = EnvGuard(env);
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
        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("error").and_then(|v| v.as_str()),
            Some("upstream_error"),
            "expected upstream_error contract, got: {json}"
        );
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_skip_classify_via_headers() {
        let env = "TEST_UPSTREAM_SKIP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
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
        // cleanup handled by EnvGuard
    }

    // ── SSE streaming tests ─────────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_returns_sse_content_type() {
        let env = "TEST_STREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let sse_body =
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_body);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Content-Type");
        assert_eq!(content_type, "text/event-stream");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Cache-Control");
        assert_eq!(cache_control, "no-cache");
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body.contains("data:"), "expected SSE data, got: {body}");
        assert!(
            body.contains("[DONE]"),
            "expected [DONE] marker, got: {body}"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_forwards_upstream_bytes() {
        let env = "TEST_STREAM_FWD";
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let sse_chunks = "data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\n\ndata: [DONE]\n\n";
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_chunks);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
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
            body.contains(r#"content":"A""#),
            "expected chunk A, got: {body}"
        );
        assert!(
            body.contains(r#"content":"B""#),
            "expected chunk B, got: {body}"
        );
        assert!(
            body.contains("[DONE]"),
            "expected [DONE] marker, got: {body}"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_non_2xx_returns_sse_error_event() {
        let env = "TEST_STREAM_ERR";
        let _env_guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":"overloaded"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.starts_with("event: error"),
            "expected SSE error event, got: {body}"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    // ── format_sse_error_event helper tests (Phase 3 — F2 helper invariants) ──
    // The helper applies the JSON-escape rule and emits the SSE event body.
    // These 6 unit tests cover plain text, each escape rule, and a combined
    // injection attempt that must still produce a valid JSON `data:` payload.

    #[test]
    fn test_format_sse_error_event_plain_text() {
        let s = format_sse_error_event("hello");
        assert_eq!(s, "event: error\ndata: {\"error\":\"hello\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_escapes_backslash() {
        let s = format_sse_error_event(r"a\b");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a\\\\b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_escapes_double_quote() {
        let s = format_sse_error_event("a\"b");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a\\\"b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_newline_with_space() {
        let s = format_sse_error_event("a\nb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_carriage_return_with_space() {
        let s = format_sse_error_event("a\rb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_combined_injection_produces_valid_json() {
        // Combined injection: ";\n}\nattack\n\r{ would break the SSE event
        // framing and the JSON payload if the escape rule were skipped.
        // The escape rule replaces \n and \r with single spaces.
        let s = format_sse_error_event("\";\n}\nattack\n\r{");
        let json_str = s
            .strip_prefix("event: error\ndata: ")
            .and_then(|s| s.strip_suffix("\n\n"))
            .expect("SSE event should have `event: error\\ndata: <json>\\n\\n` framing");
        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .expect("data: payload should be valid JSON even with injection chars");
        // " → \", \n → space, \r → space. After escape the string is
        // `"; ` + ` ` + `}` + ` ` + `attack` + ` ` + ` ` + `{`.
        // = `"; } attack  {` (one space after `}`, one space after `;`,
        // two spaces between `attack` and `{` from \n and \r).
        assert_eq!(parsed, serde_json::json!({"error": "\"; } attack  {"}));
    }

    #[test]
    fn test_format_sse_error_event_replaces_tab_with_space() {
        // \t is a C0 control char (0x09) that the helper now replaces
        // with a single space. Locks the F8 fix that extended the escape
        // rule from [\n, \r] to the full C0 range.
        let s = format_sse_error_event("a\tb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_backspace_with_space() {
        // \x08 is a C0 control char (0x08) that the helper now replaces
        // with a single space.
        let s = format_sse_error_event("a\x08\x08");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a  \"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_form_feed_with_space() {
        // \x0C is a C0 control char that the helper now replaces with
        // a single space.
        let s = format_sse_error_event("a\x0Cb");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_replaces_other_control_chars_with_space() {
        // \x01 and \x1F are at the C0 range extremes (both < 0x20 and
        // not \n, \r, \t, \b, \f). The helper must replace them with
        // a single space each, just like the named C0 chars.
        let s = format_sse_error_event("a\x01b\x1Fc");
        assert_eq!(s, "event: error\ndata: {\"error\":\"a b c\"}\n\n");
    }

    #[test]
    fn test_format_sse_error_event_preserves_printable_ascii() {
        // Sanity: chars >= 0x20 (printable ASCII) must pass through
        // unchanged. Catches a regression where the C0 replacement
        // accidentally widened its range.
        let s = format_sse_error_event("Hello, World! 123 ~`@#$%^&*()");
        assert_eq!(
            s,
            "event: error\ndata: {\"error\":\"Hello, World! 123 ~`@#$%^&*()\"}\n\n"
        );
    }

    // ── F2 integration tests (Phase 3 — handle_streaming_error 5 invariants) ──
    // These tests lock each of the 5 F2 invariants at the HTTP level. They
    // exercise the full axum stack via test_app_with_http_client and assert
    // on the response status, body, and headers.

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_truncates_oversized_body() {
        let env = "TEST_STREAM_TRUNC";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        // 3 KB body, > the 2 KB cap.
        let large_body = "x".repeat(3_000);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(large_body);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        // The SSE event body is bounded: at most 2 KB of upstream body +
        // ~50 bytes of format overhead (`event: error\ndata: {"error":"..."}\n\n`).
        assert!(
            body.len() <= 2 * 1024 + 64,
            "SSE error body should be bounded to ~2 KB + format overhead, got {} bytes",
            body.len()
        );
        assert!(
            body.starts_with("event: error"),
            "expected SSE error framing, got: {body}"
        );
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_escapes_json_injection() {
        let env = "TEST_STREAM_ESC";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        // Upstream body with all 4 JSON-unsafe chars: \, ", \n, \r.
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":"a\"b\\c\nd"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        let json_str = body
            .strip_prefix("event: error\ndata: ")
            .and_then(|s| s.strip_suffix("\n\n"))
            .expect("SSE event should have `event: error\\ndata: <json>\\n\\n` framing");
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect(
            "data: payload should be valid JSON even when upstream body has JSON-unsafe chars",
        );
        // The proxy embeds the raw upstream body in the SSE event (it
        // does NOT parse the body as JSON). The escape rule replaces
        // literal `\` with `\\` and `"` with `\"` so the data: payload
        // is valid JSON. The parsed `error` field is the JSON-decoded
        // value of the embedded raw body, which is the original raw
        // upstream body.
        let error_value = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .expect("error field should be a string");
        assert_eq!(
            error_value, r#"{"error":"a\"b\\c\nd"}"#,
            "raw upstream body should round-trip through the SSE escape rule"
        );
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_content_type_and_cache_control() {
        let env = "TEST_STREAM_CT";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(503)
                .header("content-type", "application/json")
                .body(r#"{"error":"overloaded"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "text/event-stream",
            "expected SSE content type"
        );
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(cache_control, "no-cache", "expected no-cache control");
        mock.assert();
    }

    async fn assert_status_passthrough(status: u16) {
        let env = "TEST_STREAM_ST";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(status)
                .header("content-type", "application/json")
                .body(r#"{"error":"upstream"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        let response_status = response.status().as_u16();
        assert_eq!(
            response_status, status,
            "expected upstream status {status} to be forwarded to client"
        );
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_429() {
        assert_status_passthrough(429).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_500() {
        assert_status_passthrough(500).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_502() {
        assert_status_passthrough(502).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_handler_error_status_passthrough_503() {
        assert_status_passthrough(503).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_true_returns_sse_content() {
        let env = "TEST_STREAM_TSSE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("data: hello\n\n");
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "text/event-stream",
            "expected SSE for stream:true"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_inline_mid_stream_error_uses_same_format() {
        // Trigger the inline mid-stream error branch in
        // `handle_streaming_response` (src/main.rs:790-800). The branch
        // fires when reqwest's byte stream returns `Some(Err(_e))` after
        // SSE headers have been sent. We engineer this by serving a
        // response whose `Content-Length: 1000` mismatches the bytes
        // actually written, then closing the socket — reqwest returns a
        // body-read error and the inline branch emits the same SSE error
        // event format as `handle_streaming_error`.
        //
        // This test must use a real TCP server (not httpmock) because
        // httpmock cannot simulate a mid-stream body error.
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let env = "TEST_INLINE_ERR";
        let _env_guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");

        let server_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            // Read the request (we don't care what's in it).
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            // Claim content-length: 1000 but write 10 bytes ("data: he")
            // then close — reqwest will error trying to read the
            // remaining 990 bytes the headers claimed.
            let headers = "HTTP/1.1 200 OK\r\n\
                           content-type: text/event-stream\r\n\
                           content-length: 1000\r\n\r\n";
            sock.write_all(headers.as_bytes()).await.expect("headers");
            sock.flush().await.expect("flush headers");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            sock.write_all(b"data: he").await.expect("first chunk");
            sock.flush().await.expect("flush first chunk");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // Close the socket abruptly — reqwest's next body read errors.
            drop(sock);
        });

        // Build an app that routes SYNTAX_FIX to the real TCP server.
        // Reuses the `make_test_app_state` helper from mod tests.
        let cats = test_categories();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("test reqwest client should build");
        let mut routing = std::collections::HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: url,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env.to_string()),
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let app_state = make_test_app_state(
            regex_classifier,
            Some(client),
            intent_classifier::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let app = build_app(auth_config, app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");

        // The proxy returns 200 to the client even when the upstream
        // errors mid-stream (the response body contains the SSE error
        // event, not a 5xx).
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "proxy should return 200 to client on mid-stream upstream error"
        );
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");

        // The first chunk ("data: he") should be forwarded before the
        // error branch fires. The inline branch then emits the same
        // SSE error event format as handle_streaming_error.
        assert!(
            body.starts_with("data: he"),
            "expected the upstream's first chunk to be forwarded before the error, got: {body:?}"
        );
        assert!(
            body.contains("event: error\ndata: {\"error\":"),
            "expected the inline branch to emit an SSE error event matching handle_streaming_error's format, got: {body:?}"
        );
        // The error data: payload must be parseable JSON (the helper's
        // invariant 2). Parse the data: line and confirm it's a single
        // object with an "error" string field.
        let data_line = body
            .split('\n')
            .find(|line| line.starts_with("data: ") && line.contains("\"error\""))
            .expect("expected an SSE data: line with the error event");
        let json_str = data_line.trim_start_matches("data: ");
        let parsed: serde_json::Value =
            serde_json::from_str(json_str).expect("SSE error data: must be valid JSON");
        assert!(
            parsed.get("error").and_then(|v| v.as_str()).is_some(),
            "SSE error data: payload must contain an 'error' string field, got: {parsed}"
        );

        // Wait for the server task to finish cleanly (it drops the
        // socket on its own once the response is complete).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_false_returns_buffered_json() {
        let env = "TEST_STREAM_FJSON";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"buffered"}}]}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}],"stream":false}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "application/json",
            "expected JSON for stream:false"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_absent_returns_buffered_json() {
        let env = "TEST_STREAM_AJSON";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"default"}}]}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hello"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "application/json",
            "expected JSON for absent stream field"
        );
        mock.assert();
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    async fn test_streaming_degradation_no_client() {
        // test_app() has http_client: None → classification-only degradation path
        // Even with stream: true, should return classification JSON
        let app = test_app_with_classifier();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
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
            body.contains(r#""status":"classified""#),
            "expected classification JSON, got: {body}"
        );
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
        assert!(
            content_type.starts_with("text/html"),
            "expected HTML response"
        );
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
        assert!(
            content_type.starts_with("text/html"),
            "expected HTML response"
        );
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
        assert!(
            body.contains("Database not configured"),
            "expected 'Database not configured' in response, got: {body}"
        );
    }

    // ── JSON contract shape tests (Phase 5, F4) ─────────────────────────────
    //
    // The endpoint tests above verify "what happens for a given request".
    // These tests verify the SHAPE of the JSON contract itself so that any
    // accidental change to a key name or value type — even one that would
    // happen to pass a substring assertion — is caught loudly.

    /// `classification_only_json` must emit exactly 4 keys with the right types.
    #[test]
    fn test_classification_only_json_contract_has_4_keys() {
        let result = intent_classifier::ClassificationResult {
            category: "SYNTAX_FIX".to_string(),
            model: "sf-model".to_string(),
            endpoint: "https://test.endpoint".to_string(),
            tier: intent_classifier::ClassificationTier::Regex,
            provider_type: "test_provider".to_string(),
            api_key_env: Some("TEST_API_KEY".to_string()),
        };
        let json: serde_json::Value = serde_json::from_str(&classification_only_json(&result))
            .expect("classification_only_json output should be valid JSON");

        let obj = json
            .as_object()
            .expect("classification_only_json output should be a JSON object");
        assert_eq!(
            obj.len(),
            4,
            "classification_only_json must emit exactly 4 keys, got: {obj:?}"
        );
        assert_eq!(obj.get("status"), Some(&serde_json::json!("classified")));
        assert_eq!(obj.get("category"), Some(&serde_json::json!("SYNTAX_FIX")));
        assert_eq!(obj.get("model"), Some(&serde_json::json!("sf-model")));
        assert_eq!(obj.get("tier"), Some(&serde_json::json!("Regex")));
    }

    /// `upstream_error_json` must emit exactly 3 keys with `status` as a number.
    /// This guards against an accidental change like `status: status.to_string()`
    /// turning the status code into a string.
    #[test]
    fn test_upstream_error_json_contract_has_3_keys() {
        let json: serde_json::Value =
            serde_json::from_str(&upstream_error_json(502_u16, "upstream response too large"))
                .expect("upstream_error_json output should be valid JSON");

        let obj = json
            .as_object()
            .expect("upstream_error_json output should be a JSON object");
        assert_eq!(
            obj.len(),
            3,
            "upstream_error_json must emit exactly 3 keys, got: {obj:?}"
        );
        assert_eq!(obj.get("error"), Some(&serde_json::json!("upstream_error")));
        // Crucial: status must be a number, not a string. If a future refactor
        // does `status: status.to_string()` the contract regresses silently.
        assert_eq!(
            obj.get("status"),
            Some(&serde_json::json!(502)),
            "status must be a JSON number (not a string) so clients can branch on the code"
        );
        assert_eq!(
            obj.get("message"),
            Some(&serde_json::json!("upstream response too large"))
        );
    }

    /// `json_response` must set `Content-Type: application/json` so clients
    /// can use `response.json()` without sniffing the body.
    #[test]
    fn test_json_response_sets_application_json_content_type() {
        let resp = json_response(StatusCode::CREATED, "{}".to_string());
        assert_eq!(resp.status(), StatusCode::CREATED);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("json_response must set Content-Type");
        assert_eq!(
            ct, "application/json",
            "json_response must advertise application/json so fetch().json() works"
        );
    }

    /// `classification_only_json` must serialize every real `ClassificationTier`
    /// variant. The current production code uses `format!("{:?}", tier)` for the
    /// value, which couples the JSON contract to the Rust Debug output. This
    /// test pins the exact strings so a rename of any variant breaks the test
    /// loudly.
    #[test]
    fn test_classification_only_json_serializes_all_3_tiers() {
        let tiers = [
            (intent_classifier::ClassificationTier::Regex, "Regex"),
            (intent_classifier::ClassificationTier::FewShot, "FewShot"),
            (intent_classifier::ClassificationTier::Fallback, "Fallback"),
        ];
        for (tier, expected_label) in tiers {
            let result = intent_classifier::ClassificationResult {
                category: "SYNTAX_FIX".to_string(),
                model: "sf-model".to_string(),
                endpoint: "https://test.endpoint".to_string(),
                tier,
                provider_type: "test_provider".to_string(),
                api_key_env: Some("TEST_API_KEY".to_string()),
            };
            let json: serde_json::Value = serde_json::from_str(&classification_only_json(&result))
                .expect("classification_only_json output should be valid JSON");
            assert_eq!(
                json.get("tier").and_then(|v| v.as_str()),
                Some(expected_label),
                "tier {tier:?} should serialize as {expected_label:?}"
            );
        }
    }

    // ── --init template tests (Phase 2) ──
    // run_init writes the embedded template to a path or prints it to stdout.
    // We test the file-writing path directly; the stdout path is exercised by
    // the binary's CLI (see manual verification) and by INIT_TEMPLATE's own
    // content assertions below.

    /// Each test gets its own scratch directory under the OS temp dir to keep
    /// parallel runs and CI reruns from clobbering each other.
    fn init_scratch(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cerebrum-init-{label}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir should be creatable");
        dir
    }

    #[test]
    fn init_template_contains_all_five_routing_sections() {
        for section in [
            "[routing.DEFAULT]",
            "[routing.FILE_READING]",
            "[routing.SYNTAX_FIX]",
            "[routing.COMPLEX_REASONING]",
            "[routing.CASUAL]",
        ] {
            assert!(
                INIT_TEMPLATE.contains(section),
                "init template should contain section {section}, got:\n{INIT_TEMPLATE}"
            );
        }
    }

    #[test]
    fn init_template_parses_as_valid_toml_syntax() {
        // Placeholders like "<your-model>" are not valid for ConfigRoot (the
        // schema requires a non-empty provider_type), but they ARE valid TOML
        // syntax. Verify the syntax layer at least.
        let value: toml::Value =
            toml::from_str(INIT_TEMPLATE).expect("init template should be valid TOML syntax");
        let table = value
            .as_table()
            .expect("init template should be a top-level TOML table");
        let routing = table
            .get("routing")
            .and_then(|v| v.as_table())
            .expect("init template should have a [routing] table");
        assert_eq!(
            routing.len(),
            5,
            "init template should declare exactly 5 routing entries, got: {routing:?}"
        );
    }

    #[test]
    fn run_init_writes_template_to_new_file() {
        let dir = init_scratch("write");
        let path = dir.join("cerebrum.toml");
        run_init(Some(path.to_str().unwrap()), false).expect("write should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_refuses_to_overwrite_existing_file() {
        let dir = init_scratch("refuse");
        let path = dir.join("cerebrum.toml");
        std::fs::write(&path, "preexisting content").expect("seed write should succeed");
        let err = run_init(Some(path.to_str().unwrap()), false)
            .expect_err("overwrite must be refused without --force");
        assert!(
            err.contains("refusing to overwrite"),
            "error should mention the refusal, got: {err}"
        );
        // Original content must be untouched.
        let still = std::fs::read_to_string(&path).expect("file should still be readable");
        assert_eq!(still, "preexisting content");
    }

    #[test]
    fn run_init_force_overwrites_existing_file() {
        let dir = init_scratch("force");
        let path = dir.join("cerebrum.toml");
        std::fs::write(&path, "preexisting content").expect("seed write should succeed");
        run_init(Some(path.to_str().unwrap()), true).expect("force overwrite should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_creates_missing_parent_directories() {
        let dir = init_scratch("mkdir");
        let nested = dir.join("a").join("b").join("cerebrum.toml");
        run_init(Some(nested.to_str().unwrap()), false).expect("nested write should succeed");
        assert!(nested.exists(), "file should exist at nested path");
        let content = std::fs::read_to_string(&nested).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }
}

#[cfg(test)]
mod slow_tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;

    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    // ── Keepalive test ──────────────────────────────────────────────────────
    // Uses a real TCP server that sends headers immediately, waits for the
    // keepalive interval, then sends body data. KEEPALIVE_INTERVAL_SECS=1
    // keeps total test time around 2s instead of 17s.

    async fn spawn_slow_sse_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let body = "data: hello\n\n";
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        (url, handle)
    }

    /// Build an axum test app that routes the SYNTAX_FIX category to the
    /// given upstream URL, with a 1s keepalive interval. Used by all 4
    /// keepalive slow tests (1 existing + 3 new) so the app wiring is
    /// defined in one place.
    fn build_keepalive_app(url: String, env_var: &'static str) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let cats = test_categories();
        let mut routing = std::collections::HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            intent_classifier::RouteEntry {
                model: "sf-model".to_string(),
                endpoint: url,
                cost_per_1m_input_tokens: None,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var.to_string()),
            },
        );
        let fallback = intent_classifier::RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        let regex_classifier = intent_classifier::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let model_costs = intent_classifier::ModelCosts::empty();
        let baseline_model = String::new();
        let classifier_chain =
            intent_classifier::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier = Some(Arc::new(classifier_chain));
        let mut merged_routing = HashMap::new();
        if let Some(cls) = classifier.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
            baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(1)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            #[cfg(feature = "otel")]
            metrics: None,
        });
        build_app(auth_config, app_state)
    }

    /// Count SSE keepalive comments in a body, anchored to line start.
    /// A regression to `data: keepalive\n\n` (a regular SSE event) would
    /// not match because the substring `data:` precedes `: keepalive`.
    /// The body may also start with `: keepalive\n\n` (no preceding
    /// newline), so we count start-of-body matches separately. We split
    /// on `\n` and count lines that are exactly `: keepalive` — this
    /// correctly handles consecutive keepalives (which would otherwise
    /// be missed by `str::matches` due to its non-overlapping behavior).
    fn count_anchored_keepalives(body: &str) -> usize {
        body.split('\n')
            .filter(|line| *line == ": keepalive")
            .count()
    }

    /// Fast upstream: sends `data: hello\n\n` within 100ms of headers
    /// (well below the 1s keepalive interval). The proxy must NOT inject
    /// a keepalive because the upstream data arrives first.
    async fn spawn_fast_sse_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let body = "data: hello\n\n";
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
        });
        (url, handle)
    }

    /// Chunk-then-idle upstream: sends `data: chunk1\n\n`, idles 1500ms
    /// (longer than the 1s keepalive interval), then sends
    /// `data: chunk2\n\n`. The proxy must emit at least one keepalive
    /// between the two chunks.
    async fn spawn_chunk_then_idle_sse_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.flush().await;
            // First chunk
            let _ = sock.write_all(b"data: chunk1\n\n").await;
            let _ = sock.flush().await;
            // Idle 1500ms — keepalive should fire at 1s
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            // Second chunk
            let _ = sock.write_all(b"data: chunk2\n\n").await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        (url, handle)
    }

    /// Long-stall upstream: idles 3500ms (≥ 3 keepalive intervals at 1s)
    /// then sends `data: hello\n\n`. The proxy must emit ≥ 3 keepalives
    /// to prove the keepalive loop is sustained over multiple intervals.
    async fn spawn_long_stall_sse_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1/chat/completions");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let headers = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.flush().await;
            // Idle 3500ms — should produce 3 keepalives at 1s, 2s, 3s.
            tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
            let body = "data: hello\n\n";
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.flush().await;
        });
        (url, handle)
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_keepalive_injected() {
        let (url, server_handle) = spawn_slow_sse_server().await;
        let env = "TEST_STREAM_KA_SLOW";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let app = build_keepalive_app(url, env);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            content_type, "text/event-stream",
            "expected SSE content type"
        );
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        // Tightened assertion: anchor the keepalive substring to line start.
        // A regression to `data: keepalive\n\n` (a regular SSE event with
        // name "keepalive") would NOT match because `data:` precedes `: keepalive`.
        assert!(
            count_anchored_keepalives(body) >= 1,
            "expected ≥ 1 anchored keepalive comment, got: {body}"
        );
        assert!(
            body.contains("data: hello"),
            "expected upstream data after keepalive, got: {body}"
        );
        let _ = server_handle.await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_keepalive_not_injected_when_upstream_fast() {
        let (url, server_handle) = spawn_fast_sse_server().await;
        let env = "TEST_STREAM_KA_FAST";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let app = build_keepalive_app(url, env);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
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
        // Fast upstream (< 100ms, well under the 1s keepalive interval) must
        // NOT trigger a keepalive. The proxy forwards the data immediately.
        assert_eq!(
            count_anchored_keepalives(body),
            0,
            "fast upstream should NOT inject keepalive, got: {body}"
        );
        assert!(
            body.contains("data: hello"),
            "expected upstream data to be forwarded, got: {body}"
        );
        let _ = server_handle.await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_keepalive_injected_alongside_chunk() {
        let (url, server_handle) = spawn_chunk_then_idle_sse_server().await;
        let env = "TEST_STREAM_KA_CHUNK";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let app = build_keepalive_app(url, env);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
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
        // Both chunks must be forwarded, AND at least one keepalive must
        // have fired between them (1500ms idle > 1s interval).
        assert!(
            body.contains("data: chunk1"),
            "expected first chunk, got: {body}"
        );
        assert!(
            body.contains("data: chunk2"),
            "expected second chunk, got: {body}"
        );
        assert!(
            count_anchored_keepalives(body) >= 1,
            "expected ≥ 1 keepalive between chunks, got: {body}"
        );
        let _ = server_handle.await;
    }

    #[tokio::test]
    #[serial]
    async fn test_streaming_keepalive_multiple_consecutive() {
        let (url, server_handle) = spawn_long_stall_sse_server().await;
        let env = "TEST_STREAM_KA_LONG";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let app = build_keepalive_app(url, env);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
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
        // 3500ms idle ≥ 3 keepalive intervals at 1s. The keepalive loop
        // is sustained across multiple intervals.
        assert!(
            count_anchored_keepalives(body) >= 3,
            "expected ≥ 3 keepalives during 3.5s stall, got: {body}"
        );
        assert!(
            body.contains("data: hello"),
            "expected upstream data after keepalives, got: {body}"
        );
        let _ = server_handle.await;
    }

    #[tokio::test]
    #[serial]
    async fn test_graceful_shutdown() {
        use std::time::Duration;
        use tokio::sync::oneshot;
        let app = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                "OK"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            shutdown_rx.await.ok();
        });
        let server_task = tokio::spawn(async move {
            server.await.expect("server task");
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{}/slow", addr))
            .send()
            .await
            .unwrap();
        shutdown_tx.send(()).unwrap();
        let body = resp.text().await.unwrap();
        assert_eq!(body, "OK");
        server_task.await.unwrap();
    }
}
