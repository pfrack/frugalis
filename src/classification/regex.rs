use std::collections::HashMap;
use std::ops::Range;

use async_trait::async_trait;
use regex::RegexSet;

use crate::classification::chain::IntentClassify;
use crate::classification::types::{ClassificationResult, ClassificationTier, PatternMeta};
use crate::config::types::{CategoryConfig, NegativePatternConfig};

#[cfg(test)]
use crate::config::types::{DualThreshold, PatternEntry};
use crate::config::routing::RouteEntry;

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

#[async_trait]
impl IntentClassify for RegexClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        self.classify_internal(prompt)
    }

    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
        Some(&self.routing)
    }
}

// ── Prompt Sanitization ──

/// Lowercase, strip code blocks, and collapse whitespace so pattern matching is stable.
fn sanitize(text: &str) -> String {
    let lower = text.to_lowercase();
    let no_blocks = crate::classification::code_block_re().replace_all(&lower, " ");
    let collapsed: Vec<&str> = no_blocks.split_whitespace().collect();
    collapsed.join(" ")
}

// ── Pattern assembly ──

/// Flatten all positive category patterns and negative suppression patterns into a single
/// flat vec suitable for [`RegexSet`], and record the index range of the negative entries.
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

/// Return the name of the lowest-priority (highest `priority` value) category,
/// used as the default fallback when no pattern fires.
fn fallback_category(categories: &[CategoryConfig]) -> &str {
    categories
        .iter()
        .max_by_key(|c| c.priority)
        .map(|c| c.name.as_str())
        .unwrap_or("unknown")
}

// ── Implementations ──

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

    /// Route a matched category: look up its entry in the routing table.
    /// Logs a warning and uses the fallback entry if the category has no route.
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

    /// Route to the fallback entry, preserving the supplied category name.
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
mod tests {
    use super::*;
    use crate::config::routing::{ProviderEntry, RouteEntry};

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
