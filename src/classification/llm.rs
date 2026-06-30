use std::sync::Arc;

use async_trait::async_trait;

use crate::classification::chain::IntentClassify;
use crate::classification::types::{ClassificationResult, ClassificationTier};
use crate::config::types::CategoryConfig;
use crate::config::types::{AuthProviderConfig, LlmClassifierConfig};

/// LLM-based intent classifier that fires when RegexClassifier returns Fallback.
pub struct LLMClassifier {
    client: reqwest::Client,
    pub model: String,
    pub endpoint: String,
    api_key_env: String,
    api_key: Arc<tokio::sync::RwLock<Arc<str>>>,
    provider_type: String,
    auth_providers: Arc<Vec<AuthProviderConfig>>,
    categories: Vec<CategoryConfig>,
    prompt_template: String,
    timeout: std::time::Duration,
    task_handle: tokio::task::AbortHandle,
}

impl Drop for LLMClassifier {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

impl LLMClassifier {
    /// Build an [`LLMClassifier`] from config, loading (or generating) the system prompt
    /// and spawning a background task that refreshes the API key from the environment every 60 s.
    pub fn new(
        config: LlmClassifierConfig,
        client: reqwest::Client,
        categories: Vec<CategoryConfig>,
        auth_providers: Arc<Vec<AuthProviderConfig>>,
    ) -> Self {
        let prompt_template = if let Some(ref path) = config.prompt_template_path {
            match std::fs::read_to_string(path) {
                Ok(contents) => contents,
                Err(e) => {
                    tracing::warn!("Failed to read prompt template at {}: {}", path, e);
                    build_llm_classifier_prompt(&categories)
                }
            }
        } else {
            build_llm_classifier_prompt(&categories)
        };

        let api_key = match std::env::var(&config.api_key_env) {
            Ok(k) => k,
            Err(_) => {
                tracing::warn!(
                    "LLM API key env {} not set; classifier will degrade",
                    config.api_key_env
                );
                String::new()
            }
        };
        let api_key_rwlock = Arc::new(tokio::sync::RwLock::new(Arc::from(api_key.as_str())));

        let classifier_api_key = api_key_rwlock.clone();
        let key_env = config.api_key_env.clone();

        // Spawn background refresh task for API key rotation with AbortHandle
        let task_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Ok(new_key) = std::env::var(&key_env) {
                    if !new_key.is_empty() {
                        let mut key = classifier_api_key.write().await;
                        if **key != new_key[..] {
                            tracing::debug!("LLM API key refreshed from env");
                            *key = Arc::from(new_key.as_str());
                        }
                    }
                }
            }
        })
        .abort_handle();

        Self {
            client,
            model: config.model,
            endpoint: config.endpoint,
            api_key_env: config.api_key_env,
            api_key: api_key_rwlock,
            provider_type: config.provider_type,
            auth_providers,
            categories,
            prompt_template,
            timeout: std::time::Duration::from_secs(config.timeout_secs),
            task_handle,
        }
    }

    /// Send the prompt to the configured LLM endpoint and parse the returned category.
    /// Always returns a valid [`ClassificationResult`]; falls back to `Fallback` tier on any error.
    async fn classify_async(&self, prompt: &str) -> ClassificationResult {
        // Build the request body
        let user_message = format!(
            "Classify this prompt into one of the categories above:\n\n{}",
            prompt
        );

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": self.prompt_template},
                {"role": "user", "content": user_message}
            ],
            "max_tokens": 20,
            "temperature": 0.0,
        });

        // Use pre-resolved API key
        let api_key = self.api_key.read().await.clone();

        if api_key.is_empty() {
            tracing::warn!(
                "LLM classifier API key environment variable {} is empty or unset",
                self.api_key_env
            );
        }

        let request = self
            .client
            .post(&self.endpoint)
            .timeout(self.timeout)
            .header("Content-Type", "application/json");

        let request = if !api_key.is_empty() {
            // The classifier's own LLM probe originates from Frugalis, not a
            // proxied client request, so there are no client headers to forward.
            let headers =
                auth_headers_for(&self.auth_providers, &self.provider_type, &api_key, &[]);
            let mut req = request;
            for (key, value) in headers {
                req = req.header(&key, &value);
            }
            req
        } else {
            request
        };

        // Send request
        match request.json(&body).send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    tracing::warn!("LLM classifier returned non-success: {}", response.status());
                    return ClassificationResult::fallback();
                }

                match response.json::<serde_json::Value>().await {
                    Ok(json) => self.parse_response(json),
                    Err(e) => {
                        tracing::warn!("LLM classifier failed to parse response: {}", e);
                        ClassificationResult::fallback()
                    }
                }
            }
            Err(e) => {
                tracing::warn!("LLM classifier request failed: {}", e);
                ClassificationResult::fallback()
            }
        }
    }

    /// Extract the category name from the LLM JSON response and look it up in `self.categories`.
    /// Returns `Fallback` if the response is missing, malformed, or contains an unknown category.
    fn parse_response(&self, json: serde_json::Value) -> ClassificationResult {
        let content = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str());

        match content {
            Some(response_text) => {
                // Parse category from response - look for known category names
                let response_upper = response_text.to_uppercase();
                for cat in &self.categories {
                    if response_upper.trim() == cat.name.to_uppercase() {
                        return ClassificationResult {
                            category: cat.name.clone(),
                            model: self.model.clone(),
                            tier: ClassificationTier::Regex,
                            providers: vec![],
                        };
                    }
                }
                // If no match found, return fallback
                tracing::warn!(
                    "LLM classifier returned unknown category: {}",
                    response_text
                );
                ClassificationResult::fallback()
            }
            None => {
                tracing::warn!("LLM classifier response missing choices");
                ClassificationResult::fallback()
            }
        }
    }
}

#[async_trait]
impl IntentClassify for LLMClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        self.classify_async(prompt).await
    }

    fn get_routing(
        &self,
    ) -> Option<&std::collections::HashMap<String, crate::routing::RouteEntry>> {
        None
    }
}

/// Build the system prompt for LLM classification from category configs.
pub fn build_llm_classifier_prompt(categories: &[CategoryConfig]) -> String {
    let mut prompt = String::from("You are an intent classifier for a coding assistant. ");
    prompt.push_str("Classify user prompts into exactly one of these categories:\n\n");

    for cat in categories {
        prompt.push_str(&format!("- {}: {}\n", cat.name, cat.description));
    }

    prompt.push_str("\nReturn ONLY the category name, nothing else. Examples:\n");
    for cat in categories {
        let example_hint = cat
            .description
            .split(',')
            .next()
            .unwrap_or(&cat.description);
        prompt.push_str(&format!("- \"{}\" -> {}\n", example_hint.trim(), cat.name));
    }

    prompt
}

// ── Auth Header Lookup ──

/// Maps a provider_type string and resolved API key to HTTP auth header tuples
/// using the configured auth provider list. Falls back to Bearer Authorization
/// for unknown or unconfigured provider types.
pub fn auth_headers_for(
    providers: &[AuthProviderConfig],
    provider_type: &str,
    api_key: &str,
    forward_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let pt = if provider_type.is_empty() {
        "openai_compatible"
    } else {
        provider_type
    };
    let client_version = forward_headers
        .iter()
        .find(|(n, _)| n == "anthropic-version")
        .map(|(_, v)| v.as_str());
    for provider in providers {
        if provider.type_ == pt {
            let mut headers = match (&provider.header, &provider.value_template) {
                (Some(header), Some(template)) => {
                    let value = template.replace("{api_key}", api_key);
                    vec![(header.clone(), value)]
                }
                _ => vec![],
            };
            if pt == "anthropic" {
                headers.push((
                    "anthropic-version".to_string(),
                    client_version.unwrap_or("2023-06-01").to_string(),
                ));
                append_forward_headers(&mut headers, forward_headers);
            }
            return headers;
        }
    }
    if pt == "anthropic" {
        let mut headers = vec![
            ("x-api-key".to_string(), api_key.to_string()),
            (
                "anthropic-version".to_string(),
                client_version.unwrap_or("2023-06-01").to_string(),
            ),
        ];
        append_forward_headers(&mut headers, forward_headers);
        return headers;
    }
    let mut openai_auth = vec![("authorization".into(), format!("Bearer {api_key}"))];
    // Append openai-* and x-openai-* client-forwarded headers for openai_compatible
    // and openai_responses providers (e.g. openai-beta, openai-organization).
    if pt == "openai_compatible" || pt == "openai_responses" {
        append_openai_forward_headers(&mut openai_auth, forward_headers);
    }
    openai_auth
}

/// Append client-forwarded `openai-*` / `x-openai-*` headers to `out`,
/// skipping any name already present (e.g. `authorization` is never overwritten).
fn append_openai_forward_headers(
    out: &mut Vec<(String, String)>,
    forward_headers: &[(String, String)],
) {
    for (name, value) in forward_headers {
        if !name.starts_with("openai-") && !name.starts_with("x-openai-") {
            continue;
        }
        if out.iter().any(|(n, _)| n == name.as_str()) {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
}

/// Append client-forwarded `anthropic-*` / `x-claude-code-*` headers to `out`,
/// skipping `anthropic-version` (the caller already emitted it with the
/// resolved value) and any name already present.
/// Only `anthropic-*` and `x-claude-code-*` prefixes are forwarded — other
/// provider-specific headers (e.g. `openai-*`) collected upstream are dropped here.
fn append_forward_headers(out: &mut Vec<(String, String)>, forward_headers: &[(String, String)]) {
    for (name, value) in forward_headers {
        if !name.starts_with("anthropic-") && !name.starts_with("x-claude-code-") {
            continue;
        }
        if name == "anthropic-version" {
            continue;
        }
        if out.iter().any(|(n, _)| n == name.as_str()) {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::types::ClassificationTier;
    use crate::config::types::AuthProviderConfig;
    use serial_test::serial;

    fn default_auth_providers() -> Vec<AuthProviderConfig> {
        vec![
            AuthProviderConfig {
                type_: "openai_compatible".into(),
                header: Some("authorization".into()),
                value_template: Some("Bearer {api_key}".into()),
            },
            AuthProviderConfig {
                type_: "anthropic".into(),
                header: Some("x-api-key".into()),
                value_template: Some("{api_key}".into()),
            },
            AuthProviderConfig {
                type_: "ollama".into(),
                header: None,
                value_template: None,
            },
            AuthProviderConfig {
                type_: "local".into(),
                header: None,
                value_template: None,
            },
            AuthProviderConfig {
                type_: "nvidia_nim".into(),
                header: Some("authorization".into()),
                value_template: Some("Bearer {api_key}".into()),
            },
        ]
    }

    #[test]
    fn auth_headers_for_openai_compatible() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "openai_compatible", "sk-123", &[]);
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer sk-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_empty_defaults_to_openai_compatible() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "", "sk-123", &[]);
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer sk-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_anthropic() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "anthropic", "sk-ant-123", &[]);
        assert_eq!(
            headers,
            vec![
                ("x-api-key".to_string(), "sk-ant-123".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
    }

    #[test]
    fn auth_headers_for_anthropic_no_provider_config() {
        let providers: Vec<AuthProviderConfig> = vec![];
        let headers = auth_headers_for(&providers, "anthropic", "sk-ant-fb", &[]);
        assert_eq!(
            headers,
            vec![
                ("x-api-key".to_string(), "sk-ant-fb".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
    }

    #[test]
    fn auth_headers_for_ollama() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "ollama", "dummy", &[]);
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_for_local() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "local", "dummy", &[]);
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_for_unknown() {
        let providers = default_auth_providers();
        let headers = auth_headers_for(&providers, "unknown_provider", "key", &[]);
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer key".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_anthropic_forwards_client_headers_and_prefers_version() {
        let providers = default_auth_providers();
        let forward = vec![
            ("anthropic-version".to_string(), "2024-10-22".to_string()),
            (
                "anthropic-beta".to_string(),
                "context-management-2025-09".to_string(),
            ),
            (
                "x-claude-code-session-id".to_string(),
                "sess-abc".to_string(),
            ),
        ];
        let headers = auth_headers_for(&providers, "anthropic", "sk-ant-123", &forward);
        assert!(
            headers.contains(&("anthropic-version".to_string(), "2024-10-22".to_string())),
            "client-supplied anthropic-version must be preferred, got {headers:?}"
        );
        assert!(
            !headers.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())),
            "default version must not also be emitted, got {headers:?}"
        );
        assert!(
            headers.contains(&(
                "anthropic-beta".to_string(),
                "context-management-2025-09".to_string()
            )),
            "anthropic-beta must be forwarded to an Anthropic upstream, got {headers:?}"
        );
        assert!(
            headers.contains(&(
                "x-claude-code-session-id".to_string(),
                "sess-abc".to_string()
            )),
            "x-claude-code-session-id must be forwarded, got {headers:?}"
        );
        assert!(
            headers.contains(&("x-api-key".to_string(), "sk-ant-123".to_string())),
            "resolved auth header must still be present, got {headers:?}"
        );
        let version_count = headers
            .iter()
            .filter(|(n, _)| n == "anthropic-version")
            .count();
        assert_eq!(
            version_count, 1,
            "anthropic-version must be emitted exactly once"
        );
    }

    #[test]
    fn auth_headers_for_anthropic_falls_back_to_default_version() {
        let providers = default_auth_providers();
        let forward = vec![(
            "anthropic-beta".to_string(),
            "prompt-caching-2024-07-31".to_string(),
        )];
        let headers = auth_headers_for(&providers, "anthropic", "sk-ant-123", &forward);
        assert!(
            headers.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())),
            "default version must be used when the client sent none, got {headers:?}"
        );
        assert!(
            headers.contains(&(
                "anthropic-beta".to_string(),
                "prompt-caching-2024-07-31".to_string()
            )),
            "anthropic-beta must still be forwarded without a client version, got {headers:?}"
        );
    }

    #[test]
    fn auth_headers_for_non_anthropic_drops_forward_headers() {
        let providers = default_auth_providers();
        let forward = vec![
            (
                "anthropic-beta".to_string(),
                "should-not-forward".to_string(),
            ),
            ("anthropic-version".to_string(), "2024-10-22".to_string()),
            (
                "x-claude-code-session-id".to_string(),
                "sess-abc".to_string(),
            ),
        ];
        let headers = auth_headers_for(&providers, "openai_compatible", "sk-123", &forward);
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer sk-123".to_string())],
            "non-anthropic providers must drop the entire forward set"
        );
    }

    fn test_categories() -> Vec<CategoryConfig> {
        vec![
            CategoryConfig {
                name: "FILE_READING".to_string(),
                description: "Reading, viewing, inspecting, searching, or navigating files or code".to_string(),
                threshold: 3,
                priority: 1,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "SYNTAX_FIX".to_string(),
                description: "Fixing bugs, errors, typos, compilation issues, or broken code".to_string(),
                threshold: 3,
                priority: 2,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "COMPLEX_REASONING".to_string(),
                description: "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization".to_string(),
                threshold: 3,
                priority: 3,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "CASUAL".to_string(),
                description: "Simple questions, greetings, general conversation, or short prompts".to_string(),
                threshold: 1,
                priority: 4,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
        ]
    }

    #[tokio::test]
    #[serial]
    async fn llm_classifier_success() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
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

        let config = LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = test_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats, Arc::new(vec![]));
        let result = llm.classify("fix this bug").await;

        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    #[serial]
    async fn llm_classifier_malformed_response() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": []
            }));
        });

        let config = LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = test_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats, Arc::new(vec![]));
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "unknown");
    }

    #[tokio::test]
    #[serial]
    async fn llm_classifier_network_error() {
        let config = LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: "http://127.0.0.1:1/nonexistent".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 1,
        };

        let cats = test_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats, Arc::new(vec![]));
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "unknown");
    }

    #[tokio::test]
    #[serial]
    async fn llm_classifier_unknown_category() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "content": "UNKNOWN_CATEGORY"
                        }
                    }
                ]
            }));
        });

        let config = LlmClassifierConfig {
            enabled: true,
            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = test_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats, Arc::new(vec![]));
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "unknown");
    }

    #[tokio::test]
    async fn build_llm_classifier_prompt_has_categories() {
        let cats = test_categories();
        let prompt = build_llm_classifier_prompt(&cats);

        assert!(prompt.contains("FILE_READING"));
        assert!(prompt.contains("SYNTAX_FIX"));
        assert!(prompt.contains("COMPLEX_REASONING"));
        assert!(prompt.contains("CASUAL"));
        assert!(prompt.contains("Examples:"));
    }
}
