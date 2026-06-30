use crate::app::{build_app, AppState};
use crate::classification;
use crate::config;
use crate::routing;
use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn test_categories() -> Vec<config::types::CategoryConfig> {
    vec![
        config::types::CategoryConfig {
            name: "FILE_READING".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 1,
            patterns: vec![
                config::types::PatternEntry {
                    regex: r"(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        config::types::CategoryConfig {
            name: "SYNTAX_FIX".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 2,
            patterns: vec![
                config::types::PatternEntry {
                    regex: r"(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        config::types::CategoryConfig {
            name: "COMPLEX_REASONING".to_string(),
            description: String::new(),
            threshold: 3,
            priority: 3,
            patterns: vec![
                config::types::PatternEntry {
                    regex: r"(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
        config::types::CategoryConfig {
            name: "CASUAL".to_string(),
            description: String::new(),
            threshold: 1,
            priority: 4,
            patterns: vec![
                config::types::PatternEntry {
                    regex: r"(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$".to_string(),
                    weight: 3,
                },
            ],
            patterns_file: None,
            dual_threshold: None,
        },
    ]
}

pub fn test_negative_patterns() -> Vec<config::types::NegativePatternConfig> {
    vec![]
}

pub fn make_test_app_state(
    classifier: classification::regex::RegexClassifier,
    http_client: Option<reqwest::Client>,
    model_costs: routing::ModelCosts,
    baseline_model: String,
    max_upstream_body_bytes: usize,
) -> Arc<AppState> {
    let classifier_chain =
        classification::chain::ClassifierChain::new(vec![Arc::new(classifier)]);
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

/// Build a test app + httpmock server for a single provider-type endpoint.
/// Used by the per-provider wrappers below; each one supplies the
/// (endpoint_path, provider_type) pair that distinguishes it.
pub fn test_app_with_provider(
    env_var_name: &str,
    max_upstream_body_bytes: usize,
    endpoint_path: &str,
    provider_type: &str,
) -> (Router, httpmock::MockServer) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    use std::collections::HashMap;
    let cats = test_categories();
    let server = httpmock::MockServer::start();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("test reqwest client should build");
    let auth_config = Arc::new(routing::AuthConfig::from_values(
        "proxy-token",
        "user",
        "password",
    ));
    let endpoint = server.url(endpoint_path);
    let mut routing = HashMap::new();
    routing.insert(
        cats[1].name.clone(),
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
                model: "sf-model".to_string(),
                endpoint: endpoint.clone(),
                provider_type: provider_type.to_string(),
                api_key_env: Some(env_var_name.to_string()),
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        },
    );
    routing.insert(
        cats[3].name.clone(),
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
                model: "ca-model".to_string(),
                endpoint,
                provider_type: provider_type.to_string(),
                api_key_env: Some(env_var_name.to_string()),
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        },
    );
    let fallback = routing::RouteEntry {
        providers: vec![routing::ProviderEntry {
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
        routing::ModelCosts::empty(),
        String::new(),
        max_upstream_body_bytes,
    );
    let app = build_app(auth_config, app_state);
    (app, server)
}

pub fn test_app_with_http_client(
    env_var_name: &str,
    max_upstream_body_bytes: usize,
) -> (Router, httpmock::MockServer) {
    test_app_with_provider(
        env_var_name,
        max_upstream_body_bytes,
        "/v1/chat/completions",
        "openai_compatible",
    )
}

pub fn test_app_with_anthropic_http_client(
    env_var_name: &str,
    max_upstream_body_bytes: usize,
) -> (Router, httpmock::MockServer) {
    test_app_with_provider(env_var_name, max_upstream_body_bytes, "/v1/messages", "anthropic")
}

pub fn test_app_with_nim_http_client(
    env_var_name: &str,
    max_upstream_body_bytes: usize,
) -> (Router, httpmock::MockServer) {
    test_app_with_provider(
        env_var_name,
        max_upstream_body_bytes,
        "/v1/chat/completions",
        "nvidia_nim",
    )
}

pub fn test_app_with_ollama_http_client(
    env_var_name: &str,
    max_upstream_body_bytes: usize,
) -> (Router, httpmock::MockServer) {
    test_app_with_provider(
        env_var_name,
        max_upstream_body_bytes,
        "/v1/chat/completions",
        "ollama",
    )
}

pub fn test_app() -> Router {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let auth_config = Arc::new(routing::AuthConfig::from_values(
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
            routing::ModelCosts::empty(),
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
    let auth_config = Arc::new(routing::AuthConfig::from_values(
        "proxy-token",
        "user",
        "password",
    ));
    let mut routing = HashMap::new();
    routing.insert(
        cats[1].name.clone(),
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
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
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        },
    );
    let fallback = routing::RouteEntry {
        providers: vec![routing::ProviderEntry {
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
        routing::ModelCosts::empty(),
        String::new(),
        10_485_760,
    );
    build_app(auth_config, app_state)
}

pub fn test_app_with_cache(
    ttl_secs: u64,
    max_entries: u64,
) -> (Router, httpmock::MockServer, Arc<crate::cache::ResponseCache>) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let env_var_name = "TEST_CACHE_PROXY";
    let cats = test_categories();
    let server = httpmock::MockServer::start();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("test reqwest client should build");
    let auth_config = Arc::new(routing::AuthConfig::from_values(
        "proxy-token",
        "user",
        "password",
    ));
    let endpoint = server.url("/v1/chat/completions");
    let mut routing = std::collections::HashMap::new();
    routing.insert(
        cats[1].name.clone(),
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
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
        routing::RouteEntry {
            providers: vec![routing::ProviderEntry {
                model: "ca-model".to_string(),
                endpoint,
                provider_type: "openai_compatible".to_string(),
                api_key_env: Some(env_var_name.to_string()),
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        },
    );
    let fallback = routing::RouteEntry {
        providers: vec![routing::ProviderEntry {
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
    let response_cache = Arc::new(crate::cache::ResponseCache::new(ttl_secs, max_entries));
    let app_state = Arc::new(AppState {
        persistence: None,
        classifier: classifier_arc,
        fewshot_classifier: None,
        routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
        model_costs: Arc::new(tokio::sync::RwLock::new(
            routing::ModelCosts::empty(),
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

/// Test app with an `openai_responses` provider for the CASUAL category.
/// The mock server listens on `/v1/responses` — used for R5 passthrough tests.
pub fn test_app_with_openai_responses_http_client(
    env_var_name: &str,
) -> (Router, httpmock::MockServer) {
    test_app_with_provider(env_var_name, 10_485_760, "/v1/responses", "openai_responses")
}

pub async fn parse_json_body(response: axum::response::Response) -> serde_json::Value {
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should be readable");
    serde_json::from_slice(&body_bytes)
        .unwrap_or_else(|e| panic!("response body should be JSON: {e}; body={:?}", body_bytes))
}
