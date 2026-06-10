use std::collections::HashMap;
use std::convert::Infallible;
use std::panic;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use tokio_stream::{Stream, StreamExt};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod auth;
mod config;
mod dashboard;
mod intent_classifier;
mod persistence;
mod routing;

use intent_classifier::IntentClassify;

/// Shared application state injected into handlers via Axum's `State` extractor.
/// `persistence` is `None` when `DATABASE_URL` is absent (persistence gracefully disabled).
#[derive(Clone)]
pub struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classifier::ClassifierChain>>,
    routing: Arc<tokio::sync::RwLock<std::collections::HashMap<String, intent_classifier::RouteEntry>>>,
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
}

#[tokio::main]
async fn main() {
    // Initialize tracing subscriber before any other code.
    let log_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = match std::env::var("LOG_FORMAT").as_deref() {
        Ok("json") => fmt::layer().json().with_filter(log_filter).boxed(),
        _ => fmt::layer().compact().with_filter(log_filter).boxed(),
    };

    tracing_subscriber::registry().with(fmt_layer).init();

    // Ensure any panic is logged, not silent.
    panic::set_hook(Box::new(|info| {
        eprintln!("Panic in Cerebrum: {info}");
    }));

    let auth_config = auth::AuthConfig::from_env().unwrap_or_else(|err| {
        panic!("Auth configuration error: {err}");
    });
    let auth_config = Arc::new(auth_config);

    let config_path_option = std::env::var("CONFIG_PATH").ok();

    // Embed config.toml as default
    const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
    let mut config_root = match toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML) {
        Ok(root) => root,
        Err(e) => {
            error!("Embedded config.toml is invalid: {e}; using hardcoded defaults");
            toml::Value::Table(Default::default())
        }
    };

    // Merge CONFIG_PATH overlay if provided
    if let Some(config_path) = config_path_option {
        match std::fs::read_to_string(&config_path) {
            Ok(content) => {
                match toml::from_str::<toml::Value>(&content) {
                    Ok(overlay) => {
                        config::merge_toml_values(&mut config_root, &overlay);
                        info!("Merged config from {}", config_path);
                    }
                    Err(e) => {
                        warn!("failed to parse config file at {}: {}; using embedded defaults", config_path, e);
                    }
                }
            }
            Err(e) => {
                warn!("failed to read config file at {}: {}; using embedded defaults", config_path, e);
            }
        }
    }

    let regex_config = config::load_regex_classifier_config_from_value(&config_root);

    // Load global classifiers config
    let classifiers_config = config::load_classifiers_config_from_value(&config_root);

    let http_config = config::load_http_config_from_value(&config_root);
    let max_upstream_body_bytes = http_config.max_upstream_body_bytes;
    let keepalive_interval_secs = http_config.keepalive_interval_secs;

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(http_config.client_timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(http_config.client_connect_timeout_secs))
        .build()
        .expect("reqwest client should build");

    let classify_db_log = config_root
        .get("classify_db_log")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auth_providers = Arc::new(config::load_auth_providers_from_value(&config_root));
     let (classifier, routing, model_costs, baseline_model) = {
        let categories = config::load_categories_from_value(&config_root)
            .unwrap_or_else(|_| intent_classifier::hardcoded_categories());
        let (routing_map, fallback_entry) = match config::routing_from_value(&config_root) {
            Ok((map, fallback)) => (map, fallback),
            Err(e) => {
                warn!(
                    "routing config parsing failed: {}; using hardcoded routing defaults",
                    e
                );
                config::hardcoded_routing(&categories)
            }
        };
        let model_costs = config::build_model_costs(&config_root, &routing_map);
        let baseline_model = config_root
            .get("baseline_model")
            .and_then(|v| v.as_str())
            .unwrap_or(intent_classifier::DEFAULT_MODEL_COMPLEX)
            .to_string();
        if !classifiers_config.enabled {
            info!("All classifiers disabled via config");
            (None, HashMap::new(), model_costs, baseline_model)
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
                                intent_classifier::SHORT_PROMPT_LEN,
                                categories.clone(),
                            ) {
                                Ok(c) => {
                                    info!("Regex classifier initialized");
                                    backends.push(Arc::new(c));
                                }
                                Err(e) => {
                                    warn!("RegexClassifier disabled: {e}");
                                }
                            }
                        }
                    }
                    "llm" => {
                        if let Some(llm_config) = config::load_llm_classifier_config_from_value(&config_root) {
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
                (None, HashMap::new(), model_costs, baseline_model)
            } else {
                let chain = intent_classifier::ClassifierChain::new(backends);
                let mut merged_routing = HashMap::new();
                for backend in chain.backends().iter() {
                    if let Some(r) = backend.get_routing() {
                        merged_routing.extend(r.clone());
                    }
                }
                (Some(Arc::new(chain)), merged_routing, model_costs, baseline_model)
            }
        }
    };

    let db_config = config::load_database_config_from_value(&config_root);
    let persistence_state = match persistence::PersistenceConfig::from_env(&db_config).await {
        Ok(s) => {
            info!("Database connected successfully");
            Some(s)
        }
        Err(e) => {
            warn!("persistence disabled: {e}");
            None
        }
    };

    let app_state = Arc::new(AppState {
        persistence: persistence_state,
        classifier,
        routing: Arc::new(tokio::sync::RwLock::new(routing)),
        model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
        baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
        classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(classify_db_log)),
        http_client: Some(http_client),
        max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(max_upstream_body_bytes as usize)),
        keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(keepalive_interval_secs as u64)),
        request_body_limit_bytes: http_config.request_body_limit_bytes,
        streaming_channel_capacity: http_config.streaming_channel_capacity,
        dashboard_config: config::load_dashboard_config_from_value(&config_root),
        auth_providers,
    });

    let server_config = config::load_server_config_from_value(&config_root);
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(server_config.port);

    let app = build_app(auth_config, app_state);
    let bind_addr = format!("0.0.0.0:{port}");
    info!("Starting cerebrum on {bind_addr}");

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app)
        .await
        .expect("Axum server exited unexpectedly");
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
        };
        persistence::log_inference(
            persistence.pool.clone(),
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
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
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

    let auth_headers = intent_classifier::auth_headers_for(auth_providers, &classification.provider_type, api_key);

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
                            let error_msg = _e.to_string();
                            let json_payload = serde_json::json!({"error": error_msg}).to_string();
                            let _ = tx.send(Bytes::from(
                                format!("event: error\ndata: {}\n\n", json_payload)
                            )).await;
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

async fn handle_streaming_error(mut upstream_response: reqwest::Response) -> Response {
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
    let error_text = String::from_utf8_lossy(&error_bytes)
        .chars()
        .take(512)
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ");
    let sse_error = format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", error_text);
    let mut resp = Response::new(Body::from(sse_error));
    *resp.status_mut() = upstream_response.status();
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

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            r#"{"error":"bad_request","status":415,"message":"expected application/json"}"#
                .to_string(),
        );
    }

    let body_str: String = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => {
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
        return json_response(
            StatusCode::BAD_GATEWAY,
            upstream_error_json(502, "no endpoint configured"),
        );
    }

    let (client_wants_stream, upstream_req) =
        match build_upstream_request(client, &classification, &body, &api_key, &state.auth_providers) {
            Err(msg) => {
                log_classification(&state, &classification, &body_str, start, "bad_request");
                return json_response(StatusCode::BAD_REQUEST, upstream_error_json(400, &msg));
            }
            Ok(r) => r,
        };

    let upstream_response = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            log_classification(&state, &classification, &body_str, start, "upstream_error");
            return json_response(
                StatusCode::BAD_GATEWAY,
                upstream_error_json(502, &e.to_string()),
            );
        }
    };

    if client_wants_stream {
        if !upstream_response.status().is_success() {
            let resp = handle_streaming_error(upstream_response).await;
            log_classification(&state, &classification, &body_str, start, "upstream_error");
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
    let log_status = if state.classify_db_log.load(std::sync::atomic::Ordering::Relaxed) {
        Some("classified")
    } else {
        None
    };
    classify_and_log(&headers, body_str, start, &state, log_status).await
}

fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router {
    let proxy_routes = Router::new()
        .route("/chat/completions", post(completion_handler))
        .route("/classify", post(classify_handler))
        .route_layer(auth::proxy_auth_layer(auth_config.clone()));

    let dashboard_routes = dashboard::routes(auth_config);

    // Build CORS layer from ALLOWED_ORIGINS env (comma-separated). If empty, no CORS headers (secure default).
    let allowed_origin_headers: Vec<HeaderValue> = std::env::var("ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| header::HeaderValue::from_str(s.trim()).ok())
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
        .layer(RequestBodyLimitLayer::new(app_state.request_body_limit_bytes))
        .with_state(app_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    /// Guard that removes an env var on drop to prevent test pollution.
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    /// Build an `AppState` from a `RegexClassifier` and optional HTTP client.
    /// Mergeroutes from all classifier backends.
    fn make_test_app_state(
        classifier: intent_classifier::RegexClassifier,
        http_client: Option<reqwest::Client>,
        model_costs: intent_classifier::ModelCosts,
        baseline_model: String,
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
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
            baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client,
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(
                std::env::var("MAX_UPSTREAM_BODY_BYTES")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(10_485_760),
            )),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
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
            routing: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            model_costs: Arc::new(tokio::sync::RwLock::new(intent_classifier::ModelCosts::empty())),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: None,
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
        });
        build_app(auth_config, app_state)
    }

    fn test_app_with_classifier() -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
        let app_state = make_test_app_state(
            regex_classifier,
            None,
            intent_classifier::ModelCosts::empty(),
            String::new(),
        );
        build_app(auth_config, app_state)
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""category":"SYNTAX_FIX""#),
            "expected SYNTAX_FIX category, got: {body}"
        );
        assert!(
            body.contains(r#""status":"classified""#),
            "expected classified status"
        );
        assert!(body.contains(r#""tier":"Regex""#), "expected Regex tier");
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""category":"SYNTAX_FIX""#),
            "expected SYNTAX_FIX category, got: {body}"
        );
        assert!(
            body.contains(r#""model":"sf-model""#),
            "expected sf-model, got: {body}"
        );
        assert!(
            body.contains(r#""status":"classified""#),
            "expected classified status"
        );
        assert!(body.contains(r#""tier":"Regex""#), "expected Regex tier");
    }

    #[tokio::test]
    #[serial]
    async fn test_max_upstream_body_bytes_truncation() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }
        let _guard1 = EnvGuard("MAX_UPSTREAM_BODY_BYTES");
        let _guard2 = EnvGuard("TEST_API_KEY");
        // Set limit to 1.1MB (above 1MB min) and send response > limit to trigger truncation
        std::env::set_var("MAX_UPSTREAM_BODY_BYTES", "1100000");
        std::env::set_var("TEST_API_KEY", "sk-test");
        let (app, server) = test_app_with_http_client("TEST_API_KEY");
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
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body_str = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(body_str.contains("upstream response too large"));
        mock.assert();
    }

    fn test_app_with_enriched_classifier(
        provider_type_val: &str,
        api_key_env_val: Option<&str>,
    ) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
        let app_state = make_test_app_state(
            regex_classifier,
            None,
            intent_classifier::ModelCosts::empty(),
            String::new(),
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""category":"SYNTAX_FIX""#),
            "expected SYNTAX_FIX category"
        );
        assert!(
            !body.contains(r#""provider_type""#),
            "response should NOT contain provider_type"
        );
        assert!(
            !body.contains(r#""endpoint""#),
            "response should NOT contain endpoint"
        );
        assert!(
            !body.contains(r#""api_key""#),
            "response should NOT contain api_key"
        );
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            !body.contains(r#""api_key""#),
            "response should NOT contain api_key"
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

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            !body.contains(r#""provider_type""#),
            "classify response should not contain provider_type"
        );
        assert!(
            !body.contains(r#""api_key""#),
            "classify response should not contain api_key"
        );
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

        let request_id = uuid::Uuid::new_v4();
        let record = persistence::InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("chat".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(10),
            prompt_snippet: "integration test snippet".to_string(),
            prompt_char_count: Some(25),
        };
        let handle = persistence::log_inference(pool.clone(), semaphore, record);
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

    pub(crate) fn test_app_with_http_client(env_var_name: &str) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
        let app_state = make_test_app_state(
            regex_classifier,
            Some(client),
            intent_classifier::ModelCosts::empty(),
            String::new(),
        );
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    /// Build app state and router with a real database pool for integration tests.
    pub(crate) fn build_app_with_persistence(
        pool: Arc<sqlx::PgPool>,
        semaphore: Arc<tokio::sync::Semaphore>,
        http_client: Option<reqwest::Client>,
    ) -> (Router, httpmock::MockServer) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
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
        let max_upstream_body_bytes = config::parse_env_int(
            "MAX_UPSTREAM_BODY_BYTES",
            10_485_760,
            Some(1_048_576),
            Some(100_485_760),
        );
        let keepalive_interval_secs = config::parse_env_int(
            "KEEPALIVE_INTERVAL_SECS",
            15,
            Some(1),
            None,
        );
        let app_state = Arc::new(AppState {
            persistence: Some(persistence::PersistenceConfig {
                pool,
                task_semaphore: semaphore,
            }),
            classifier: classifier_arc,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(intent_classifier::ModelCosts::empty())),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(max_upstream_body_bytes as usize)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(keepalive_interval_secs as u64)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
        });
        let app = build_app(auth_config, app_state);
        (app, server)
    }

    fn test_app_with_dead_endpoint(env_var_name: &str) -> Router {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
        let app_state = make_test_app_state(
            regex_classifier,
            Some(client),
            intent_classifier::ModelCosts::empty(),
            String::new(),
        );
        build_app(auth_config, app_state)
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_returns_response() {
        let env = "TEST_UPSTREAM_RESP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
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
        let (app, server) = test_app_with_http_client(env);
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
        let (app, server) = test_app_with_http_client(env);
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
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let body = std::str::from_utf8(&body_bytes).expect("body should be UTF-8");
        assert!(
            body.contains(r#""error":"upstream_error""#),
            "expected upstream_error in body, got: {body}"
        );
        // cleanup handled by EnvGuard
    }

    #[tokio::test]
    #[serial]
    async fn test_upstream_skip_classify_via_headers() {
        let env = "TEST_UPSTREAM_SKIP";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
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
        let (app, server) = test_app_with_http_client(env);
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
        let (app, server) = test_app_with_http_client(env);
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
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
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

    #[tokio::test]
    #[serial]
    async fn test_streaming_true_returns_sse_content() {
        let env = "TEST_STREAM_TSSE";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
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
    async fn test_streaming_false_returns_buffered_json() {
        let env = "TEST_STREAM_FJSON";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");
        let (app, server) = test_app_with_http_client(env);
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
        let (app, server) = test_app_with_http_client(env);
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

    #[tokio::test]
    #[serial]
    async fn test_streaming_keepalive_injected() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        std::env::set_var("KEEPALIVE_INTERVAL_SECS", "1");
        let _guard_ka = EnvGuard("KEEPALIVE_INTERVAL_SECS");
        let (url, server_handle) = spawn_slow_sse_server().await;
        let env = "TEST_STREAM_KA_SLOW";
        let _guard = EnvGuard(env);
        std::env::set_var(env, "sk-test");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let cats = intent_classifier::hardcoded_categories();
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
        let regex_classifier =
            intent_classifier::RegexClassifier::from_values(routing, fallback, 30, cats);
        let model_costs = intent_classifier::ModelCosts::empty();
        let baseline_model = String::new();
        let classifier_chain =
            intent_classifier::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier = Some(Arc::new(classifier_chain));
        // Merge routing from all backends in the chain
        let mut merged_routing = HashMap::new();
        if let Some(cls) = classifier.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let max_upstream_body_bytes = config::parse_env_int(
            "MAX_UPSTREAM_BODY_BYTES",
            10_485_760,
            Some(1_048_576),
            Some(100_485_760),
        );
        let keepalive_interval_secs = config::parse_env_int(
            "KEEPALIVE_INTERVAL_SECS",
            15,
            Some(1),
            None,
        );
        let auth_config = Arc::new(auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let app_state = Arc::new(AppState {
            persistence: None,
            classifier,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(model_costs)),
            baseline_model: Arc::new(tokio::sync::RwLock::new(baseline_model)),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(max_upstream_body_bytes as usize)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(keepalive_interval_secs as u64)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: config::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
        });
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
        assert!(
            body.contains(": keepalive\n\n"),
            "expected keepalive comment in stream, got: {body}"
        );
        assert!(
            body.contains("data: hello"),
            "expected upstream data after keepalive, got: {body}"
        );
        let _ = server_handle.await;
    }

    #[tokio::test]
    #[serial]
    async fn test_graceful_shutdown() {
        use std::time::Duration;
        use tokio::sync::oneshot;
        let app = Router::new().route("/slow", get(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            "OK"
        }));
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
        let resp = client.get(format!("http://{}/slow", addr)).send().await.unwrap();
        shutdown_tx.send(()).unwrap();
        let body = resp.text().await.unwrap();
        assert_eq!(body, "OK");
        server_task.await.unwrap();
    }

}
