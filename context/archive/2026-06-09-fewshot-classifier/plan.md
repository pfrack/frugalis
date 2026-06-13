# Few-Shot Intent Classifier — Implementation Plan

## Overview

Build a `FewShotClassifier` that implements the existing `IntentClassify` trait, slots into cerebrum's `ClassifierChain` between the regex and LLM backends, and learns from user-supplied corrections over time via a new `/v1/feedback` endpoint.

## Current State Analysis

Cerebrum's classification chain (`src/main.rs:209-273`) currently supports two backends:
- **RegexClassifier** — fast, rule-based pattern matching. Rigid: unknown or ambiguous prompts return `Fallback`.
- **LLMClassifier** — external LLM API call. Accurate but adds cost and latency per request.

The gap: no middle ground. When regex returns Fallback, the only option is the expensive LLM call. There is no mechanism for learning from misclassifications to improve over time.

### Key Architecture Points

- `IntentClassify` trait (`src/intent_classifier.rs:96-105`) — async `classify(&self, prompt: &str) -> ClassificationResult`
- `ClassifierChain` (`src/intent_classifier.rs:136-166`) — tries backends in order; stops at first non-`Fallback` tier
- `ClassificationTier` (`src/intent_classifier.rs:89-93`) — currently `Regex | Fallback`
- `ClassifiersConfig` (`src/config.rs:889-904`) — `enabled: bool` + `order: Vec<String>`, default `["regex", "llm"]`
- `ConfigRoot` (`src/config.rs:851-886`) — per-backend config fields (`regex_classifier`, `llm_classifier`)
- Proxy routes live at `/v1/` behind `proxy_auth_layer` (`src/main.rs:874-878`)

## Desired End State

A three-tier classification chain: `regex → fewshot → llm`. When the regex classifier cannot match a prompt (returns Fallback), the few-shot classifier attempts classification using bag-of-words + cosine similarity against training examples. If its confidence is too low, the chain falls through to the LLM.

Users (or upstream agents) submit corrections via `POST /v1/feedback`, protected by bearer auth. The classifier retrains its vocabulary and feature vectors when enough feedback accumulates. Training data persists as YAML so it survives restarts. Operators are warned when the vocabulary grows unusually large.

### Key Discoveries:

- Trait + chain pattern is the established extension point — `src/intent_classifier.rs:96` and `src/intent_classifier.rs:136`
- Routing is shared across backends; each backend receives a clone of the same `HashMap<String, RouteEntry>` — `src/main.rs:221`
- The `tier` field in JSON responses uses `Debug` formatting (e.g. `"Regex"`) — `src/main.rs:451,481`
- OpenAPI spec at `openapi/completions.yaml:50` constrains tier to `[Regex, Fallback]` — needs updating
- Project uses `include_str!` for embedded defaults (e.g. `config.toml`) — `src/main.rs:90`
- `serde_yaml` is already a dependency (Cargo.toml:21)

## What We're NOT Doing

- NOT changing the regex or LLM classifiers
- NOT adding a database table for training data (filesystem-first per lessons.md)
- NOT exposing a dashboard page for training data management (out of scope for initial implementation)
- NOT supporting per-user training data — one global model per deployment
- NOT enabling the LLM classifier unless it's already in the chain order

## Implementation Approach

The few-shot classifier uses bag-of-words feature extraction and cosine similarity scoring, inspired by `ciresnave/intent-classifier` but adapted to cerebrum's 4-category system and trait-based architecture.

**Classification flow:**
1. Preprocess prompt (lowercase, strip code blocks, collapse whitespace)
2. Exact match against all training examples (fast path)
3. Extract TF-normalized bag-of-words feature vector (1000 dimensions)
4. Compute cosine similarity against each category's stored pattern vectors
5. If `max_score >= threshold`, return classification with `FewShot` tier
6. Otherwise, return `Fallback` (chain moves to LLM)

**Cold start:** On first run (only bootstrap examples), use a higher confidence threshold (0.6). After accumulating N real feedback examples, relax to the normal threshold (0.4). This balances immediate utility against bootstrap quality risk.

**Retraining:** When feedback count reaches the configured threshold, the vocabulary and feature vectors are rebuilt from all training data (bootstrap + feedback). Training data is saved to YAML after each retraining.

**Training data model:** flat list of `(text, category, confidence)` tuples. Bootstrap examples have `confidence: 1.0`. Feedback examples have `confidence` derived from the user's satisfaction signal.

## Critical Implementation Details

- **Exact match is O(n)** — when training data is small (hundreds of examples), this is cheap. If it ever becomes a bottleneck, an indexed lookup can replace the linear scan later.
- **Vocabulary clearing during retraining** — `DashMap::clear()` followed by `insert()` rebuild is atomic per-key. Concurrent classify calls during retraining may see an empty vocabulary and return Fallback, which is fine (chain falls through to LLM).
- **Routing sharing** — pass the same `routing_map.clone()` to `FewShotClassifier::new()` that is passed to `RegexClassifier::from_env()` at `src/main.rs:221`. The few-shot classifier's `get_routing()` returns this map for the merged routing table.

## Phase 1: Foundation

### Overview

Add the new tier variant, data structures, config loading, and bootstrap YAML file. Everything compiles but the classifier doesn't run yet.

### Changes Required:

#### 1. Add `dashmap` dependency

**File**: `Cargo.toml`

**Intent**: Add lock-free concurrent hashmap for the classifier's hot read path (vocabulary and intent_patterns).

**Contract**: `dashmap = "6"` under `[dependencies]`.

#### 2. Add `FewShot` variant to `ClassificationTier`

**File**: `src/intent_classifier.rs`

**Intent**: New tier so the chain and response serialization can distinguish few-shot classifications from regex and fallback.

**Contract**: Enum variant `FewShot` added to `ClassificationTier`. The `Debug` output `"FewShot"` becomes the JSON `tier` value in responses.

#### 3. Define `FewShotExample` struct

**File**: `src/intent_classifier.rs`

**Intent**: Serialisable training example (text + category + confidence) used for bootstrap loading, feedback recording, and YAML persistence.

**Contract**:
```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FewShotExample {
    pub text: String,
    pub category: String,
    pub confidence: f64,
}
```

#### 4. Define `FewShotConfig` struct

**File**: `src/config.rs`

**Intent**: Configuration surface for the few-shot classifier, loaded from `[fewshot_classifier]` in config.toml.

**Contract**:
```rust
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct FewShotConfig {
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    #[serde(default = "default_cold_start_threshold")]
    pub cold_start_threshold: f64,
    #[serde(default = "default_cold_start_feedback_count")]
    pub cold_start_feedback_count: usize,
    #[serde(default = "default_feature_dimensions")]
    pub feature_dimensions: usize,
    #[serde(default = "default_retraining_threshold")]
    pub retraining_threshold: usize,
    #[serde(default = "default_fewshot_data_path")]
    pub data_path: String,
    #[serde(default = "default_max_vocabulary_warn")]
    pub max_vocabulary_warn: usize,
}
```
Defaults: `confidence_threshold=0.4`, `cold_start_threshold=0.6`, `cold_start_feedback_count=5`, `feature_dimensions=1000`, `retraining_threshold=5`, `data_path="data/fewshot_training.yaml"`, `max_vocabulary_warn=5000`.

#### 5. Add `fewshot_classifier` field to `ConfigRoot`

**File**: `src/config.rs`

**Intent**: Wire the new config struct into the top-level config root so it can be loaded from config.toml.

**Contract**: Add `pub fewshot_classifier: Option<FewShotConfig>` field to `ConfigRoot`. Add merge/override line to `merge_configs` (full replacement, matching the existing pattern for `llm_classifier`).

#### 6. Add config loading function

**File**: `src/config.rs`

**Intent**: Extract `FewShotConfig` from `ConfigRoot`, returning `None` if the section is absent or `enabled=false`.

**Contract**: `pub(crate) fn load_fewshot_config_from_value(root: &ConfigRoot) -> Option<FewShotConfig>` — mirrors `load_llm_classifier_config_from_value`.

#### 7. Add default config section to embedded config.toml

**File**: `config.toml`

**Intent**: Document the new section with commented-out defaults so operators can discover and tune it.

**Contract**: Add a commented `[fewshot_classifier]` block after the llm_classifier section, showing all fields with their default values.

#### 8. Create bootstrap examples YAML file

**File**: `data/fewshot_bootstrap.yaml`

**Intent**: ~15-20 hardcoded training examples per category, bundled via `include_str!`, providing the initial model before any user feedback.

**Contract**: YAML array of `{ text, category, confidence: 1.0 }` entries. Embedded at compile time via `include_str!("../data/fewshot_bootstrap.yaml")`.

#### 9. Update OpenAPI spec — tier enum

**File**: `openapi/completions.yaml`

**Intent**: The `tier` enum in the API spec must include `FewShot` so consumers know it's a valid response value.

**Contract**: Change `enum: [Regex, Fallback]` to `enum: [Regex, FewShot, Fallback]` on lines 50 and 117. Add the `/v1/feedback` path (full spec deferred to Phase 4).

#### 10. Add `serde::Serialize` derive to `FewShotExample` and export

**File**: `src/intent_classifier.rs`

**Intent**: Enable YAML serialization for training data persistence.

**Contract**: `FewShotExample` derives `Serialize` + `Deserialize`. Add `use serde::Serialize;` import if not already present.

### Success Criteria:

#### Automated Verification:

- Project compiles: `cargo build`
- Unit tests pass: `cargo test`
- Linting passes: `cargo clippy -- -D warnings` (if available)
- Config validation accepts the new section: `cargo run -- --validate`

#### Manual Verification:

- `config.toml` docs are clear and all fields have sensible defaults
- Bootstrap YAML parses without errors (verify with `serde_yaml::from_str` in a quick test)

---

## Phase 2: Classifier Core

### Overview

Implement the `FewShotClassifier` struct with bag-of-words feature extraction, cosine similarity scoring, exact match fast path, cold-start threshold gating, and the `IntentClassify` trait impl.

### Changes Required:

#### 1. Create `FewShotClassifier` struct

**File**: `src/fewshot_classifier.rs` (new)

**Intent**: The main classifier struct holding vocabulary, feature vectors, training data, config, and routing.

**Contract**:
```rust
pub(crate) struct FewShotClassifier {
    vocabulary: dashmap::DashMap<String, usize>,
    intent_patterns: dashmap::DashMap<String, Vec<Vec<f64>>>,
    training_data: Arc<tokio::sync::RwLock<Vec<FewShotExample>>>,
    routing: HashMap<String, RouteEntry>,
    fallback_entry: RouteEntry,
    config: FewShotConfig,
}
```

#### 2. Implement `new()` constructor

**File**: `src/fewshot_classifier.rs`

**Intent**: Initialize from config, routing, and bootstrap YAML. Load bootstrap examples, build initial vocabulary and feature vectors. Optionally load persisted training data from YAML.

**Contract**: `FewShotClassifier::new(config: FewShotConfig, routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry) -> Self`. Bootstrap examples are loaded via `include_str!("../data/fewshot_bootstrap.yaml")`. If `config.data_path` exists on disk, deserialize and merge with bootstrap.

#### 3. Implement prompt preprocessing

**File**: `src/fewshot_classifier.rs`

**Intent**: Normalize prompts: lowercase, strip code blocks via the existing `code_block_re()` regex, collapse whitespace. Reuse the existing `sanitize` function pattern from `src/intent_classifier.rs:432-437`.

**Contract**: `fn preprocess(text: &str) -> String` — returns a normalized string suitable for feature extraction and exact match comparison.

#### 4. Implement bag-of-words feature extraction

**File**: `src/fewshot_classifier.rs`

**Intent**: Convert preprocessed text into a TF-normalized feature vector. Each word is looked up in the vocabulary (assigning a new dimension index if unseen). Feature values are `word_count / total_words`.

**Contract**: `fn extract_features(&self, text: &str) -> Vec<f64>` — returns a vector of length `config.feature_dimensions` (zero-padded to fixed dimension).

#### 5. Implement cosine similarity scoring

**File**: `src/fewshot_classifier.rs`

**Intent**: Compare the input feature vector against all stored pattern vectors for each category. The highest cosine similarity across all patterns for a category is that category's score.

**Contract**: `fn score_categories(&self, input_features: &[f64]) -> HashMap<String, f64>` — returns `category_name -> max_cosine_similarity`.

#### 6. Implement exact match fast path

**File**: `src/fewshot_classifier.rs`

**Intent**: Before feature extraction, check if the preprocessed input text exactly matches any training example's text. Return immediately with that example's category and confidence as the score.

**Contract**: `fn exact_match(&self, preprocessed: &str) -> Option<(String, f64)>` — returns `(category, confidence)` if found, `None` otherwise.

#### 7. Implement cold-start threshold gating

**File**: `src/fewshot_classifier.rs`

**Intent**: If the number of real feedback examples (non-bootstrap) is below `cold_start_feedback_count`, use `cold_start_threshold`. Otherwise, use `confidence_threshold`.

**Contract**: The effective threshold is chosen at the start of `classify()` based on `training_data.len()` minus bootstrap count. Bootstrap examples are stored with `confidence > 0.99`; feedback examples have lower confidence. The count of non-bootstrap examples drives the threshold selection.

#### 8. Implement `IntentClassify` trait

**File**: `src/fewshot_classifier.rs`

**Intent**: The core classification method called by the chain. Preprocesses, tries exact match, extracts features, scores categories, applies threshold, routes the winner or returns Fallback.

**Contract**: Implements `IntentClassify` for `FewShotClassifier`. The `classify` method returns `ClassificationResult` with `tier: FewShot` on success, `tier: Fallback` on low confidence.

#### 9. Implement `get_routing()`

**File**: `src/fewshot_classifier.rs`

**Intent**: Return the routing table so the chain can merge it into the global routing map.

**Contract**: `fn get_routing(&self) -> Option<&HashMap<String, RouteEntry>>` returns `Some(&self.routing)`.

#### 10. Unit tests

**File**: `src/fewshot_classifier.rs`

**Intent**: Verify classification correctness, cold-start behavior, exact match, and fallback on low confidence.

**Contract**: `#[cfg(test)] mod tests` with tests for: known bootstrap text → correct category, unknown text → Fallback, exact match returns bootstrap confidence, preprocessor strips code blocks, cosine similarity is symmetric, empty training → Fallback.

### Success Criteria:

#### Automated Verification:

- Project compiles: `cargo build`
- All tests pass: `cargo test`
- Linting passes: `cargo clippy -- -D warnings` (if available)

#### Manual Verification:

- Bootstrap examples produce correct categories for obvious prompts
- Unknown gibberish text returns Fallback (chain continues to LLM)
- Cold-start threshold (0.6) is enforced when no feedback exists

---

## Phase 3: Chain Integration

### Overview

Wire the few-shot classifier into the chain assembly loop in `main.rs` so it runs in production. Update the default chain order.

### Changes Required:

#### 1. Add `"fewshot"` match arm in chain builder

**File**: `src/main.rs`

**Intent**: When `"fewshot"` appears in the `classifiers.order` config array, instantiate and push a `FewShotClassifier` into the backend list.

**Contract**: New match arm in the `for name in &classifiers_config.order` loop (after `"regex"`, before `"llm"`):
```rust
"fewshot" => {
    if let Some(config) = config::load_fewshot_config_from_value(&config_root) {
        let fewshot = FewShotClassifier::new(
            config,
            routing_map.clone(),
            fallback_entry.clone(),
        );
        info!("Few-shot classifier enabled");
        backends.push(Arc::new(fewshot));
    }
}
```

#### 2. Register module

**File**: `src/main.rs`

**Intent**: Add `mod fewshot_classifier;` to the module declarations.

#### 3. Update default `ClassifiersConfig::order`

**File**: `src/config.rs`

**Intent**: The default classifier order should include `"fewshot"` between regex and llm.

**Contract**: Change `ClassifiersConfig::default()` order to `vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()]`.

#### 4. Update embedded config.toml default order

**File**: `config.toml`

**Intent**: Match the code default so the embedded config and code defaults stay in sync.

**Contract**: Change `order = ["regex", "llm"]` to `order = ["regex", "fewshot", "llm"]` (uncommented, active default).

#### 5. Add integration test

**File**: `src/main.rs` (test module)

**Intent**: Verify the chain with few-shot + regex classifies correctly through the chain.

**Contract**: Test constructs a chain with regex + few-shot backends, verifies regex catches its patterns, and few-shot catches a prompt that regex returns Fallback on (but bootstrap handles).

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with the new module wired in
- `cargo test` passes (all existing tests + new integration test)
- `cargo test routes_auth` passes
- `cargo clippy -- -D warnings` (if available)

#### Manual Verification:

- `RUST_LOG=info cargo run` logs "Few-shot classifier enabled" on startup
- A request that regex can't classify gets classified by few-shot (check response `tier: "FewShot"`)
- Config `order = ["regex", "llm"]` (omitting fewshot) still works — few-shot is never instantiated

---

## Phase 4: Feedback Endpoint

### Overview

Add `POST /v1/feedback` behind bearer auth. Accepts `{ text, predicted_category, actual_category, satisfaction }`, records the feedback as a training example, and triggers retraining when the threshold is met.

### Changes Required:

#### 1. Add `/v1/feedback` route

**File**: `src/main.rs`

**Intent**: New POST endpoint behind the existing `proxy_auth_layer` that accepts classification feedback.

**Contract**: Add `.route("/feedback", post(feedback_handler))` to the `proxy_routes` router at `src/main.rs:874-878`.

#### 2. Implement `feedback_handler`

**File**: `src/main.rs`

**Intent**: Parse the JSON body, validate categories against known routing keys, call `add_feedback` on the few-shot classifier, and retrain if needed. Logs operational failures before falling back (per lessons.md).

**Contract**: Handler receives `State<Arc<AppState>>` and `Json<FeedbackRequest>`. Looks up the few-shot backend in the chain via `backends().iter().find_map()`. Calls `fewshot.add_feedback(...)`. Returns `200 {"status": "accepted"}` on success, `400` for invalid categories, `503` if no few-shot backend is configured.

**FeedbackRequest** struct:
```rust
#[derive(Deserialize)]
struct FeedbackRequest {
    text: String,
    predicted_category: Option<String>,
    actual_category: String,
    #[serde(default = "default_satisfaction")]
    satisfaction: f64,
}
fn default_satisfaction() -> f64 { 1.0 }
```

#### 3. Implement `add_feedback` method on `FewShotClassifier`

**File**: `src/fewshot_classifier.rs`

**Intent**: Record user feedback, convert to a training example with confidence = satisfaction, and trigger retraining when the retraining threshold is reached.

**Contract**: `pub async fn add_feedback(&self, text: String, predicted_category: Option<String>, actual_category: String, satisfaction: f64)`. Appends a `FewShotExample` to `training_data`. If `training_data.len() >= config.retraining_threshold`, calls `retrain()`. After retrain, saves training data to YAML via `save_training_data()`.

#### 4. Implement `retrain` method

**File**: `src/fewshot_classifier.rs`

**Intent**: Rebuild vocabulary and feature vectors from all training data (bootstrap + feedback). Clears existing DashMap contents and repopulates.

**Contract**: `fn retrain(&self)`. Clears `vocabulary` and `intent_patterns`. Iterates all training data, extracts features for each example, assigns vocabulary indices, and groups feature vectors by category into `intent_patterns`.

#### 5. Add OpenAPI spec for `/v1/feedback`

**File**: `openapi/completions.yaml`

**Intent**: Document the new endpoint per lessons.md ("Use OpenAPI Generator for Endpoints").

**Contract**: Add a `POST /v1/feedback` path with `security: bearerAuth`, request body schema matching `FeedbackRequest`, and 200/400/401/503 responses.

#### 6. Unit tests for feedback

**File**: `src/main.rs` (test module) and `src/fewshot_classifier.rs`

**Intent**: Verify feedback handler auth, validation, and the add_feedback + retrain cycle.

**Contract**: Tests for: 401 without bearer token, 400 for invalid category, 200 on valid feedback, retraining triggers after N feedback items, classification improves after retraining.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes (all existing tests + new feedback tests)
- `cargo test auth` passes — feedback endpoint requires auth
- `cargo test routes_auth` passes
- `cargo clippy -- -D warnings` (if available)

#### Manual Verification:

- `curl -X POST localhost:10000/v1/feedback -H "Authorization: Bearer <token>" -H "Content-Type: application/json" -d '{"text":"fix this bug","actual_category":"SYNTAX_FIX"}'` returns 200
- After N feedback items, check logs for retraining message
- Classification changes after feedback (e.g., a prompt that was Fallback now gets FewShot tier)

---

## Phase 5: Persistence

### Overview

Add training data save/load to YAML, vocabulary size operator warning, and `.gitignore` entry.

### Changes Required:

#### 1. Implement `save_training_data`

**File**: `src/fewshot_classifier.rs`

**Intent**: Serialize training data to YAML and write to `config.data_path`. Log failures before falling back (per lessons.md).

**Contract**: `fn save_training_data(&self)`. Serializes `training_data` to YAML via `serde_yaml::to_string`, writes to `config.data_path`. Warns on I/O error.

#### 2. Implement `load_training_data`

**File**: `src/fewshot_classifier.rs`

**Intent**: Deserialize persisted training data from YAML at startup. Called from `new()`. Merges with bootstrap; if a training example's text matches a bootstrap example, the persisted version wins.

**Contract**: `fn load_training_data(path: &str) -> Vec<FewShotExample>`. Returns empty vec if file doesn't exist (first run). Warns on parse error but continues with bootstrap only.

#### 3. Add vocabulary size operator warning

**File**: `src/fewshot_classifier.rs`

**Intent**: After each retraining, check if vocabulary size exceeds `max_vocabulary_warn` and emit a `warn!` log so operators know when to consider a manual reset.

**Contract**: At the end of `retrain()`, if `vocabulary.len() > config.max_vocabulary_warn`, emit `warn!("Few-shot vocabulary size ({}) exceeds max_vocabulary_warn ({}); consider resetting training data", vocab_len, config.max_vocabulary_warn)`.

#### 4. Add `.gitignore` entry

**File**: `.gitignore`

**Intent**: Prevent training data from being accidentally committed to version control.

**Contract**: Add line `data/fewshot_training.yaml` under a new `# Training data` section.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles
- `cargo test` passes
- No `data/fewshot_training.yaml` appears in `git status`

#### Manual Verification:

- First run: no file exists, classifier uses bootstrap only (check logs)
- After N feedback submissions, `data/fewshot_training.yaml` is created with correct YAML structure
- Restart: file loads correctly, training data survives restart
- Delete the file: classifier reverts to bootstrap-only, no crash

---

## Testing Strategy

### Unit Tests:

- `FewShotClassifier` classification with known bootstrap text → correct category
- Unknown text → `Fallback` tier
- Exact match fast path returns correct category
- Preprocessor handles code blocks, whitespace, case
- Cosine similarity is 1.0 for identical vectors, close to 0 for orthogonal
- Cold-start threshold gating (0.6 vs 0.4)
- `add_feedback` increments training data length
- Retraining rebuilds vocabulary and intent_patterns
- `save_training_data` / `load_training_data` round-trip

### Integration Tests:

- Chain with regex + few-shot: regex handler gets first chance, few-shot handles Fallback
- Feedback endpoint: 401 without auth, 400 for bad category, 200 for valid
- Full retrain cycle: feedback → retrain threshold hit → classification changes

### Manual Testing Steps:

1. Start with clean `data/fewshot_training.yaml` (or delete it)
2. Send a classify request with text regex can't match → verify FewShot tier
3. Send feedback for a misclassification → verify 200
4. After N feedback items, verify retrain log message appears
5. Send the same classify request again → verify classification changed
6. Restart the server → verify training data reloads

## Performance Considerations

- **Exact match**: O(n) linear scan over training data. With hundreds of entries this adds <1ms. If training data grows to thousands, consider an indexed lookup (future optimization).
- **Feature extraction**: O(words_in_prompt * feature_dimensions) per classify call. Typical prompts are <500 words, so <500K operations — negligible.
- **Cosine similarity**: O(categories * patterns_per_category * feature_dimensions). With 4 categories, ~20 patterns each, 1000 dimensions = ~80K dot products. Well under 1ms.
- **Retraining**: O(training_data_size * avg_words_per_entry). Only triggered by feedback handler, not on the request path. Acceptable to take 10-50ms.
- **DashMap**: Lock-free concurrent reads. No contention between parallel classify calls.

## Migration Notes

- Existing deployments with `order = ["regex", "llm"]` continue to work — few-shot is never instantiated.
- Deployments updating to the new default `order = ["regex", "fewshot", "llm"]` get few-shot automatically, but it starts cold (bootstrap only) and won't misclassify harshly due to the 0.6 cold-start threshold.
- The `data/fewshot_training.yaml` file is optional — its absence is handled gracefully.
- The OpenAPI tier enum change (`FewShot` variant) is backward-compatible: existing consumers that validate against the spec will accept the new value.

## References

- Research: `context/changes/fewshot-classifier/research.md`
- `IntentClassify` trait: `src/intent_classifier.rs:96-105`
- `ClassifierChain`: `src/intent_classifier.rs:136-166`
- `ClassificationTier`: `src/intent_classifier.rs:89-93`
- `ClassifiersConfig`: `src/config.rs:889-904`
- `ConfigRoot`: `src/config.rs:851-886`
- Chain assembly: `src/main.rs:209-273`
- Route setup: `src/main.rs:874-878`
- OpenAPI spec: `openapi/completions.yaml`
- Reference implementation: `ciresnave/intent-classifier`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Foundation

#### Automated

- [x] 1.1 Project compiles: `cargo build` — 87cb72c
- [x] 1.2 Unit tests pass: `cargo test` — 87cb72c
- [x] 1.3 Config validation accepts new section: `cargo run -- --validate` — 87cb72c

#### Manual

- [ ] 1.4 config.toml docs are clear and defaults sensible
- [ ] 1.5 Bootstrap YAML parses without errors

### Phase 2: Classifier Core

#### Automated

- [x] 2.1 Project compiles: `cargo build` — 3e7145f
- [x] 2.2 All tests pass: `cargo test` — 3e7145f

#### Manual

- [ ] 2.3 Bootstrap examples produce correct categories for obvious prompts
- [ ] 2.4 Unknown gibberish returns Fallback
- [ ] 2.5 Cold-start threshold (0.6) enforced when no feedback

### Phase 3: Chain Integration

#### Automated

- [x] 3.1 `cargo build` compiles with new module wired in — ae654ba
- [x] 3.2 `cargo test` passes (all existing + integration) — ae654ba
- [x] 3.3 `cargo test routes_auth` passes — ae654ba

#### Manual

- [ ] 3.4 `RUST_LOG=info cargo run` logs "Few-shot classifier enabled"
- [ ] 3.5 Few-shot classification appears in responses when regex can't match
- [ ] 3.6 Config without fewshot in order still works

### Phase 4: Feedback Endpoint

#### Automated

- [x] 4.1 `cargo test` passes (all tests) — adee010
- [x] 4.2 `cargo test auth` passes — feedback requires auth — adee010
- [x] 4.3 `cargo test routes_auth` passes — adee010

#### Manual

- [ ] 4.4 `POST /v1/feedback` with valid data returns 200
- [ ] 4.5 After N feedback items, retrain log message appears
- [ ] 4.6 Classification changes after feedback retraining

### Phase 5: Persistence

#### Automated

- [x] 5.1 `cargo build` compiles — adee010
- [x] 5.2 `cargo test` passes — adee010
- [x] 5.3 `data/fewshot_training.yaml` not in `git status` — adee010

#### Manual

- [ ] 5.4 First run uses bootstrap only (no persistence file)
- [ ] 5.5 After feedback, `data/fewshot_training.yaml` created with valid YAML
- [ ] 5.6 Restart reloads training data correctly
- [ ] 5.7 Delete file → reverts to bootstrap only, no crash
