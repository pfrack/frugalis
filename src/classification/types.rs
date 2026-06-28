use crate::config::routing::{ProviderEntry, DEFAULT_MODEL};
use serde::{Deserialize, Serialize};

/// A single regex pattern entry with its weight for intent classification.
#[derive(Clone, Debug, Deserialize)]
pub struct PatternEntry {
    pub regex: String,
    #[serde(default = "default_weight")]
    pub weight: u8,
}

fn default_weight() -> u8 {
    1
}

/// Dual-threshold configuration for a category.
#[derive(Clone, Debug, Deserialize)]
pub struct DualThreshold {
    #[serde(default = "default_alt_score")]
    pub alt_score: u32,
    pub suppress_if_present: String,
}

fn default_alt_score() -> u32 {
    1
}

/// A negative suppression pattern configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct NegativePatternConfig {
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
/// - manual-test/run.sh — x-frugalis-category header (line 179)
/// - templates/dashboard/inferences.html — placeholder text (line 19)
///
/// Category names are a PUBLIC API contract. Renaming any value here
/// is a breaking change requiring updates to all listed consumers.
/// Names must stay [A-Z_]+ for compatibility with key.to_uppercase()
/// normalization in the routing config loader.
#[derive(Clone, Debug, Deserialize)]
pub struct CategoryConfig {
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

/// A chain of classifiers that tries each in order until one returns a non-Fallback result.
pub struct PatternMeta {
    pub category: String,
    pub weight: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FewShotExample {
    pub text: String,
    pub category: String,
    pub confidence: f64,
}

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
