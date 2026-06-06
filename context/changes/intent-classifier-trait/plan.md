# Intent Classifier Trait — Implementation Plan

## Overview

Extract the `IntentClassifier` concrete struct into an `IntentClassify` trait with a single `classify()` method. Rename `IntentClassifier` to `RegexClassifier`, add a `ClassifierChain` that iterates backends on Fallback tier, and update `AppState` to hold the chain plus extracted `model_costs` and `baseline_model` fields. Pure refactoring — zero behavioral change. Roadmap S-07.

## Current State Analysis

- `IntentClassifier` (intent_classificator.rs:74–82) is a concrete struct with 7 public fields and a `classify()` method (line 504). No trait exists.
- `AppState` (main.rs:27–32) holds `classifier: Option<Arc<IntentClassifier>>`.
- Handlers call `state.classifier.as_ref().map(|c| c.classify(&prompt))` at 3 sites (lines 172, 252, 267). The `completion_handler` also directly accesses `c.routing` (line 241) for the X-Cerebrum-Category header path.
- Dashboard handlers (dashboard.rs:113–117, 286–289) clone `model_costs` and `baseline_model` from the classifier for `fetch_savings_estimate`.
- A `CostProvider` trait already exists in persistence.rs:12–14, implemented by `ModelCosts`, demonstrating the narrow-trait-boundary pattern this plan extends.
- The original classification research (`proxy-intent-routing/research.md:483`) rejected a trait as YAGNI. The roadmap (S-07) reverses this — the codebase is mature enough for pluggable backends (S-09: LLM-based classifier).
- 6 test helpers in main.rs and 1 in intent_classificator.rs construct `IntentClassifier` via `from_values`. All use a `HashMap<String, RouteEntry>` routing table and identical fallback entries.

## Desired End State

- `IntentClassify` trait with `fn classify(&self, prompt: &str) -> ClassificationResult`.
- `RegexClassifier` struct (renamed `IntentClassifier`) implements `IntentClassify`. All existing fields stay `pub`.
- `ClassifierChain` struct holds `Vec<Arc<dyn IntentClassify + Send + Sync>>` and implements `IntentClassify`. When a backend returns `ClassificationTier::Fallback`, the chain tries the next backend.
- `AppState.classifier` changes from `Option<Arc<IntentClassifier>>` to `Option<Arc<ClassifierChain>>`.
- `AppState` gains `routing: Arc<HashMap<String, RouteEntry>>`, `model_costs: ModelCosts`, `baseline_model: String`. These are populated from `RegexClassifier` at startup.
- All existing tests pass without modification to their assertions. New tests cover chain fallback behavior and the trait boundary.

### Key Discoveries

- `completion_handler` line 241 accesses `state.classifier.as_ref().and_then(|c| c.routing.get(category))` — this is the only production code that reads a classifier field beyond `classify()`. Extracting `routing` to a separate AppState field eliminates this coupling.
- Dashboard handlers duplicate the same costs-extraction pattern twice (dashboard.rs:114–117 and 286–289) — extracting costs to AppState fields eliminates both duplications.
- `RouteEntry` does not derive `Clone`. Adding `#[derive(Clone)]` is required since `HashMap<String, RouteEntry>` is stored in `Arc`, and `RouteEntry` must be `Clone` for the `Arc` to be constructible from a `HashMap` (at least for test convenience). Production code can move the HashMap into the Arc.
- The `from_values` test constructor hardcodes `baseline_model: "claude-3.5-sonnet"` (line 498) — after refactoring, test helpers that previously got baseline from the classifier must set it explicitly on `AppState`.

## What We're NOT Doing

- Adding the `LLMClassifier` backend (roadmap S-09) — that's a separate change.
- Adding `provider_path` URL derivation (roadmap S-08) — separate change.
- Introducing `async` into the trait — the `classify` method is synchronous. Future `LLMClassifier` will handle async inside its own implementation.
- Changing the classification algorithm, regex patterns, or routing configuration.
- Removing `from_env`/`from_values` from `RegexClassifier`.
- Adding a `Config` associated type to the trait — config is bundled at construction time (roadmap S-07 unknown resolution).
- Changing `ModelCosts` — it stays where it is, accessed via the new AppState field instead of via the classifier.

## Implementation Approach

Bottom-up: define the trait first, rename the struct, implement the trait, add the chain, then update consumers. Each phase is independently testable (compile + `cargo test`).

## Critical Implementation Details

- **Routing field extraction**: `completion_handler` line 241 currently chains `state.classifier.as_ref().and_then(|c| c.routing.get(category))`. After the refactoring, this becomes `state.routing.get(category)` — a simpler access on a dedicated AppState field. The routing HashMap must be extracted from `RegexClassifier` in `main()` and stored as `Arc<HashMap<String, RouteEntry>>` on AppState. When the classifier is `None` (graceful degradation), routing is an empty HashMap.

- **RouteEntry Clone**: `RouteEntry` is used in `HashMap<String, RouteEntry>` stored behind `Arc`. In `main()`, the HashMap is moved into the Arc (no Clone needed). In tests, test helpers currently construct routing inline and pass to `from_values` which takes ownership. For AppState construction in tests, the routing HashMap can be moved into `Arc::new()`. No `Clone` derive on `RouteEntry` is required.

- **Test helpers — routing sharing**: Previously each test helper passed routing to `IntentClassifier::from_values` and the classifier owned it. After the refactoring, the same routing HashMap is used both for `RegexClassifier` construction AND stored separately in `AppState.routing`. Since `RegexClassifier` takes ownership via its constructor, the routing must either be cloned before passing to the classifier, or the classifier's routing field should be `Arc<HashMap<...>>` internally to allow sharing. Simplest approach for tests: construct routing once, `clone()` it for AppState, move original into `RegexClassifier`. Production `main()` can also clone (cheap for small routing tables).

## Phase 1: Define Trait + Rename Struct

### Overview

Create the `IntentClassify` trait in `intent_classificator.rs`, rename `IntentClassifier` to `RegexClassifier`, implement the trait. Update all internal references and tests within `intent_classificator.rs`. Nothing outside this file changes yet.

### Changes Required

#### 1. Add `IntentClassify` trait

**File**: `src/intent_classificator.rs`

**Intent**: Define a public trait with a single synchronous method `fn classify(&self, prompt: &str) -> ClassificationResult` that all classifier backends implement. Place immediately after the public types section (before `ModelCosts`).

**Contract**: The trait block exports one method signature. No associated types, no async, no default methods.

#### 2. Rename `IntentClassifier` to `RegexClassifier`

**File**: `src/intent_classificator.rs`

**Intent**: Rename the struct (line 74) and all references within this file, including `impl` blocks and `#[cfg(test)]` construction. The rename makes space for the trait name and clarifies that this is the regex-based implementation.

**Contract**: Every `IntentClassifier` identifier in the file (struct definition, `impl IntentClassifier`, `from_env`, `from_values`, test helper `test_classifier`, `from_values` calls) becomes `RegexClassifier`.

#### 3. Implement `IntentClassify` for `RegexClassifier`

**File**: `src/intent_classificator.rs`

**Intent**: Delegate `classify()` to the existing method body. No code changes to `classify()` itself.

**Contract**: `impl IntentClassify for RegexClassifier { fn classify(&self, prompt: &str) -> ClassificationResult { self.classify(prompt) } }` — placed after the trait definition and before `impl RegexClassifier`.

#### 4. Update `ClassificationResult::fallback()` doc comment

**File**: `src/intent_classificator.rs`

**Intent**: The doc comment on `ClassificationResult::fallback()` (line 426) says "Used when the classifier is None" — update to reflect the new chain-based architecture.

**Contract**: Change the doc comment to note it's for graceful degradation when no chain is configured.

### Success Criteria

#### Automated Verification

- `cargo build` compiles cleanly
- `cargo test` passes all existing tests (including `intent_classificator` module tests)
- `cargo test auth` passes
- `cargo test routes_auth` passes

---

## Phase 2: Add ClassifierChain

### Overview

Add `ClassifierChain` struct that holds a vector of trait-object backends and implements `IntentClassify`. The chain iterates backends in order, calling `classify()` on each, returning the first classification with `tier != Fallback`. If all backends return Fallback, the last backend's result is returned.

### Changes Required

#### 1. Define `ClassifierChain` struct

**File**: `src/intent_classificator.rs`

**Intent**: A new public struct that wraps a `Vec<Arc<dyn IntentClassify + Send + Sync>>` and delegates `classify()` across them in order. Placed after `RegexClassifier` implementations.

**Contract**: The struct has one field:
- `backends: Vec<Arc<dyn IntentClassify + Send + Sync>>`

Constructor: `pub fn new(backends: Vec<Arc<dyn IntentClassify + Send + Sync>>) -> Self`

#### 2. Implement `IntentClassify` for `ClassifierChain`

**File**: `src/intent_classificator.rs`

**Intent**: Iterate backends in order. If a backend returns `ClassificationTier::Regex`, return immediately. If it returns `Fallback`, continue to the next backend. If all backends fall through, return the last one's result (even if Fallback).

**Contract**: The classify implementation should handle the edge case of an empty backends vector: if `backends.is_empty()`, return `ClassificationResult::fallback()`.

---

## Phase 3: Update AppState and All Consumers

### Overview

Modify `AppState` to hold `Option<Arc<ClassifierChain>>`, `Arc<HashMap<String, RouteEntry>>`, `ModelCosts`, and `baseline_model`. Update `main()` initialization, all production handlers, dashboard handlers, and all test helpers.

### Changes Required

#### 1. AppState struct

**File**: `src/main.rs` (line 27)

**Intent**: Replace `classifier: Option<Arc<intent_classificator::IntentClassifier>>` with `classifier: Option<Arc<intent_classificator::ClassifierChain>>`. Add three new fields for configuration that dashboard and the completion handler need.

**Contract**: New AppState fields:
- `classifier: Option<Arc<intent_classificator::ClassifierChain>>`
- `routing: Arc<std::collections::HashMap<String, intent_classificator::RouteEntry>>`
- `model_costs: intent_classificator::ModelCosts`
- `baseline_model: String`

#### 2. Update `main()` initialization

**File**: `src/main.rs` (lines 69–92)

**Intent**: After building the `RegexClassifier`, wrap it in a `ClassifierChain`, extract routing/model_costs/baseline_model into AppState. Handle the `None` (classifier disabled) case gracefully with empty defaults.

**Contract**: The classifier initialization block needs restructuring. On success:
1. Build `RegexClassifier::from_env()`
2. Clone `routing` from the classifier (for AppState)
3. Clone `model_costs` and `baseline_model` from the classifier (for AppState)
4. Wrap in `Arc::new(RegexClassifier::from_env()?)` and then `Arc::new(ClassifierChain::new(vec![regex_arc]))`
5. Populate AppState with all four fields plus defaults

On failure: `classifier: None`, `routing: Arc::new(HashMap::new())`, `model_costs: ModelCosts::empty()`, `baseline_model: String::new()`.

#### 3. Update `completion_handler` — routing lookup

**File**: `src/main.rs` (line 241)

**Intent**: Replace the `state.classifier.as_ref().and_then(|c| c.routing.get(category))` expression with `state.routing.get(category)` — a direct HashMap lookup on the new AppState field.

**Contract**: Line 241 changes from:
```rust
match state.classifier.as_ref().and_then(|c| c.routing.get(category)) {
```
to:
```rust
match state.routing.get(category.as_deref().unwrap_or("")) {
```

#### 4. Update `completion_handler` and `classify_and_log` — classify calls

**File**: `src/main.rs` (lines 172, 252, 267)

**Intent**: The `.map(|c| c.classify(...))` pattern works unchanged since both `RegexClassifier` and `ClassifierChain` implement `IntentClassify`. The `classify` method signature is identical. No code change needed at these call sites — the type of `c` changes from `Arc<IntentClassifier>` to `Arc<ClassifierChain>`, but the method resolution is through the trait.

**Contract**: No explicit code change required. The call sites at lines 172, 252, 267 remain exactly as-is — the trait dispatch is transparent.

#### 5. Update `dashboard_handler` — costs and baseline access

**File**: `src/dashboard.rs` (lines 113–117)

**Intent**: Replace the "destructure classifier or use defaults" pattern with direct reads from the new AppState fields.

**Contract**: Lines 114–117 change from:
```rust
let (model_costs, baseline_model) = match &state.classifier {
    Some(c) => (c.model_costs().clone(), c.baseline_model.clone()),
    None => (intent_classificator::ModelCosts::empty(), "unknown".to_string()),
};
```
to two direct field accesses: `&state.model_costs` and `&state.baseline_model`. The downstream usage at line 136 already takes references (`&model_costs`, `&baseline_model`). Line 113 (`classifier_active`) still checks `state.classifier.is_some()` — the chain is a drop-in replacement.

#### 6. Update `savings_handler` — costs and baseline access

**File**: `src/dashboard.rs` (lines 286–289)

**Intent**: Same pattern as dashboard_handler — replace the destructure with direct reads.

**Contract**: Lines 286–289 replaced with direct reads from `state.model_costs` and `state.baseline_model`. The `baseline_model` in the `SavingsTemplate` is already populated as `baseline_model.clone()` from the extracted value.

#### 7. Update test helper `test_app()`

**File**: `src/main.rs` (lines 593–608)

**Intent**: Add the three new AppState fields with empty defaults.

**Contract**: Add `routing: Arc::new(HashMap::new())`, `model_costs: intent_classificator::ModelCosts::empty()`, `baseline_model: String::new()` to the AppState literal.

#### 8. Update test helper `test_app_with_classifier()`

**File**: `src/main.rs` (lines 610–656)

**Intent**: Use `RegexClassifier::from_values()` instead of `IntentClassifier::from_values()`. Clone the routing HashMap for AppState. Build a `ClassifierChain` wrapping the RegexClassifier. Set model_costs and baseline_model explicitly on AppState.

**Contract**: The routing HashMap is constructed, cloned for AppState.routing, the original moved into `RegexClassifier::from_values()`. The RegexClassifier is wrapped in `Arc::new(ClassifierChain::new(vec![regex_arc]))`. `model_costs` is obtained from `regex_classifier.model_costs.clone()`. `baseline_model` is `regex_classifier.baseline_model.clone()`.

#### 9. Update test helper `test_app_with_enriched_classifier()`

**File**: `src/main.rs` (lines 724–773)

**Intent**: Same pattern as `test_app_with_classifier()` — clone routing for AppState, wrap RegexClassifier in ClassifierChain, extract model_costs and baseline_model.

**Contract**: Identical structural changes to the previous test helper. The parameterized `provider_type_val` and `api_key_env_val` arguments are unaffected.

#### 10. Update test helper `test_app_with_http_client()`

**File**: `src/main.rs` (lines 1186–1238)

**Intent**: Same pattern — clone routing, wrap in ClassifierChain, set AppState fields.

**Contract**: Identical structural changes.

#### 11. Update test helper `test_app_with_dead_endpoint()`

**File**: `src/main.rs` (lines 1240–1290)

**Intent**: Same pattern.

**Contract**: Identical structural changes.

#### 12. Update slow test `test_streaming_keepalive_injected()`

**File**: `src/main.rs` (lines 1931–2012)

**Intent**: Same pattern — rename `IntentClassifier` to `RegexClassifier`, clone routing, wrap in `ClassifierChain`, set AppState fields.

**Contract**: Identical structural changes. The classifier construction is inline in the test body, not in a helper — the same transformations apply.

### Success Criteria

#### Automated Verification

- `cargo build --release` compiles
- `cargo test` — all fast unit/integration tests pass
- `cargo test auth` passes
- `cargo test routes_auth` passes
- `cargo test slow_tests` — keepalive test still passes

#### Manual Verification

- Spin up locally with `RUST_LOG=info cargo run`, verify `/v1/classify` returns correct classification for a test prompt
- Verify `/v1/chat/completions` routes to the correct upstream model
- Verify X-Cerebrum-Category/X-Cerebrum-Model header override still works
- Verify dashboard `/dashboard/savings` shows savings estimate with baseline model
- Verify graceful degradation: remove `ROUTING_CONFIG_PATH` env, confirm server still starts (hardcoded routing)

**Implementation Note**: After all automated verification passes, pause for manual confirmation before proceeding to Phase 4.

---

## Phase 4: New Tests

### Overview

Add tests for the `ClassifierChain` chaining logic and a stub implementation of `IntentClassify` to verify the trait boundary compiles and dispatches correctly.

### Changes Required

#### 1. ClassifierChain unit tests

**File**: `src/intent_classificator.rs` (inside `#[cfg(test)] mod tests`)

**Intent**: Test the chain's iteration behavior: first backend wins on Regex tier, chain falls through on Fallback, empty chain returns fallback.

**Contract**: New test functions:
- `chain_returns_first_regex_match` — two backends; first returns Regex, second never consulted.
- `chain_falls_through_to_next` — first returns Fallback, second returns Regex; result comes from second.
- `chain_returns_last_on_all_fallback` — both return Fallback; result is the last backend's output.
- `chain_handles_empty_backends` — empty vector returns `ClassificationResult::fallback()`.

These tests use stub implementations — a simple struct with a hardcoded return value. No need for a full mock framework.

#### 2. Trait boundary compilation test

**File**: `src/intent_classificator.rs` (inside `#[cfg(test)] mod tests`)

**Intent**: Define a minimal stub struct that implements `IntentClassify`, verify it can be used as `Arc<dyn IntentClassify + Send + Sync>` in a `ClassifierChain`, and that `classify()` dispatches through the trait.

**Contract**: A `StubClassifier` struct with an `expected_result: ClassificationResult` field. Implement `IntentClassify` to return the field. Test that wrapping it in `Arc` and calling through the chain works.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests including the new chain and stub tests
- `cargo build --release` compiles

### Manual Verification

- Review test coverage: confirm chain iteration order is explicitly tested
- Verify stub implementation catches trait signature mismatches at compile time

---

## Testing Strategy

### Unit Tests

- Chain fallback logic: first-match-wins, fall-through, empty-chain edge case
- Stub trait implementation: compile-time verification of trait contract

### Integration Tests

- All existing integration tests from `main.rs` continue to pass — they cover the full request lifecycle through the new chain

### Manual Testing Steps

1. Start server: `RUST_LOG=info cargo run`
2. POST to `/v1/classify` with a known FILE_READING prompt → verify correct category
3. POST to `/v1/classify` with a COMPLEX_REASONING prompt → verify correct category
4. POST to `/v1/chat/completions` with X-Cerebrum-Category/X-Cerebrum-Model headers → verify routing bypass works
5. Open `/dashboard` → verify savings estimate renders
6. Unset `ROUTING_CONFIG_PATH` → restart, verify hardcoded routing fallback

## Performance Considerations

The `dyn IntentClassify` dispatch adds one vtable indirection per `classify()` call — negligible compared to regex matching (~0.01ms) and network I/O (10–1000ms). `ClassifierChain` iteration over a single-element vector (the common case until S-09 adds LLMClassifier) is equally negligible.

## Migration Notes

No database migrations. No configuration file changes. Environment variables unchanged (`BASELINE_MODEL`, `ROUTING_CONFIG_PATH`, `DEFAULT_MODEL` etc. all read by `RegexClassifier::from_env()` as before).

## References

- Roadmap S-07: `context/foundation/roadmap.md:262–273`
- Roadmap S-09 (LLM classifier, depends on this): `context/foundation/roadmap.md:288–299`
- Original "No trait" decision: `context/changes/proxy-intent-routing/research.md:483`
- Existing `CostProvider` trait pattern: `src/persistence.rs:12–14`
- `IntentClassifier` definition: `src/intent_classificator.rs:74–82`
- `AppState` definition: `src/main.rs:26–32`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Define Trait + Rename Struct

#### Automated

- [x] 1.1 `cargo build` compiles
- [x] 1.2 `cargo test` passes all existing tests
- [x] 1.3 `cargo test auth` passes
- [x] 1.4 `cargo test routes_auth` passes

### Phase 2: Add ClassifierChain

#### Automated

- [x] 2.1 `cargo build` compiles
- [x] 2.2 `cargo test` passes all tests

### Phase 3: Update AppState and All Consumers

#### Automated

- [x] 3.1 `cargo build --release` compiles
- [x] 3.2 `cargo test` passes all fast tests
- [x] 3.3 `cargo test auth` passes
- [x] 3.4 `cargo test routes_auth` passes
- [x] 3.5 `cargo test slow_tests` passes

#### Manual

- [x] 3.6 `/v1/classify` returns correct classification for test prompts
- [x] 3.7 `/v1/chat/completions` routes to correct upstream model
- [x] 3.8 X-Cerebrum-Category/X-Cerebrum-Model header override works
- [x] 3.9 Dashboard `/dashboard/savings` shows savings estimate
- [x] 3.10 Graceful degradation: server starts without routing.toml

### Phase 4: New Tests

#### Automated

- [x] 4.1 `cargo test` passes including new chain and stub tests
- [x] 4.2 `cargo build --release` compiles
