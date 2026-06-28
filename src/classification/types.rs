use crate::config::routing::{ProviderEntry, DEFAULT_MODEL};
use serde::{Deserialize, Serialize};

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
