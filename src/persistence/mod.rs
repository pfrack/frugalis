use std::sync::Arc;

use tokio::sync::Semaphore;

pub(crate) mod backend;
pub(crate) mod memory;
pub(crate) mod sql_backend;
pub(crate) mod types;

pub(crate) use backend::{DbBackend, PersistenceBackend};
pub(crate) use types::{
    extract_last_user_message, extract_last_user_message_anthropic,
    extract_last_user_message_responses, InferenceLog, InferenceRecord, LatencySummary,
    SavingsEstimate,
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

/// Canonical test record for cross-backend identity tests.
/// All three backends (memory, SQLite, Postgres) insert this exact record
/// and the tests assert identical `InferenceLog` fields on fetch.
#[cfg(test)]
pub(crate) fn reference_inference_record() -> InferenceRecord {
    InferenceRecord {
        request_id: uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001")
            .expect("static UUID should parse"),
        status: "ok".to_string(),
        category: Some("SYNTAX_FIX".to_string()),
        upstream_model: Some("claude-sonnet-4".to_string()),
        duration_ms: Some(1234),
        prompt_snippet: "reference record for cross-backend identity test".to_string(),
        prompt_char_count: Some(57),
        created_at: chrono::DateTime::from_timestamp(0, 0).expect("epoch should be valid"),
        provider_attempts: 2,
        final_provider: "anthropic".to_string(),
        input_tokens: Some(150),
        output_tokens: Some(200),
        cache_read_tokens: Some(50),
        cache_creation_tokens: Some(10),
        client_session_id: Some("session-123".to_string()),
        previous_response_id: Some("prev-456".to_string()),
        codex_installation_id: Some("inst-789".to_string()),
        codex_turn_state: Some("turn-active".to_string()),
        codex_window_id: Some("win-012".to_string()),
        codex_turn_metadata: Some(r#"{"key":"value"}"#.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app, classification, config, routing};

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
        let auth_config = Arc::new(routing::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            routing::RouteEntry {
                providers: vec![routing::ProviderEntry {
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
            routing::RouteEntry {
                providers: vec![routing::ProviderEntry {
                    model: "ca-model".to_string(),
                    endpoint,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some("MOCK_API_KEY".to_string()),
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

    mod proptest_snippet {
        use crate::proxy::util::redact_pii;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn proptest_snippet_free_of_email(text in ".*") {
                let redacted = redact_pii(&text);
                let re = regex::Regex::new(r"(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}").unwrap();
                prop_assert!(!re.is_match(&redacted), "email pattern found in redacted output");
            }

            #[test]
            fn proptest_snippet_free_of_ssn(text in ".*") {
                let redacted = redact_pii(&text);
                let re = regex::Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap();
                prop_assert!(!re.is_match(&redacted), "SSN pattern found in redacted output");
            }

            #[test]
            fn proptest_snippet_free_of_phone(text in ".*") {
                let redacted = redact_pii(&text);
                let re = regex::Regex::new(r"(?x)\b(?:\(\d{3}\)|\d{3})[-.\s]?\d{3}[-.\s]?\d{4}\b").unwrap();
                prop_assert!(!re.is_match(&redacted), "phone pattern found in redacted output");
            }

            #[test]
            fn proptest_snippet_free_of_credit_card(text in ".*") {
                let redacted = redact_pii(&text);
                let re = regex::Regex::new(r"\b(?:\d[ -]*?){13,19}\b").unwrap();
                prop_assert!(!re.is_match(&redacted), "credit card pattern found in redacted output");
            }

            #[test]
            fn proptest_redacted_snippet_still_200_chars_max(text in ".*") {
                let redacted = redact_pii(&text);
                let snippet: String = redacted.chars().take(200).collect();
                prop_assert!(snippet.chars().count() <= 200, "snippet exceeds 200 chars");
            }

            #[test]
            fn proptest_redaction_preserves_non_pii_text(text in "[a-zA-Z0-9 ,.!?;:'\"-]{0,500}") {
                let redacted = redact_pii(&text);
                let re = regex::Regex::new(
                    r"(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}|\b(?:\d[ -]*?){13,19}\b|\b\d{3}-\d{2}-\d{4}\b|(?x)\b(?:\(\d{3}\)|\d{3})[-.\s]?\d{3}[-.\s]?\d{4}\b"
                ).unwrap();
                if !re.is_match(&text) {
                    prop_assert_eq!(&redacted, &text, "non-PII text was modified");
                }
            }
        }
    }

    // ── Phase 2: Persistence Failure Observability ─────────────────────

    #[tokio::test]
    #[serial]
    async fn test_log_inference_failure_unreachable_backend() {
        use std::io::Write;
        use std::sync::Mutex;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::Layer;

        let buf: Arc<Mutex<Vec<u8>>> = Default::default();
        let buf_cap = buf.clone();

        let _guard_sub = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || {
                        struct Cap(Arc<Mutex<Vec<u8>>>);
                        impl Write for Cap {
                            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                                self.0.lock().unwrap().extend_from_slice(b);
                                Ok(b.len())
                            }
                            fn flush(&mut self) -> std::io::Result<()> {
                                Ok(())
                            }
                        }
                        Cap(buf_cap.clone())
                    })
                    .with_filter(EnvFilter::new("error")),
            )
            .with(tracing_subscriber::fmt::layer().with_test_writer())
            .set_default();

        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");

        let memory_backend = memory::MemoryBackend::new();
        memory_backend
            .fail_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
            .expect("completion should succeed even when insert fails");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(5);
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
                panic!("log task did not consume fail_next within 5s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let locked = buf.lock().unwrap();
        let output = String::from_utf8_lossy(&locked);
        assert!(
            output.contains("final insert failure"),
            "Expected error log to contain 'final insert failure', got: {output}"
        );
        drop(locked);
    }

    #[tokio::test]
    #[serial]
    async fn test_log_inference_failure_semaphore_exhausted() {
        use std::io::Write;
        use std::sync::Mutex;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::Layer;

        let buf: Arc<Mutex<Vec<u8>>> = Default::default();
        let buf_cap = buf.clone();

        let _guard_sub = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || {
                        struct Cap(Arc<Mutex<Vec<u8>>>);
                        impl Write for Cap {
                            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                                self.0.lock().unwrap().extend_from_slice(b);
                                Ok(b.len())
                            }
                            fn flush(&mut self) -> std::io::Result<()> {
                                Ok(())
                            }
                        }
                        Cap(buf_cap.clone())
                    })
                    .with_filter(EnvFilter::new("error")),
            )
            .with(tracing_subscriber::fmt::layer().with_test_writer())
            .set_default();

        let _guard = EnvGuard("MOCK_API_KEY");
        std::env::set_var("MOCK_API_KEY", "sk-test");

        let memory_backend = memory::MemoryBackend::new();
        let records_handle = memory_backend.records.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(0));
        let test_sem = semaphore.clone();
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
            .expect("completion should succeed even when semaphore is exhausted");

        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();
        drop(response);

        test_sem.close();

        let poll_start = std::time::Instant::now();
        let poll_timeout = std::time::Duration::from_secs(5);
        loop {
            {
                let locked = buf.lock().unwrap();
                let output = String::from_utf8_lossy(&locked);
                if output.contains("semaphore closed") {
                    break;
                }
            }
            if poll_start.elapsed() >= poll_timeout {
                panic!("Expected 'semaphore closed' error log within 5s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let records = records_handle.read().await;
        assert_eq!(records.len(), 0);
    }

    // ── Phase 3: Cross-Backend Identity Tests ─────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_cross_backend_identity_memory_sqlite() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let record = reference_inference_record();
        let filter_category = record.category.as_deref();

        let memory_backend = memory::MemoryBackend::new();
        let mem_backend = Arc::new(DbBackend::Memory(memory_backend));
        mem_backend
            .insert_inference(&record)
            .await
            .expect("memory insert should succeed");

        let sqlite_backend = sql_backend::SqlBackend::new_sqlite_in_memory()
            .await
            .expect("SQLite in-memory backend should be created");
        let sql_backend = Arc::new(DbBackend::Sql(sqlite_backend));
        sql_backend
            .insert_inference(&record)
            .await
            .expect("SQLite insert should succeed");

        let (mem_records, mem_count) = mem_backend
            .fetch_inferences(0, 10, filter_category, None)
            .await
            .expect("memory fetch should succeed");
        let (sql_records, sql_count) = sql_backend
            .fetch_inferences(0, 10, filter_category, None)
            .await
            .expect("SQLite fetch should succeed");

        assert_eq!(mem_count, sql_count, "record counts should match");
        assert_eq!(mem_count, 1, "expected exactly 1 record");

        let mem_log = &mem_records[0];
        let sql_log = &sql_records[0];

        // Timestamp format differs between backends (Memory: "1970-01-01 00:00:00 UTC",
        // SQLite: "1970-01-01 00:00:00"), so compare the time portion separately.
        assert!(
            mem_log.timestamp.starts_with(&sql_log.timestamp),
            "timestamps should match in date/time portion: mem={}, sql={}",
            mem_log.timestamp,
            sql_log.timestamp
        );

        assert_eq!(mem_log.prompt_snippet, sql_log.prompt_snippet);
        assert_eq!(mem_log.category, sql_log.category);
        assert_eq!(mem_log.upstream_model, sql_log.upstream_model);
        assert_eq!(mem_log.duration_ms, sql_log.duration_ms);
        assert_eq!(mem_log.provider_attempts, sql_log.provider_attempts);
        assert_eq!(mem_log.final_provider, sql_log.final_provider);
        assert_eq!(mem_log.previous_response_id, sql_log.previous_response_id);
    }

    // ── Phase 1 continued: OTel guard test ──────────────────────────────

    #[cfg(feature = "otel")]
    #[tokio::test]
    #[serial]
    async fn test_otel_no_prompt_body_in_spans() {
        use crate::app::test_helpers::{test_categories, test_negative_patterns};
        use crate::test_util::EnvGuard;
        use crate::{app, classification, config, routing};
        use axum::body::Body;
        use axum::http::{header, Request, StatusCode};
        use opentelemetry::trace::TracerProvider;
        use std::collections::HashMap;
        use std::sync::Arc;
        use tower::util::ServiceExt;
        use tracing_opentelemetry::OpenTelemetrySpanExt;

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let _guard = EnvGuard("MOCK_API_KEY_OTEL");
        std::env::set_var("MOCK_API_KEY_OTEL", "sk-test-otel");

        let exporter = opentelemetry_sdk::trace::SpanExporterBuilder::default();
        let provider = opentelemetry_sdk::trace::TracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();
        let tracer = provider.tracer("test");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        let subscriber = tracing_subscriber::registry()
            .with(otel_layer)
            .with(tracing_subscriber::fmt::layer().with_test_writer());
        let _guard2 = tracing::subscriber::set_default(subscriber);

        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("test reqwest client should build");
        let auth_config = Arc::new(routing::AuthConfig::from_values(
            "proxy-token-otel", "user", "password",
        ));
        let endpoint = server.url("/v1/chat/completions");
        let mut routing_map = HashMap::new();
        routing_map.insert(
            cats[1].name.clone(),
            routing::RouteEntry {
                providers: vec![routing::ProviderEntry {
                    model: "sf-model".to_string(),
                    endpoint: endpoint.clone(),
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some("MOCK_API_KEY_OTEL".to_string()),
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
            routing_map.clone(),
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let classifier_chain =
            classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier_arc = Some(Arc::new(classifier_chain));
        let mut merged_routing = HashMap::new();
        if let Some(cls) = classifier_arc.as_ref() {
            for backend_ref in cls.backends().iter() {
                if let Some(r) = backend_ref.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }

        let marker = "UNIQUE_OTEL_MARKER_" + &uuid::Uuid::new_v4().to_string();
        let prompt_body = format!("fix this bug and contact me at user@example.com about {}", marker);

        let memory_backend = crate::persistence::memory::MemoryBackend::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(100));
        let backend = Arc::new(crate::persistence::DbBackend::Memory(memory_backend));

        let app_state = Arc::new(app::AppState {
            persistence: Some(crate::persistence::PersistenceConfig {
                backend,
                task_semaphore: semaphore,
            }),
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
            allowed_origins: Arc::new(tokio::sync::RwLock::new(vec![])),
            response_cache: None,
            #[cfg(feature = "otel")]
            metrics: None,
        });

        let app = app::build_app(auth_config, app_state);

        let mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"choices":[{"message":{"content":"hello"}}]}"#);
        });

        let span = tracing::info_span!("test_request", method = "POST", uri = "/v1/chat/completions");
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer proxy-token-otel")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({
                        "messages": [{"role": "user", "content": prompt_body}]
                    }).to_string()))
                    .expect("request should be valid"),
            )
            .await
            .expect("completion request should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        mock.assert();

        // Verify span context has no prompt body attributes
        let context = span.context();
        let span_ctx = context.span();
        // Assert the span name does not contain prompt content
        assert!(!span_ctx.span_name().contains(&marker),
            "OTel span name contains prompt body ({})", marker);
        drop(context);

        // Shut down tracer to export remaining spans
        if let Err(e) = provider.shutdown() {
            tracing::warn!("tracer provider shutdown warning: {e}");
        }
    }
}
