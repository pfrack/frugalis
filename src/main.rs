use std::collections::HashMap;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    http::{header, HeaderValue, Method},
    routing::{get, post},
    Router,
};
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::{info, warn, Subscriber};
use tracing_subscriber::{fmt, layer::Layer, prelude::*, EnvFilter, Registry};

#[cfg(feature = "otel")]
mod telemetry;

mod auth;
mod cache;
mod classification;
mod config;
mod dashboard;
mod persistence;
mod protocol;
mod proxy;
mod quickstart;

#[cfg(test)]
mod test_util;

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<classification::chain::ClassifierChain>>,
    fewshot_classifier: Option<Arc<classification::fewshot::FewShotClassifier>>,
    routing:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, config::routing::RouteEntry>>>,
    model_costs: Arc<tokio::sync::RwLock<config::routing::ModelCosts>>,
    baseline_model: Arc<tokio::sync::RwLock<String>>,
    classify_db_log: Arc<std::sync::atomic::AtomicBool>,
    http_client: Option<reqwest::Client>,
    max_upstream_body_bytes: Arc<tokio::sync::RwLock<usize>>,
    keepalive_interval_secs: Arc<tokio::sync::RwLock<u64>>,
    request_body_limit_bytes: usize,
    streaming_channel_capacity: usize,
    dashboard_config: config::types::DashboardConfig,
    auth_providers: Arc<Vec<config::types::AuthProviderConfig>>,
    allowed_origins: Arc<RwLock<Vec<String>>>,
    response_cache: Option<Arc<cache::ResponseCache>>,
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
            // flags (e.g. `frugalis --init --validate` would otherwise drop
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
                        format!(
                            "refusing to overwrite existing file: {p} (use --force to overwrite)"
                        )
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
frugalis — intent-aware routing gateway

USAGE:
    frugalis [OPTIONS]

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

pub(crate) fn format_sse_error_event(error_msg: &str) -> String {
    let mut escaped = String::with_capacity(error_msg.len() * 2);
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
fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    // Unauthenticated v1 routes — model discovery must be accessible
    // without auth (Claude Code probes /v1/models before authenticating).
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
fn test_categories() -> Vec<classification::types::CategoryConfig> {
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

#[cfg(test)]
fn test_negative_patterns() -> Vec<classification::types::NegativePatternConfig> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::chain::IntentClassify;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;
    use super::test_util::EnvGuard;

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

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let fewshot_config = config::types::FewShotConfig {
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
        let fewshot = classification::fewshot::FewShotClassifier::new(
            fewshot_config,
            HashMap::new(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "fallback-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );

        let chain = classification::chain::ClassifierChain::new(vec![
            Arc::new(regex_classifier),
            Arc::new(fewshot),
        ]);

        // Regex should catch "fix this bug"
        let result = chain.classify("fix this bug").await;
        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, classification::types::ClassificationTier::Regex);

        // Regex returns Fallback on non-matching prompt, few-shot catches bootstrap text
        let result = chain.classify("can you explain what a hash map is").await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, classification::types::ClassificationTier::FewShot);
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
        use classification::chain::test_util::CountingClassifier;
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
            "CASUAL".to_string(),
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

        // CountingClassifier for the fewshot tier — always returns Fallback.
        // Forces the chain to escalate past fewshot into the LLM tier.
        let fewshot_counter = Arc::new(AtomicUsize::new(0));
        let fewshot_stub = CountingClassifier {
            counter: fewshot_counter.clone(),
            result: classification::types::ClassificationResult::fallback(),
        };

        let llm_config = config::types::LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };
        let llm = classification::llm::LLMClassifier::new(
            llm_config,
            reqwest::Client::new(),
            cats_for_llm,
            Arc::new(vec![]),
        );

        let chain = classification::chain::ClassifierChain::new(vec![
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
        assert_eq!(result.tier, classification::types::ClassificationTier::Regex);
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: "https://test.endpoint".to_string(),
                    provider_type: provider_type_val.to_string(),
                    api_key_env: api_key_env_val.map(|s| s.to_string()),
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
    async fn test_models_endpoint_returns_valid_json_no_auth() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("models request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("response should have Content-Type");
        assert!(
            content_type.starts_with("application/json"),
            "expected application/json, got {content_type}"
        );
        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("object").and_then(|v| v.as_str()),
            Some("list"),
            "expected object=list"
        );
        assert_eq!(
            json.get("has_more").and_then(|v| v.as_bool()),
            Some(false),
            "expected has_more=false"
        );
        let data = json
            .get("data")
            .and_then(|v| v.as_array())
            .expect("data should be an array");
        assert_eq!(data.len(), 3, "expected 3 models, got {}", data.len());
        let model_ids: Vec<&str> = data
            .iter()
            .map(|m| m.get("id").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(
            model_ids.contains(&"claude-sonnet-4-6-20250514"),
            "should contain claude-sonnet-4-6-20250514"
        );
        assert!(
            model_ids.contains(&"claude-haiku-4-5-20250514"),
            "should contain claude-haiku-4-5-20250514"
        );
        assert!(
            model_ids.contains(&"claude-opus-4-20250514"),
            "should contain claude-opus-4-20250514"
        );
        for model in data {
            assert_eq!(
                model.get("object").and_then(|v| v.as_str()),
                Some("model"),
                "each model should have object=model"
            );
            assert_eq!(
                model.get("owned_by").and_then(|v| v.as_str()),
                Some("anthropic"),
                "each model should be owned_by anthropic"
            );
        }
    }

    #[tokio::test]
    async fn test_models_endpoint_entries_have_display_name_and_prefixed_id() {
        // Claude Code's gateway discovery requires `display_name` for friendly
        // names and filters entries whose IDs do not begin with `claude` or
        // `anthropic`. This locks both invariants independently of the
        // broader shape test above.
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .expect("request should be valid"),
            )
            .await
            .expect("models request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let data = json
            .get("data")
            .and_then(|v| v.as_array())
            .expect("data should be an array");
        assert!(!data.is_empty(), "data should not be empty");
        for model in data {
            let id = model
                .get("id")
                .and_then(|v| v.as_str())
                .expect("each entry must have an id");
            assert!(
                id.starts_with("claude") || id.starts_with("anthropic"),
                "id must be claude/anthropic-prefixed for Claude Code discovery, got {id}"
            );
            let display_name = model
                .get("display_name")
                .and_then(|v| v.as_str())
                .expect("each entry must have a display_name");
            assert!(
                !display_name.is_empty(),
                "display_name must be non-empty for id {id}"
            );
            assert_eq!(
                model.get("type").and_then(|v| v.as_str()),
                Some("model"),
                "each Anthropic-shape entry should have type=model for id {id}"
            );
        }
    }

    #[test]
    fn test_sanitize_for_nim_strips_unsupported_fields() {
        let mut body = serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "top_k": 40,
            "metadata": {"key": "value"},
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "stream": true
        });
        proxy::util::sanitize_for_nim(&mut body);
        assert!(body.get("top_k").is_none(), "top_k should be stripped");
        assert!(
            body.get("metadata").is_none(),
            "metadata should be stripped"
        );
        assert!(
            body.get("thinking").is_none(),
            "thinking should be stripped"
        );
        assert!(body.get("model").is_some(), "model should be preserved");
        assert!(
            body.get("messages").is_some(),
            "messages should be preserved"
        );
        assert!(body.get("stream").is_some(), "stream should be preserved");
    }

    #[tokio::test]
    async fn test_count_tokens_returns_estimated_tokens() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hello world"}
            ]
        });
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .expect("request should be valid"),
            )
            .await
            .expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .expect("input_tokens should be a number");
        // "hello world" = 11 chars → 11 / 4 = 2
        assert_eq!(tokens, 2, "expected 2 tokens for 'hello world'");
    }

    #[tokio::test]
    async fn test_count_tokens_array_content_blocks() {
        let body = serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "hello world test"}
                    ]
                }
            ]
        });
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .expect("request should be valid"),
            )
            .await
            .expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .expect("input_tokens should be a number");
        // "hello world test" = 16 chars → 16 / 4 = 4
        assert_eq!(tokens, 4, "expected 4 tokens for 'hello world test'");
    }

    #[tokio::test]
    async fn test_count_tokens_empty_messages() {
        let body = serde_json::json!({"messages": []});
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages/count_tokens")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .expect("request should be valid"),
            )
            .await
            .expect("count_tokens request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let tokens = json
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .expect("input_tokens should be a number");
        assert_eq!(tokens, 0, "expected 0 tokens for empty messages");
    }

    #[test]
    fn test_optimize_empty_messages_returns_canned_response() {
        let body = serde_json::json!({"messages": []}).to_string().into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(resp.is_some(), "empty messages should be optimized");
        let resp = resp.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn test_optimize_hello_probe_openai_format() {
        let body = serde_json::json!({
            "model": "test",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(resp.is_some(), "'hello' probe should be optimized");
    }

    #[test]
    fn test_optimize_hello_probe_anthropic_format() {
        let body = serde_json::json!({
            "anthropic_version": "2023-06-01",
            "model": "test",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(
            resp.is_some(),
            "anthropic 'hello' probe should be optimized"
        );
    }

    #[test]
    fn test_optimize_normal_request_not_matched() {
        let body = serde_json::json!({
            "model": "test",
            "messages": [{"role": "user", "content": "explain quantum computing in detail"}]
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(resp.is_none(), "normal request should not be optimized");
    }

    #[test]
    fn test_optimize_array_content_hello() {
        let body = serde_json::json!({
            "model": "test",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hi"}]
            }]
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(
            resp.is_some(),
            "array content 'hi' probe should be optimized"
        );
    }

    #[test]
    fn test_optimize_missing_messages_not_matched() {
        let body = serde_json::json!({"model": "test"})
            .to_string()
            .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(resp.is_none(), "missing messages should not be optimized");
    }

    #[test]
    fn test_optimize_stream_true_not_matched() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, false);
        assert!(resp.is_none(), "streaming requests should not be optimized");
    }

    #[test]
    fn test_optimize_probe_returns_anthropic_format() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6-20250514",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, true).expect("should match probe pattern");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn test_optimize_empty_messages_returns_anthropic_format() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6-20250514",
            "messages": []
        })
        .to_string()
        .into_bytes();
        let resp = proxy::util::try_optimize_request(&body, true).expect("should match empty messages");
        assert_eq!(resp.status(), StatusCode::OK);
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
        let pool = match persistence::backend::test_pool().await {
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
        let pool = match persistence::backend::test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP persistence_integration_insert_and_read_back: DATABASE_URL not set or unreachable");
                return;
            }
        };
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = persistence::postgres::PostgresBackend {
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
            provider_attempts: 1,
            final_provider: "test-model".to_string(),
            // Phase 4 token usage + Claude Code attribution fields.
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_read_tokens: Some(80),
            cache_creation_tokens: Some(5),
            client_session_id: Some("sess-integration".to_string()),
        };
        let handle = persistence::log_inference(db_backend, semaphore, record);
        handle.await.expect("logging task should complete");

        // Read back using non-macro query (no offline cache required).
        let row =
            sqlx::query("SELECT status, prompt_snippet, prompt_char_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, client_session_id FROM inferences WHERE request_id = $1")
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
        // Phase 4 token/attribution columns round-trip through Postgres.
        assert_eq!(
            row.try_get::<Option<i32>, _>("input_tokens").unwrap(),
            Some(100)
        );
        assert_eq!(
            row.try_get::<Option<i32>, _>("output_tokens").unwrap(),
            Some(20)
        );
        assert_eq!(
            row.try_get::<Option<i32>, _>("cache_read_tokens").unwrap(),
            Some(80),
            "cache_read_tokens must round-trip"
        );
        assert_eq!(
            row.try_get::<Option<i32>, _>("cache_creation_tokens")
                .unwrap(),
            Some(5)
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("client_session_id")
                .unwrap()
                .as_deref(),
            Some("sess-integration"),
            "client_session_id must round-trip"
        );
    }

    /// Integration test: verifies that a successful SSE streaming request
    /// produces exactly two inference records with statuses "streaming" and "ok".
    /// Requires DATABASE_URL to be set; skips gracefully otherwise.
    #[tokio::test]
    #[serial]
    async fn persistence_integration_sse_streaming_success() {
        let pool = match persistence::backend::test_pool().await {
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
        let pool = match persistence::backend::test_pool().await {
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
        let memory_backend = persistence::memory::MemoryBackend::new();
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
        let memory_backend = persistence::memory::MemoryBackend::new();
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
        let memory_backend = persistence::memory::MemoryBackend::new();
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
        // Poll with a 2s timeout instead of a fixed sleep to reduce flakiness.
        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(2);
        loop {
            match backend.as_ref() {
                persistence::DbBackend::Memory(mb) => {
                    if !mb.fail_next.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                }
                _ => panic!("test fixture invariant: backend must be DbBackend::Memory"),
            }
            if poll_start.elapsed() >= poll_timeout {
                panic!("log task did not consume fail_next within 2s; test setup failed");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

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
            body.contains("Frugalis Dashboard"),
            "body should contain 'Frugalis Dashboard'"
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
                    endpoint,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
            Some(client),
            config::routing::ModelCosts::empty(),
            String::new(),
            max_upstream_body_bytes,
        );
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    /// Anthropic-flavored variant of `test_app_with_http_client`. Routes the
    /// mock at `/v1/messages` and tags the route with `provider_type: "anthropic"`
    /// so the proxy emits `x-api-key` + `anthropic-version` headers instead
    /// of `Authorization: Bearer …`. The mock assertions in the tests below
    /// rely on this header contract.
    pub(crate) fn test_app_with_anthropic_http_client(
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
        let endpoint = server.url("/v1/messages");
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "anthropic".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
                    endpoint,
                    provider_type: "anthropic".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
            Some(client),
            config::routing::ModelCosts::empty(),
            String::new(),
            max_upstream_body_bytes,
        );
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    // ── /v1/messages (Anthropic pass-through) integration tests ──────────────

    #[tokio::test]
    async fn test_messages_handler_requires_auth() {
        // Auth must fail before any handler logic runs — same contract as
        // /v1/chat/completions and /v1/feedback, covered by the proxy_auth_layer.
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_non_streaming_passthrough() {
        let env = "TEST_ANTHROPIC_NS";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .header("anthropic-version", "2023-06-01");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}]}"#,
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body_str.contains("hello"),
            "expected upstream text in response, got: {body_str}"
        );
        assert!(
            body_str.contains("msg_1"),
            "expected upstream id in response, got: {body_str}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_forwards_anthropic_client_headers() {
        // Claude Code pairs each anthropic-beta capability with an
        // anthropic-version + x-claude-code-* attribution header. Routed to an
        // Anthropic upstream, Frugalis must forward them unchanged, prefer the
        // client's anthropic-version over the 2023-06-01 default, and still
        // apply the resolved x-api-key credential. The mock matches only if
        // every forwarded header is present with the expected value.
        let env = "TEST_ANTHROPIC_FWD";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .header("anthropic-version", "2024-10-22")
                .header("anthropic-beta", "context-management-2025-09")
                .header("x-claude-code-session-id", "sess-123");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("anthropic-version", "2024-10-22")
                    .header("anthropic-beta", "context-management-2025-09")
                    .header("x-claude-code-session-id", "sess-123")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_does_not_forward_anthropic_headers_to_openai() {
        // anthropic-* headers are meaningless to an OpenAI-compatible upstream
        // and must be dropped on cross-protocol routing. httpmock 0.7 has no
        // header-absence matcher and exposes no request inspection, so we use
        // FIFO "canary" mocks registered BEFORE the serving mock: a canary
        // matches ONLY if its header_exists criterion is satisfied, so it gets
        // hit iff that header leaked to the upstream. In the correct case the
        // canaries never match and the serving mock (Authorization only) wins.
        let env = "TEST_OPENAI_NO_FWD";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env, 10_485_760);
        let beta_canary = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header_exists("anthropic-beta");
            then.status(200).body("canary-beta");
        });
        let version_canary = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header_exists("anthropic-version");
            then.status(200).body("canary-version");
        });
        let positive = server.mock(|when, then| {
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
                    .header("anthropic-version", "2024-10-22")
                    .header("anthropic-beta", "context-management-2025-09")
                    .header("x-claude-code-session-id", "sess-123")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            beta_canary.hits(),
            0,
            "anthropic-beta must NOT be forwarded to an OpenAI-compatible upstream"
        );
        assert_eq!(
            version_canary.hits(),
            0,
            "anthropic-version must NOT be forwarded to an OpenAI-compatible upstream"
        );
        assert_eq!(
            positive.hits(),
            1,
            "request must still reach the upstream with the resolved Authorization credential"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_streaming_passthrough() {
        let env = "TEST_ANTHROPIC_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n");
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"stream":true,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "expected text/event-stream content type"
        );
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body_str.contains("message_start"),
            "expected upstream SSE bytes forwarded, got: {body_str}"
        );
        assert!(
            body_str.contains("content_block_delta"),
            "expected second SSE event forwarded, got: {body_str}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_upstream_error_forwards_body() {
        let env = "TEST_ANTHROPIC_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();
        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("error"),
            "expected upstream Anthropic error envelope, got: {json}"
        );
    }

    #[tokio::test]
    async fn test_messages_handler_classification_only_when_no_http_client() {
        // No http_client configured → proxy returns classification JSON
        // instead of attempting an upstream call (parity with /v1/chat/completions).
        let response = test_app_with_classifier()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("classified"),
            "expected classified status, got: {json}"
        );
        assert_eq!(
            json.get("category").and_then(|v| v.as_str()),
            Some("SYNTAX_FIX"),
            "expected SYNTAX_FIX category from 'fix this bug', got: {json}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_overrides_model_to_classifier_choice() {
        let env = "TEST_ANTHROPIC_MODEL";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        // Mock verifies the body has the classifier-selected "sf-model" (from
        // SYNTAX_FIX routing), NOT the client's "claude-3.5". Mock only fires
        // when the body_contains matcher passes.
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .body_contains("\"model\":\"sf-model\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"msg_1","type":"message"}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some("MOCK_API_KEY".to_string()),
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
                    endpoint,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some("MOCK_API_KEY".to_string()),
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
        let classifier_chain =
            classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
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
                config::routing::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
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
        let pg_backend = persistence::postgres::PostgresBackend {
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
                    endpoint: "http://127.0.0.1:1/v1/chat/completions".to_string(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
            Some(client),
            config::routing::ModelCosts::empty(),
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
                    .header("x-frugalis-category", "SYNTAX_FIX")
                    .header("x-frugalis-model", "gpt-4o-mini")
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: url,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env.to_string()),
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
            Some(client),
            config::routing::ModelCosts::empty(),
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

        // Wait for the server task to finish; propagate panics or timeouts.
        match tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await {
            Ok(Ok(())) => {} // task completed cleanly
            Ok(Err(e)) => panic!("server task panicked: {e:?}"),
            Err(_) => panic!("server task did not complete within 2s"),
        }
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
                        r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":false}"#,
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
                        r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#,
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
        let result = classification::types::ClassificationResult {
            category: "SYNTAX_FIX".to_string(),
            model: "sf-model".to_string(),
            tier: classification::types::ClassificationTier::Regex,
            providers: vec![],
        };
        let json: serde_json::Value = serde_json::from_str(&proxy::util::classification_only_json(&result))
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
            serde_json::from_str(&proxy::util::upstream_error_json(502_u16, "upstream response too large"))
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
        let resp = proxy::util::json_response(StatusCode::CREATED, "{}".to_string());
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
            (classification::types::ClassificationTier::Regex, "Regex"),
            (classification::types::ClassificationTier::FewShot, "FewShot"),
            (classification::types::ClassificationTier::Fallback, "Fallback"),
        ];
        for (tier, expected_label) in tiers {
            let result = classification::types::ClassificationResult {
                category: "SYNTAX_FIX".to_string(),
                model: "sf-model".to_string(),
                tier,
                providers: vec![],
            };
            let json: serde_json::Value = serde_json::from_str(&proxy::util::classification_only_json(&result))
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
        let dir = std::env::temp_dir().join(format!("frugalis-init-{label}-{nanos}"));
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
        let path = dir.join("frugalis.toml");
        run_init(Some(path.to_str().unwrap()), false).expect("write should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_refuses_to_overwrite_existing_file() {
        let dir = init_scratch("refuse");
        let path = dir.join("frugalis.toml");
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
        let path = dir.join("frugalis.toml");
        std::fs::write(&path, "preexisting content").expect("seed write should succeed");
        run_init(Some(path.to_str().unwrap()), true).expect("force overwrite should succeed");
        let content = std::fs::read_to_string(&path).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    #[test]
    fn run_init_creates_missing_parent_directories() {
        let dir = init_scratch("mkdir");
        let nested = dir.join("a").join("b").join("frugalis.toml");
        run_init(Some(nested.to_str().unwrap()), false).expect("nested write should succeed");
        assert!(nested.exists(), "file should exist at nested path");
        let content = std::fs::read_to_string(&nested).expect("file should be readable");
        assert_eq!(content, INIT_TEMPLATE);
    }

    // ── OpenAI → Anthropic translation e2e tests ──────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_translation() {
        let env = "TEST_TRANSLATE_O2A";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        // Mock Anthropic upstream — receives Anthropic Messages format,
        // returns Anthropic Messages response.
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .header("anthropic-version", "2023-06-01")
                .body_contains("\"system\"")
                .body_contains("\"max_tokens\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_1","type":"message","role":"assistant","model":"sf-model","content":[{"type":"text","text":"translated response"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}}"#,
                );
        });
        // Send OpenAI-format request to /v1/chat/completions.
        // "fix this bug" matches SYNTAX_FIX → routes to anthropic upstream.
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"fix this bug"}],"max_tokens":100}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let json = parse_json_body(response).await;
        // Verify response is valid OpenAI Chat Completions format.
        assert_eq!(
            json.get("object").and_then(|v| v.as_str()),
            Some("chat.completion"),
            "expected chat.completion object, got: {json}"
        );
        let choices = json
            .get("choices")
            .and_then(|v| v.as_array())
            .expect("choices array");
        assert_eq!(choices.len(), 1);
        let message = choices[0].get("message").expect("message field");
        assert_eq!(
            message.get("content").and_then(|v| v.as_str()),
            Some("translated response")
        );
        assert_eq!(
            choices[0].get("finish_reason").and_then(|v| v.as_str()),
            Some("stop")
        );
        let usage = json.get("usage").expect("usage field");
        assert_eq!(
            usage.get("prompt_tokens").and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            usage.get("completion_tokens").and_then(|v| v.as_u64()),
            Some(5)
        );
        assert_eq!(usage.get("total_tokens").and_then(|v| v.as_u64()), Some(15));
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_translation_inserts_cache_control() {
        // OAI→Anthropic: an OpenAI client request routed to an Anthropic
        // upstream must arrive with a top-level cache_control so Anthropic
        // automatic prompt caching activates. The mock matches only if the
        // translated upstream body contains "cache_control".
        let env = "TEST_TRANSLATE_O2A_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .body_contains("\"cache_control\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"max_tokens":100}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_translates_cache_tokens_in_usage() {
        // OAI client → Anthropic upstream: the upstream reports
        // cache_read_input_tokens / cache_creation_input_tokens in Anthropic
        // shape; the OpenAI client must receive them as
        // usage.prompt_tokens_details.cached_tokens, with prompt_tokens being
        // the full prompt (non-cached + cached). End-to-end companion to the
        // protocol_translation unit tests.
        let env = "TEST_USAGE_O2A_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages").header("x-api-key", "sk-ant-test");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_u","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":5}}"#,
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"max_tokens":100}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        let json = parse_json_body(response).await;
        let usage = json.get("usage").expect("usage in client response");
        assert_eq!(
            usage.get("prompt_tokens").and_then(|v| v.as_u64()),
            Some(100 + 80 + 5),
            "prompt_tokens must be the full prompt (non-cached + cache_read + cache_creation)"
        );
        assert_eq!(
            usage.get("completion_tokens").and_then(|v| v.as_u64()),
            Some(20)
        );
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64());
        assert_eq!(
            cached,
            Some(80),
            "cached_tokens must map from Anthropic cache_read_input_tokens end-to-end, got: {usage}"
        );
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_strips_cache_control() {
        // Anth→OAI: an Anthropic body carrying a cache_control breakpoint,
        // routed to an OpenAI upstream, must arrive WITHOUT cache_control
        // (OpenAI has no native equivalent). A FIFO canary mock registered
        // before the serving mock matches only if "cache_control" leaked into
        // the upstream body; in the correct case it is never hit.
        let env = "TEST_A2O_NO_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);
        let canary = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .body_contains("cache_control");
            then.status(200).body("canary");
        });
        let positive = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("authorization", "Bearer sk-openai-test");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-frugalis-category", "SYNTAX_FIX")
                    .header("x-frugalis-model", "gpt-4o")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":[{"type":"text","text":"fix this bug","cache_control":{"type":"ephemeral"}}]}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            canary.hits(),
            0,
            "cache_control must NOT survive Anth→OpenAI translation"
        );
        assert_eq!(
            positive.hits(),
            1,
            "translated request must still reach the OpenAI upstream"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_anthropic_passthrough_preserves_cache_control() {
        // Anth→Anthropic same-protocol passthrough: a client cache_control
        // breakpoint must reach the upstream unchanged (byte passthrough, not
        // translator allowlist). The mock matches only if the upstream body
        // contains "cache_control".
        let env = "TEST_ANT_PASSTHROUGH_CACHE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .header("anthropic-version", "2023-06-01")
                .body_contains("\"cache_control\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":[{"type":"text","text":"fix this bug","cache_control":{"type":"ephemeral"}}]}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_streaming() {
        let env = "TEST_TRANSLATE_O2A_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        // Mock returns Anthropic SSE stream.
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_s1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"sf-model\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                );
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
        );
        mock.assert();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        // Verify OpenAI SSE format.
        assert!(
            body_str.contains("chatcmpl-"),
            "expected OpenAI chunk IDs, got: {body_str}"
        );
        assert!(
            body_str.contains("\"role\":\"assistant\""),
            "expected role chunk, got: {body_str}"
        );
        assert!(
            body_str.contains("Hello "),
            "expected text content, got: {body_str}"
        );
        assert!(
            body_str.contains("\"finish_reason\":\"stop\""),
            "expected finish_reason, got: {body_str}"
        );
        assert!(
            body_str.contains("[DONE]"),
            "expected [DONE] terminator, got: {body_str}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_completion_handler_anthropic_error() {
        let env = "TEST_TRANSLATE_O2A_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-ant-test");
        let (app, server) = test_app_with_anthropic_http_client(env, 10_485_760);
        // Mock returns Anthropic error.
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/messages");
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#);
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .expect("request should be valid"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();
        let json = parse_json_body(response).await;
        // Verify error is translated to OpenAI envelope.
        let error = json.get("error").expect("error field");
        assert_eq!(
            error.get("type").and_then(|v| v.as_str()),
            Some("rate_limit_error"),
            "expected rate_limit_error type, got: {json}"
        );
        assert_eq!(
            error.get("message").and_then(|v| v.as_str()),
            Some("Too many requests"),
            "expected error message, got: {json}"
        );
    }

    // ── /v1/messages translation (Anthropic→OpenAI→Anthropic) e2e tests ─────

    /// Helper: creates a test app where the messages handler routes to an
    /// openai_compatible mock (triggers Anthropic→OpenAI translation).
    fn test_app_with_openai_translation(env_var_name: &str) -> (Router, httpmock::MockServer) {
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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "gpt-4o".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var_name.to_string()),
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
            Some(client),
            config::routing::ModelCosts::empty(),
            String::new(),
            10_485_760,
        );
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_non_streaming() {
        let env = "TEST_A2O_NS";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);

        let mock = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("authorization", "Bearer sk-openai-test");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"chatcmpl-abc","object":"chat.completion","model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"Hello from OpenAI"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#);
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-frugalis-category", "SYNTAX_FIX")
                    .header("x-frugalis-model", "gpt-4o")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(body.get("type").unwrap().as_str().unwrap(), "message");
        assert_eq!(body.get("role").unwrap().as_str().unwrap(), "assistant");
        assert_eq!(
            body.get("stop_reason").unwrap().as_str().unwrap(),
            "end_turn"
        );
        let content = body.get("content").unwrap().as_array().unwrap();
        assert_eq!(content[0].get("type").unwrap().as_str().unwrap(), "text");
        assert_eq!(
            content[0].get("text").unwrap().as_str().unwrap(),
            "Hello from OpenAI"
        );
        let usage = body.get("usage").unwrap();
        assert_eq!(usage.get("input_tokens").unwrap().as_u64().unwrap(), 10);
        assert_eq!(usage.get("output_tokens").unwrap().as_u64().unwrap(), 5);
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_streaming() {
        let env = "TEST_A2O_STREAM";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);

        let sse_body = "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
                        data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n\
                        data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                        data: [DONE]\n\n";

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
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-frugalis-category", "SYNTAX_FIX")
                    .header("x-frugalis-model", "gpt-4o")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":1024,"stream":true,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();

        assert!(
            body_str.contains("event: message_start"),
            "missing message_start: {body_str}"
        );
        assert!(
            body_str.contains("event: content_block_start"),
            "missing content_block_start: {body_str}"
        );
        assert!(
            body_str.contains("text_delta"),
            "missing text_delta: {body_str}"
        );
        assert!(body_str.contains("Hi"), "missing content 'Hi': {body_str}");
        assert!(
            body_str.contains("event: message_delta"),
            "missing message_delta: {body_str}"
        );
        assert!(
            body_str.contains("end_turn"),
            "missing stop_reason: {body_str}"
        );
        assert!(
            body_str.contains("event: message_stop"),
            "missing message_stop: {body_str}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_messages_handler_openai_translation_error() {
        let env = "TEST_A2O_ERR";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-openai-test");
        let (app, server) = test_app_with_openai_translation(env);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit","code":"rate_limit_exceeded"}}"#);
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-frugalis-category", "SYNTAX_FIX")
                    .header("x-frugalis-model", "gpt-4o")
                    .body(Body::from(
                        r#"{"model":"claude-3.5","max_tokens":1024,"messages":[{"role":"user","content":"fix this bug"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        mock.assert();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(body.get("type").unwrap().as_str().unwrap(), "error");
        let error = body.get("error").unwrap();
        assert_eq!(
            error.get("type").unwrap().as_str().unwrap(),
            "rate_limit_error"
        );
        assert_eq!(
            error.get("message").unwrap().as_str().unwrap(),
            "Rate limit exceeded"
        );
    }

    // ── Cache Integration Tests ──

    fn test_app_with_cache(
        ttl_secs: u64,
        max_entries: u64,
    ) -> (Router, httpmock::MockServer, Arc<cache::ResponseCache>) {
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
        let env = "TEST_CACHE_PROXY";
        // Note: callers must set this env var with EnvGuard.
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env.to_string()),
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
                    endpoint,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env.to_string()),
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
        let classifier_chain =
            classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier_arc = Some(Arc::new(classifier_chain));
        let mut merged_routing = std::collections::HashMap::new();
        if let Some(cls) = classifier_arc.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let response_cache =
            Arc::new(cache::ResponseCache::new(ttl_secs, max_entries));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier: classifier_arc,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(
                config::routing::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::types::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            response_cache: Some(response_cache.clone()),
            #[cfg(feature = "otel")]
            metrics: None,
        });
        let app = build_app(auth_config, app_state);
        (app, server, response_cache)
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_hit_returns_cached_response() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test-cache");
        let (app, server, _cache) = test_app_with_cache(60, 10);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"test","choices":[{"message":{"content":"hello"}}]}"#);
        });
        let body = r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#;

        let resp1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(resp1.status(), StatusCode::OK);
        let body1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
            .await
            .expect("body readable");
        assert_eq!(mock.hits(), 1);

        let resp2 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
            .await
            .expect("body readable");
        assert_eq!(body1, body2);
        assert_eq!(mock.hits(), 1, "cache hit should not call upstream again");
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_miss_proceeds_to_upstream() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test-cache");
        let (app, server, _cache) = test_app_with_cache(60, 10);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"test","choices":[{"message":{"content":"ok"}}]}"#);
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
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_bypass_header_skips_cache() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test-cache");
        let (app, server, _cache) = test_app_with_cache(60, 10);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"id":"test","choices":[{"message":{"content":"ok"}}]}"#);
        });
        let body = r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#;

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(mock.hits(), 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-frugalis-no-cache", "true")
                    .body(Body::from(body))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(mock.hits(), 2, "bypass header should force upstream call");
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_streaming_not_cached() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test-cache");
        let (app, server, cache) = test_app_with_cache(60, 10);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body("data: [DONE]\n\n");
        });
        let body = r#"{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}"#;

        for _ in 0..2 {
            let _resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, "Bearer proxy-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .expect("valid request"),
                )
                .await
                .expect("request should succeed");
        }
        assert_eq!(mock.hits(), 2, "streaming should never be cached");
        assert_eq!(cache.stats().entry_count, 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_cache_error_not_cached() {
        let _guard = EnvGuard("TEST_CACHE_PROXY");
        std::env::set_var("TEST_CACHE_PROXY", "sk-test-cache");
        let (app, server, cache) = test_app_with_cache(60, 10);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(500)
                .header("content-type", "application/json")
                .body(r#"{"error":"internal"}"#);
        });
        let body = r#"{"messages":[{"role":"user","content":"fix this bug"}]}"#;

        for _ in 0..2 {
            let _resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, "Bearer proxy-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .expect("valid request"),
                )
                .await
                .expect("request should succeed");
        }
        assert_eq!(mock.hits(), 2, "errors should never be cached");
        assert_eq!(cache.stats().entry_count, 0);
    }

    #[tokio::test]
    async fn test_cache_disabled_when_no_config() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_cache_dashboard_requires_auth() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/cache")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_cache_dashboard_authenticated() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/dashboard/cache")
                    .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body readable");
        let body_str = std::str::from_utf8(&body).expect("UTF-8");
        assert!(
            body_str.contains("not configured"),
            "should show disabled message: {body_str}"
        );
    }
}

#[cfg(test)]
mod slow_tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::util::ServiceExt;
    use super::test_util::EnvGuard;

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
            config::routing::RouteEntry {
                providers: vec![config::routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: url,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env_var.to_string()),
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
        let model_costs = config::routing::ModelCosts::empty();
        let baseline_model = String::new();
        let classifier_chain =
            classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
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
            dashboard_config: config::types::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(RwLock::new(vec![])),
            response_cache: None,
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
