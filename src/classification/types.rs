use crate::routing::{ProviderEntry, DEFAULT_MODEL};
use serde::{Deserialize, Serialize};

/// The output of any [`IntentClassify`] backend: resolved category, model, routing tier, and provider list.
#[derive(Clone)]
pub struct ClassificationResult {
    pub category: String,
    pub model: String,
    pub tier: ClassificationTier,
    pub providers: Vec<ProviderEntry>,
}

/// Which stage of the classifier pipeline produced this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationTier {
    Regex,
    FewShot,
    Llm,
    Fallback,
}

/// Metadata attached to a compiled regex pattern: which category it belongs to and its match weight.
pub struct PatternMeta {
    pub category: String,
    pub weight: u8,
}

/// A labelled training example for the few-shot classifier, persisted to YAML.
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
