use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tokio::fs;

use crate::classification::chain::IntentClassify;
use crate::classification::types::{ClassificationResult, ClassificationTier, FewShotExample};
use crate::config::routing::{RouteEntry, DEFAULT_MODEL};
use crate::config::types::FewShotConfig;

pub struct FewShotClassifier {
    vocabulary: RwLock<dashmap::DashMap<String, usize>>,
    intent_patterns: RwLock<dashmap::DashMap<String, Vec<Vec<f64>>>>,
    training_data: Arc<tokio::sync::RwLock<Vec<FewShotExample>>>,
    routing: HashMap<String, RouteEntry>,
    fallback_entry: RouteEntry,
    config: FewShotConfig,
    retraining_in_progress: AtomicBool,
}

impl FewShotClassifier {
    /// Build a [`FewShotClassifier`] from config, loading bootstrap examples from the
    /// embedded YAML and merging any persisted training data from disk.
    pub fn new(
        config: FewShotConfig,
        routing: HashMap<String, RouteEntry>,
        fallback_entry: RouteEntry,
    ) -> Self {
        let bootstrap: Vec<FewShotExample> =
            serde_yaml::from_str(include_str!("../../data/fewshot_bootstrap.yaml"))
                .expect("bootstrap YAML must be valid");

        // Load persisted training data; merge with bootstrap (persisted wins on text match)
        let persisted = Self::load_training_data(&config.data_path);
        let mut merged = bootstrap.clone();
        let persisted_len = persisted.len();
        if persisted_len > 0 {
            for pe in persisted {
                if let Some(pos) = merged.iter().position(|e| e.text == pe.text) {
                    merged[pos] = pe;
                } else {
                    merged.push(pe);
                }
            }
            tracing::info!(
                "Few-shot: loaded {} persisted examples ({} total after merge)",
                persisted_len,
                merged.len()
            );
        }

        let training_data = Arc::new(tokio::sync::RwLock::new(merged.clone()));

        let classifier = Self {
            vocabulary: RwLock::new(dashmap::DashMap::new()),
            intent_patterns: RwLock::new(dashmap::DashMap::new()),
            training_data,
            routing,
            fallback_entry,
            config,
            retraining_in_progress: AtomicBool::new(false),
        };

        classifier.retrain_internal(&merged);
        classifier
    }

    /// Lowercase, strip code blocks, and collapse whitespace — mirrors [`RegexClassifier`] sanitization.
    fn preprocess(text: &str) -> String {
        let lower = text.to_lowercase();
        let no_blocks = crate::classification::code_block_re().replace_all(&lower, " ");
        let collapsed: Vec<&str> = no_blocks.split_whitespace().collect();
        collapsed.join(" ")
    }

    /// Convert preprocessed text into a TF-weighted feature vector over the current vocabulary.
    fn extract_features(&self, text: &str) -> Vec<f64> {
        let vocab = self.vocabulary.read().unwrap();
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let total_words = tokens.len();
        if total_words == 0 {
            return vec![0.0; self.config.feature_dimensions];
        }
        let mut word_counts: HashMap<usize, f64> = HashMap::new();
        for token in &tokens {
            if let Some(idx) = vocab.get(*token) {
                *word_counts.entry(*idx).or_insert(0.0) += 1.0;
            }
        }
        let mut features = vec![0.0; self.config.feature_dimensions];
        for (idx, count) in word_counts {
            if idx < self.config.feature_dimensions {
                features[idx] = count / total_words as f64;
            }
        }
        features
    }

    /// Score each intent category against `input_features` using max cosine similarity
    /// over all stored pattern vectors for that category.
    fn score_categories(&self, input_features: &[f64]) -> HashMap<String, f64> {
        let mut scores = HashMap::new();
        for entry in self.intent_patterns.read().unwrap().iter() {
            let category = entry.key();
            let patterns = entry.value();
            let mut max_score = 0.0_f64;
            for pattern_vec in patterns {
                let sim = cosine_similarity(input_features, pattern_vec);
                if sim > max_score {
                    max_score = sim;
                }
            }
            scores.insert(category.clone(), max_score);
        }
        scores
    }

    /// Check whether `preprocessed` exactly matches any training example; returns
    /// `(category, confidence)` if found, `None` otherwise.
    fn exact_match_in(&self, preprocessed: &str, td: &[FewShotExample]) -> Option<(String, f64)> {
        for example in td {
            let example_preprocessed = Self::preprocess(&example.text);
            if example_preprocessed == preprocessed {
                return Some((example.category.clone(), example.confidence));
            }
        }
        None
    }

    /// Count examples whose confidence is below 0.99 (i.e., user-feedback examples, not bootstrap).
    fn feedback_count_in(td: &[FewShotExample]) -> usize {
        td.iter().filter(|e| e.confidence < 0.99).count()
    }

    /// Return `cold_start_threshold` until enough feedback examples exist, then `confidence_threshold`.
    fn effective_threshold_for(td: &[FewShotExample], config: &FewShotConfig) -> f64 {
        if Self::feedback_count_in(td) < config.cold_start_feedback_count {
            config.cold_start_threshold
        } else {
            config.confidence_threshold
        }
    }

    /// Rebuild vocabulary and per-category pattern vectors from `data` in-place.
    /// Called on startup and after each feedback-triggered retrain cycle.
    fn retrain_internal(&self, data: &[FewShotExample]) {
        let new_vocab: dashmap::DashMap<String, usize> = dashmap::DashMap::new();
        let new_patterns: dashmap::DashMap<String, Vec<Vec<f64>>> = dashmap::DashMap::new();

        let mut vocab_map: HashMap<String, usize> = HashMap::new();
        let mut next_idx = 0usize;

        let mut category_patterns: HashMap<String, Vec<Vec<f64>>> = HashMap::new();

        for example in data {
            let preprocessed = Self::preprocess(&example.text);
            let tokens: Vec<&str> = preprocessed.split_whitespace().collect();
            let total_words = tokens.len();
            if total_words == 0 {
                continue;
            }
            let mut word_counts: HashMap<usize, f64> = HashMap::new();
            for token in &tokens {
                let idx = *vocab_map.entry(token.to_string()).or_insert_with(|| {
                    let i = next_idx;
                    next_idx += 1;
                    i
                });
                *word_counts.entry(idx).or_insert(0.0) += 1.0;
            }
            let dim = self.config.feature_dimensions;
            let mut features = vec![0.0; dim];
            for (idx, count) in word_counts {
                if idx < dim {
                    features[idx] = count / total_words as f64;
                }
            }
            category_patterns
                .entry(example.category.clone())
                .or_default()
                .push(features);
        }

        for (word, idx) in &vocab_map {
            new_vocab.insert(word.clone(), *idx);
        }
        for (category, patterns) in category_patterns {
            new_patterns.insert(category, patterns);
        }

        let vocab_len = new_vocab.len();
        if vocab_len > self.config.max_vocabulary_warn {
            tracing::warn!(
                "Few-shot vocabulary size ({}) exceeds max_vocabulary_warn ({}); consider resetting training data",
                vocab_len,
                self.config.max_vocabulary_warn
            );
        }

        *self.vocabulary.write().unwrap() = new_vocab;
        *self.intent_patterns.write().unwrap() = new_patterns;
    }

    /// Accept a feedback signal: append the corrected example, trim excess training data,
    /// and trigger a synchronous retrain cycle when the retraining threshold is reached.
    pub async fn add_feedback(
        &self,
        text: String,
        _predicted_category: Option<String>,
        actual_category: String,
        satisfaction: f64,
    ) {
        let example = FewShotExample {
            text,
            category: actual_category,
            confidence: satisfaction,
        };
        let (should_retrain, data_clone) = {
            let mut data = self.training_data.write().await;
            data.push(example);

            if data.len() > self.config.max_training_examples {
                let mut boots = Vec::new();
                let mut feedback = Vec::new();
                for ex in data.iter() {
                    if ex.confidence >= 0.99 {
                        boots.push(ex.clone());
                    } else {
                        feedback.push(ex.clone());
                    }
                }
                let total_allowed = self.config.max_training_examples;
                let allowed_feedback = total_allowed.saturating_sub(boots.len());
                if feedback.len() > allowed_feedback {
                    feedback = feedback.split_off(feedback.len() - allowed_feedback);
                }
                *data = boots.into_iter().chain(feedback).collect();
                tracing::info!(
                    "Few-shot training data trimmed to {} examples (max_training_examples={})",
                    data.len(),
                    self.config.max_training_examples
                );
            }

            let need_retrain = data.len() >= self.config.retraining_threshold;
            let currently_retraining = self.retraining_in_progress.load(Ordering::SeqCst);
            let will_retrain = need_retrain && !currently_retraining;
            if will_retrain {
                if self
                    .retraining_in_progress
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let data_clone = data.clone();
                    (true, Some(data_clone))
                } else {
                    (false, None)
                }
            } else {
                (false, None)
            }
        };

        if should_retrain {
            if let Some(data_snapshot) = data_clone {
                self.retrain_internal(&data_snapshot);
                self.save_training_data(&data_snapshot).await;
                self.retraining_in_progress.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Persist the current training set to the configured YAML file path.
    async fn save_training_data(&self, data: &[FewShotExample]) {
        match serde_yaml::to_string(data) {
            Ok(yaml) => {
                if let Err(e) = fs::write(&self.config.data_path, &yaml).await {
                    tracing::warn!(
                        "Failed to write few-shot training data to {}: {}",
                        self.config.data_path,
                        e
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize few-shot training data: {}", e);
            }
        }
    }

    /// Load persisted training examples from `path`; returns an empty vec if the file
    /// is missing or unparseable (non-fatal — bootstrap data covers cold start).
    fn load_training_data(path: &str) -> Vec<FewShotExample> {
        match std::fs::read_to_string(path) {
            Ok(content) => match serde_yaml::from_str(&content) {
                Ok(data) => data,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse few-shot training data from {}: {}",
                        path,
                        e
                    );
                    vec![]
                }
            },
            Err(e) => {
                tracing::debug!("No persisted few-shot training data at {}: {}", path, e);
                vec![]
            }
        }
    }
}

/// Cosine similarity between two equal-length feature vectors.
/// Returns 0.0 when either vector is all-zeros.
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[async_trait]
impl IntentClassify for FewShotClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        let preprocessed = Self::preprocess(prompt);
        let td = self.training_data.read().await;

        if let Some((category, _confidence)) = self.exact_match_in(&preprocessed, &td) {
            let route = self.routing.get(&category).unwrap_or(&self.fallback_entry);
            return ClassificationResult {
                category,
                model: route.primary().model.clone(),
                tier: ClassificationTier::FewShot,
                providers: route.providers.clone(),
            };
        }

        let features = self.extract_features(&preprocessed);
        let scores = self.score_categories(&features);

        let threshold = Self::effective_threshold_for(&td, &self.config);
        let best = scores
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        match best {
            Some((category, score)) if score >= threshold => {
                let route = self.routing.get(&category).unwrap_or(&self.fallback_entry);
                ClassificationResult {
                    category,
                    model: route.primary().model.clone(),
                    tier: ClassificationTier::FewShot,
                    providers: route.providers.clone(),
                }
            }
            _ => ClassificationResult {
                category: "unknown".to_string(),
                model: DEFAULT_MODEL.to_string(),
                tier: ClassificationTier::Fallback,
                providers: vec![],
            },
        }
    }

    fn get_routing(&self) -> Option<&HashMap<String, RouteEntry>> {
        Some(&self.routing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::routing::ProviderEntry;

    fn make_config() -> FewShotConfig {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        FewShotConfig {
            enabled: true,
            confidence_threshold: 0.4,
            cold_start_threshold: 0.6,
            cold_start_feedback_count: 5,
            feature_dimensions: 1000,
            retraining_threshold: 5,
            data_path: format!("/tmp/fewshot_test_{}.yaml", nanos),
            max_vocabulary_warn: 5000,
            max_training_examples: 10000,
        }
    }

    fn make_classifier() -> FewShotClassifier {
        let config = make_config();
        let routing = HashMap::new();
        let fallback = RouteEntry {
            providers: vec![ProviderEntry {
                model: "fallback".to_string(),
                endpoint: String::new(),
                provider_type: String::new(),
                api_key_env: None,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: None,
        };
        FewShotClassifier::new(config, routing, fallback)
    }

    #[tokio::test]
    async fn known_bootstrap_text_returns_correct_category() {
        let classifier = make_classifier();
        let result = classifier
            .classify("show me the contents of src/main.rs")
            .await;
        assert_eq!(result.category, "FILE_READING");
        assert_eq!(result.tier, ClassificationTier::FewShot);
    }

    #[tokio::test]
    async fn unknown_text_returns_fallback() {
        let classifier = make_classifier();
        let result = classifier.classify("zxcvbnm qwertyuiop asdfghjkl").await;
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn exact_match_returns_confidence_one() {
        let classifier = make_classifier();
        let result = classifier.classify("hello").await;
        assert_eq!(result.category, "CASUAL");
        assert_eq!(result.tier, ClassificationTier::FewShot);
    }

    #[tokio::test]
    async fn preprocessor_strips_code_blocks() {
        let input = "```rust\nfn main() {}\n```";
        let result = FewShotClassifier::preprocess(input);
        assert!(!result.contains("fn main"));
        assert!(!result.contains("```"));
    }

    #[tokio::test]
    async fn preprocessor_collapses_whitespace() {
        let input = "hello    world\n\n  test";
        let result = FewShotClassifier::preprocess(input);
        assert_eq!(result, "hello world test");
    }

    #[tokio::test]
    async fn preprocessor_lowercases() {
        let input = "HELLO WORLD";
        let result = FewShotClassifier::preprocess(input);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn cosine_similarity_is_one_for_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_is_zero_for_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_is_symmetric() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let sim_ab = cosine_similarity(&a, &b);
        let sim_ba = cosine_similarity(&b, &a);
        assert!((sim_ab - sim_ba).abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_zero_for_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn empty_training_returns_fallback() {
        let config = make_config();
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
        let classifier = FewShotClassifier::new(config, routing, fallback);
        let result = classifier.classify("anything").await;
        assert_eq!(result.tier, ClassificationTier::Fallback);
    }

    #[tokio::test]
    async fn cold_start_threshold_enforced_when_no_feedback() {
        let classifier = make_classifier();
        let td = classifier.training_data.read().await;
        assert_eq!(FewShotClassifier::feedback_count_in(&td), 0);
        assert!(
            (FewShotClassifier::effective_threshold_for(&td, &classifier.config) - 0.6).abs()
                < 1e-10
        );
    }

    #[tokio::test]
    async fn add_feedback_increments_training_data() {
        let classifier = make_classifier();
        let initial_len = classifier.training_data.read().await.len();
        classifier
            .add_feedback(
                "custom text".to_string(),
                None,
                "SYNTAX_FIX".to_string(),
                0.8,
            )
            .await;
        let new_len = classifier.training_data.read().await.len();
        assert_eq!(new_len, initial_len + 1);
    }

    #[tokio::test]
    async fn retraining_triggers_after_threshold_feedback() {
        let classifier = make_classifier();
        let config_threshold = classifier.config.retraining_threshold;
        for i in 0..config_threshold {
            classifier
                .add_feedback(
                    format!("feedback text {}", i),
                    None,
                    "SYNTAX_FIX".to_string(),
                    0.8,
                )
                .await;
        }
        assert!(!classifier.vocabulary.read().unwrap().is_empty());
    }
}
