use std::panic;
use std::sync::Arc;

use tokio::sync::RwLock;
#[allow(unused_imports)]
use tracing::{info, warn, Subscriber};
use tracing_subscriber::{fmt, layer::Layer, prelude::*, EnvFilter, Registry};

#[cfg(feature = "otel")]
mod telemetry;

mod app;
mod auth;
mod cache;
mod classification;
mod config;
mod dashboard;
mod persistence;
mod protocol;
mod proxy;

#[cfg(test)]
mod test_util;

use app::{build_app, AppState};
use app::cli::CliMode;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app::cli::CliResult { mode, force } = app::cli::parse_args();

    // Early-exit commands (before config loading or tracing init)
    if let CliMode::Help = mode {
        app::cli::print_help();
        std::process::exit(0);
    }

    if let CliMode::Init(path_opt) = &mode {
        match app::cli::run_init(path_opt.as_deref(), force) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }

    if let CliMode::Quickstart = mode {
        match app::quickstart::run_quickstart() {
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
    let classifiers_result = app::build_classifiers(
        &config_root,
        http_client.clone(),
        auth_providers.clone(),
        &regex_config,
        &classifiers_config,
        &negative_patterns,
    );

    let persistence_state = app::build_persistence(&config_root).await;

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
        classifier: classifiers_result.classifier,
        fewshot_classifier: classifiers_result.fewshot_classifier,
        routing: Arc::new(tokio::sync::RwLock::new(classifiers_result.routing)),
        model_costs: Arc::new(tokio::sync::RwLock::new(classifiers_result.model_costs)),
        baseline_model: Arc::new(tokio::sync::RwLock::new(classifiers_result.baseline_model)),
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
