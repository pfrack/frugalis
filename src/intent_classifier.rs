use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

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
/// Category names are a PUBLIC API contract. Renaming any value here
/// is a breaking change requiring updates to all listed consumers.
/// Names must stay [A-Z_]+ for compatibility with key.to_uppercase()
/// normalization in the routing config loader.
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
pub trait IntentClassify {
    fn classify(&self, prompt: &str) -> ClassificationResult;

    /// Returns a reference to this backend's routing table, if it has one.
    /// Used to construct the merged routing map in `AppState`.
    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
        None
    }
}

impl IntentClassify for RegexClassifier {
    fn classify(&self, prompt: &str) -> ClassificationResult {
        self.classify(prompt)
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

impl IntentClassify for ClassifierChain {
    fn classify(&self, prompt: &str) -> ClassificationResult {
        if self.backends.is_empty() {
            return ClassificationResult::fallback();
        }

        let mut last_result = None;
        for backend in &self.backends {
            let result = backend.classify(prompt);
            if result.tier != ClassificationTier::Fallback {
                return result;
            }
            last_result = Some(result);
        }
        // All backends returned Fallback; return the last one.
        last_result.unwrap_or_else(ClassificationResult::fallback)
    }
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
    RE.get_or_init(|| Regex::new(r"(?s)```[^`]*```").unwrap())
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
    pub fn classify(&self, prompt: &str) -> ClassificationResult {
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
        let mut routing = HashMap::new();
        routing.insert(
            "FILE_READING".to_string(),
            RouteEntry {
                model: "fr-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            "COMPLEX_REASONING".to_string(),
            RouteEntry {
                model: "cr-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            "SYNTAX_FIX".to_string(),
            RouteEntry {
                model: "sf-model".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
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
        RegexClassifier::from_values(routing, fallback, 30, hardcoded_categories())
    }

    #[test]
    fn intent_classify_file_reading() {
        let c = test_classifier();
        let result = c.classify("please read the file src/main.rs");
        assert_eq!(result.category, "FILE_READING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[test]
    fn intent_classify_complex_reasoning() {
        let c = test_classifier();
        let result = c.classify("architect a distributed rate limiter");
        assert_eq!(result.category, "COMPLEX_REASONING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[test]
    fn intent_classify_syntax_fix() {
        let c = test_classifier();
        let result = c.classify("fix this bug please");
        assert_eq!(result.category, "SYNTAX_FIX");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[test]
    fn intent_classify_casual() {
        let c = test_classifier();
        assert_eq!(c.classify("hello").category, "CASUAL");
    }

    #[test]
    fn intent_classify_empty_prompt() {
        let c = test_classifier();
        let result = c.classify("");
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[test]
    fn intent_classify_fallback_on_ambiguous() {
        let c = test_classifier();
        let result = c.classify("please read this file and fix this bug and compilation error");
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[test]
    fn intent_classify_negative_suppression() {
        let c = test_classifier();
        let result = c.classify("read the architecture document");
        assert_ne!(result.category, "COMPLEX_REASONING");
    }

    // ── ClassifierChain Tests ────────────────────────────────────────────────────

    struct StubClassifier {
        result: ClassificationResult,
    }

    impl IntentClassify for StubClassifier {
        fn classify(&self, _prompt: &str) -> ClassificationResult {
            self.result.clone()
        }
    }

    #[test]
    fn chain_returns_first_regex_match() {
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
        let result = chain.classify("any prompt");
        assert_eq!(result.category, "CAT1");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[test]
    fn chain_falls_through_to_next() {
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
        let result = chain.classify("prompt");
        assert_eq!(result.category, "COMPLEX_REASONING");
        assert_eq!(result.tier, ClassificationTier::Regex);
    }

    #[test]
    fn chain_returns_last_on_all_fallback() {
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
        let result = chain.classify("any");
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[test]
    fn chain_handles_empty_backends() {
        let chain = ClassifierChain::new(vec![]);
        let result = chain.classify("prompt");
        assert_eq!(result.tier, ClassificationTier::Fallback);
        assert_eq!(result.category, "CASUAL");
    }

    #[test]
    fn trait_boundary_compilation() {
        struct AnotherStub;
        impl IntentClassify for AnotherStub {
            fn classify(&self, _prompt: &str) -> ClassificationResult {
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
        let result = chain.classify("test");
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
}
