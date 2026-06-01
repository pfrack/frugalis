# Intent Classification Implementation Plan

## Overview

Add regex-based intent classification to the proxy gateway. A new `intent_classificator` module classifies prompts into 4 categories (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) using ~45 `RegexSet` patterns with weighted scoring and negative suppression. Classification populates `InferenceRecord.category` and `upstream_model`, making results visible in the dashboard. The handler returns classification metadata as JSON. Upstream model proxying is out of scope — this plan delivers classification only.

## Current State Analysis

The scaffolding is complete. `completion_handler` at `src/main.rs:87-121` receives `body: Bytes` (OpenAI-compatible JSON), extracts a snippet via `extract_snippet()`, builds an `InferenceRecord` with `category: None` and `upstream_model: None`, and enqueues fire-and-forget logging. The handler returns a static placeholder `"proxy route is protected"`.

`InferenceRecord` at `src/persistence.rs:46-53` already has `category: Option<String>` and `upstream_model: Option<String>`. The migrations schema at `migrations/001_create_inferences.sql:6-7` has `category TEXT` and `upstream_model TEXT`. The dashboard template at `templates/dashboard/inferences.html:63-68` renders both fields with `Some`/`None` handling.

No regex or pattern-matching crate exists in `Cargo.toml`. The codebase uses two initialization patterns: mandatory (`AuthConfig::from_env()` → panic on failure) and optional (`PersistenceConfig::from_env()` → `Option<T>`, warn + `None` on failure).

### Key Discoveries:

- `src/main.rs:16-17` — Module declarations; `mod intent_classificator;` goes after line 17
- `src/main.rs:36-39` — `AppState` has one field: `persistence: Option<PersistenceConfig>`
- `src/main.rs:48-57` — Persistence init block: `match from_env()` → `Ok`/`Err`, prints WARN on error, returns `None`
- `src/main.rs:105-112` — `InferenceRecord` construction with `category: None` and `upstream_model: None`
- `src/main.rs:233` — `test_app()` constructs `AppState { persistence: None }`
- `src/persistence.rs:217-247` — `extract_snippet()` parses JSON, finds last user message, truncates to 200 chars
- `Cargo.toml` — 11 direct dependencies; no `regex`, no `toml`; `serde_json = "1.0"` present

## Desired End State

After this plan, each POST to `/v1/chat/completions`:
1. Passes auth middleware (unchanged)
2. Extracts the full last user message from the JSON body
3. Classifies the prompt via regex patterns → returns a category + upstream model
4. Populates `InferenceRecord.category` and `InferenceRecord.upstream_model` with the classification result
5. Returns `200 OK` with JSON: `{"status":"classified","category":"COMPLEX_REASONING","model":"claude-3.5-sonnet","tier":"Regex"}`
6. Enqueues inference logging with the populated fields (unchanged fire-and-forget)

If the classifier fails to initialize (missing/invalid `routing.toml` or regex compilation error), the gateway warns at startup and degrades gracefully: all requests classified as CASUAL with the cheapest model.

## What We're NOT Doing

- No upstream model proxying (no `reqwest`, no SSE streaming, no OpenRouter API calls)
- No ONNX model inference (Tier 2 is deferred to a future change)
- No changes to `src/auth.rs`, `migrations/`, or `templates/`
- Refactor `extract_snippet` in `src/persistence.rs` to use a shared `extract_last_user_message` utility (returns full text; snippet truncation moves to the call site). No schema or API changes to persistence.
- No `serde` derive macros (use runtime `toml::Value` and `serde_json::Value` APIs to avoid new direct deps beyond `toml`)

## Implementation Approach

Create a self-contained `src/intent_classificator.rs` module following the established `from_env() -> Result<Self, String>` + `Option<Arc<T>>` patterns. The module owns all classification concerns: 45 regex patterns, weighted scoring, negative suppression, prompt sanitization, full-text extraction, and TOML routing map loading.

Wire the module into `completion_handler` at the existing insertion point between body receipt and `InferenceRecord` construction. The classifier is optional: if initialization fails, the gateway logs a warning and defaults all requests to CASUAL.

## Critical Implementation Details

- **Prompt length**: `extract_snippet()` at `persistence.rs:217` truncates to 200 chars. The classifier needs the full prompt for accurate keyword matching (e.g., a COMPLEX_REASONING prompt may have its distinguishing keywords >200 chars in). Extract the existing JSON-parsing + last-user-message logic into a shared `persistence::extract_last_user_message(body: &str) -> String` that returns the untruncated text (capped at 10,000 chars to prevent memory/CPU DoS). Refactor `extract_snippet` to call this utility and truncate to 200 chars for the snippet. The classifier imports and calls `extract_last_user_message` directly — no duplicate JSON parsing.
- **TOMl fallback ordering**: Try `ROUTING_CONFIG_PATH` env var first, then `routing.toml` in the working directory, then hardcoded defaults. Only warn (never panic) — the gateway must function without a routing file.
- **RegexSet lifetime**: `RegexSet` is `Send + Sync`. The `IntentClassifier` struct can be shared across tokio tasks via `Arc` without any mutex. Classification is CPU-bound but measured in microseconds — no `spawn_blocking` needed.

## Phase 1: New Module Scaffolding

### Overview

Create `src/intent_classificator.rs` with all classification logic, add new dependencies, and declare the module. No handler integration yet — this phase delivers a testable, self-contained classifier.

### Changes Required:

#### 1. Dependencies

**File**: `Cargo.toml`

**Intent**: Add `regex` for RegexSet pattern matching and `toml` for routing configuration parsing.

**Contract**: Two new lines in `[dependencies]`:
```toml
regex = "1"
toml = "0.8"
```

#### 2. Module declaration

**File**: `src/main.rs`

**Intent**: Register the new module so it compiles.

**Contract**: After line 17 (`mod persistence;`), add `mod intent_classificator;`.

#### 3. New module: classifier core

**File**: `src/intent_classificator.rs` (new file)

**Intent**: Create the classifier module with pattern definitions, scoring algorithm, routing config loading, and prompt extraction. The module must be self-contained — it does not import from `persistence` or `auth`.

**Contract**: The module exposes these public items:

- `struct RouteEntry { model: String, endpoint: String }` — a single routing target
- `struct ClassificationResult { category: String, model: String, endpoint: String, tier: ClassificationTier }` — what `classify()` returns
- `enum ClassificationTier { Regex, Fallback }` — whether regex matched or the result is a default
- `struct IntentClassifier { set: RegexSet, metadata: Vec<PatternMeta>, negative_idx: Range<usize>, routing: HashMap<String, RouteEntry>, fallback_entry: RouteEntry }` — the classifier itself
- `impl IntentClassifier` with:
  - `pub fn from_env() -> Result<Self, String>` — compile RegexSet from built-in patterns, load routing.toml, return Self or error
  - `pub fn classify(&self, prompt: &str) -> ClassificationResult` — never fails, returns Fallback tier for unmatched prompts
  - `#[cfg(test)] pub fn from_values(...) -> Self` — test-only constructor (mirrors `AuthConfig::from_values`)

The module internally contains:
- Five `const` pattern slices: `FILE_READING`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`, `NEGATIVE`
- `fn sanitize(text: &str) -> &str` — lowercase, strip code blocks (` ```...``` `), collapse whitespace, trim
- `fn load_routing() -> Result<(HashMap<String, RouteEntry>, RouteEntry), String>` — tries `ROUTING_CONFIG_PATH` env → `routing.toml` file → hardcoded defaults

The 45 pattern inventory and scoring algorithm are defined in `research.md` sections 8-9. The implementation follows the pseudocode from section 9 exactly: sanitize → RegexSet::matches → tally weights → apply negative suppression → threshold check → resolve or fallback.

**Contract (TOML parsing)**: Use `toml::Value` API (no serde derive):
```rust
fn load_routing_from_file(path: &str) -> Result<HashMap<String, RouteEntry>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {}", path, e))?;
    let root: toml::Value = toml::from_str(&content)
        .map_err(|e| format!("Invalid TOML in {}: {}", path, e))?;
    // iterate root.as_table(), extract "model" and "endpoint" per category
    // ...
}
```

#### 4. Shared prompt extraction utility

**File**: `src/persistence.rs`

**Intent**: Extract the existing "parse JSON → find last user message" logic into a shared public function so both snippet extraction and the classifier reuse the same code path. Avoid duplicating the JSON parse and the 1000-message DoS guard.

**Contract**: Add `pub fn extract_last_user_message(body: &str) -> String` that parses the OpenAI-compatible JSON, finds the last user message, and returns the full content **capped at 10,000 characters** to prevent memory/CPU DoS (or `""` on failure). Refactor the existing `extract_snippet` to call `extract_last_user_message` and truncate the result to 200 chars.

### Success Criteria:

#### Automated Verification:
- Module compiles: `cargo build`
- Module tests pass: `cargo test intent_classificator`
- Persistence tests pass (extract_snippet refactored but behavior unchanged): `cargo test persistence`

#### Manual Verification:
- `routing.toml` is readable by the classifier (run locally, check stderr for no TOML-related warnings)
- Hardcoded fallback routing works when `routing.toml` is absent (delete the file, restart, check "WARN" appears and gateway still starts)

---

## Phase 2: Wire into Handler

### Overview

Integrate the classifier into `completion_handler`. Add to `AppState`, initialize in `main()`, call `classify()` before `InferenceRecord` construction, and return classification metadata as JSON. Update `test_app()`.

### Changes Required:

#### 1. AppState field

**File**: `src/main.rs`

**Intent**: Store the classifier in shared application state, following the `Option<Arc<T>>` pattern used by persistence.

**Contract**: In the `AppState` struct (lines 36-39), add:
```rust
classifier: Option<Arc<intent_classificator::IntentClassifier>>,
```

#### 2. Classifier initialization in main()

**File**: `src/main.rs`

**Intent**: Initialize the classifier after persistence, using the same `match` + `eprintln!("WARN: ...")` + `None` graceful-degradation pattern.

**Contract**: After the persistence init block (line 57) and before `Arc::new(AppState { ... })` (line 58), add:
```rust
let classifier = match intent_classificator::IntentClassifier::from_env() {
    Ok(c) => {
        println!("Intent classifier initialized");
        Some(Arc::new(c))
    }
    Err(e) => {
        eprintln!("WARN: intent classification disabled: {e}");
        None
    }
};
```

Add `classifier` to the `Arc::new(AppState { ... })` construction at line 59.

#### 3. Modify completion_handler

**File**: `src/main.rs`

**Intent**: Insert classification between body receipt and inference logging. Extract full prompt text, classify it, populate `InferenceRecord` fields, and return a JSON response with classification metadata instead of the placeholder string.

**Contract**: In `completion_handler` (lines 87-121):

a) Change the return type from `(StatusCode, &'static str)` to `(StatusCode, String)` — needed for dynamic JSON responses.

b) Extract the full prompt text (after the `let start` timer, before classification):
```rust
let body_str = std::str::from_utf8(&body).unwrap_or("");
let prompt = persistence::extract_last_user_message(body_str);
```

c) Classify the extracted prompt (using CASUAL fallback when classifier is None):
```rust
let classification = state.classifier.as_ref()
    .map(|c| c.classify(&prompt))
    .unwrap_or_else(ClassificationResult::fallback);
```

d) Build the JSON response from classification fields *after* classification is complete (replaces the static placeholder at line 94):
```rust
let response_body = serde_json::json!({
    "status": "classified",
    "category": classification.category,
    "model": classification.model,
    "tier": format!("{:?}", classification.tier),
}).to_string();
let response = (StatusCode::OK, response_body);
```

e) Construct `InferenceRecord` with populated `category` and `upstream_model` (replaces `category: None` / `upstream_model: None` at lines 108-109):
```rust
category: Some(classification.category.clone()),
upstream_model: Some(classification.model.clone()),
```

f) Keep the fire-and-forget logging unchanged.

#### 4. Update test_app()

**File**: `src/main.rs`

**Intent**: Set classifier to `None` in the test app so existing auth tests continue to work without classification.

**Contract**: In `test_app()` (line 233), add `classifier: None` to the `AppState` construction.

#### 5. OpenAPI specification

**File**: `openapi/completions.yaml` (new file, per `lessons.md` rule)

**Intent**: Document the modified `/v1/chat/completions` response contract. The endpoint now returns JSON instead of a plain-text placeholder.

**Contract**: A minimal OpenAPI 3.0 spec for the `POST /v1/chat/completions` endpoint documenting:
- 200 response schema: `{"status": "classified", "category": "SYNTAX_FIX", "model": "gpt-4-turbo", "tier": "Regex"}`
- 401 response (auth failure, unchanged)
- Bearer token security requirement

### Success Criteria:

#### Automated Verification:
- `cargo build` — compiles with new `src/main.rs` changes
- `cargo test auth` — all auth tests pass (routes_auth_proxy_requires_valid_bearer_token returns 200 with correct token, handler returns JSON with CASUAL fallback since classifier is None in test_app)
- `cargo test routes_auth` — all route authorization tests pass
- `cargo test` — all existing tests pass, no regressions
- `openapi/completions.yaml` — valid OpenAPI 3.0, passes `swagger-cli validate` (or equivalent)

#### Manual Verification:
- Start gateway with `cargo run`, send a POST to `/v1/chat/completions` with a JSON body containing `{"messages":[{"role":"user","content":"fix this bug"}]}` via curl — response is 200 with JSON containing `"category":"SYNTAX_FIX"`
- Start gateway without `routing.toml` — WARN printed, but gateway still starts and classifies (using hardcoded fallback routing)
- Check dashboard `/dashboard/inferences` — new inference records show `category` and `upstream_model` populated (no longer "—")

---

## Phase 3: Tests

### Overview

Add unit tests for the classifier's `classify()` method, verify existing handler tests still pass, and add a basic integration test through the handler.

### Changes Required:

#### 1. Classifier unit tests

**File**: `src/intent_classificator.rs` (append `#[cfg(test)] mod tests`)

**Intent**: Verify each category is correctly classified with representative prompts, and edge cases produce the expected Fallback result.

**Contract**: Tests follow the existing co-located test pattern (`auth.rs:163-223`, `persistence.rs:334-629`):

- `test_classify_file_reading` — prompts like "read the contents of src/main.rs" and "show me Cargo.toml" → FILE_READING
- `test_classify_complex_reasoning` — "architect a distributed rate limiter", "how would you design a caching layer" → COMPLEX_REASONING
- `test_classify_syntax_fix` — "fix this compilation error", "why doesn't this compile" → SYNTAX_FIX
- `test_classify_casual` — "hello", "what is Rust?", empty string → CASUAL
- `test_classify_fallback_on_ambiguous` — multiple categories equally scored → Fallback
- `test_classify_negative_suppression` — "read the architecture document" → FILE_READING (not COMPLEX_REASONING)
- `test_extract_prompt_text_extracts_last_user_message` — parse standard OpenAI JSON body
- `test_extract_prompt_text_returns_empty_on_invalid_json` — graceful on malformed input

#### 2. Handler integration test

**File**: `src/main.rs` (append to `#[cfg(test)] mod tests`)

**Intent**: Verify the handler returns JSON with classification fields when the classifier is available.

**Contract**: A new test function `test_completion_handler_returns_classification_json` that:
- Builds a test app with a real `IntentClassifier` (constructed via `from_values` with test routing)
- Sends a POST with `{"messages":[{"role":"user","content":"fix this bug"}]}`
- Asserts 200 OK, Content-Type is `application/json`, body contains `"category":"SYNTAX_FIX"`

### Success Criteria:

#### Automated Verification:
- `cargo test intent_classificator` — all new unit tests pass
- `cargo test test_completion_handler_returns_classification_json` — integration test passes
- `cargo test` — full test suite passes with no regressions

#### Manual Verification:
- Run `cargo test -- --nocapture` and verify test output shows classification decisions as expected

---

## Testing Strategy

### Unit Tests:
- Classification accuracy for each of the 4 intent categories (~6 tests)
- Edge cases: empty prompt, ambiguous prompt, negative suppression, non-English, very long prompts
- `extract_prompt_text()`: valid JSON, invalid JSON, missing messages field, no user message, >1000 messages (DoS guard)
- Routing TOML parsing: valid file, missing file (fallback), malformed TOML

### Integration Tests:
- Handler returns 200 with JSON for valid request + valid token
- JSON response contains `status`, `category`, `model`, `tier` fields
- Handler returns 401 without token (existing test — must still pass)
- Handler with `classifier: None` returns CASUAL fallback (existing test_pattern — no crash)

### Manual Testing Steps:
1. `cargo run` with `routing.toml` present — verify classification via curl
2. `cargo run` without `routing.toml` — verify WARN + CASUAL fallback
3. Send several diverse prompts through curl — verify categories match expectations
4. Check dashboard after 5-10 requests — verify category/model badges appear

## References

- Research: `context/changes/proxy-intent-routing/research.md`
- Roadmap S-01 definition: `context/foundation/roadmap.md:24`
- Existing pattern: persistence init block `src/main.rs:48-57`
- Existing pattern: auth from_env `src/auth.rs:18-28`
- Existing pattern: test_app `src/main.rs:226-235`
- Existing pattern: AuthConfig::from_values `src/auth.rs:47-58`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: New Module Scaffolding

#### Automated

- [x] 1.1 `cargo build` compiles with new deps and module — a06ad2b
- [x] 1.2 `cargo test intent_classificator` — module tests pass — a06ad2b
- [x] 1.3 `cargo test persistence` — persistence tests pass (extract_snippet unchanged behavior) — a06ad2b

#### Manual

- [ ] 1.4 `routing.toml` readable by classifier (run locally, no TOML-related WARNs)
- [ ] 1.5 Hardcoded fallback routing works when `routing.toml` absent (delete file, restart, verify WARN + startup)

### Phase 2: Wire into Handler

#### Automated

- [x] 2.1 `cargo build` compiles with handler changes — 6585c1f
- [x] 2.2 `cargo test auth` — all auth tests pass — 6585c1f
- [x] 2.3 `cargo test routes_auth` — all route authorization tests pass — 6585c1f
- [x] 2.4 `cargo test` — all existing tests pass with no regressions — 6585c1f
- [x] 2.5 `openapi/completions.yaml` — valid OpenAPI 3.0 spec — 6585c1f

#### Manual

- [ ] 2.6 curl POST with "fix this bug" → 200 JSON with `"category":"SYNTAX_FIX"`
- [ ] 2.7 Gateway starts and classifies without `routing.toml` (WARN + CASUAL fallback)
- [ ] 2.8 Dashboard `/dashboard/inferences` shows populated category/model badges

### Phase 3: Tests

#### Automated

- [ ] 3.1 `cargo test intent_classificator` — all unit tests pass
- [ ] 3.2 `cargo test test_completion_handler_returns_classification_json` — integration test passes
- [ ] 3.3 `cargo test` — full suite passes, no regressions
