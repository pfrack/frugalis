---
date: 2026-06-09T17:23:38+02:00
researcher: kiro
git_commit: 83c2703
branch: code-review-cleanup
repository: cerebrum
topic: "Building a few-shot intent classifier for cerebrum inspired by ciresnave/intent-classifier"
tags: [research, codebase, intent-classifier, few-shot-learning, machine-learning]
status: complete
last_updated: 2026-06-09
last_updated_by: kiro
---

# Research: Building a Few-Shot Intent Classifier for Cerebrum

**Date**: 2026-06-09T17:23:38+02:00
**Researcher**: kiro
**Git Commit**: 83c2703
**Branch**: code-review-cleanup
**Repository**: cerebrum

## Research Question

How to build a few-shot learning intent classifier (similar to ciresnave/intent-classifier) that fits into cerebrum's existing architecture — using the same `IntentClassify` trait, `ClassifierChain`, routing, and 4-category system.

## Summary

ciresnave/intent-classifier uses bag-of-words feature extraction + cosine similarity + feedback learning to classify intents with minimal training data. Cerebrum already has a trait-based classifier chain where backends are tried in order. The new `FewShotClassifier` implements the existing `IntentClassify` trait, slots into the chain via config (`order: ["regex", "fewshot", "llm"]`), and learns from corrections over time — filling the gap between the cheap-but-rigid regex classifier and the expensive LLM classifier.

## Detailed Findings

### ciresnave/intent-classifier Core Techniques

The crate's classification engine works as follows:

1. **Bag-of-words feature extraction** (`src/classifier.rs:extract_text_features`):
   - Text is preprocessed: lowercased, non-alphanumeric stripped, whitespace-collapsed
   - Each word is looked up in a vocabulary `DashMap<String, usize>` (word → dimension index)
   - Feature vector is 1000 dimensions, values are TF-normalized (count / total_words)

2. **Cosine similarity scoring** (`src/classifier.rs:calculate_intent_scores`):
   - For each known intent, stored pattern texts are converted to feature vectors
   - Input features are compared against each pattern via cosine similarity
   - Maximum similarity across all patterns for an intent = that intent's score

3. **Few-shot bootstrap** (`src/classifier.rs:get_bootstrap_examples`):
   - ~50 hardcoded (text, intent) pairs across 16 categories
   - Loaded at initialization via `load_bootstrap_data()`
   - Each example updates both the vocabulary and the intent_patterns map

4. **Exact match fast path** (`src/classifier.rs:find_exact_match`):
   - Before feature extraction, checks if input text == any training example text
   - Returns immediately with confidence from the matching example

5. **Feedback learning** (`src/classifier.rs:add_feedback`):
   - User provides: original text, predicted intent, actual intent, satisfaction score
   - Converts to a training example (confidence = satisfaction/5.0)
   - Triggers `retrain()` when feedback count >= `retraining_threshold` (default 10)
   - Retrain clears vocabulary and rebuilds from all training data

6. **Concurrency model**:
   - `DashMap` for vocabulary and intent_patterns (lock-free concurrent reads)
   - `Arc<RwLock<Vec<TrainingExample>>>` for training data (write-heavy on feedback)

7. **Persistence**: Export/import as JSON (`export_training_data`, `import_training_data`)

8. **Context features** (5 dimensions): text length, word count, contains '?', contains 'file', contains 'data' — added as a minor boost

### Cerebrum's Current Classification Architecture

| Component | Role | File |
|-----------|------|------|
| `IntentClassify` trait | Async classification interface | `src/intent_classifier.rs:109` |
| `RegexClassifier` | Weighted regex pattern matching | `src/intent_classifier.rs:123` |
| `LLMClassifier` | External LLM API call | `src/intent_classifier.rs:172` |
| `ClassifierChain` | Tries backends in order | `src/intent_classifier.rs:136` |
| `ClassifiersConfig` | Config-driven order | `src/config.rs:312` |
| `ClassificationTier` | Tracks which backend classified | `src/intent_classifier.rs:103` |

**Key contracts**:
- `IntentClassify::classify(&self, prompt: &str) -> ClassificationResult`
- `ClassificationResult` carries: category, model, endpoint, tier, provider_type, api_key_env
- Chain stops at first non-`Fallback` tier result
- Config `order: ["regex", "llm"]` controls which backends are instantiated and in what order

### Gap Analysis

| Capability | Regex | LLM | Few-Shot (needed) |
|------------|-------|-----|-------------------|
| Static patterns | ✓ | — | — |
| Learning from corrections | — | — | ✓ |
| Low per-call cost | ✓ | — | ✓ |
| Handles ambiguous/novel input | — | ✓ | ✓ (with training) |
| Feedback loop | — | — | ✓ |
| Persistence | — | — | ✓ |

The few-shot classifier fills the middle ground: it handles cases where regex patterns are too rigid (returns Fallback) but avoids the cost/latency of an LLM call. Over time, as users provide feedback, it gets more accurate without any manual regex authoring.

## Implementation Approach

### 1. New struct: `FewShotClassifier`

```rust
pub struct FewShotClassifier {
    vocabulary: DashMap<String, usize>,
    intent_patterns: DashMap<String, Vec<Vec<f64>>>,  // category -> feature vectors
    training_data: Arc<RwLock<Vec<FewShotExample>>>,
    routing: HashMap<String, RouteEntry>,
    fallback_entry: RouteEntry,
    config: FewShotConfig,
}
```

### 2. Bootstrap examples (mapped to cerebrum's 4 categories)

~15-20 examples per category, tailored to coding assistant use cases:

- **FILE_READING**: "read the file src/main.rs", "show me the contents of config.toml", "what's in the tests directory", ...
- **SYNTAX_FIX**: "fix this compile error", "why doesn't this code work", "there's a type mismatch", ...
- **COMPLEX_REASONING**: "design a caching layer", "how should I architect this service", "compare these two approaches", ...
- **CASUAL**: "hello", "thanks", "what is rust", ...

### 3. Classification flow

```
Input text
  → preprocess (lowercase, strip code blocks, collapse whitespace)
  → exact match check (O(n) scan of training data)
  → extract features (bag-of-words, TF-normalized, 1000 dims)
  → for each category: cosine_similarity(input_features, best_pattern_features)
  → if max_score >= confidence_threshold → return ClassificationResult with FewShot tier
  → else → return Fallback (chain continues to LLM)
```

### 4. Integration points

1. **New tier variant**: `ClassificationTier::FewShot`
2. **Config extension**: `order: ["regex", "fewshot", "llm"]` — add `"fewshot"` as recognized backend name
3. **Chain assembly** (`src/main.rs:~195`): instantiate `FewShotClassifier` alongside regex/llm
4. **Feedback endpoint**: `POST /api/feedback` — accepts JSON body `{ text, predicted_category, actual_category }`
5. **Persistence**: load/save `training_data.json` from configured path (env var or default)

### 5. New dependencies

```toml
dashmap = "6"           # Concurrent hashmap (lock-free reads)
serde = { version = "1", features = ["derive"] }  # Already implicit via serde_json
```

No other new dependencies needed. The algorithm is simple enough to implement in ~200 lines without ML crates.

### 6. Config additions (`config.toml`)

```toml
[classifiers]
order = ["regex", "fewshot", "llm"]

[classifiers.fewshot]
enabled = true
confidence_threshold = 0.4    # Minimum cosine similarity to accept
feature_dimensions = 1000
retraining_threshold = 5      # Feedback count before vocabulary rebuild
data_path = "data/fewshot_training.json"
```

### 7. Feedback API (per lessons.md: use OpenAPI for endpoint design)

```yaml
/api/feedback:
  post:
    summary: Submit classification feedback
    requestBody:
      content:
        application/json:
          schema:
            type: object
            required: [text, actual_category]
            properties:
              text: { type: string }
              predicted_category: { type: string }
              actual_category:
                type: string
                enum: [FILE_READING, SYNTAX_FIX, COMPLEX_REASONING, CASUAL]
    responses:
      200: { description: Feedback accepted }
      401: { description: Unauthorized }
```

### 8. File layout

```
src/
  fewshot_classifier.rs       # FewShotClassifier struct + IntentClassify impl
  fewshot_bootstrap.rs        # Bootstrap training examples (hardcoded)
  intent_classifier.rs        # Add FewShot variant to ClassificationTier
  config.rs                   # Add FewShotConfig loading
  main.rs                     # Wire "fewshot" in chain assembly + feedback route
data/
  fewshot_training.json       # Persisted training data (gitignored)
```

## Code References

- `src/intent_classifier.rs:103` — `ClassificationTier` enum (add `FewShot` variant)
- `src/intent_classifier.rs:109-117` — `IntentClassify` trait (the contract to implement)
- `src/intent_classifier.rs:136-166` — `ClassifierChain` (the slot for the new backend)
- `src/config.rs:312-356` — `ClassifiersConfig` and `load_classifiers_config_from_value`
- `src/main.rs:185-210` — Chain assembly loop (add `"fewshot"` match arm)
- `src/main.rs:34` — `AppState.classifier` field

## Architecture Insights

1. **Trait-based polymorphism is the extension point**. The `IntentClassify` trait + `ClassifierChain` pattern means adding a new classifier requires only: implementing the trait, adding a config entry, and one match arm in the chain builder.

2. **Routing is owned by backends**. Each backend can supply its own routing map via `get_routing()`. The few-shot classifier should share routing with regex (same categories → same routes). Simplest: pass the same `routing: HashMap<String, RouteEntry>` to it.

3. **Fallback semantics drive the chain**. Returning `ClassificationTier::Fallback` means "I don't know" — the chain moves on. The few-shot classifier should return Fallback when its best cosine similarity is below the confidence threshold, rather than guessing.

4. **No database needed for training data**. Per lessons.md, the project already has filesystem-first patterns for config. JSON file persistence avoids coupling to the PostgreSQL persistence layer (which is for inference logs).

5. **DashMap matches project style**. The project doesn't currently use DashMap, but it uses `Arc<tokio::sync::RwLock<>>` extensively. DashMap is a better fit for the hot read path (classify is called per-request, feedback is rare). The one new crate (`dashmap`) is well-known and actively maintained.

## Historical Context (from prior changes)

No prior changes in `context/changes/` or `context/archive/` directly address few-shot classification. The project evolved through:
- Regex-first classification with weighted patterns
- LLM fallback added as a second tier
- ClassifierChain abstraction allowing ordered backends

The few-shot classifier is a natural next step in this evolution.

## Open Questions

1. **Should the feedback endpoint require authentication?** The proxy already has bearer auth (`proxy_auth_layer`). The feedback endpoint should probably sit behind the same auth.

2. **Cold start behavior**: On first run with no persisted data, should the classifier rely solely on bootstrap examples, or also return Fallback until it accumulates N feedback examples?

3. **Vocabulary drift**: If the classifier accumulates many training examples over months, the vocabulary grows. Should there be a max_vocabulary_size cap with LRU eviction, or is periodic manual reset acceptable?

4. **Confidence threshold tuning**: The 0.4 default from ciresnave may not be optimal for 4-class coding prompts. May need A/B testing against the regex classifier to find the sweet spot.

5. **Should the regex and fewshot classifiers run in parallel?** Currently the chain is sequential. For the fewshot classifier to only handle regex-Fallback cases, it should come after regex in the chain. But if you want fewshot to override regex, it should come first.
