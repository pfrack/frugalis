use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use regex::Regex;
use regex::RegexSet;

#[allow(unused_imports)]
pub use crate::routing::{RouteEntry, ModelCosts, DEFAULT_MODEL, DEFAULT_MODEL_COMPLEX, DEFAULT_MODEL_READING};

/// Hardcoded default costs per 1M input tokens for known models.
pub(crate) fn hardcoded_model_costs() -> HashMap<String, f64> {
    let mut m = HashMap::new();
    m.insert("claude-3.5-sonnet".to_string(), 3.00);
    m.insert("gpt-4o".to_string(), 2.50);
    m.insert("gpt-4o-mini".to_string(), 0.15);
    m.insert("deepseek-chat".to_string(), 0.14);
    m
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
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct CategoryConfig {
    pub name: String,
    pub description: String,
    pub threshold: u32,
    pub priority: u8,
    pub model_env_var: Option<String>,
}

pub(crate) fn hardcoded_categories() -> Vec<CategoryConfig> {
    vec![
        CategoryConfig {
            name: "FILE_READING".to_string(),
            description: "Reading, viewing, inspecting, searching, or navigating files or code".to_string(),
            threshold: 3,
            priority: 1,
            model_env_var: Some("DEFAULT_MODEL_READING".to_string()),
        },
        CategoryConfig {
            name: "SYNTAX_FIX".to_string(),
            description: "Fixing bugs, errors, typos, compilation issues, or broken code".to_string(),
            threshold: 3,
            priority: 2,
            model_env_var: Some("DEFAULT_MODEL".to_string()),
        },
        CategoryConfig {
            name: "COMPLEX_REASONING".to_string(),
            description: "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization".to_string(),
            threshold: 3,
            priority: 3,
            model_env_var: Some("DEFAULT_MODEL_COMPLEX".to_string()),
        },
        CategoryConfig {
            name: "CASUAL".to_string(),
            description: "Simple questions, greetings, general conversation, or short prompts".to_string(),
            threshold: 1,
            priority: 4,
            model_env_var: Some("DEFAULT_MODEL".to_string()),
        },
    ]
}

#[derive(Clone)]
pub struct ClassificationResult {
    pub category: String,
    pub model: String,
    pub endpoint: String,
    pub tier: ClassificationTier,
    pub provider_type: String,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationTier {
    Regex,
    Fallback,
}

/// Trait for intent classification backends.
#[async_trait]
pub trait IntentClassify: Send + Sync {
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

use crate::config::LlmClassifierConfig;

/// LLM-based intent classifier that fires when RegexClassifier returns Fallback.
pub struct LLMClassifier {
    client: reqwest::Client,
    pub model: String,
    pub endpoint: String,
    api_key_env: String,
    api_key: String,
    provider_type: String,
    categories: Vec<CategoryConfig>,
    prompt_template: String,
    timeout: std::time::Duration,
}

impl LLMClassifier {
    pub fn new(config: LlmClassifierConfig, client: reqwest::Client, categories: Vec<CategoryConfig>) -> Self {
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

        let api_key = std::env::var(&config.api_key_env)
            .unwrap_or_else(|_| String::new());

        Self {
            client,
            model: config.model,
            endpoint: config.endpoint,
            api_key_env: config.api_key_env,
            api_key,
            provider_type: config.provider_type,
            categories,
            prompt_template,
            timeout: std::time::Duration::from_secs(config.timeout_secs),
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
            "response_format": { "type": "json_object" }
        });

        // Use pre-resolved API key
        let api_key = &self.api_key;

        if api_key.is_empty() {
            tracing::warn!("LLM classifier API key environment variable {} is empty or unset", self.api_key_env);
        }

        let request = self.client
            .post(&self.endpoint)
            .timeout(self.timeout)
            .header("Content-Type", "application/json");

        let request = if !api_key.is_empty() {
            let headers = auth_headers_for(&self.provider_type, api_key);
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
                    Ok(json) => {
                        self.parse_response(json)
                    }
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
                            endpoint: self.endpoint.clone(),
                            tier: ClassificationTier::Regex,
                            provider_type: self.provider_type.clone(),
                            api_key_env: Some(self.api_key_env.clone()),
                        };
                    }
                }
                // If no match found, return fallback
                tracing::warn!("LLM classifier returned unknown category: {}", response_text);
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
    // 4 few-shot examples, one per category
    prompt.push_str("- \"read the file src/main.rs\" -> FILE_READING\n");
    prompt.push_str("- \"fix this compile error\" -> SYNTAX_FIX\n");
    prompt.push_str("- \"design a distributed system\" -> COMPLEX_REASONING\n");
    prompt.push_str("- \"hello how are you\" -> CASUAL\n");

    prompt
}

// ── Internal Types ──

pub struct PatternMeta {
    pub category: &'static str,
    pub weight: u8,
}

struct NegativeMeta {
    suppressed: &'static str,
    penalty: u8,
}

// ── Defaults ──

// ── Pattern Counts ──

const NEG_COUNT: usize = 4;

// ── Weight Arrays ──

const FR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];
const CR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1];
const SF_WEIGHTS: &[u8] = &[3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];
const CA_WEIGHTS: &[u8] = &[3, 2, 1, 1, 1];

// ── Classification Thresholds ──

pub const SHORT_PROMPT_LEN: usize = 30;

// ── Pattern Constants ──

const FILE_READING: &[&str] = &[
    r"(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b",
    r"(?i)\b(?:show|display|print|cat)\s+(?:me\s+)?(?:the\s+)?(?:content|output)(?:\s+of)?",
    r"(?i)\b(?:[a-zA-Z0-9_\-./\\]+\.(?:rs|py|js|ts|go|java|c|cpp|h|to?ml|ya?ml|json|md|sql|sh|html))",
    r"(?i)\b(?:line|lines)\s+\d+",
    r"(?i)\b(?:what(?:\s+is|'s)\s+(?:in|inside))\s+(?:the\s+)?(?:file|directory|folder)",
    r"(?i)\b(?:look|go|navigate)\s+(?:at|through|to|into)\s+(?:the\s+)?(?:file|directory|code|source)",
    r"(?i)\b(?:list|ls|dir|tree)\s+(?:files|directories|contents|all|the)",
    r"(?i)\b(?:find|search|grep|locate|where\s+is)\s+(?:in|through|within|the)\s+(?:the\s+)?(?:file|code|project|source)",
    r"(?i)\b(?:where\s+is|locate\s+the|find\s+the)\s+(?:file|definition|function|class|module|struct|trait|impl)",
    r"(?i)\b(?:what\s+does\s+this\s+file|show\s+me\s+the\s+code|view\s+the\s+source|check\s+the\s+file)",
    r"(?i)\b(?:see|check|inspect|examine)\s+(?:the\s+)?(?:file|code|content|output|log)",
    r"(?i)\b(?:around\s+line|near\s+line|before\s+line|after\s+line)",
];

const COMPLEX_REASONING: &[&str] = &[
    r"(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)",
    r"(?i)\b(?:how\s+would\s+you\s+(?:design|architect|structure|build|implement|approach|solve))",
    r"(?i)\b(?:multi.?step|concurr|distributed|pipeline|scal(?:e|ing|able)|optimiz|bottleneck)",
    r"(?i)\b(?:redesign\s+the|rewrite\s+(?:the\s+)?(?:entire|whole)|audit\s+the\s+codebase|rearchitect)",
    r"(?i)\b(?:deep\s+dive|analy(?:ze|sis)|evaluat|compare\s+and\s+contrast|trade.?off|pros?\s+and\s+cons?\b)",
    r"(?i)\b(?:best\s+(?:practice|approach|way|pattern)|design\s+(?:a|the)\s+(?:system|architecture|api|database|schema|service))",
    r"(?i)\b(?:reason\s+about|explain\s+why|what(?:\s+is|'s)\s+the\s+(?:best|optimal|right|correct)\s+way)",
    r"(?i)\b(?:multi.?thread|async|event.?driven|microservice|rate\s+limit|load\s+balanc)",
    r"(?i)\b(?:integrat(?:e|ion)\s+(?:with|into|between)|migrat(?:e|ion)\s+(?:from|to|strategy)|orchestrat)",
    r"(?i)\b(?:performance\s+(?:bottleneck|issue|problem|analysis|tuning|profiling|regression)|memory\s+leak|race\s+condition|deadlock)",
    r"(?i)\b(?:can\s+you\s+(?:help\s+me\s+)?(?:design|plan|architect|think\s+(?:through|about)|reason\s+about))",
    r"(?i)\b(?:strategy|blueprint|roadmap|plan\s+(?:out|for)|approach\s+to)",
    r"(?i)\b(?:security\s+(?:audit|review|analysis)|threat\s+model)",
    r"(?i)\b(?:state\s+machine|algorithm\s+(?:design|complexity|analysis))",
    r"(?i)\b(?:dependenc(?:y|ies)\s+(?:graph|tree|injection|management)|coupling|cohesion)",
    r"(?i)\b(?:resilien(?:t|ce)|fault\s+toleran|circuit\s+breaker|retry\s+strategy)",
];

const SYNTAX_FIX: &[&str] = &[
    r"(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)",
    r"(?i)\b(?:doesn't\s+compile|won't\s+compile|doesn't\s+build|won't\s+build|compilation\s+error|syntax\s+error|build\s+error)",
    r"(?i)\b(?:type\s+error|linter?\s+(?:error|warning)|runtime\s+error|segfault|null\s+pointer|borrow\s+check)",
    r"(?i)\b(?:why\s+doesn't\s+this\s+work|what(?:\s+is|'s)\s+wrong\s+with|this\s+(?:is|seems)\s+broken)",
    r"(?i)\b(?:stack\s+trace|backtrace|panic|exception|traceback|\.unwrap)",
    r"(?i)\b(?:missing\s+(?:semicolon|import|parenthesis|brace|bracket|quote|comma|colon|use\s+statement|dependency|argument|parameter))",
    r"(?i)\b(?:undefined\s+(?:variable|function|symbol|reference|type|method)|not\s+found\s+in\s+this\s+scope|unresolved\s+reference)",
    r"(?i)\b(?:typo|misspell(?:ed|ing)?|copy.?paste\s+error|fat\s+finger)",
    r"(?i)\b(?:doesn't\s+work|is\s+broken|stopped\s+working|broke|isn't\s+working|not\s+working)",
    r"(?i)\b(?:here(?:'s|\s+is)\s+(?:the|an|my)\s+error|getting\s+(?:this|an)\s+error|seeing\s+(?:this|an)\s+error)",
    r"(?i)\b(?:error[:;].{0,40}\b(?:\d+|E\d{4}|0x[0-9a-fA-F]+)\b)",
];

const CASUAL: &[&str] = &[
    r"(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$",
    r"(?i)^\s*(?:thanks|thank\s+you|thx|ty|appreciate\s+it|cheers|thanks\s+a\s+lot)[\s!.,]*$",
    r"(?i)^\s*(?:what\s+is|what\s+are|what's|what\s+does|define|definition\s+of)\s+\w+(?:\s+\w+){0,2}\s*\??$",
    r"(?i)^\s*(?:how\s+(?:do|can|should)\s+I\s+\w+)(?:\s+\w+){0,4}\s*\??$",
    r"(?i)^\s*(?:ok|okay|got\s+it|understood|alright|cool|nice|good|great|sure|yes|no|maybe|idk)[\s!.,]*$",
];

const NEGATIVE: &[&str] = &[
    r"(?i)\b(?:read|show|display|cat|view|open)\s+(?:the|this|my|a)\s+\w*(?:architecture|design|system|pattern|refactor)",
    r"(?i)\b(?:fix|correct|repair)\s+(?:the|this|my)\s+(?:compile|syntax|typo|lint|warning|error)",
    r"(?i)\b(?:design|architect|refactor|rearchitect|restructure)\s+(?:a|the|an)\s+(?:fix|solution|remedy|patch|workaround)",
    r"(?i)\b(?:explain|describe|tell\s+me\s+about|what\s+do\s+you\s+think\s+about)\s+(?:the|this|that)\s+(?:file|code|class|module)",
];

// ── Negative suppression metadata (parallel to NEGATIVE patterns) ──

const NEGATIVE_META: &[NegativeMeta] = &[
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "SYNTAX_FIX",         penalty: 2 },
    NegativeMeta { suppressed: "FILE_READING",       penalty: 2 },
];

// ── Auth Header Lookup ──

/// Maps a provider_type string and resolved API key to HTTP auth header tuples.
/// Called by the upstream proxy (Change 4) to attach the correct auth header
/// before forwarding the request to the provider.
pub fn auth_headers_for(provider_type: &str, api_key: &str) -> Vec<(String, String)> {
    match provider_type {
        "openai_compatible" | "" => vec![("authorization".into(), format!("Bearer {api_key}"))],
        "anthropic" => vec![("x-api-key".into(), api_key.to_string())],
        "ollama" | "local" => vec![],
        _ => vec![("authorization".into(), format!("Bearer {api_key}"))],
    }
}

// ── Code-block regex (lazily compiled once) ──

fn code_block_re() -> &'static Regex {
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

fn build_all_patterns(categories: &[CategoryConfig]) -> (Vec<&'static str>, Vec<PatternMeta>, Range<usize>) {
    let mut patterns = Vec::new();
    let mut metadata = Vec::new();

    for config in categories {
        match config.name.as_str() {
            "FILE_READING" => {
                for (i, p) in FILE_READING.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "FILE_READING", weight: FR_WEIGHTS[i] });
                }
            }
            "COMPLEX_REASONING" => {
                for (i, p) in COMPLEX_REASONING.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "COMPLEX_REASONING", weight: CR_WEIGHTS[i] });
                }
            }
            "SYNTAX_FIX" => {
                for (i, p) in SYNTAX_FIX.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "SYNTAX_FIX", weight: SF_WEIGHTS[i] });
                }
            }
            "CASUAL" => {
                for (i, p) in CASUAL.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "CASUAL", weight: CA_WEIGHTS[i] });
                }
            }
            unknown => {
                tracing::warn!(category = %unknown, "CategoryConfig name has no pattern array");
            }
        }
    }

    let positive_count = metadata.len();
    let negative_start = positive_count;

    for p in NEGATIVE.iter() {
        patterns.push(*p);
        metadata.push(PatternMeta { category: "NEG", weight: 0 });
    }
    let negative_idx = negative_start..(negative_start + NEG_COUNT);

    (patterns, metadata, negative_idx)
}

fn fallback_category(categories: &[CategoryConfig]) -> &str {
    categories.iter()
        .max_by_key(|c| c.priority)
        .map(|c| c.name.as_str())
        .unwrap_or("CASUAL")
}

// ── Implementations ──

impl ClassificationResult {
    /// Creates a CASUAL fallback result with Fallback tier.
    /// Used when no classifier chain is configured (graceful degradation).
    pub fn fallback() -> Self {
        ClassificationResult {
            category: "CASUAL".to_string(),
            model: crate::config::env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
            endpoint: String::new(),
            tier: ClassificationTier::Fallback,
            provider_type: String::new(),
            api_key_env: None,
        }
    }
}

impl RegexClassifier {
    /// Build the classifier from built-in patterns and environment configuration.
    /// Always succeeds — regex compilation errors are the only failure mode.
    /// When routing.toml is missing, hardcoded defaults are used.
    pub fn from_env(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry, short_prompt_len: usize, categories: Vec<CategoryConfig>) -> Result<Self, String> {
        let (patterns, metadata, negative_idx) = build_all_patterns(&categories);
        let set = RegexSet::new(&patterns).map_err(|e| format!("regex compilation failed: {e}"))?;

        Ok(IntentClassifier {
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
    pub fn from_values(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry, short_prompt_len: usize, categories: Vec<CategoryConfig>) -> Self {
        let (patterns, metadata, negative_idx) = build_all_patterns(&categories);
        let set = RegexSet::new(&patterns).expect("built-in patterns should always compile");
        IntentClassifier {
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
                *scores.entry(meta.category).or_insert(0) += meta.weight as u32;
            }
        }

        // Apply negative suppression
        for &i in &matches {
            if self.negative_idx.contains(&i) {
                let neg_idx = i - self.negative_idx.start;
                if neg_idx < NEGATIVE_META.len() {
                    let neg = &NEGATIVE_META[neg_idx];
                    if let Some(score) = scores.get_mut(neg.suppressed) {
                        *score = score.saturating_sub(neg.penalty as u32);
                    }
                }
            }
        }

        // Short prompts (< short_prompt_len chars, no matches) → CASUAL
        let all_zero = scores.values().all(|&s| s == 0);
        if sanitized.len() < self.short_prompt_len && all_zero {
            return self.route_fallback(fallback_category(&self.categories));
        }

        // Check thresholds per config-driven algorithm
        let mut met: Vec<(&CategoryConfig, bool)> = self.categories.iter()
            .map(|c| {
                let score = *scores.get(c.name.as_str()).unwrap_or(&0);
                (c, score >= c.threshold)
            })
            .collect();

        // SF dual-threshold special case (SYNTAX_FIX only)
        let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
        let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
        let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);

        // Update the met flag for SYNTAX_FIX
        if let Some(entry) = met.iter_mut().find(|(c, _)| c.name == "SYNTAX_FIX") {
            entry.1 = sf_met;
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
        if category != "CASUAL" && !self.routing.contains_key(category) {
            tracing::warn!(%category, "route_match: category not in routing table — falling back");
        }
        let route = self.routing.get(category).unwrap_or(&self.fallback_entry);
        ClassificationResult {
            category: category.to_string(),
            model: route.model.clone(),
            endpoint: route.endpoint.clone(),
            tier: ClassificationTier::Regex,
            provider_type: route.provider_type.clone(),
            api_key_env: route.api_key_env.clone(),
        }
    }

    fn route_fallback(&self, category: &str) -> ClassificationResult {
        ClassificationResult {
            category: category.to_string(),
            model: self.fallback_entry.model.clone(),
            endpoint: self.fallback_entry.endpoint.clone(),
            tier: ClassificationTier::Fallback,
            provider_type: self.fallback_entry.provider_type.clone(),
            api_key_env: self.fallback_entry.api_key_env.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_classifier() -> RegexClassifier {
        let cats = hardcoded_categories();
        let mut routing = HashMap::new();
        routing.insert(
            cats[0].name.clone(),
            RouteEntry {
                model: "fr-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            cats[1].name.clone(),
            RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            cats[2].name.clone(),
            RouteEntry {
                model: "cr-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            cats[3].name.clone(),
            RouteEntry {
                model: "ca-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        let fallback = RouteEntry {
            model: "fallback-model".to_string(),
            endpoint: String::new(),
            cost_per_1m_input_tokens: None,
            provider_type: String::new(),
            api_key_env: None,
        };
        RegexClassifier::from_values(routing, fallback, 30, cats)
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
        let result = c.classify("please read this file and fix this bug and compilation error").await;
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
    async fn hardcoded_categories_match_test_routing_keys() {
        let classifier = test_classifier();
        let cats = hardcoded_categories();
        let routing_keys: std::collections::HashSet<&str> = classifier.routing.keys().map(|s| s.as_str()).collect();
        let cat_names: std::collections::HashSet<&str> = cats.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(routing_keys, cat_names, "test_classifier routing keys must match hardcoded_categories names");
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
                endpoint: "ep1".to_string(),
                tier: ClassificationTier::Regex,
                provider_type: "prov1".to_string(),
                api_key_env: None,
            },
        };
        let stub2 = StubClassifier {
            result: ClassificationResult {
                category: "CAT2".to_string(),
                model: "model2".to_string(),
                endpoint: "ep2".to_string(),
                tier: ClassificationTier::Regex,
                provider_type: "prov2".to_string(),
                api_key_env: None,
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
                endpoint: String::new(),
                tier: ClassificationTier::Fallback,
                provider_type: String::new(),
                api_key_env: None,
            },
        };
        let stub2 = StubClassifier {
            result: ClassificationResult {
                category: "COMPLEX_REASONING".to_string(),
                model: "model2".to_string(),
                endpoint: "ep2".to_string(),
                tier: ClassificationTier::Regex,
                provider_type: "prov2".to_string(),
                api_key_env: None,
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
                endpoint: String::new(),
                tier: ClassificationTier::Fallback,
                provider_type: String::new(),
                api_key_env: None,
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
        assert_eq!(result.category, "CASUAL");
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
                    endpoint: "stub-endpoint".to_string(),
                    tier: ClassificationTier::Regex,
                    provider_type: "stub".to_string(),
                    api_key_env: None,
                }
            }
        }
        // Verify it can be used as a trait object and wrapped in a chain
        let stub = Arc::new(AnotherStub) as Arc<dyn IntentClassify + Send + Sync>;
        let chain = ClassifierChain::new(vec![stub]);
        let result = chain.classify("test").await;
        assert_eq!(result.category, "STUB");
    }

    #[test]
    fn auth_headers_for_openai_compatible() {
        let headers = auth_headers_for("openai_compatible", "sk-123");
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer sk-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_empty_defaults_to_openai_compatible() {
        let headers = auth_headers_for("", "sk-123");
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer sk-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_anthropic() {
        let headers = auth_headers_for("anthropic", "sk-ant-123");
        assert_eq!(
            headers,
            vec![("x-api-key".to_string(), "sk-ant-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_for_ollama() {
        let headers = auth_headers_for("ollama", "dummy");
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_for_local() {
        let headers = auth_headers_for("local", "dummy");
        assert!(headers.is_empty());
    }

    #[test]
    fn auth_headers_for_unknown() {
        let headers = auth_headers_for("unknown_provider", "key");
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer key".to_string())]
        );
    }

    #[tokio::test]
    async fn llm_classifier_success() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions");
            then.status(200)
                .json_body(serde_json::json!({
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

            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = hardcoded_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");
        
        let llm = LLMClassifier::new(config, client, cats);
        let result = llm.classify("fix this bug").await;

        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[tokio::test]
    async fn llm_classifier_malformed_response() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions");
            then.status(200)
                .json_body(serde_json::json!({
                    "choices": []
                }));
        });

        let config = LlmClassifierConfig {

            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = hardcoded_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats);
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "CASUAL");
    }

    #[tokio::test]
    async fn llm_classifier_network_error() {
        let config = LlmClassifierConfig {

            model: "gpt-4o-mini".to_string(),
            endpoint: "http://127.0.0.1:1/nonexistent".to_string(), // Invalid endpoint
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 1,
        };

        let cats = hardcoded_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats);
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "CASUAL");
    }

    #[tokio::test]
    async fn llm_classifier_unknown_category() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions");
            then.status(200)
                .json_body(serde_json::json!({
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

            model: "gpt-4o-mini".to_string(),
            endpoint: server.url("/v1/chat/completions"),
            api_key_env: "OPENAI_API_KEY".to_string(),
            provider_type: "openai_compatible".to_string(),
            prompt_template_path: None,
            timeout_secs: 3,
        };

        let cats = hardcoded_categories();
        let client = reqwest::Client::new();
        std::env::set_var("OPENAI_API_KEY", "sk-test");

        let llm = LLMClassifier::new(config, client, cats);
        let result = llm.classify("test").await;

        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "CASUAL");
    }

    #[tokio::test]
    async fn build_llm_classifier_prompt_has_categories() {
        let cats = hardcoded_categories();
        let prompt = build_llm_classifier_prompt(&cats);

        assert!(prompt.contains("FILE_READING"));
        assert!(prompt.contains("SYNTAX_FIX"));
        assert!(prompt.contains("COMPLEX_REASONING"));
        assert!(prompt.contains("CASUAL"));
        assert!(prompt.contains("Examples:"));
    }
}
