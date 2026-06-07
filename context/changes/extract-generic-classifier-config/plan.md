# Extract Generic Classifier Config (S-07a) Implementation Plan

## Overview

Move 7 generic configuration items from `RegexClassifier::from_env()` to a new `src/config.rs` module and `main()`, so that `RegexClassifier` becomes a pure classification constructor (patterns + weights + thresholds only). The same config becomes available to future `LLMClassifier` (S-09) without duplication.

## Current State Analysis

`RegexClassifier::from_env()` at `src/intent_classifier.rs:531-558` currently reads or builds:

- **Routing** (`ROUTING_CONFIG_PATH` + TOML parsing + hardcoded fallback) — lines 487-508, 434-485, 297-351
- **BASELINE_MODEL** env var — lines 538-539
- **ModelCosts** (hardcoded defaults + routing.toml overrides) — lines 541-547
- **DEFAULT_MODEL*** constants used in routing fallback — lines 161-163
- **NVIDIA_ENDPOINT** (hardcoded endpoint default) — lines 298-301
- **SHORT_PROMPT_LEN** (30 chars, used in `classify()`) — line 191
- **ClassificationResult::fallback()** default model reading — line 518

`main()` at lines 71-100 clones `model_costs` and `baseline_model` out of the classifier after construction, then builds `AppState`. This cloning is a symptom — the classifier shouldn't own these generic configs.

`make_test_app_state()` at lines 685-712 mirrors this cloning pattern. The slow test at lines 2123-2153 constructs AppState inline, also cloning from the classifier.

## Desired End State

After S-07a, `RegexClassifier::from_env()` receives pre-built routing, fallback, and prompt length as parameters. It constructs only patterns + weights + RegexSet internally. `main()` loads routing, builds costs, and reads baseline model directly — then passes results to both `RegexClassifier` and `AppState`. No config is cloned from the classifier.

### Key Discoveries:

- The regex classifier needs `routing` and `fallback_entry` internally for `route_match()`/`route_fallback()` — these fields stay on the struct, but the loading moves to `config.rs`
- `model_costs` and `baseline_model` are used ONLY by AppState (dashboard savings/latency) — they should never have been classifier fields
- `from_values()` (test constructor) at line 561 already injects `routing` and `fallback_entry` — adding `short_prompt_len` follows the same pattern
- `ClassificationResult::fallback()` at line 518 uses `env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)` — after `env_or_default` moves, it imports from `crate::config`

## What We're NOT Doing

- NOT changing `IntentClassify` trait or `ClassificationResult` struct
- NOT changing `ClassifierChain` or `AppState` fields
- NOT touching `completion_handler`, `classify_handler`, `log_classification`, or dashboard handlers
- NOT changing pattern arrays, weights, thresholds, or the `classify()` algorithm
- NOT moving `DEFAULT_MODEL*` constants or `hardcoded_model_costs()` out of intent_classifier.rs
- NOT adding new dependencies or changing Cargo.toml
- NOT creating `CategoryConfig` — that's S-07b

## Implementation Approach

Two-phase extraction: first create the `config.rs` module with all moved functions (independently compilable), then change `RegexClassifier`'s constructor and reconnect everything in `main()` and tests.

The `SHORT_PROMPT_LEN` constant stays in `intent_classifier.rs` as `pub const SHORT_PROMPT_LEN: usize = 30;`, passed as a constructor parameter to each classifier.

On error path: if classifier init fails, `main()` continues with empty `ModelCosts` and empty `baseline_model` (current behavior, no change).

## Critical Implementation Details

- **ModelCosts constructor visibility**: `build_model_costs()` in config.rs needs to construct a `ModelCosts` from a `HashMap`. Currently only `#[cfg(test)] from_costs()` can do this and `empty()` for the zero case. Make `from_costs` a `pub(crate)` constructor (remove `#[cfg(test)]` gate) so config.rs can use it in production.
- **env_or_default cross-reference**: After moving `env_or_default` to config.rs, `ClassificationResult::fallback()` at `src/intent_classifier.rs:518` must import it from `crate::config` — this is the only cross-module reference from intent_classifier back to config.

## Phase 1: Create config.rs Module and Extract Shared Types

### Overview

Create two new modules and refactor to eliminate circular dependencies:
1. `src/routing.rs` — shared types and constants used by both config and intent_classifier
2. `src/config.rs` — generic configuration functions

This phase is independently verifiable — the modules compile, types are correctly re-exported, and old functions are removed from intent_classifier.rs.

### Changes Required:

#### 1. New File: src/routing.rs

**File**: `src/routing.rs`

**Intent**: Centralize shared types and constants that are used by both the configuration system and the intent classifier. This eliminates circular dependencies when config.rs needs to reference RouteEntry and ModelCosts.

**Contract**: A `pub(crate)` module exporting:
- `pub(crate) struct RouteEntry` (moved from intent_classifier.rs:12-18)
- `pub(crate) struct ModelCosts` (moved from intent_classifier.rs:22-49)
- `DEFAULT_MODEL: &str = "meta/llama-3.1-8b-instruct"`
- `DEFAULT_MODEL_COMPLEX: &str = "meta/llama-3.3-70b-instruct"`
- `DEFAULT_MODEL_READING: &str = "meta/llama-3.1-70b-instruct"`

All three DEFAULT_MODEL* constants are `pub(crate)` to allow config.rs to use them. Move the `impl ModelCosts` block (get, empty, from_costs) alongside the struct.

#### 2. src/intent_classifier.rs — Re-export Shared Types

**File**: `src/intent_classifier.rs`

**Intent**: Preserve the public API `intent_classifier::RouteEntry` and `intent_classifier::ModelCosts` for downstream consumers (main.rs, dashboard.rs, etc.) by re-exporting from routing.rs.

**Contract**:
- Remove the local definitions of `RouteEntry` (lines 12-18), `ModelCosts` (lines 22-49), and the three DEFAULT_MODEL* constants (lines 161-163).
- Add: `pub use crate::routing::{RouteEntry, ModelCosts, DEFAULT_MODEL, DEFAULT_MODEL_COMPLEX, DEFAULT_MODEL_READING};`
- Keep the `impl ModelCosts` methods re-exported via the routing.rs impl.

#### 3. New File: src/config.rs

**File**: `src/config.rs`

**Intent**: House all generic configuration logic extracted from the regex classifier. This module is the single source of truth for routing loading, cost building, environment variable reading, and hardcoded routing defaults.

**Contract**: A `pub(crate)` module exporting:
- `ROUTING_CONFIG_DEFAULT: &str = "routing.toml"` (moved from intent_classifier.rs:200)
- `NVIDIA_ENDPOINT_DEFAULT: &str = "https://integrate.api.nvidia.com/v1/chat/completions"` (extracted from hardcoded_routing)
- `pub(crate) fn env_or_default(key: &str, default: &str) -> String` — moved from intent_classifier.rs:291-293
- `pub(crate) fn load_routing_from_file(path: &str) -> Result<HashMap<String, RouteEntry>, String>` — moved from intent_classifier.rs:434-485. Imports `RouteEntry` from `crate::routing`
- `pub(crate) fn hardcoded_routing() -> (HashMap<String, RouteEntry>, RouteEntry)` — moved from intent_classifier.rs:297-351. Uses `env_or_default` and the three DEFAULT_MODEL* constants from `crate::routing`, and `NVIDIA_ENDPOINT_DEFAULT`
- `pub(crate) fn load_routing() -> (HashMap<String, RouteEntry>, RouteEntry)` — moved from intent_classifier.rs:487-508. Calls `load_routing_from_file()` with `ROUTING_CONFIG_PATH` env var, falling back to `hardcoded_routing()`
- `pub(crate) fn build_model_costs(routing: &HashMap<String, RouteEntry>) -> ModelCosts` — new function. Seeds with `hardcoded_model_costs()` from intent_classifier (which remains there), then iterates routing entries applying per-model overrides from `cost_per_1m_input_tokens`. Uses `ModelCosts::from_costs()`. Note: `hardcoded_model_costs()` stays in intent_classifier.rs because it is classifier-specific knowledge (which models we know about), not generic routing logic.

#### 4. src/main.rs — Register New Modules

**File**: `src/main.rs`

**Intent**: Register the new modules so they're part of the crate.

**Contract**: Add `mod routing;` and `mod config;` alongside the existing module declarations (after line 22, alongside `mod persistence;`). Order: `mod routing;` must come before `mod config;` because config imports from routing, and `mod intent_classifier;` must come after `mod routing;` because intent_classifier re-exports from routing.

#### 5. src/intent_classifier.rs — Remove Moved Functions and Types

**File**: `src/intent_classifier.rs`

**Intent**: Remove all items that have moved to routing.rs or config.rs. Keep `hardcoded_model_costs()` (classifier-specific) and `SHORT_PROMPT_LEN`.

**Contract**: Remove the following items:
- The `env_or_default` function (lines 291-293)
- The entire `load_routing_from_file` function (lines 434-485)
- The entire `hardcoded_routing` function (lines 297-351)
- The entire `load_routing` function (lines 487-508)
- The constant `ROUTING_CONFIG_DEFAULT` (line 200)
- The `RouteEntry` struct definition (lines 12-18)
- The `ModelCosts` struct and its impl block (lines 22-49)
- The DEFAULT_MODEL* constants (lines 161-163) — these are now in routing.rs and re-exported

#### 6. src/intent_classifier.rs — Update ClassificationResult::fallback() Import

**File**: `src/intent_classifier.rs`

**Intent**: `fallback()` reads `DEFAULT_MODEL` from environment using `env_or_default`, which now lives in `crate::config`.

**Contract**: Replace the local `env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)` call at line 518 with `crate::config::env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)` (or add a `use crate::config::env_or_default` import).

#### 7. src/intent_classifier.rs — Make ModelCosts::from_costs Production-Visible

**File**: `src/intent_classifier.rs`

**Intent**: `build_model_costs()` in config.rs needs to construct a `ModelCosts` from a `HashMap`. Currently `from_costs` is `#[cfg(test)]`.

**Contract**: Change `#[cfg(test)]` on `from_costs` (line 46 in original; after moving to routing.rs, adjust accordingly) to `pub(crate)`. Keep the function body unchanged.

#### 8. src/intent_classifier.rs — Make SHORT_PROMPT_LEN Public

**File**: `src/intent_classifier.rs`

**Intent**: `main()` needs to pass this value to the classifier constructor.

**Contract**: Change the `const SHORT_PROMPT_LEN: usize = 30;` declaration at line 191 from private to `pub const SHORT_PROMPT_LEN: usize = 30;`.

#### 9. Phase 1.4 — Unit Tests for config.rs

**Files**: `src/config.rs` (new), `src/main.rs` (update tests if needed)

**Intent**: Provide fast, focused unit tests for the new configuration module to catch edge cases and ensure correct behavior before integration.

**Contract**: Add a new `#[cfg(test)]` module `tests` in `config.rs` with the following test cases:

- `env_or_default` returns env var when set; falls back to default when unset
- `load_routing_from_file` succeeds with valid TOML and returns correct `HashMap<String, RouteEntry>` and `RouteEntry` fallback
- `load_routing_from_file` returns error on malformed TOML or missing file
- `hardcoded_routing` produces the expected default routes; respects `NVIDIA_ENDPOINT` env var
- `load_routing` returns file-loaded routing when `ROUTING_CONFIG_PATH` points to a valid file; falls back to `hardcoded_routing()` on missing file or parse error
- `build_model_costs` seeds with hardcoded costs and applies per-model `cost_per_1m_input_tokens` overrides from RouteEntry

Each test uses temp files for TOML content and does not require a database or network.

Use the existing `pub(crate)` visibility to allow tests to access internals. Add `#[cfg(test)]` at the top of the file to conditionally compile the test module.

If any helper functions are needed for test data (e.g., `make_route_entry`), keep them `#[cfg(test)]` within `config.rs`.

**Success Criteria**: All new unit tests pass with `cargo test config`. Phase 1 remains independently verifiable.

### Success Criteria:

#### Automated Verification:

- [ ] 1.1 `cargo build` passes
- [ ] 1.2 `cargo test` passes (includes new config.rs unit tests)
- [ ] 1.3 `cargo check` passes (type checking)

#### Manual Verification:

- [ ] 1.4 No behavioral change — all existing integration test scenarios work identically

---

## Phase 2: Slim RegexClassifier + Reconnect main() + Update Tests

### Overview

Change `RegexClassifier`'s constructor to receive pre-built config instead of reading env and building it internally. Remove `model_costs` and `baseline_model` fields. Update `main()` to build config and pass it. Update all test call sites.

### Changes Required:

#### 1. src/intent_classifier.rs — RegexClassifier Struct Fields

**File**: `src/intent_classifier.rs`

**Intent**: Remove the two fields that represent generic config (`model_costs`, `baseline_model`) and add one that was previously a global constant (`short_prompt_len`). Routing and fallback_entry stay — the classifier needs them for `route_match()` and `route_fallback()`.

**Contract**: On the `RegexClassifier` struct (lines 99-107):
- **Remove**: `pub model_costs: ModelCosts,` and `pub baseline_model: String,`
- **Add**: `pub short_prompt_len: usize,`

The struct after the change has: `set`, `metadata`, `negative_idx`, `routing`, `fallback_entry`, `short_prompt_len`.

#### 2. src/intent_classifier.rs — RegexClassifier::from_env() Signature and Body

**File**: `src/intent_classifier.rs`

**Intent**: The constructor no longer reads env vars or builds config — it receives pre-built values from main().

**Contract**: Change signature at line 531 from:
- `pub fn from_env() -> Result<Self, String>`

To:
- `pub fn from_env(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry, short_prompt_len: usize) -> Result<Self, String>`

Body changes:
- Remove the `load_routing()` call (line 536) — use the passed `routing` and `fallback_entry` parameters directly
- Remove the `BASELINE_MODEL` env read (lines 538-539)
- Remove the `ModelCosts` construction (lines 541-547) — caller owns this now
- Store `short_prompt_len` in the struct instead of reading the constant
- Keep `build_all_patterns()` and `RegexSet::new()` — these are classifier-specific
- Return error only on regex compilation failure

The `Ok(IntentClassifier { ... })` block at lines 549-557 removes the `model_costs` and `baseline_model` fields, adds `short_prompt_len`.

#### 3. src/intent_classifier.rs — RegexClassifier::from_values() Signature and Body

**File**: `src/intent_classifier.rs`

**Intent**: The test constructor mirrors the production constructor.

**Contract**: Change signature at line 561 from:
- `pub fn from_values(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry) -> Self`

To:
- `pub fn from_values(routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry, short_prompt_len: usize) -> Self`

Body changes:
- Remove the `hardcoded_model_costs()` + routing override cost building (lines 566-571)
- Remove `baseline_model: "claude-3.5-sonnet".to_string()` (line 573)
- Remove `model_costs: ModelCosts { costs }` (line 578)
- Add `short_prompt_len,` to the struct literal

#### 4. src/intent_classifier.rs — classify() Method

**File**: `src/intent_classifier.rs`

**Intent**: The prompt length shortcut check uses the field instead of the constant.

**Contract**: At line 613, replace `SHORT_PROMPT_LEN` with `self.short_prompt_len`.

#### 5. src/main.rs — main() Function Body

**File**: `src/main.rs`

**Intent**: `main()` now builds routing, costs, and baseline model independently, then passes them to the classifier constructor and directly to AppState.

**Contract**: Replace the `match intent_classifier::RegexClassifier::from_env()` block at lines 71-100. The new flow:
1. Call `config::load_routing()` to get `(routing, fallback_entry)` — this is the same logic, just called from main
2. Call `config::build_model_costs(&routing)` to get `ModelCosts`
3. Read `BASELINE_MODEL` via `config::env_or_default("BASELINE_MODEL", &intent_classifier::DEFAULT_MODEL_COMPLEX)`
4. Try `RegexClassifier::from_env(routing.clone(), fallback_entry.clone(), intent_classifier::SHORT_PROMPT_LEN)`
5. On **success**: build `ClassifierChain` from the classifier, merge routing from backends (same as current), return `(Some(classifier), routing, model_costs, baseline_model)`
6. On **failure**: return `(None, Arc::new(HashMap::new()), intent_classifier::ModelCosts::empty(), String::new())` — same empty defaults as today
7. Build `AppState` with these values directly (no cloning from classifier)

The `AppState` construction at lines 109-117 is unchanged — it receives the same types from the same-named locals.

#### 6. src/main.rs — make_test_app_state() Signature

**File**: `src/main.rs`

**Intent**: Tests now pass explicit costs and baseline instead of cloning from the classifier.

**Contract**: Change signature at line 685 from:
- `fn make_test_app_state(classifier: intent_classifier::RegexClassifier, http_client: Option<reqwest::Client>) -> Arc<AppState>`

To:
- `fn make_test_app_state(classifier: intent_classifier::RegexClassifier, http_client: Option<reqwest::Client>, model_costs: intent_classifier::ModelCosts, baseline_model: String) -> Arc<AppState>`

Body: remove the clone lines (689-690) and use the parameters directly. The classifier chain construction, routing merge, and AppState construction are unchanged.

#### 7. src/main.rs — Update All from_values() Call Sites

**File**: `src/main.rs`

**Intent**: Every call to `RegexClassifier::from_values()` now requires a third argument for `short_prompt_len`. All tests use the default value of 30.

**Contract**: Add `30` as the third argument to every `from_values()` call:
- `test_app_with_classifier()` — line 771
- `test_app_with_enriched_classifier()` — line 888
- `test_app_with_http_client()` — line 1373
- `test_app_with_dead_endpoint()` — line 1420
- `slow_tests::test_streaming_keepalive_injected()` — line 2124

#### 8. src/main.rs — Update All make_test_app_state() Call Sites

**File**: `src/main.rs`

**Intent**: Each call to `make_test_app_state()` now passes explicit model_costs and baseline_model. Tests use empty values since they don't exercise cost calculations.

**Contract**: Add `intent_classifier::ModelCosts::empty(), String::new()` as the last two arguments at each call site:
- `test_app_with_classifier()` — line 772
- `test_app_with_enriched_classifier()` — line 889
- `test_app_with_http_client()` — line 1374
- `test_app_with_dead_endpoint()` — line 1421

#### 9. src/main.rs — Update Slow Test Inline AppState Construction

**File**: `src/main.rs`

**Intent**: The slow test at lines 2123-2153 clones `model_costs` and `baseline_model` from the classifier. After extraction, these fields don't exist on the classifier.

**Contract**: Replace the clone lines (2125-2126) with:
- `let model_costs = intent_classifier::ModelCosts::empty();`
- `let baseline_model = String::new();`

These match the test's behavior — the slow test doesn't query costs.

#### 10. src/intent_classifier.rs — Update Test Code

**File**: `src/intent_classifier.rs`

**Intent**: All test functions that call `from_values()` need the new `short_prompt_len` parameter. One test that directly reads `c.model_costs` or `c.baseline_model` needs updating.

**Contract**:
- `test_classifier()` at line 723: add `, 30` to `from_values(routing, fallback)` call
- `model_costs_returns_some_for_hardcoded_models` at line 782: this test reads `c.model_costs.get(...)` — the field no longer exists. This test belongs in persistence or config tests (it tests cost lookup logic, not classification). Remove the `model_costs` tests from this file — they're covered by persistence tests at `src/persistence.rs:1081-1202` which already test `ModelCosts` with real DB data.
- `model_costs_override_via_route_entry` at line 797: same — this tests cost override logic now in config.rs. Remove.
- `model_costs_baseline_model_default` at line 834: reads `c.baseline_model`. Remove.
- `model_costs_returns_none_for_unknown_model` at line 791: Remove.
- The `#[cfg(test)]` import of `ModelCosts::from_costs` in persistence tests at lines 1090-1092, 1142, etc. — these use `super::super::intent_classifier::ModelCosts::from_costs(...)` which is now `pub(crate)`. These continue to work unchanged since `from_costs` is now pub(crate).

### Success Criteria:

#### Automated Verification:

- `cargo build` passes with no warnings
- `cargo test` — all fast unit/integration tests pass
- `cargo test auth` passes
- `cargo test routes_auth` passes
- `cargo test slow_tests` passes (requires KEEPALIVE_INTERVAL_SECS=1 for the streaming test)

#### Manual Verification:

- Start the service: `RUST_LOG=info cargo run` — logs show "Routing: loaded from routing.toml" (or the hardcoded fallback warning)
- POST to `/v1/classify` with a classified prompt — response includes correct category, model, and tier
- POST to `/v1/chat/completions` — returns classification JSON when no upstream key is configured, or proxies to upstream when configured
- Dashboard `/dashboard/savings` shows cost data when DB is connected
- When `DATABASE_URL` is absent, all routes still work (graceful degradation)
- When `routing.toml` is absent or malformed, service starts with hardcoded defaults (graceful degradation)

---

## Testing Strategy

### Unit Tests:

Existing tests in `src/intent_classifier.rs` cover classification behavior (category matching, negative suppression, ambiguity) and auth header generation. These continue to pass unchanged — the classifier's `classify()` method is unmodified in logic.

Persistence tests in `src/persistence.rs` cover `ModelCosts` interactions with savings estimates — these pass unchanged since `ModelCosts` is unchanged.

### Integration Tests:

All tests in `src/main.rs` (routes auth, completion handler, classify handler, upstream routing, SSE streaming) pass unchanged — they use the test builder pattern which is updated to match the new constructor.

### Manual Testing Steps:

1. Start with `RUST_LOG=info cargo run` — verify routing loads from `routing.toml` or falls back to hardcoded defaults
2. `curl -X POST localhost:10000/v1/classify -H "Authorization: Bearer <token>" -H "Content-Type: application/json" -d '{"messages":[{"role":"user","content":"fix this bug"}]}'` — returns `SYNTAX_FIX`
3. Dashboard `/dashboard/savings` with `DATABASE_URL` set — shows cost data (if DB has records)

---

## References

- Research: `context/changes/extract-generic-classifier-config/research.md`
- Follow-on: `context/changes/shared-category-config/research.md` (S-07b — depends on this)
- Future: `context/changes/llm-classifier/research.md` (S-09 — depends on S-07b)
- Prior art: `context/archive/2026-06-06-intent-classifier-trait/plan.md` (S-07 — "config bundled at construction time" principle)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Create config.rs Module and Extract Shared Types

#### Automated

- [ ] 1.1 `cargo build` passes
- [ ] 1.2 `cargo test` passes (includes new config.rs unit tests)
- [ ] 1.3 `cargo check` passes (type checking)

#### Manual

- [ ] 1.4 No behavioral change — manual smoke test confirms service starts and classifies correctly

### Phase 2: Slim RegexClassifier + Reconnect main() + Update Tests

#### Automated

- [ ] 2.1 `cargo build` passes with no warnings
- [ ] 2.2 `cargo test` all fast tests pass
- [ ] 2.3 `cargo test auth` passes
- [ ] 2.4 `cargo test routes_auth` passes
- [ ] 2.5 `cargo test slow_tests` passes

#### Manual

- [ ] 2.6 Service starts with `RUST_LOG=info cargo run` — routing loads correctly
- [ ] 2.7 `/v1/classify` returns correct classification
- [ ] 2.8 `/dashboard/savings` shows cost data (with DATABASE_URL set)
- [ ] 2.9 Graceful degradation: missing DATABASE_URL or routing.toml does not crash
