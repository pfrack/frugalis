use std::sync::Arc;

use tokio::sync::Semaphore;

pub(crate) mod backend;
pub(crate) mod memory;
pub(crate) mod sql_backend;
pub(crate) mod types;

pub(crate) use backend::{DbBackend, PersistenceBackend};
pub(crate) use types::{
    extract_last_user_message, extract_last_user_message_anthropic, InferenceLog, InferenceRecord,
    LatencySummary, SavingsEstimate,
};

/// Shared persistence handle injected into the Axum router state.
///
/// Cheaply cloned per-request (both fields are `Arc`). The `task_semaphore`
/// bounds the number of in-flight background log tasks so a slow database
/// cannot cause unbounded memory growth under burst traffic.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub backend: Arc<DbBackend>,
    /// Semaphore whose capacity equals `[database].log_concurrency_limit`.
    /// Tasks block on `acquire()` rather than spawning unboundedly when the
    /// database cannot keep up with insert throughput.
    pub task_semaphore: Arc<Semaphore>,
}

/// Enqueue one [`InferenceRecord`] for asynchronous background persistence.
///
/// Returns immediately — the caller is never blocked by database latency.
/// Internally, a detached `tokio::spawn` task:
/// 1. Acquires one permit from `semaphore` (blocks if the pool is exhausted).
/// 2. Calls `backend.insert_inference(&record)`.
/// 3. On failure, logs `tracing::error!` with the `request_id` for observability.
///
/// The returned `JoinHandle` can be awaited in tests; production code discards it.
pub fn log_inference(
    backend: Arc<DbBackend>,
    semaphore: Arc<Semaphore>,
    record: InferenceRecord,
) -> tokio::task::JoinHandle<()> {
    let semaphore = semaphore.clone();
    tokio::spawn(async move {
        let request_id = record.request_id;
        let _permit = match semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => {
                tracing::error!("semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = backend.insert_inference(&record).await {
            tracing::error!("final insert failure request_id={request_id} class={class}");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app, auth, classification, config};

    use std::sync::Arc;

    use crate::app::test_helpers::{test_categories, test_negative_patterns};
    use crate::test_util::EnvGuard;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    pub(crate) fn build_app_with_persistence_backend(
        backend: Arc<DbBackend>,
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
            for backend_ref in cls.backends().iter() {
                if let Some(r) = backend_ref.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let app_state = Arc::new(app::AppState {
            persistence: Some(PersistenceConfig {
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
            allowed_origins: Arc::new(tokio::sync::RwLock::new(vec![])),
            response_cache: None,
            #[cfg(feature = "otel")]
            metrics: None,
        });
        let app = app::build_app(auth_config, app_state);
        (app, server)
    }

    // ── Snippet truncation tests (MemoryBackend) ─────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_snippet_path_truncates_to_200_chars() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = memory::MemoryBackend::new();
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(DbBackend::Memory(memory_backend));
        let (app, server) = build_app_with_persistence_backend(backend, semaphore, None);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });
        let long_message = format!("fix this bug {}", "x".repeat(487));
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
        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(2);
        loop {
            if !records_handle.read().await.is_empty() {
                break;
            }
            if poll_start.elapsed() >= poll_timeout {
                panic!("inference record did not appear within 2s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let records = records_handle.read().await;
        assert_eq!(records.len(), 1);
        let snippet = &records[0].prompt_snippet;
        assert!(snippet.chars().count() <= 200);
        assert_eq!(records[0].prompt_char_count, Some(500));
    }

    #[tokio::test]
    #[serial]
    async fn test_snippet_path_does_not_contain_full_prompt() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = memory::MemoryBackend::new();
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(DbBackend::Memory(memory_backend));
        let (app, server) = build_app_with_persistence_backend(backend, semaphore, None);
        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });
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
        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(2);
        loop {
            if !records_handle.read().await.is_empty() {
                break;
            }
            if poll_start.elapsed() >= poll_timeout {
                panic!("inference record did not appear within 2s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let records = records_handle.read().await;
        assert_eq!(records.len(), 1);
        let snippet = &records[0].prompt_snippet;
        assert!(snippet.contains(&prefix));
        assert!(!snippet.contains(marker));
        assert!(snippet.chars().count() <= 200);
        assert_eq!(records[0].prompt_char_count, Some(full_message_len as i32));
    }

    #[tokio::test]
    #[serial]
    async fn test_log_classification_failure_does_not_block_response() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");
        let memory_backend = memory::MemoryBackend::new();
        memory_backend
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(DbBackend::Memory(memory_backend));
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
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(2);
        loop {
            match backend.as_ref() {
                DbBackend::Memory(mb) => {
                    if !mb.fail_next.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                }
                _ => panic!("test fixture invariant: backend must be DbBackend::Memory"),
            }
            if poll_start.elapsed() >= poll_timeout {
                panic!("log task did not consume fail_next within 2s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let records = records_handle.read().await;
        assert_eq!(records.len(), 0);
        drop(records);
        if let DbBackend::Memory(ref mb) = *backend {
            assert!(!mb.fail_next.load(std::sync::atomic::Ordering::SeqCst));
        }
    }
}
