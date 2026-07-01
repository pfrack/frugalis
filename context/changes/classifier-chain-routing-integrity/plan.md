# Classifier Chain Routing Integrity — Implementation Plan

## Overview

Fix the gap where `LLMClassifier` returns `providers: vec![]` on successful classification, causing the handler to fall through to 502 "all providers failed". Add a `ClassificationTier::Llm` variant for observability, a defensive empty-providers guard in the handlers, headless-mode routing preservation, and FewShot category-name casing normalization.

## Current State Analysis

### The Bug

When the classifier chain's default order (`regex → fewshot → llm`) escalates to the LLM backend and the LLM matches a category:

1. `LLMClassifier.parse_response()` (`src/classification/llm.rs:188-193`) returns `providers: vec![]` — the LLM owns no routing table
2. The chain short-circuits to this result (`src/classification/chain.rs:49-53`)
3. `completion_handler` iterates empty providers (`src/proxy/handlers.rs:314-315`), produces no `last_error_response`
4. Falls through to "all providers exhausted" → 502 (`src/proxy/handlers.rs:924-949`)

The `messages_handler` has the same pattern and same bug.

### Secondary Gaps

- `ClassificationTier` enum lacks an `Llm` variant (`src/classification/types.rs:14-18`); LLM results report as `Regex`
- `ClassificationResult::fallback()` returns `providers: vec![]` (`src/classification/types.rs:38-45`) — when classifiers are disabled, every request hits this
- FewShot lookups by category name may fail if training-data casing doesn't match the uppercased routing keys
- `build_classifiers()` discards the loaded routing map when classifiers are disabled (`src/app/mod.rs:139`)

### Constraints

- The `ClassificationTier` enum is used in OTel metric tags (lines 288, 1101 of handlers.rs) and chain test assertions
- `LLMClassifier::new()` already accepts 4 params; adding 2 more (routing + fallback) follows the `RegexClassifier` constructor pattern
- The handler pattern of `classification_only_json` for no-client / unknown-category paths is the precedent for the defensive guard

## Desired End State

After this plan, every classification path produces a valid response:

- **LLM classifier match** → populated `providers`, routed to upstream, returns upstream response
- **Empty providers after any classification** → 200 with `classification_only_json` (intent info, no upstream body)
- **All classifiers disabled** → routing still loaded for header bypass; classifier fallback returns 200 with classification JSON
- **FewShot with mixed-case training data** → routing lookup succeeds after case normalization
- **LLM classifier tier** → reports `ClassificationTier::Llm` in metrics and test assertions

## What We're NOT Doing

- Fixing `ClassificationResult::fallback()` to return populated providers (not needed — the handler guard catches this)
- Implementing `get_routing()` on `ClassifierChain` itself (backends contribute individually; chain merging is in `build_classifiers`)
- Changing the OTel metric label format for the new `Llm` variant (the existing `format!("{:?}", tier)` debug output handles new variants automatically)
- Fixing Regex classifier's `route_match` casing (categories from config are already uppercase, as are routing keys)

## Implementation Approach

Three phases, each independently testable:

1. Give `LLMClassifier` a routing table matching the existing `RegexClassifier` pattern, add `ClassificationTier::Llm`
2. Add handler guards and fix headless mode; prove the end-to-end fix with an integration test
3. Normalize FewShot category-name casing on routing lookup

Phase 1 fixes the root cause. Phase 2 is defense-in-depth and coverage. Phase 3 is a standalone correctness fix.

## Phase 1: LLMClassifier Routing Table + ClassificationTier::Llm

### Overview

Give `LLMClassifier` its own `HashMap<String, RouteEntry>` and fallback `RouteEntry`, mirroring the `RegexClassifier` and `FewShotClassifier` pattern. On successful classification, populate `providers` from the routing lookup. Add the `Llm` variant to `ClassificationTier` so LLM results are distinguishable. Update the chain escalation test.

### Changes Required:

#### 1. Enum: Add Llm variant

**File**: `src/classification/types.rs`

**Intent**: Extend `ClassificationTier` with an `Llm` variant so metrics and tests can distinguish LLM-produced results from regex-produced ones. Insert before `Fallback` so the ordering matches the chain priority.

**Contract**: The enum becomes `Regex | FewShot | Llm | Fallback`.

#### 2. LLMClassifier: Add routing fields

**File**: `src/classification/llm.rs`

**Intent**: Add `routing` and `fallback_entry` fields to the struct, matching the pattern in `RegexClassifier` (`regex.rs:19-20`). The LLM now resolves which upstream provider to use for the category it identifies.

**Contract**:
- Add fields: `routing: HashMap<String, RouteEntry>`, `fallback_entry: RouteEntry`
- Add `use crate::routing::RouteEntry` import if not already present
- Update `new()` signature to accept `routing` and `fallback_entry` — two new params after the existing four

#### 3. LLMClassifier: Populate providers on match

**File**: `src/classification/llm.rs` — `parse_response()` method

**Intent**: After matching a category name from the LLM response, look up the routing table and populate `ClassificationResult.providers`. Uses the same pattern as `RegexClassifier::route_match()` (`regex.rs:223-233`).

**Contract**:
- After confirming a category match, do: `let route = self.routing.get(&cat.name.to_uppercase()).unwrap_or(&self.fallback_entry);`
- Return `tier: ClassificationTier::Llm` instead of `Regex`
- Populate `model` from `route.primary().model`
- Populate `providers` from `route.providers.clone()`

#### 4. LLMClassifier: Implement get_routing()

**File**: `src/classification/llm.rs`

**Intent**: Return a reference to the routing table so `build_classifiers()` can merge it into `AppState.routing`. Currently returns `None` (line 218-221). This ensures the handler's `X-Frugalis-Category` bypass path works for LLM-owned categories.

**Contract**: `fn get_routing(&self) -> Option<&HashMap<String, RouteEntry>> { Some(&self.routing) }`

#### 5. build_classifiers: Pass routing to LLMClassifier

**File**: `src/app/mod.rs:191-196`

**Intent**: Thread `routing_map.clone()` and `fallback_entry.clone()` into `LLMClassifier::new()` so the LLM backend has access to the same routing table as Regex and FewShot.

**Contract**: Add two arguments to the `LLMClassifier::new()` call. The variables `routing_map` and `fallback_entry` are already in scope at this point.

#### 6. Chain test: Update escalation test

**File**: `src/classification/chain.rs:538-543` — `test_chain_3_backend_escalates_to_llm`

**Intent**: The test constructs an `LLMClassifier` without routing. Add the routing map and fallback entry that the test already builds (lines 478-514). Update the tier assertion from `ClassificationTier::Regex` to `ClassificationTier::Llm`.

**Contract**:
- Pass `routing.clone()` and `fallback.clone()` to `LLMClassifier::new()`
- Change `assert_eq!(result.tier, ClassificationTier::Regex)` to `ClassificationTier::Llm`

#### 7. LLM unit tests: Pass routing + fallback

**File**: `src/classification/llm.rs` — `#[cfg(test)] mod tests`

**Intent**: All existing tests construct `LLMClassifier::new(config, client, cats, Arc::new(vec![]))`. After the signature change, they need routing and fallback args. The tests don't exercise routing lookup, but they must compile.

**Contract**: Each `LLMClassifier::new(...)` call site receives `HashMap::new()` (empty routing) and a bare `RouteEntry` fallback. The empty-routing tests still work because `parse_response` already falls back to `self.fallback_entry` when `routing.get()` returns `None`.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with new enum variant and LLMClassifier signature
- `cargo test classification` — all module tests pass (chain, llm, regex, fewshot)
- `cargo test` — full suite passes, no regressions
- `cargo clippy` — clean

#### Manual Verification:

- Build and run with all three classifiers enabled; send a prompt that LLM classifies; confirm upstream receives the request and returns a real response (not 502)

---

## Phase 2: Handler Defensive Guard + Headless Mode + Integration Test

### Overview

Add an empty-providers guard in both `completion_handler` and `messages_handler`: when classification produces empty providers, return 200 with `classification_only_json()` instead of falling through to 502. Fix the headless (classifiers disabled) path to preserve the loaded routing table for header-bypass routing. Add an integration test that verifies the 3-backend chain escalation produces classification JSON (not 502) when the LLM matches.

### Changes Required:

#### 1. completion_handler: Add empty-providers guard

**File**: `src/proxy/handlers.rs` — after line 291, before line 293

**Intent**: Catch empty providers after classification and return classification-only JSON with 200 OK. Prevents the "all providers exhausted" 502 path. Logs the classification event.

**Contract**: Insert after the OTel metrics block (line 291):
- `if classification.providers.is_empty()`: log with `log_classification`, return `json_response(StatusCode::OK, classification_only_json(&classification))`

#### 2. messages_handler: Add empty-providers guard

**File**: `src/proxy/handlers.rs` — after line 1104, before line 1106

**Intent**: Same guard for the Anthropic Messages handler. Mirrors the completion_handler pattern.

**Contract**: Same guard and response shape, placed between the OTel metrics block and the `http_client` check.

#### 3. Headless mode: Preserve routing when classifiers disabled

**File**: `src/app/mod.rs:137-143`

**Intent**: When `classifiers_config.enabled` is false, the routing map loaded from config is discarded (`routing: HashMap::new()`). Change this to pass the loaded routing map through so `X-Frugalis-Category` header bypass still works.

**Contract**: Change `routing: HashMap::new()` to `routing: routing_map`. The `routing_map` variable is still in scope (computed at lines 81-91). The `model_costs` and `baseline_model` fields are already correctly populated.

#### 4. Integration test: 3-backend chain escalation

**File**: `src/proxy/handlers.rs` — `#[cfg(test)] mod tests`

**Intent**: Prove the exact scenario that triggered the bug: regex doesn't match, fewshot returns Fallback, LLM identifies the category. Verify the handler returns 200 with classification JSON (not 502).

**Contract**: A new `#[tokio::test]` function:
- Set up `httpmock` server with a `/v1/chat/completions` endpoint that returns `{"choices":[{"message":{"content":"SYNTAX_FIX"}}]}`
- Build a `RegexClassifier` from test categories + patterns
- Build `CountingClassifier` returning `ClassificationResult::fallback()` as the fewshot stub
- Build `LLMClassifier` pointing at the mock server, with test routing and fallback
- Wrap them in a `ClassifierChain`
- Construct `AppState` with the chain and the merged routing
- Build the app via `build_app`
- Send POST to `/v1/chat/completions` with a prompt that regex won't match
- Assert 200 OK
- Assert response body contains `"category":"SYNTAX_FIX"` and `"tier":"Llm"`

The test is modeled on `test_chain_3_backend_escalates_to_llm` (chain.rs:456) but drives through the full handler pipeline instead of calling `chain.classify()` directly.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles
- `cargo test test_llm_escalation_produces_classification_json` — new integration test passes
- `cargo test` — full suite passes, no regressions
- `cargo clippy` — clean

#### Manual Verification:

- Run the gateway with all classifiers enabled; send a prompt that triggers LLM classification; confirm 200 with classification JSON (no 502)
- Run the gateway with `classifiers.enabled = false`; send a request with `X-Frugalis-Category` header; confirm routing resolution works

---

## Phase 3: FewShot Category Name Casing

### Overview

FewShotClassifier looks up categories in `self.routing` without normalizing case. Routing keys are uppercased by `routing_from_value()` (`loader.rs:336`). If training data uses non-uppercase category names, the lookup silently falls through to `fallback_entry`. Uppercase the category before the routing lookup.

### Changes Required:

#### 1. Uppercase before routing lookup

**File**: `src/classification/fewshot.rs` — lines 335 and 354

**Intent**: Normalize the category name to uppercase before `self.routing.get()`, matching the convention used by `routing_from_value()`. Defends against training data with non-uppercase category names.

**Contract**: Change `self.routing.get(&category)` to `self.routing.get(&category.to_uppercase())` in both locations.

#### 2. Casing unit test

**File**: `src/classification/fewshot.rs` — `#[cfg(test)] mod tests`

**Intent**: Verify that a classifier with uppercase routing keys correctly routes a category matched from training data regardless of original casing.

**Contract**: A test that:
- Builds a `FewShotClassifier` with an uppercase routing key (e.g. `"SYNTAX_FIX"`)
- Calls `add_feedback` with a lowercase category name, triggering retraining
- Asserts that subsequent classification with a matching prompt returns `tier: FewShot` with correct `category` and providers

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles
- `cargo test fewshot` — existing tests + new casing test pass
- `cargo test` — full suite passes
- `cargo clippy` — clean

---

## Testing Strategy

### Unit Tests:

- `LLMClassifier.parse_response()` produces populated `providers` when category matches a routing entry
- `LLMClassifier.parse_response()` falls back to `fallback_entry.providers` when category has no routing entry
- `LLMClassifier` tier is `Llm` on match, `Fallback` on no match / error
- `ClassificationTier::Llm` debug format is valid (used in OTel metric tags)
- FewShot routing lookup works with lowercase category from training data

### Integration Tests:

- 3-backend chain (regex → fewshot → llm) through the full handler pipeline returns 200 with classification JSON (Phase 2)
- `test_app()` (no classifiers, no http_client) returns 200 with classification JSON for any request

### Manual Testing Steps:

1. Run with all three classifiers and a real LLM endpoint; send a complex prompt that regex doesn't match; confirm LLM classifies and routes correctly
2. Run with `classifiers.enabled = false`; send `X-Frugalis-Category: SYNTAX_FIX` header; confirm routing lookup succeeds
3. Check OTel metrics after LLM classification; confirm `tier` tag shows `Llm`

---

## References

- Research: `context/changes/classifier-chain-routing-integrity/research.md`
- LLMClassifier empty providers: `src/classification/llm.rs:188-193`
- ClassificationTier enum: `src/classification/types.rs:14-18`
- ClassificationResult::fallback(): `src/classification/types.rs:38-45`
- Handler provider loop: `src/proxy/handlers.rs:314-315`
- Handler "all providers exhausted": `src/proxy/handlers.rs:924-949`
- Chain escalation test: `src/classification/chain.rs:456-557`
- FewShot routing lookup: `src/classification/fewshot.rs:335, 354`
- Routing key uppercasing: `src/config/loader.rs:336`
- Headless routing discard: `src/app/mod.rs:139`
- build_classifiers LLM construction: `src/app/mod.rs:191-196`
- Chain classification: `src/classification/chain.rs:43-58`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: LLMClassifier Routing Table + ClassificationTier::Llm

#### Automated

- [x] 1.1 `cargo build` compiles with new enum variant and LLMClassifier signature — f640993
- [x] 1.2 `cargo test classification` — all module tests pass — f640993
- [x] 1.3 `cargo test` — full suite passes — f640993
- [x] 1.4 `cargo clippy` — clean — f640993

#### Manual

- [ ] 1.5 Run with all classifiers; send prompt that triggers LLM escalation; confirm upstream gets the request (no 502)

### Phase 2: Handler Defensive Guard + Headless Mode + Integration Test

#### Automated

- [x] 2.1 `cargo build` compiles — 310628d
- [x] 2.2 `cargo test test_llm_escalation_produces_classification_json` — new integration test passes — 310628d
- [x] 2.3 `cargo test` — full suite passes — 310628d
- [x] 2.4 `cargo clippy` — clean — 310628d

#### Manual

- [ ] 2.5 Run with classifiers; send LLM-triggering prompt; confirm 200 with classification JSON (no 502)
- [ ] 2.6 Run with `classifiers.enabled = false`; send `X-Frugalis-Category` header; confirm routing works

### Phase 3: FewShot Category Name Casing

#### Automated

- [x] 3.1 `cargo build` compiles — 360598c
- [x] 3.2 `cargo test fewshot` — existing + casing test pass — 360598c
- [x] 3.3 `cargo test` — full suite passes — 360598c
- [x] 3.4 `cargo clippy` — clean — 360598c
