use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use regex::Regex;
use regex::RegexSet;

pub use crate::routing::{
    ModelCosts, ProviderEntry, RouteEntry, DEFAULT_MODEL, DEFAULT_MODEL_COMPLEX,
};

/// A single regex pattern entry with its weight for intent classification.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PatternEntry {
    pub regex: String,
    #[serde(default = "default_weight")]
    pub weight: u8,
}

fn default_weight() -> u8 {
    1
}

/// Dual-threshold configuration for a category.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DualThreshold {
    #[serde(default = "default_alt_score")]
    pub alt_score: u32,
    pub suppress_if_present: String,
}

fn default_alt_score() -> u32 {
    1
}

/// A negative suppression pattern configuration.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NegativePatternConfig {
    pub regex: String,
    pub suppressed: String,
    #[serde(default = "default_penalty")]
    pub penalty: u8,
}

fn default_penalty() -> u8 {
    2
}

/// Single source of truth for intent category definitions.
/// Consumed by RegexClassifier (patterns, thresholds, routing) and
/// LLMClassifier (prompt template descriptions).
///
/// External files hardcoding category name strings:
/// - routing_examples/routing-*.toml (4 files) — section names
/// - openapi/completions.yaml — enum constraint values (line 44, 111)
/// - manual-test/run.sh — x-cerebrum-category header (line 179)
/// - templates/dashboard/inferences.html — placeholder text (line 19)
///
/// Category names are a PUBLIC API contract. Renaming any value here
/// is a breaking change requiring updates to all listed consumers.
/// Names must stay [A-Z_]+ for compatibility with key.to_uppercase()
/// normalization in the routing config loader.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CategoryConfig {
    #[serde(default)]
    pub name: String,
    pub description: String,
    #[serde(default = "default_threshold")]
    pub threshold: u32,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub patterns: Vec<PatternEntry>,
    #[serde(default)]
    pub patterns_file: Option<String>,
    pub dual_threshold: Option<DualThreshold>,
}

fn default_threshold() -> u32 {
    1
}
fn default_priority() -> u8 {
    99
}

#[derive(Clone)]
pub struct ClassificationResult {
    pub category: String,
    pub model: String,
    pub tier: ClassificationTier,
    pub providers: Vec<ProviderEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationTier {
    Regex,
    FewShot,
    Fallback,
}

/// Trait for intent classification backends.
#[async_trait]
pub trait IntentClassify: Send + Sync + 'static {
    async fn classify(&self, prompt: &str) -> ClassificationResult;

    /// Returns a reference to this backend's routing table, if it has one.
    /// Used to construct the merged routing map in `AppState`.
    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
        None
    }
}

#[async_trait]
impl IntentClassify for RegexClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        self.classify_internal(prompt)
    }

    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
        Some(&self.routing)
    }
}

pub struct RegexClassifier {
    pub set: RegexSet,
    pub metadata: Vec<PatternMeta>,
    pub negative_idx: Range<usize>,
    pub routing: HashMap<String, RouteEntry>,
    pub fallback_entry: RouteEntry,
    pub short_prompt_len: usize,
    pub categories: Vec<CategoryConfig>,
    pub negative_patterns: Vec<NegativePatternConfig>,
}

// Backward compatibility alias until Phase 3 updates consumers
pub type IntentClassifier = RegexClassifier;

/// A chain of classifiers that tries each in order until one returns a non-Fallback result.
pub struct ClassifierChain {
    backends: Vec<Arc<dyn IntentClassify + Send + Sync>>,
}

impl ClassifierChain {
    pub fn new(backends: Vec<Arc<dyn IntentClassify + Send + Sync>>) -> Self {
        Self { backends }
    }

    /// Get the slice of backend classifiers.
    pub fn backends(&self) -> &[Arc<dyn IntentClassify + Send + Sync>] {
        &self.backends
    }
}

#[async_trait]
impl IntentClassify for ClassifierChain {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        if self.backends.is_empty() {
            return ClassificationResult::fallback();
        }

        let mut last_result = None;
        for backend in &self.backends {
            let result = backend.classify(prompt).await;
            if result.tier != ClassificationTier::Fallback {
                return result;
            }
            last_result = Some(result);
        }
        // All backends returned Fallback; return the last one.
        last_result.unwrap_or_else(ClassificationResult::fallback)
    }
}

// ── LLM Classifier ────────────────────────────────────────────────────────────

use crate::config::{AuthProviderConfig, LlmClassifierConfig};

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
            // The classifier's own LLM probe originates from Cerebrum, not a
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

    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
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

// ── Internal Types ──

pub struct PatternMeta {
    pub category: String,
    pub weight: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FewShotExample {
    pub text: String,
    pub category: String,
    pub confidence: f64,
}

// ── Auth Header Lookup ──

/// Maps a provider_type string and resolved API key to HTTP auth header tuples
/// using the configured auth provider list. Falls back to Bearer Authorization
/// for unknown or unconfigured provider types.
///
/// This function is the SINGLE emission point for the full upstream header
/// set, including client-forwarded headers. For `provider_type == "anthropic"`
/// it appends the `anthropic-*` / `x-claude-code-*` entries from
/// `forward_headers` (so beta-gated Claude Code features reach the upstream),
/// prefers a client-supplied `anthropic-version` over the hard-coded
/// `2023-06-01` protocol constant, and de-dupes so every name is emitted at
/// most once. For non-anthropic providers the forward set is dropped entirely —
/// those headers are meaningless to OpenAI / Ollama upstreams and forwarding
/// them would only add noise (the plan's contract: drop `anthropic-*` for
/// non-anthropic).
///
/// `forward_headers` is the output of `collect_forward_headers` (see main.rs),
/// which already excludes `authorization` / `x-api-key`; combined with callers
/// applying only the set returned here, a client can never overwrite the
/// resolved upstream credential.
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
    // Prefer a client-supplied anthropic-version over the protocol constant so
    // Claude Code can pin a newer API version without a Cerebrum change. We
    // resolve it once here and skip the raw entry when appending the forward
    // set, so the version header is emitted exactly once.
    let client_version = forward_headers
        .iter()
        .find(|(n, _)| n == "anthropic-version")
        .map(|(_, v)| v.as_str());
    // Anthropic protocol constant: every Anthropic request must carry the
    // version header. Append it to whatever auth header the user configured
    // (or the hard-coded fallback below) so callers don't need to manage two
    // parallel config entries.
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
    // No matching provider config — hard-coded fallback for "anthropic",
    // generic Bearer for everything else.
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
    vec![("authorization".into(), format!("Bearer {api_key}"))]
}

/// Append client-forwarded `anthropic-*` / `x-claude-code-*` headers to `out`,
/// skipping `anthropic-version` (the caller already emitted it with the
/// resolved value) and any name already present. De-duplication guarantees a
/// client value can never duplicate a header the proxy set and keeps emission
/// deterministic when a name appears more than once in the inbound request.
fn append_forward_headers(out: &mut Vec<(String, String)>, forward_headers: &[(String, String)]) {
    for (name, value) in forward_headers {
        if name == "anthropic-version" {
            continue;
        }
        if out.iter().any(|(n, _)| n == name.as_str()) {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
}

// ── Code-block regex (lazily compiled once) ──

pub(crate) fn code_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```[^`]*```").expect("code_block_re regex must be valid"))
}

// ── Prompt Sanitization ──

fn sanitize(text: &str) -> String {
    let lower = text.to_lowercase();
    let no_blocks = code_block_re().replace_all(&lower, " ");
    let collapsed: Vec<&str> = no_blocks.split_whitespace().collect();
    collapsed.join(" ")
}

// ── Pattern assembly ──

fn build_all_patterns(
    categories: &[CategoryConfig],
    negative_patterns: &[NegativePatternConfig],
) -> (Vec<String>, Vec<PatternMeta>, Range<usize>) {
    let mut patterns = Vec::new();
    let mut metadata = Vec::new();

    for config in categories {
        for entry in &config.patterns {
            patterns.push(entry.regex.clone());
            metadata.push(PatternMeta {
                category: config.name.clone(),
                weight: entry.weight,
            });
        }
    }

    let positive_count = metadata.len();
    let negative_start = positive_count;

    for neg in negative_patterns {
        patterns.push(neg.regex.clone());
        metadata.push(PatternMeta {
            category: "NEG".to_string(),
            weight: 0,
        });
    }
    let negative_idx = negative_start..(negative_start + negative_patterns.len());

    (patterns, metadata, negative_idx)
}

fn fallback_category(categories: &[CategoryConfig]) -> &str {
    categories
        .iter()
        .max_by_key(|c| c.priority)
        .map(|c| c.name.as_str())
        .unwrap_or("unknown")
}

// ── Implementations ──

impl ClassificationResult {
    /// Creates a fallback result with Fallback tier.
    /// Used when no classifier chain is configured (graceful degradation).
    pub fn fallback() -> Self {
        ClassificationResult {
            category: "unknown".to_string(),
            model: DEFAULT_MODEL.to_string(),
            tier: ClassificationTier::Fallback,
            providers: vec![],
        }
    }
}

impl RegexClassifier {
    /// Build the classifier from built-in patterns and environment configuration.
    /// Always succeeds — regex compilation errors are the only failure mode.
    /// When routing.toml is missing, hardcoded defaults are used.
    pub fn from_env(
        routing: HashMap<String, RouteEntry>,
        fallback_entry: RouteEntry,
        short_prompt_len: usize,
        categories: Vec<CategoryConfig>,
        negative_patterns: &[NegativePatternConfig],
    ) -> Result<Self, String> {
        let (patterns, metadata, negative_idx) = build_all_patterns(&categories, negative_patterns);
        let set = RegexSet::new(&patterns).map_err(|e| format!("regex compilation failed: {e}"))?;

        Ok(IntentClassifier {
            negative_patterns: negative_patterns.to_vec(),
            set,
            metadata,
            negative_idx,
            routing,
            fallback_entry,
            short_prompt_len,
            categories,
        })
    }

    #[cfg(test)]
    pub fn from_values(
        routing: HashMap<String, RouteEntry>,
        fallback_entry: RouteEntry,
        short_prompt_len: usize,
        categories: Vec<CategoryConfig>,
        negative_patterns: &[NegativePatternConfig],
    ) -> Self {
        let (patterns, metadata, negative_idx) = build_all_patterns(&categories, negative_patterns);
        let set = RegexSet::new(&patterns).expect("built-in patterns should always compile");
        IntentClassifier {
            negative_patterns: negative_patterns.to_vec(),
            set,
            metadata,
            negative_idx,
            routing,
            fallback_entry,
            short_prompt_len,
            categories,
        }
    }

    /// Classify a prompt string into one of the 4 intent categories.
    /// Never fails — returns Fallback tier for unmatched or ambiguous prompts.
    /// This is the synchronous implementation (used by the async wrapper).
    pub fn classify_internal(&self, prompt: &str) -> ClassificationResult {
        let sanitized = sanitize(prompt);
        let matches: Vec<usize> = self.set.matches(&sanitized).into_iter().collect();

        // Tally scores by category (FR, CR, SF, CA)
        let mut scores: HashMap<&str, u32> = HashMap::new();
        for &i in &matches {
            if i < self.negative_idx.start {
                let meta = &self.metadata[i];
                *scores.entry(meta.category.as_str()).or_insert(0) += meta.weight as u32;
            }
        }

        // Apply negative suppression
        for &i in &matches {
            if self.negative_idx.contains(&i) {
                let neg_idx = i - self.negative_idx.start;
                if neg_idx < self.negative_patterns.len() {
                    let neg = &self.negative_patterns[neg_idx];
                    if let Some(score) = scores.get_mut(neg.suppressed.as_str()) {
                        *score = score.saturating_sub(neg.penalty as u32);
                    }
                }
            }
        }

        // Short prompts (< short_prompt_len chars, no matches) → CASUAL
        let all_zero = scores.values().all(|&s| s == 0);
        if sanitized.chars().count() < self.short_prompt_len && all_zero {
            return self.route_fallback(fallback_category(&self.categories));
        }

        // Check thresholds per config-driven algorithm
        let mut met: Vec<(&CategoryConfig, bool)> = self
            .categories
            .iter()
            .map(|c| {
                let score = *scores.get(c.name.as_str()).unwrap_or(&0);
                (c, score >= c.threshold)
            })
            .collect();

        // Apply dual_threshold overrides from config
        for (config, met_flag) in met.iter_mut() {
            if let Some(dt) = &config.dual_threshold {
                let score = *scores.get(config.name.as_str()).unwrap_or(&0);
                let suppress_score = *scores.get(dt.suppress_if_present.as_str()).unwrap_or(&0);
                *met_flag =
                    score >= dt.alt_score || (score >= config.threshold && suppress_score == 0);
            }
        }

        let met_count = met.iter().filter(|(_, m)| *m).count();

        if met_count == 0 {
            return self.route_fallback(fallback_category(&self.categories));
        }
        if met_count >= 2 {
            return self.route_fallback(fallback_category(&self.categories));
        }

        // Sort by priority (lower = higher), pick first that met
        met.sort_by_key(|(c, _)| c.priority);
        for (config, is_met) in &met {
            if *is_met {
                return self.route_match(&config.name);
            }
        }

        self.route_fallback(fallback_category(&self.categories))
    }

    fn route_match(&self, category: &str) -> ClassificationResult {
        if !self.routing.contains_key(category) {
            tracing::warn!(%category, "route_match: category not in routing table — falling back");
        }
        let route = self.routing.get(category).unwrap_or(&self.fallback_entry);
        ClassificationResult {
            category: category.to_string(),
            model: route.primary().model.clone(),
            tier: ClassificationTier::Regex,
            providers: route.providers.clone(),
        }
    }

    fn route_fallback(&self, category: &str) -> ClassificationResult {
        ClassificationResult {
            category: category.to_string(),
            model: self.fallback_entry.primary().model.clone(),
            tier: ClassificationTier::Fallback,
            providers: self.fallback_entry.providers.clone(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    //! Shared test utilities for the classifier subsystem.
    //!
    //! Exposed to `#[cfg(test)]` modules in other files of this crate
    //! (e.g. `src/main.rs` integration tests) via `pub(crate)`.
    //! Production code never sees this module.

    use std::sync::Arc;

    use super::*;

    /// Test-only `IntentClassify` impl that records how many times
    /// `classify()` is invoked and returns a configurable
    /// `ClassificationResult`. The chain tests use this to prove
    /// which backend fired, because `LLMClassifier` returns
    /// `tier: ClassificationTier::Regex` on success and the
    /// `ClassificationTier` enum has only `Regex | FewShot | Fallback`
    /// (no `Llm` variant) — tier inspection cannot distinguish
    /// "regex matched" from "LLM matched".
    pub struct CountingClassifier {
        pub counter: Arc<std::sync::atomic::AtomicUsize>,
        pub result: ClassificationResult,
    }

    #[async_trait]
    impl IntentClassify for CountingClassifier {
        async fn classify(&self, _prompt: &str) -> ClassificationResult {
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.result.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn test_categories() -> Vec<CategoryConfig> {
        vec![
            CategoryConfig {
                name: "FILE_READING".to_string(),
                description: "Reading, viewing, inspecting, searching, or navigating files or code".to_string(),
                threshold: 3,
                priority: 1,
                patterns: vec![
                    PatternEntry {
                        regex: r"(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "SYNTAX_FIX".to_string(),
                description: "Fixing bugs, errors, typos, compilation issues, or broken code".to_string(),
                threshold: 3,
                priority: 2,
                patterns: vec![
                    PatternEntry {
                        regex: r"(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "COMPLEX_REASONING".to_string(),
                description: "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization".to_string(),
                threshold: 3,
                priority: 3,
                patterns: vec![
                    PatternEntry {
                        regex: r"(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "CASUAL".to_string(),
                description: "Simple questions, greetings, general conversation, or short prompts".to_string(),
                threshold: 1,
                priority: 4,
                patterns: vec![
                    PatternEntry {
                        regex: r"(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$".to_string(),
                        weight: 3,
                    },
                ],
                patterns_file: None,
                dual_threshold: None,
            },
        ]
    }

    fn test_negative_patterns() -> Vec<NegativePatternConfig> {
        vec![
            NegativePatternConfig {
                regex: r"(?i)\b(?:read|show|display|cat|view|open)\s+(?:the|this|my|a)\s+\w*(?:architecture|design|system|pattern|refactor)".to_string(),
                suppressed: "COMPLEX_REASONING".to_string(),
                penalty: 2,
            },
        ]
    }

    fn test_classifier() -> RegexClassifier {
        let cats = test_categories();
        let neg = test_negative_patterns();
        let mut routing = HashMap::new();
        routing.insert(
            cats[0].name.clone(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "fr-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            cats[1].name.clone(),
            RouteEntry {
                providers: vec![ProviderEntry {
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
            cats[2].name.clone(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "cr-model".to_string(),
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
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "ca-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fallback-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        RegexClassifier::from_values(routing, fallback, 30, cats, &neg)
    }

    #[tokio::test]
    async fn intent_classify_file_reading() {
        let c = test_classifier();
        let result = c.classify("please read the file src/main.rs").await;
        assert_eq!(result.category, "FILE_READING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn intent_classify_complex_reasoning() {
        let c = test_classifier();
        let result = c.classify("architect a distributed rate limiter").await;
        assert_eq!(result.category, "COMPLEX_REASONING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn intent_classify_syntax_fix() {
        let c = test_classifier();
        let result = c.classify("fix this bug please").await;
        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn intent_classify_casual() {
        let c = test_classifier();
        assert_eq!(c.classify("hello").await.category, "CASUAL");
    }

    #[tokio::test]
    async fn intent_classify_empty_prompt() {
        let c = test_classifier();
        let result = c.classify("").await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn intent_classify_fallback_on_ambiguous() {
        let c = test_classifier();
        let result = c
            .classify("please read this file and fix this bug and compilation error")
            .await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn intent_classify_negative_suppression() {
        let c = test_classifier();
        let result = c.classify("read the architecture document").await;
        assert_ne!(result.category, "COMPLEX_REASONING");
    }

    #[tokio::test]
    async fn test_routing_keys_match_categories() {
        let classifier = test_classifier();
        let cats = test_categories();
        let routing_keys: std::collections::HashSet<&str> =
            classifier.routing.keys().map(|s| s.as_str()).collect();
        let cat_names: std::collections::HashSet<&str> =
            cats.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            routing_keys, cat_names,
            "test_classifier routing keys must match category names"
        );
    }

    // ── ClassifierChain Tests ────────────────────────────────────────────────────

    struct StubClassifier {
        result: ClassificationResult,
    }

    #[async_trait]
    impl IntentClassify for StubClassifier {
        async fn classify(&self, _prompt: &str) -> ClassificationResult {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn chain_returns_first_regex_match() {
        let stub1 = StubClassifier {
            result: ClassificationResult {
                category: "CAT1".to_string(),
                model: "model1".to_string(),
                tier: ClassificationTier::Regex,
                providers: vec![],
            },
        };
        let stub2 = StubClassifier {
            result: ClassificationResult {
                category: "CAT2".to_string(),
                model: "model2".to_string(),
                tier: ClassificationTier::Regex,
                providers: vec![],
            },
        };
        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2)]);
        let result = chain.classify("any prompt").await;
        assert_eq!(result.category, "CAT1");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn chain_falls_through_to_next() {
        let stub1 = StubClassifier {
            result: ClassificationResult {
                category: "CASUAL".to_string(),
                model: "fallback1".to_string(),
                tier: ClassificationTier::Fallback,
                providers: vec![],
            },
        };
        let stub2 = StubClassifier {
            result: ClassificationResult {
                category: "COMPLEX_REASONING".to_string(),
                model: "model2".to_string(),
                tier: ClassificationTier::Regex,
                providers: vec![],
            },
        };
        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2)]);
        let result = chain.classify("prompt").await;
        assert_eq!(result.category, "COMPLEX_REASONING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn chain_returns_last_on_all_fallback() {
        let stub1 = StubClassifier {
            result: ClassificationResult::fallback(),
        };
        let stub2 = StubClassifier {
            result: ClassificationResult {
                category: "CASUAL".to_string(),
                model: "last".to_string(),
                tier: ClassificationTier::Fallback,
                providers: vec![],
            },
        };
        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2)]);
        let result = chain.classify("any").await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn chain_handles_empty_backends() {
        let chain = ClassifierChain::new(vec![]);
        let result = chain.classify("prompt").await;
        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "unknown");
    }

    #[tokio::test]
    async fn trait_boundary_compilation() {
        struct AnotherStub;
        #[async_trait]
        impl IntentClassify for AnotherStub {
            async fn classify(&self, _prompt: &str) -> ClassificationResult {
                ClassificationResult {
                    category: "STUB".to_string(),
                    model: "stub-model".to_string(),
                    tier: ClassificationTier::Regex,
                    providers: vec![],
                }
            }
        }
        // Verify it can be used as a trait object and wrapped in a chain
        let stub = Arc::new(AnotherStub) as Arc<dyn IntentClassify + Send + Sync>;
        let chain = ClassifierChain::new(vec![stub]);
        let result = chain.classify("test").await;
        assert_eq!(result.category, "STUB");
    }

    // ── 3-backend chain tests (Risk #1 contract) ────────────────────────────
    // These tests prove the chain's "first-non-Fallback wins, later backends
    // not called" and "last-Fallback returned when all fail" contracts with
    // three backends, using CountingClassifier for side-effect observation
    // (tier inspection cannot distinguish regex-tier from LLM-tier matches).

    #[tokio::test]
    async fn chain_3_backend_short_circuits_when_first_matches() {
        use crate::intent_classifier::test_util::CountingClassifier;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let counter3 = Arc::new(AtomicUsize::new(0));

        let stub1 = CountingClassifier {
            counter: counter1.clone(),
            result: ClassificationResult {
                category: "FIRST".to_string(),
                model: "first-model".to_string(),
                tier: ClassificationTier::Regex,
                providers: vec![],
            },
        };
        let stub2 = CountingClassifier {
            counter: counter2.clone(),
            result: ClassificationResult::fallback(),
        };
        let stub3 = CountingClassifier {
            counter: counter3.clone(),
            result: ClassificationResult::fallback(),
        };

        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2), Arc::new(stub3)]);
        let result = chain.classify("any prompt").await;

        assert_eq!(result.category, "FIRST");
        assert_eq!(result.tier, ClassificationTier::Regex);
        assert_eq!(
            counter1.load(Ordering::SeqCst),
            1,
            "first backend should be called once"
        );
        assert_eq!(
            counter2.load(Ordering::SeqCst),
            0,
            "second backend should NOT be called when first matches"
        );
        assert_eq!(
            counter3.load(Ordering::SeqCst),
            0,
            "third backend should NOT be called when first matches"
        );
    }

    #[tokio::test]
    async fn chain_3_backend_short_circuits_when_middle_matches() {
        use crate::intent_classifier::test_util::CountingClassifier;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let counter3 = Arc::new(AtomicUsize::new(0));

        let stub1 = CountingClassifier {
            counter: counter1.clone(),
            result: ClassificationResult::fallback(),
        };
        let stub2 = CountingClassifier {
            counter: counter2.clone(),
            result: ClassificationResult {
                category: "MIDDLE".to_string(),
                model: "middle-model".to_string(),
                tier: ClassificationTier::FewShot,
                providers: vec![],
            },
        };
        let stub3 = CountingClassifier {
            counter: counter3.clone(),
            result: ClassificationResult {
                category: "LAST".to_string(),
                model: "last-model".to_string(),
                tier: ClassificationTier::Regex,
                providers: vec![],
            },
        };

        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2), Arc::new(stub3)]);
        let result = chain.classify("any prompt").await;

        assert_eq!(result.category, "MIDDLE");
        assert_eq!(result.tier, ClassificationTier::FewShot);
        assert_eq!(
            counter1.load(Ordering::SeqCst),
            1,
            "first backend should be called (returns Fallback)"
        );
        assert_eq!(
            counter2.load(Ordering::SeqCst),
            1,
            "middle backend should be called once"
        );
        assert_eq!(
            counter3.load(Ordering::SeqCst),
            0,
            "third backend should NOT be called when middle matches"
        );
    }

    #[tokio::test]
    async fn chain_3_backend_returns_last_on_all_fallback() {
        use crate::intent_classifier::test_util::CountingClassifier;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let counter3 = Arc::new(AtomicUsize::new(0));

        let stub1 = CountingClassifier {
            counter: counter1.clone(),
            result: ClassificationResult::fallback(),
        };
        let stub2 = CountingClassifier {
            counter: counter2.clone(),
            result: ClassificationResult::fallback(),
        };
        let stub3 = CountingClassifier {
            counter: counter3.clone(),
            result: ClassificationResult {
                category: "LAST_FALLBACK".to_string(),
                model: "last-fb-model".to_string(),
                tier: ClassificationTier::Fallback,
                providers: vec![],
            },
        };

        let chain = ClassifierChain::new(vec![Arc::new(stub1), Arc::new(stub2), Arc::new(stub3)]);
        let result = chain.classify("any prompt").await;

        assert_eq!(result.category, "LAST_FALLBACK");
        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(
            counter1.load(Ordering::SeqCst),
            1,
            "all backends should be called when all return Fallback"
        );
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
        assert_eq!(counter3.load(Ordering::SeqCst), 1);
    }

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
        // Hard-coded fallback: even with no [[auth_providers]] entry, an
        // Anthropic provider_type must still emit x-api-key + the protocol
        // version header so the upstream accepts the request.
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
        // Client pinned a newer anthropic-version and sent an anthropic-beta
        // capability plus a Claude Code session id. The proxy must forward the
        // beta + session id verbatim, prefer the client's version over the
        // 2023-06-01 default, and emit the version exactly once.
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
        // No client version on the wire -> default 2023-06-01; beta still
        // forwarded so GA features keep working even when the client omits the
        // version header.
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
        // anthropic-* is meaningless to an OpenAI-compatible upstream and must
        // be dropped entirely so we never forward Anthropic-only noise.
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
            endpoint: "http://127.0.0.1:1/nonexistent".to_string(), // Invalid endpoint
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

    // ── Engine-generality tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_engine_works_with_custom_categories() {
        let cats = vec![
            CategoryConfig {
                name: "DATABASE".to_string(),
                description: "Database queries, schema migrations, SQL optimization".to_string(),
                threshold: 2,
                priority: 1,
                patterns: vec![PatternEntry {
                    regex: r"(?i)\b(?:select|insert|update|delete|create\s+table|alter|drop)\b"
                        .to_string(),
                    weight: 2,
                }],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "DEPLOYMENT".to_string(),
                description: "Deploy, CI/CD, Docker, Kubernetes".to_string(),
                threshold: 2,
                priority: 2,
                patterns: vec![PatternEntry {
                    regex: r"(?i)\b(?:deploy|docker|kubernetes|ci/cd|pipeline)\b".to_string(),
                    weight: 2,
                }],
                patterns_file: None,
                dual_threshold: None,
            },
        ];
        let neg = vec![];
        let mut routing = HashMap::new();
        routing.insert(
            "DATABASE".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "db-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            "DEPLOYMENT".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "dep-model".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fb-model".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let c = RegexClassifier::from_values(routing, fallback, 30, cats, &neg);
        let result = c.classify("SELECT * FROM users").await;
        assert_eq!(result.category, "DATABASE");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn test_engine_works_with_custom_dual_threshold() {
        let cats = vec![
            CategoryConfig {
                name: "ALPHA".to_string(),
                description: "Alpha category".to_string(),
                threshold: 3,
                priority: 1,
                patterns: vec![PatternEntry {
                    regex: r"(?i)\balpha\b".to_string(),
                    weight: 3,
                }],
                patterns_file: None,
                dual_threshold: Some(DualThreshold {
                    alt_score: 2,
                    suppress_if_present: "BETA".to_string(),
                }),
            },
            CategoryConfig {
                name: "BETA".to_string(),
                description: "Beta category".to_string(),
                threshold: 1,
                priority: 2,
                patterns: vec![PatternEntry {
                    regex: r"(?i)\bbeta\b".to_string(),
                    weight: 1,
                }],
                patterns_file: None,
                dual_threshold: None,
            },
        ];
        let neg = vec![];
        let mut routing = HashMap::new();
        routing.insert(
            "ALPHA".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "a".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            "BETA".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "b".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fb".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let c = RegexClassifier::from_values(routing, fallback, 30, cats, &neg);
        // "alpha beta" gives ALPHA score=3 (meets threshold=3), BETA score=1 (meets threshold=1)
        // Dual threshold: ALPHA alt_score=2, suppress_if_present=BETA
        // ALPHA: score=3 >= alt_score=2 → met
        // BETA: score=1 >= threshold=1 → met
        // With both met, fallback to lowest priority (BETA)
        let result = c.classify("alpha beta").await;
        assert_eq!(result.category, "BETA");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn test_engine_works_with_no_categories() {
        let cats = vec![];
        let neg = vec![];
        let routing = HashMap::new();
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fb".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let c = RegexClassifier::from_values(routing, fallback, 30, cats, &neg);
        let result = c.classify("anything").await;
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn test_engine_works_with_custom_negative_patterns() {
        let cats = vec![
            CategoryConfig {
                name: "CODING".to_string(),
                description: "Coding questions".to_string(),
                threshold: 2,
                priority: 1,
                patterns: vec![PatternEntry {
                    regex: r"(?i)\b(?:code|program|function)\b".to_string(),
                    weight: 2,
                }],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "GENERAL".to_string(),
                description: "General questions".to_string(),
                threshold: 1,
                priority: 2,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
        ];
        let neg = vec![NegativePatternConfig {
            regex: r"(?i)\bcode\b".to_string(),
            suppressed: "CODING".to_string(),
            penalty: 3,
        }];
        let mut routing = HashMap::new();
        routing.insert(
            "CODING".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "c".to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fb".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        let c = RegexClassifier::from_values(routing, fallback, 30, cats, &neg);
        // "code" matches CODING pattern (score=2), but negative pattern penalizes CODING by 3 → score=0
        // CODING threshold=2 not met → fallback to GENERAL
        let result = c.classify("write some code").await;
        assert_eq!(result.category, "GENERAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }
}
