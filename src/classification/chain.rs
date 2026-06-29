use std::sync::Arc;

use async_trait::async_trait;

use crate::classification::types::ClassificationResult;
use crate::config::routing::RouteEntry;

/// Trait for intent classification backends.
#[async_trait]
pub trait IntentClassify: Send + Sync + 'static {
    /// Classify a prompt string and return the best matching [`ClassificationResult`].
    async fn classify(&self, prompt: &str) -> ClassificationResult;

    /// Returns a reference to this backend's routing table, if it has one.
    /// Used to construct the merged routing map in `AppState`.
    fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
        None
    }
}

/// A chain of classifiers that tries each in order until one returns a non-Fallback result.
pub struct ClassifierChain {
    backends: Vec<Arc<dyn IntentClassify + Send + Sync>>,
}

impl ClassifierChain {
    /// Create a new chain from an ordered list of classifier backends.
    /// Backends are tried left-to-right; the first non-Fallback result wins.
    pub fn new(backends: Vec<Arc<dyn IntentClassify + Send + Sync>>) -> Self {
        Self { backends }
    }

    /// Get the slice of backend classifiers.
    pub fn backends(&self) -> &[Arc<dyn IntentClassify + Send + Sync>] {
        &self.backends
    }
}

use crate::classification::types::ClassificationTier;

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

#[cfg(test)]
pub(crate) mod test_util {
    //! Shared test utilities for the classifier subsystem.
    //!
    //! Exposed to `#[cfg(test)]` modules in other files of this crate
    //! (e.g. `src/main.rs` integration tests) via `pub(crate)`.
    //! Production code never sees this module.

    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::classification::types::ClassificationResult;

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
    use crate::classification::types::ClassificationResult;

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
        use crate::classification::chain::test_util::CountingClassifier;
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
        use crate::classification::chain::test_util::CountingClassifier;
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
        use crate::classification::chain::test_util::CountingClassifier;
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
}
