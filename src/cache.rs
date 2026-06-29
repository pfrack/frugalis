use std::sync::atomic::{AtomicU64, Ordering};

/// A cached upstream response entry, stored keyed by request body SHA-256.
#[derive(Clone, Debug)]
pub struct CachedEntry {
    pub body: String,
    pub status: u16,
}

/// Snapshot of cache statistics for the dashboard.
#[derive(Debug)]
pub struct CacheStats {
    pub hit_count: u64,
    pub miss_count: u64,
    pub entry_count: u64,
    pub max_entries: u64,
    pub ttl_secs: u64,
}

/// Thread-safe response cache backed by `moka::sync::Cache`.
///
/// Tracks hit/miss counts via lock-free atomics. TTL and capacity eviction
/// are handled by moka's internal housekeeping thread.
pub struct ResponseCache {
    cache: moka::sync::Cache<String, CachedEntry>,
    hits: AtomicU64,
    misses: AtomicU64,
    max_entries: u64,
    ttl_secs: u64,
}

impl ResponseCache {
    pub fn new(ttl_secs: u64, max_entries: u64) -> Self {
        let cache = moka::sync::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(ttl_secs))
            .max_capacity(max_entries)
            .build();
        Self {
            cache,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            max_entries,
            ttl_secs,
        }
    }

    /// Look up a cached entry by its SHA-256 hex key.
    /// Increments `hits` atomically on success or `misses` on lookup miss.
    pub fn get(&self, key: &str) -> Option<CachedEntry> {
        if let Some(entry) = self.cache.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(entry)
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Store an entry in the cache. moka handles TTL and capacity eviction automatically.
    pub fn put(&self, key: String, entry: CachedEntry) {
        self.cache.insert(key, entry);
    }

    /// Return a snapshot of current cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hit_count: self.hits.load(Ordering::Relaxed),
            miss_count: self.misses.load(Ordering::Relaxed),
            entry_count: self.cache.entry_count(),
            max_entries: self.max_entries,
            ttl_secs: self.ttl_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::app::test_helpers::{test_app, test_categories, test_negative_patterns};
    use crate::test_util::EnvGuard;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    #[test]
    fn test_cache_get_put() {
        let cache = ResponseCache::new(60, 10);
        let entry = CachedEntry {
            body: "test body".to_string(),
            status: 200,
        };
        cache.put("key1".to_string(), entry.clone());
        let retrieved = cache.get("key1");
        assert!(retrieved.is_some());
        let r = retrieved.unwrap();
        assert_eq!(r.body, "test body");
        assert_eq!(r.status, 200);
    }

    #[test]
    fn test_cache_hit_miss_counters() {
        let cache = ResponseCache::new(60, 10);
        let entry = CachedEntry {
            body: "body".to_string(),
            status: 200,
        };
        cache.put("hit".to_string(), entry);

        let _ = cache.get("hit");
        let _ = cache.get("miss");

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = ResponseCache::new(60, 10);
        assert!(cache.get("nonexistent").is_none());
        let stats = cache.stats();
        assert_eq!(stats.miss_count, 1);
    }

    #[test]
    fn test_cache_stats() {
        let cache = ResponseCache::new(120, 50);
        let entry = CachedEntry {
            body: "b".to_string(),
            status: 200,
        };
        cache.put("a".to_string(), entry);
        let _ = cache.get("a");
        let _ = cache.get("a");
        let _ = cache.get("b");

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 2);
        assert_eq!(stats.miss_count, 1);
        // moka's entry_count() is approximate; at most one entry was inserted
        assert!(
            stats.entry_count <= 1,
            "entry_count={} should be <= 1",
            stats.entry_count
        );
        assert_eq!(stats.max_entries, 50);
        assert_eq!(stats.ttl_secs, 120);
    }

    #[test]
    fn test_cache_max_capacity() {
        let cache = ResponseCache::new(60, 2);
        for i in 0..4 {
            cache.put(
                format!("key{i}"),
                CachedEntry {
                    body: format!("body{i}"),
                    status: 200,
                },
            );
        }
        // moka evicts oldest entries; at most max_capacity remain
        assert!(
            cache.stats().entry_count <= 2,
            "cache should evict entries beyond max_capacity"
        );
    }

    fn test_app_with_cache(
        ttl_secs: u64,
        max_entries: u64,
    ) -> (Router, httpmock::MockServer, Arc<ResponseCache>) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        use std::collections::HashMap;
        let cats = test_categories();
        let server = httpmock::MockServer::start();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("test reqwest client should build");
        let auth_config = Arc::new(crate::auth::AuthConfig::from_values(
            "proxy-token",
            "user",
            "password",
        ));
        let endpoint = server.url("/v1/chat/completions");
        let env = "TEST_CACHE_PROXY";
        let mut routing = HashMap::new();
        routing.insert(
            cats[1].name.clone(),
            crate::config::routing::RouteEntry {
                providers: vec![crate::config::routing::ProviderEntry {
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
            crate::config::routing::RouteEntry {
                providers: vec![crate::config::routing::ProviderEntry {
                    model: "ca-model".to_string(),
                    endpoint,
                    provider_type: "openai_compatible".to_string(),
                    api_key_env: Some(env.to_string()),
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = crate::config::routing::RouteEntry {
            providers: vec![crate::config::routing::ProviderEntry {
                model: "fallback-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let regex_classifier = crate::classification::regex::RegexClassifier::from_values(
            routing,
            fallback,
            30,
            cats,
            &test_negative_patterns(),
        );
        let classifier_chain =
            crate::classification::chain::ClassifierChain::new(vec![Arc::new(regex_classifier)]);
        let classifier_arc = Some(Arc::new(classifier_chain));
        let mut merged_routing = std::collections::HashMap::new();
        if let Some(cls) = classifier_arc.as_ref() {
            for backend in cls.backends().iter() {
                if let Some(r) = backend.get_routing() {
                    merged_routing.extend(r.clone());
                }
            }
        }
        let response_cache = Arc::new(ResponseCache::new(ttl_secs, max_entries));
        let app_state = Arc::new(crate::app::AppState {
            persistence: None,
            classifier: classifier_arc,
            fewshot_classifier: None,
            routing: Arc::new(tokio::sync::RwLock::new(merged_routing)),
            model_costs: Arc::new(tokio::sync::RwLock::new(
                crate::config::routing::ModelCosts::empty(),
            )),
            baseline_model: Arc::new(tokio::sync::RwLock::new(String::new())),
            classify_db_log: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http_client: Some(client),
            max_upstream_body_bytes: Arc::new(tokio::sync::RwLock::new(10_485_760)),
            keepalive_interval_secs: Arc::new(tokio::sync::RwLock::new(15)),
            request_body_limit_bytes: 10_485_760,
            streaming_channel_capacity: 32,
            dashboard_config: crate::config::types::DashboardConfig::default(),
            auth_providers: Arc::new(vec![]),
            allowed_origins: Arc::new(tokio::sync::RwLock::new(vec![])),
            response_cache: Some(response_cache.clone()),
            #[cfg(feature = "otel")]
            metrics: None,
        });
        let app = crate::app::build_app(auth_config, app_state);
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
        assert_eq!(mock.hits(), 1);
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
        assert_eq!(mock.hits(), 2);
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
        assert_eq!(mock.hits(), 2);
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
        assert_eq!(mock.hits(), 2);
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
        assert!(body_str.contains("not configured"));
    }
}
