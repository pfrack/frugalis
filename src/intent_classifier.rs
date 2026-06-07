use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use regex::RegexSet;
use tracing::{info, warn};

// ── Public Types ──

#[derive(Clone)]
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,
    pub cost_per_1m_input_tokens: Option<f64>,
    pub provider_type: String,
    pub api_key_env: Option<String>,
}

/// Maps model names to their cost per 1M input tokens.
/// Defaults are hardcoded; routing.toml entries can override.
#[derive(Clone)]
pub struct ModelCosts {
    costs: HashMap<String, f64>,
}

impl crate::persistence::CostProvider for ModelCosts {
    fn get_cost(&self, model: &str) -> Option<f64> {
        self.get(model)
    }
}

impl ModelCosts {
    /// Look up a model's cost per 1M input tokens.
    pub fn get(&self, model: &str) -> Option<f64> {
        self.costs.get(model).copied()
    }

    /// An empty cost table — all model lookups return None.
    pub fn empty() -> Self {
        ModelCosts {
            costs: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub fn from_costs(costs: HashMap<String, f64>) -> Self {
        ModelCosts { costs }
    }
}

/// Hardcoded default costs per 1M input tokens for known models.
pub(crate) fn hardcoded_model_costs() -> HashMap<String, f64> {
    let mut m = HashMap::new();
    m.insert("claude-3.5-sonnet".to_string(), 3.00);
    m.insert("gpt-4o".to_string(), 2.50);
    m.insert("gpt-4o-mini".to_string(), 0.15);
    m.insert("deepseek-chat".to_string(), 0.14);
    m
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
    pub model_costs: ModelCosts,
    pub baseline_model: String,
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

const DEFAULT_MODEL: &str = "meta/llama-3.1-8b-instruct";
const DEFAULT_MODEL_COMPLEX: &str = "meta/llama-3.3-70b-instruct";
const DEFAULT_MODEL_READING: &str = "meta/llama-3.1-70b-instruct";
const DEFAULT_ENDPOINT: &str = "";

// ── Category Name Constants ──

const CAT_FILE_READING: &str = "FILE_READING";
const CAT_COMPLEX_REASONING: &str = "COMPLEX_REASONING";
const CAT_SYNTAX_FIX: &str = "SYNTAX_FIX";
const CAT_CASUAL: &str = "CASUAL";
const CAT_NEG: &str = "NEG";

// ── Pattern Counts ──

const FR_COUNT: usize = 12;
const CR_COUNT: usize = 16;
const SF_COUNT: usize = 11;
const CA_COUNT: usize = 5;
const NEG_COUNT: usize = 4;

// ── Weight Arrays ──

const FR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];
const CR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1];
const SF_WEIGHTS: &[u8] = &[3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];
const CA_WEIGHTS: &[u8] = &[3, 2, 1, 1, 1];

// ── Classification Thresholds ──

const SHORT_PROMPT_LEN: usize = 30;
const FR_THRESHOLD: u32 = 3;
const SF_THRESHOLD_HIGH: u32 = 4;
const SF_THRESHOLD_LOW: u32 = 3;
const CR_THRESHOLD: u32 = 3;
const CA_THRESHOLD: u32 = 1;

// ── Config Defaults ──

const ROUTING_CONFIG_DEFAULT: &str = "routing.toml";

// ── Pattern Constants ──
// Section 8 from research.md — 44 positive + 4 negative = 48 patterns

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
    NegativeMeta {
        suppressed: CAT_COMPLEX_REASONING,
        penalty: 2,
    },
    NegativeMeta {
        suppressed: CAT_COMPLEX_REASONING,
        penalty: 2,
    },
    NegativeMeta {
        suppressed: CAT_SYNTAX_FIX,
        penalty: 2,
    },
    NegativeMeta {
        suppressed: CAT_FILE_READING,
        penalty: 2,
    },
];

// ── Env-or-default helper ──

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

// ── Hardcoded routing defaults ──

fn hardcoded_routing() -> (HashMap<String, RouteEntry>, RouteEntry) {
    let endpoint = env_or_default(
        "NVIDIA_ENDPOINT",
        "https://integrate.api.nvidia.com/v1/chat/completions",
    );
    let mut routing = HashMap::new();
    routing.insert(
        CAT_COMPLEX_REASONING.to_string(),
        RouteEntry {
            model: env_or_default("DEFAULT_MODEL_COMPLEX", DEFAULT_MODEL_COMPLEX),
            endpoint: endpoint.clone(),
            cost_per_1m_input_tokens: None,
            provider_type: "nvidia_nim".to_string(),
            api_key_env: Some("NVIDIA_API_KEY".to_string()),
        },
    );
    routing.insert(
        CAT_FILE_READING.to_string(),
        RouteEntry {
            model: env_or_default("DEFAULT_MODEL_READING", DEFAULT_MODEL_READING),
            endpoint: endpoint.clone(),
            cost_per_1m_input_tokens: None,
            provider_type: "nvidia_nim".to_string(),
            api_key_env: Some("NVIDIA_API_KEY".to_string()),
        },
    );
    routing.insert(
        CAT_SYNTAX_FIX.to_string(),
        RouteEntry {
            model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
            endpoint: endpoint.clone(),
            cost_per_1m_input_tokens: None,
            provider_type: "nvidia_nim".to_string(),
            api_key_env: Some("NVIDIA_API_KEY".to_string()),
        },
    );
    routing.insert(
        CAT_CASUAL.to_string(),
        RouteEntry {
            model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
            endpoint: endpoint.clone(),
            cost_per_1m_input_tokens: None,
            provider_type: "nvidia_nim".to_string(),
            api_key_env: Some("NVIDIA_API_KEY".to_string()),
        },
    );
    let fallback = RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint,
        cost_per_1m_input_tokens: None,
        provider_type: "nvidia_nim".to_string(),
        api_key_env: Some("NVIDIA_API_KEY".to_string()),
    };
    (routing, fallback)
}

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

fn build_all_patterns() -> (Vec<&'static str>, Vec<PatternMeta>) {
    let mut patterns = Vec::new();
    let mut metadata = Vec::new();

    for (i, p) in FILE_READING.iter().enumerate() {
        patterns.push(*p);
        metadata.push(PatternMeta {
            category: CAT_FILE_READING,
            weight: FR_WEIGHTS[i],
        });
    }

    for (i, p) in COMPLEX_REASONING.iter().enumerate() {
        patterns.push(*p);
        metadata.push(PatternMeta {
            category: CAT_COMPLEX_REASONING,
            weight: CR_WEIGHTS[i],
        });
    }

    for (i, p) in SYNTAX_FIX.iter().enumerate() {
        patterns.push(*p);
        metadata.push(PatternMeta {
            category: CAT_SYNTAX_FIX,
            weight: SF_WEIGHTS[i],
        });
    }

    for (i, p) in CASUAL.iter().enumerate() {
        patterns.push(*p);
        metadata.push(PatternMeta {
            category: CAT_CASUAL,
            weight: CA_WEIGHTS[i],
        });
    }

    for p in NEGATIVE.iter() {
        patterns.push(*p);
        metadata.push(PatternMeta {
            category: CAT_NEG,
            weight: 0,
        });
    }

    (patterns, metadata)
}

// ── TOML Routing Loader ──

fn load_routing_from_file(path: &str) -> Result<HashMap<String, RouteEntry>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {}", path, e))?;
    let root: toml::Value =
        toml::from_str(&content).map_err(|e| format!("Invalid TOML in {}: {}", path, e))?;
    let table = root
        .as_table()
        .ok_or_else(|| format!("Root must be a table in {}", path))?;
    let mut routing = HashMap::new();
    for (key, value) in table {
        if key == "fallback" {
            continue;
        }
        let model = if let Some(m) = value.get("model").and_then(|v| v.as_str()) {
            m.to_string()
        } else {
            warn!(category = %key, "routing.toml missing 'model' for category; using DEFAULT_MODEL");
            env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)
        };
        let endpoint = value
            .get("endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_ENDPOINT)
            .to_string();
        let cost_per_1m_input_tokens = value
            .get("cost_per_1m_input_tokens")
            .and_then(|v| v.as_float());
        let provider_type = if let Some(pt) = value.get("provider_type").and_then(|v| v.as_str()) {
            pt.to_string()
        } else {
            warn!(category = %key, "routing.toml missing 'provider_type' for category; defaulting to openai_compatible (empty)");
            String::new()
        };
        let api_key_env = if let Some(ake) = value.get("api_key_env").and_then(|v| v.as_str()) {
            Some(ake.to_string())
        } else {
            warn!(category = %key, "routing.toml missing 'api_key_env' for category; no API key will be resolved");
            None
        };
        routing.insert(
            key.to_uppercase(),
            RouteEntry {
                model,
                endpoint,
                cost_per_1m_input_tokens,
                provider_type,
                api_key_env,
            },
        );
    }
    Ok(routing)
}

fn load_routing() -> (HashMap<String, RouteEntry>, RouteEntry) {
    let path =
        std::env::var("ROUTING_CONFIG_PATH").unwrap_or_else(|_| ROUTING_CONFIG_DEFAULT.to_string());
    let mut routing = match load_routing_from_file(&path) {
        Ok(r) => {
            info!("Routing: loaded from {path}");
            r
        }
        Err(e) => {
            warn!("{e}; using hardcoded routing defaults (no routing.toml)");
            return hardcoded_routing();
        }
    };
    let fallback_entry = routing.remove("FALLBACK").unwrap_or_else(|| RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint: String::new(),
        cost_per_1m_input_tokens: None,
        provider_type: String::new(),
        api_key_env: None,
    });
    (routing, fallback_entry)
}

// ── Implementations ──

impl ClassificationResult {
    /// Creates a CASUAL fallback result with Fallback tier.
    /// Used when no classifier chain is configured (graceful degradation).
    pub fn fallback() -> Self {
        ClassificationResult {
            category: CAT_CASUAL.to_string(),
            model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
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
    pub fn from_env() -> Result<Self, String> {
        let (patterns, metadata) = build_all_patterns();
        let set = RegexSet::new(&patterns).map_err(|e| format!("regex compilation failed: {e}"))?;
        let negative_start = FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT;
        let negative_idx = negative_start..(negative_start + NEG_COUNT);
        let (routing, fallback_entry) = load_routing();

        let baseline_model =
            std::env::var("BASELINE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_COMPLEX.to_string());

        // Merge routing.toml overrides into the hardcoded cost table.
        let mut costs = hardcoded_model_costs();
        for (_category, entry) in &routing {
            if let Some(override_cost) = entry.cost_per_1m_input_tokens {
                costs.insert(entry.model.clone(), override_cost);
            }
        }

        Ok(IntentClassifier {
            set,
            metadata,
            negative_idx,
            routing,
            fallback_entry,
            model_costs: ModelCosts { costs },
            baseline_model,
        })
    }

    #[cfg(test)]
    pub fn from_values(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry) -> Self {
        let (patterns, metadata) = build_all_patterns();
        let set = RegexSet::new(&patterns).expect("built-in patterns should always compile");
        let negative_start = FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT;
        let negative_idx = negative_start..(negative_start + NEG_COUNT);
        let mut costs = hardcoded_model_costs();
        for (_category, entry) in &routing {
            if let Some(override_cost) = entry.cost_per_1m_input_tokens {
                costs.insert(entry.model.clone(), override_cost);
            }
        }
        IntentClassifier {
            set,
            metadata,
            negative_idx,
            routing,
            fallback_entry,
            model_costs: ModelCosts { costs },
            baseline_model: "claude-3.5-sonnet".to_string(),
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

        // Short prompts (<30 chars, no matches) → CASUAL
        let all_zero = scores.values().all(|&s| s == 0);
        if sanitized.len() < SHORT_PROMPT_LEN && all_zero {
            return self.route_fallback(CAT_CASUAL);
        }

        // Check thresholds per Section 9 algorithm
        let fr = *scores.get(CAT_FILE_READING).unwrap_or(&0) >= FR_THRESHOLD;
        let sf = *scores.get(CAT_SYNTAX_FIX).unwrap_or(&0) >= SF_THRESHOLD_HIGH
            || (*scores.get(CAT_SYNTAX_FIX).unwrap_or(&0) >= SF_THRESHOLD_LOW
                && *scores.get(CAT_FILE_READING).unwrap_or(&0) == 0);
        let cr = *scores.get(CAT_COMPLEX_REASONING).unwrap_or(&0) >= CR_THRESHOLD;
        let ca = *scores.get(CAT_CASUAL).unwrap_or(&0) >= CA_THRESHOLD;

        let met = [fr, sf, cr, ca].iter().filter(|&&b| b).count();

        if met == 0 {
            return self.route_fallback(CAT_CASUAL);
        }
        if met >= 2 {
            return self.route_fallback(CAT_CASUAL);
        }

        if fr {
            return self.route_match(CAT_FILE_READING);
        }
        if sf {
            return self.route_match(CAT_SYNTAX_FIX);
        }
        if cr {
            return self.route_match(CAT_COMPLEX_REASONING);
        }
        self.route_match(CAT_CASUAL)
    }

    fn route_match(&self, category: &str) -> ClassificationResult {
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
        RegexClassifier::from_values(routing, fallback)
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

    // ── ModelCosts ───────────────────────────────────────────────────────────

    #[test]
    fn model_costs_returns_some_for_hardcoded_models() {
        let c = test_classifier();
        assert_eq!(c.model_costs.get("claude-3.5-sonnet"), Some(3.00));
        assert_eq!(c.model_costs.get("gpt-4o"), Some(2.50));
        assert_eq!(c.model_costs.get("gpt-4o-mini"), Some(0.15));
        assert_eq!(c.model_costs.get("deepseek-chat"), Some(0.14));
    }

    #[test]
    fn model_costs_returns_none_for_unknown_model() {
        let c = test_classifier();
        assert_eq!(c.model_costs.get("nonexistent-model"), None);
    }

    #[test]
    fn model_costs_override_via_route_entry() {
        let mut routing = HashMap::new();
        routing.insert(
            "COMPLEX_REASONING".to_string(),
            RouteEntry {
                model: "claude-3.5-sonnet".to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: Some(5.0),
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
        let classifier = RegexClassifier::from_values(routing, fallback);
        // claude-3.5-sonnet should be 5.0 (override), not 3.00 (hardcoded)
        assert_eq!(classifier.model_costs.get("claude-3.5-sonnet"), Some(5.0));
        // ca-model gets no override and is not in hardcoded table → None
        assert_eq!(classifier.model_costs.get("ca-model"), None);
    }

    #[test]
    fn model_costs_baseline_model_default() {
        let c = test_classifier();
        assert_eq!(c.baseline_model, "claude-3.5-sonnet");
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
