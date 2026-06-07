# LLM Classifier Backend (S-09) Implementation Plan

## Overview

Implement `LLMClassifier` — a second `IntentClassify` backend that sends prompts to a small/cheap LLM (e.g., `gpt-4o-mini`) for intent classification. It fires only when `RegexClassifier` returns `Fallback` (ambiguous/no match), adding ~200-500ms latency only for uncertain cases. Configuration lives in a `[llm_classifier]` section of `config.toml`.

## Current State Analysis

The codebase is architecturally ready for this feature:

- `IntentClassify` trait exists (`src/intent_classifier.rs:79`) with `classify(&self, &str) -> ClassificationResult`
- `ClassifierChain` exists (`src/intent_classifier.rs:113`) — iterates backends, first non-`Fallback` wins
- `CategoryConfig` is extracted (`src/intent_classifier.rs:40-48`) with `name`, `description`, `threshold`, `priority`, `model_env_var`
- `AppState` holds `classifier: Option<Arc<ClassifierChain>>` and shared `http_client: Option<reqwest::Client>`
- `auth_headers_for()` maps `provider_type` to auth headers (Bearer, x-api-key, or none)
- `config.rs` loads TOML via `toml::Value` manual traversal — same pattern for `[llm_classifier]`
- Chain construction in `main.rs:78-100` builds `vec![Arc::new(regex_classifier)]` — adding a second element is trivial

### Key Discoveries:

- `config.toml` currently has `[[categories]]` array-of-tables and flat `[CATEGORY_NAME]` routing sections — `[llm_classifier]` will be the first non-routing section
- `reqwest::Client` is built at `main.rs:105-108` with 300s timeout — LLM classifier needs its own 3s timeout per request (not client-level)
- `ClassificationResult::fallback()` returns CASUAL with `Fallback` tier — the terminal fallback for the chain
- No `reqwest::blocking` feature exists; sync-over-async uses `Handle::current().block_on()`

## Desired End State

When `[llm_classifier]` is present and `enabled = true` in `config.toml`, the `ClassifierChain` contains two backends: `RegexClassifier` (Tier 1) → `LLMClassifier` (Tier 2). When regex returns `Fallback`, the LLM classifier fires, sends a chat completion request with a system prompt built from `CategoryConfig` descriptions + 4 few-shot examples, parses the response, and returns the classified category with `ClassificationTier::Regex`. On any error/timeout, it returns `ClassificationResult::fallback()` (CASUAL).

**Verification**: Run with `LLM_CLASSIFIER_ENABLED` in TOML pointing to a real or mocked endpoint. Send ambiguous prompts that regex can't classify — verify LLM classification appears in inference logs. Send prompts regex handles — verify LLM is never called.

## What We're NOT Doing

- Caching LLM classification results (post-MVP optimization)
- Adding a new `ClassificationTier::Llm` variant (returns `Regex` tier on success, `Fallback` on failure)
- Retry logic on LLM call failure (single attempt, then fallback)
- Streaming the classification response (small response, buffered is fine)
- Adding `reqwest::blocking` feature (use `Handle::block_on` instead)
- Changing the `IntentClassify` trait signature
- S-09a config boundary formalization (separate follow-up)

## Implementation Approach

1. Add config struct and TOML parsing for `[llm_classifier]`
2. Redesign `IntentClassify` trait `classify()` method to be async: `async fn classify(&self, prompt: &str) -> ClassificationResult`
3. Update `RegexClassifier::classify()` to be async (wraps sync regex logic in trivial async wrapper)
4. Update all call sites in `main.rs` to await the async classify calls
5. Implement `LLMClassifier` struct with async prompt generation and HTTP call
6. Wire into `ClassifierChain` in `main.rs` when enabled
7. Test with mock HTTP server using `httpmock`

**Rationale for async trait**: The sync trait was a blocking point for `LLMClassifier` which needs async HTTP. Redesigning to async eliminates the sync/async bridge complexity, lets all backends (regex and LLM) use their natural I/O model, and simplifies call sites — they simply await instead of guessing at sync-over-async patterns.

## Phase 1: LLM Classifier Config

### Overview

Add `LlmClassifierConfig` struct and TOML parsing in `config.rs` for the `[llm_classifier]` section.

### Changes Required:

#### 1. Config struct and loader

**File**: `src/config.rs`

**Intent**: Define `LlmClassifierConfig` with fields matching the TOML section, and a `load_llm_classifier_config()` function that reads `[llm_classifier]` from config.toml using the same `toml::Value` traversal pattern as `load_categories_from_file`.

**Contract**: 
```rust
pub(crate) struct LlmClassifierConfig {
    pub enabled: bool,
    pub model: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub provider_type: String,
    pub prompt_template_path: Option<String>,
    pub timeout_secs: u64,
}
```
`load_llm_classifier_config() -> Option<LlmClassifierConfig>` — returns `None` if section absent or `enabled = false`. Defaults: `model = "gpt-4o-mini"`, `provider_type = "openai_compatible"`, `timeout_secs = 3`.

#### 2. Config.toml example section

**File**: `config.toml`

**Intent**: Add a commented-out `[llm_classifier]` section documenting available fields with defaults.

**Contract**: TOML section `[llm_classifier]` with all fields documented as comments. Not active by default.

---

## Phase 2: LLM Classifier Implementation

### Overview

Redesign `IntentClassify` trait `classify()` to be async, then implement `LLMClassifier` struct. Update `RegexClassifier` to wrap its sync logic in an async method.

### Changes Required:

#### 1. Redesign IntentClassify trait to async

**File**: `src/intent_classifier.rs`

**Intent**: Change trait method from `fn classify(&self, prompt: &str) -> ClassificationResult` to `async fn classify(&self, prompt: &str) -> ClassificationResult`. This removes the sync/async impedance mismatch and lets all backends use their natural I/O model.

**Contract**:
```rust
pub trait IntentClassify {
    async fn classify(&self, prompt: &str) -> ClassificationResult;

    fn get_routing(&self) -> Option<&HashMap<String, RouteEntry>> {
        None
    }
}
```

#### 2. Update RegexClassifier to async

**File**: `src/intent_classifier.rs`

**Intent**: Wrap the existing sync regex logic in an async wrapper. The actual classification is still synchronous (regex matching is CPU-bound, not I/O-bound), but the method is now async to match the trait.

**Contract**: 
```rust
impl IntentClassify for RegexClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        // Sync regex logic inside async wrapper
        // No actual async work, just wraps the result
        self.classify_internal(prompt)
    }
}
```
The existing `classify()` is renamed to `classify_internal()` (private), and the async wrapper calls it.

#### 3. LLMClassifier struct and IntentClassify impl

**File**: `src/intent_classifier.rs`

**Intent**: Add `LLMClassifier` struct holding config fields + `reqwest::Client` + `Vec<CategoryConfig>`. Implement async `IntentClassify::classify()` that: (a) builds the prompt, (b) makes an async HTTP request with 3s timeout, (c) parses the response body for a category name, (d) returns `ClassificationResult` with `Regex` tier on success or `fallback()` on any error.

**Contract**:
```rust
pub struct LLMClassifier {
    client: reqwest::Client,
    model: String,
    endpoint: String,
    api_key_env: String,
    provider_type: String,
    categories: Vec<CategoryConfig>,
    prompt_template: String,
    timeout: std::time::Duration,
}

impl IntentClassify for LLMClassifier {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        // Builds prompt, makes async HTTP call, parses response
        // Returns ClassificationResult with Regex tier on success, Fallback on error
    }
}
```

Constructor: `LLMClassifier::new(config: LlmClassifierConfig, client: reqwest::Client, categories: Vec<CategoryConfig>) -> Self`

#### 4. Prompt template builder

**File**: `src/intent_classifier.rs`

**Intent**: Function that generates the system prompt from `CategoryConfig` slice, with 4 hardcoded few-shot examples. If `prompt_template_path` is `Some`, read that file instead.

**Contract**: `fn build_llm_classifier_prompt(categories: &[CategoryConfig], template_path: Option<&str>) -> String` — returns the system message content. Default template includes category list with descriptions + 4 few-shot examples (one per category).

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Chain Wiring

### Overview

Update all call sites to await the async `classify()` calls, then wire `LLMClassifier` as the second backend in `ClassifierChain` when config is present and enabled.

### Changes Required:

#### 1. Update call sites to await classify

**File**: `src/main.rs`

**Intent**: Find all places where `classifier.classify()` or `c.classify()` is called and add `.await`. These are in async contexts (handlers), so await is straightforward.

**Contract**: Pattern changes from `state.classifier.as_ref().map(|c| c.classify(&prompt))` to `state.classifier.as_ref().map(|c| c.classify(&prompt).await)`. Similar for any direct calls.

#### 2. Construct and push LLMClassifier into chain

**File**: `src/main.rs`

**Intent**: After building `regex_classifier`, call `config::load_llm_classifier_config()`. If it returns `Some`, construct `LLMClassifier::new(config, http_client.clone(), categories.clone())` and push it as the second element in the backends vec before creating `ClassifierChain`.

**Contract**: The chain construction changes from `vec![Arc::new(regex_classifier)]` to conditionally `vec![Arc::new(regex_classifier), Arc::new(llm_classifier)]`. Ordering: regex first (fast, free), LLM second (slow, paid, only fires on Fallback).

#### 3. Log LLM classifier status at startup

**File**: `src/main.rs`

**Intent**: Add an `info!` log line when LLM classifier is enabled (model name, endpoint), or `debug!` when disabled/absent.

**Contract**: `info!("LLM classifier enabled: model={}, endpoint={}", ...)` on successful construction.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles cleanly
- Existing tests pass (call sites updated to await)
- `cargo test` all green

#### Manual Verification:

- Start server with `[llm_classifier]` section in config.toml → see "LLM classifier enabled" in logs
- Start server without section → no error, regex-only chain works as before
- Send an ambiguous prompt → verify LLM classifier fires (check logs for warn/debug from LLM classifier)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: Testing

### Overview

Integration test with `httpmock` verifying the full LLM classification flow end-to-end.

### Changes Required:

#### 1. Integration test with mock LLM endpoint

**File**: `src/intent_classifier.rs` (test module) or `tests/llm_classifier.rs`

**Intent**: Use `httpmock` to stand up a mock OpenAI-compatible endpoint. Construct `LLMClassifier` pointing at the mock. Verify: (a) successful classification, (b) timeout handling, (c) invalid response handling, (d) network error handling.

**Contract**: Tests use `httpmock::MockServer` (already in `[dev-dependencies]`). Each test scenario validates the `ClassificationResult` fields and tier.

#### 2. Chain integration test

**File**: `src/intent_classifier.rs` (test module)

**Intent**: Build a `ClassifierChain` with a stub regex (always returns Fallback) + real `LLMClassifier` (pointing at mock). Verify the chain falls through to LLM and returns its result.

**Contract**: Test validates that when first backend returns `Fallback`, second backend's result is used.

### Success Criteria:

#### Automated Verification:

- `cargo test` all green including new integration tests
- `cargo clippy` clean
- All 4 error scenarios tested: success, timeout, invalid response, network error

#### Manual Verification:

- Test with a real LLM endpoint (e.g., OpenRouter with gpt-4o-mini) — send 5 diverse prompts, verify sensible classifications in logs

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests:

- `LlmClassifierConfig` parsing from TOML (valid, missing, disabled)
- Prompt template generation (all categories present, descriptions correct)
- Response parsing (exact match, lowercase match, whitespace, invalid)
- Error handling (timeout, network error, malformed JSON, missing `choices`)

### Integration Tests:

- Full classify flow with `httpmock` mock server
- Chain fallthrough: regex Fallback → LLM success
- Chain fallthrough: regex Fallback → LLM failure → CASUAL

### Manual Testing Steps:

1. Add `[llm_classifier]` to config.toml pointing at OpenRouter/OpenAI
2. Start server, send ambiguous prompt like "help me think about this code structure"
3. Verify LLM classifier fires and returns a sensible category
4. Kill the LLM endpoint (wrong URL), send same prompt → verify graceful CASUAL fallback
5. Remove `[llm_classifier]` section, restart → verify regex-only works unchanged

## Performance Considerations

- LLM classifier adds ~200-500ms latency **only when regex returns Fallback** (ambiguous prompts)
- `Handle::current().block_on()` blocks one Tokio worker thread during the call — acceptable for a fallback path
- 3s timeout prevents slow providers from stalling the request indefinitely
- `max_tokens: 20` and `temperature: 0` minimize response size and variance
- No connection pooling overhead — reuses existing `reqwest::Client`

## References

- Research: `context/changes/llm-classifier/research.md`
- S-07 archived plan: `context/archive/2026-06-06-intent-classifier-trait/plan.md`
- S-07b (CategoryConfig): `src/intent_classifier.rs:40-48`
- Roadmap S-09 definition: `context/foundation/roadmap.md:283-294`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: LLM Classifier Config

#### Automated

- [x] 1.1 cargo build compiles cleanly
- [x] 1.2 Unit test: parse TOML with [llm_classifier] → correct LlmClassifierConfig
- [x] 1.3 Unit test: parse TOML without section → None
- [x] 1.4 Unit test: parse TOML with enabled = false → None

#### Manual

- [x] 1.5 N/A (pure config parsing)

### Phase 2: LLM Classifier Implementation

#### Automated

- [x] 2.1 cargo build compiles cleanly with async trait
- [x] 2.2 Unit test: RegexClassifier async wrapper works (call and await)
- [x] 2.3 Unit test: build_llm_classifier_prompt generates expected format
- [x] 2.4 Unit test: LLMClassifier classify with mocked success → correct category, Regex tier
- [x] 2.5 Unit test: LLMClassifier classify with timeout/error → Fallback tier, CASUAL
- [x] 2.6 Unit test: LLMClassifier classify with invalid response → Fallback

#### Manual

- [x] 2.7 N/A (tested via mocks)

### Phase 3: Chain Wiring

#### Automated

- [x] 3.1 cargo build compiles cleanly
- [x] 3.2 Existing tests pass (call sites updated to await)
- [x] 3.3 cargo test all green

#### Manual

- [ ] 3.4 Server with [llm_classifier] → "LLM classifier enabled" in logs
- [ ] 3.5 Server without section → regex-only works
- [ ] 3.6 Ambiguous prompt → LLM classifier fires

### Phase 4: Testing

#### Automated

- [ ] 4.1 cargo test all green including integration tests
- [ ] 4.2 cargo clippy clean
- [ ] 4.3 All 4 error scenarios tested

#### Manual

- [ ] 4.4 Real LLM endpoint test — 5 diverse prompts with sensible classifications
