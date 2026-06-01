# Provider-Agnostic Routing Configuration — Implementation Plan

## Overview

Add `provider_type` and `api_key_env` fields to `RouteEntry` and `ClassificationResult`, update TOML parsing to read them, enrich the `/v1/chat/completions` JSON response with endpoint/provider_type/api_key, and define an `auth_headers_for` lookup function. This makes the routing config final before SSE streaming (Change 4), replacing the single `UPSTREAM_API_KEY` assumption with per-category provider configuration.

## Current State Analysis

- **`RouteEntry`** (`src/intent_classificator.rs:10-14`) carries `model`, `endpoint`, `cost_per_1m_input_tokens` — but has no notion of which provider or auth scheme to use. All entries implicitly assume OpenRouter-compatible Bearer auth.
- **`ClassificationResult`** (`src/intent_classificator.rs:56-61`) carries `category`, `model`, `endpoint`, `tier` — the handler builds JSON from these but omits provider details.
- **`load_routing_from_file`** (`src/intent_classificator.rs:326-354`) parses `model`, `endpoint`, `cost_per_1m_input_tokens` from TOML — adding two more fields is a straightforward extension.
- **`hardcoded_routing`** (`src/intent_classificator.rs:217-257`) provides compile-time defaults when `routing.toml` is missing — these currently assume empty endpoint (no upstream proxy).
- **`completion_handler`** (`src/main.rs:186-194`) and **`classify_handler`** (`:199-208`) both call `classify_and_log` (`:126-182`) which builds `{status, category, model, tier}` JSON. Only `completion_handler` should be enriched per user decision.
- **`routing.toml.example`** hardcodes all endpoints to `https://openrouter.ai/api/v1/chat/completions` — the last remaining provider-specific assumption in the repository.
- **Test fixtures** in both `main.rs` and `intent_classificator.rs` construct `RouteEntry` without the new fields and will need updating.

### Key Discoveries

- The Rust source has zero references to OpenRouter, Anthropic, or any specific provider — the source is already provider-agnostic (`research.md:45-46`).
- `toml` crate (`0.8`) is already in `Cargo.toml` — no new dependencies.
- `ClassifyAndLog` is shared between both proxy endpoints; enriching only `completion_handler` requires diverging the handler implementations or extracting a logging helper.

## Desired End State

- `RouteEntry` carries `provider_type: String` (`"openai_compatible"`, `"anthropic"`, `"ollama"`) and `api_key_env: Option<String>` — used for TOML config and hardcoded defaults.
- `ClassificationResult` carries the same two fields, propagated from the matched route entry.
- `routing.toml` supports `provider_type` and `api_key_env` per category, with sensible defaults (empty `provider_type` = `openai_compatible`, absent `api_key_env` = `None`).
- The fallback entry in `routing.toml` supports the same fields as regular entries.
- `POST /v1/chat/completions` returns enriched JSON: `{status, category, model, tier, endpoint, provider_type, api_key}` — where `api_key` is `null` if the env var is missing.
- `POST /v1/classify` continues returning the existing minimal JSON: `{status, category, model, tier}`.
- `auth_headers_for(provider_type, api_key)` is defined in `intent_classificator.rs` and maps provider type to auth header tuples, ready for Change 4's upstream proxying.
- All existing tests pass; new test cases cover the new fields and the `null` api_key fallback.

## What We're NOT Doing

- **Anthropic body translation** — `provider_type = "anthropic"` is defined in TOML and the auth lookup, but the request body is not translated. Routing to Anthropic will error until Change 4 adds the body adapter.
- **`extra_headers` field** — Provider-specific headers (e.g., OpenRouter's `HTTP-Referer`) are not configurable in this change.
- **Two-level TOML design** (`[providers.X]` sections) — flat single-level format only for MVP.
- **Timeout / retry configuration per provider** — deferred.
- **Changing `/v1/classify` response format** — stays minimal.

## Implementation Approach

Bottom-up: define the data model first (Phase 1), wire it through TOML parsing (Phase 2), then surface it in the handler (Phase 3), and finally update config examples and test fixtures (Phase 4).

## Critical Implementation Details

- **Timing & lifecycle**: The handler resolves `api_key_env` lazily at classification time (not at startup). If the env var is missing, `api_key` is `null` in the response — no 502, no fallback to a different route. This means the downstream proxy (Change 4) receives a `null` api_key and must decide whether to 502 or skip.

---

## Phase 1: Core Data Model

### Overview

Add `provider_type` and `api_key_env` to `RouteEntry` and `ClassificationResult`, update all constructors and propagation code paths, and define the `auth_headers_for` lookup function.

### Changes Required

#### 1. RouteEntry struct

**File**: `src/intent_classificator.rs:10-14`

**Intent**: Add two fields so each route entry carries its provider identity and API key source.

**Contract**: Add after `cost_per_1m_input_tokens`:
```rust
pub provider_type: String,
pub api_key_env: Option<String>,
```

#### 2. ClassificationResult struct + fallback() constructor

**File**: `src/intent_classificator.rs:56-61`, `:379-389`

**Intent**: ClassificationResult must carry provider details forward to the handler so it can build the enriched response.

**Contract**: Add the same two fields (`provider_type: String`, `api_key_env: Option<String>`) after `tier`. In `ClassificationResult::fallback()`, initialize them to `String::new()` and `None`.

#### 3. route_match and route_fallback methods

**File**: `src/intent_classificator.rs:516-533`

**Intent**: Propagate the new fields from the matched/fallback `RouteEntry` into the `ClassificationResult`.

**Contract**: In `route_match`, add `.provider_type: route.provider_type.clone(), .api_key_env: route.api_key_env.clone()`. Same in `route_fallback` using `self.fallback_entry`.

#### 4. hardcoded_routing

**File**: `src/intent_classificator.rs:217-257`

**Intent**: All hardcoded route entries must initialize the new fields with sensible defaults.

**Contract**: Add `provider_type: String::new(), api_key_env: None` to every `RouteEntry` constructor in this function (5 entries total: COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL, fallback).

#### 5. auth_headers_for function

**File**: `src/intent_classificator.rs` (new function, near `hardcoded_routing` or after `ClassificationResult`)

**Intent**: Provide a public lookup that maps provider_type strings to HTTP auth header tuples. This is the canonical auth mapping used by the downstream proxy in Change 4.

**Contract**:
```rust
pub fn auth_headers_for(provider_type: &str, api_key: &str) -> Vec<(String, String)> {
    match provider_type {
        "openai_compatible" | "" =>
            vec![("authorization".into(), format!("Bearer {api_key}"))],
        "anthropic" =>
            vec![("x-api-key".into(), api_key.to_string())],
        "ollama" | "local" =>
            vec![],
        _ =>
            vec![("authorization".into(), format!("Bearer {api_key}"))],
    }
}
```

### Success Criteria

#### Automated Verification

- Project compiles: `cargo build`
- All existing tests pass: `cargo test`
- Unit tests pass: `cargo test intent_classificator`
- Type checking passes: `cargo check`

#### Manual Verification

- Inspect `RouteEntry` and `ClassificationResult` — confirm new fields are present and all constructors supply them
- Inspect `auth_headers_for` — confirm all provider_type variants map correctly

---

## Phase 2: TOML Parsing

### Overview

Extend `load_routing_from_file` to read `provider_type` and `api_key_env` from TOML entries, including the fallback entry.

### Changes Required

#### 1. load_routing_from_file parsing

**File**: `src/intent_classificator.rs:326-354`

**Intent**: Read the two new fields from each TOML table entry, with sensible defaults when absent.

**Contract**: After reading `cost_per_1m_input_tokens` (line 346-347), add:
- `provider_type` — read via `value.get("provider_type").and_then(|v| v.as_str()).unwrap_or("")` → `to_string()`
- `api_key_env` — read via `value.get("api_key_env").and_then(|v| v.as_str()).map(|s| s.to_string())` → `Option<String>`

Update the `RouteEntry` constructor on line 350 to include `provider_type, api_key_env`.

#### 2. Fallback entry fallback defaults

**File**: `src/intent_classificator.rs:369-373`

**Intent**: When `routing.toml` has no `[FALLBACK]` section, the hardcoded fallback RouteEntry must include the new fields.

**Contract**: Add `provider_type: String::new(), api_key_env: None` to the `unwrap_or_else` closure's RouteEntry.

### Success Criteria

#### Automated Verification

- Project compiles: `cargo build`
- All existing tests pass: `cargo test`
- Unit tests pass: `cargo test intent_classificator`

#### Manual Verification

- Create a temporary `routing.toml` with provider_type and api_key_env fields, verify the parser reads them correctly (inspect via debug print or test)
- Verify that a `routing.toml` without the new fields still parses (backward compatible — fields default to empty/None)

---

## Phase 3: Handler Enrichment

### Overview

Modify `completion_handler` to resolve the API key from the env var and build an enriched JSON response including `endpoint`, `provider_type`, and `api_key`. The `classify_handler` stays unchanged (minimal response). Extract a shared logging helper to avoid duplicating the inference record construction between the two handlers.

### Changes Required

#### 1. Extract logging helper from classify_and_log

**File**: `src/main.rs` (new private function)

**Intent**: Both handlers need to log inference records. Extracting the logging portion into a standalone function avoids duplicating ~15 lines when `completion_handler` diverges from `classify_and_log`.

**Contract**: Signature:
```rust
fn log_classification(
    state: &AppState,
    classification: &ClassificationResult,
    body_str: &str,
    start: std::time::Instant,
    log_status: &str,
)
```

This function encapsulates: snippet extraction, prompt char count, record construction, and `persistence::log_inference` call. It is called by both handlers after they build their respective responses.

#### 2. Rewrite completion_handler

**File**: `src/main.rs:186-194`

**Intent**: `completion_handler` no longer calls `classify_and_log`. Instead it validates content-type, extracts the prompt, classifies, resolves the API key, builds the enriched JSON, logs, and returns.

**Contract**: The handler performs these steps:

1. Validate `Content-Type: application/json` (return 415 if missing)
2. Extract prompt via `persistence::extract_last_user_message(&body_str)`
3. Classify: `state.classifier.as_ref().map(|c| c.classify(&prompt)).unwrap_or_else(ClassificationResult::fallback)`
4. Resolve api_key: `classification.api_key_env.as_ref().and_then(|env_name| std::env::var(env_name).ok())` → `Option<String>`
5. Build enriched JSON with:
   ```json
   {
     "status": "classified",
     "category": "<category>",
     "model": "<model>",
     "tier": "<tier>",
     "endpoint": "<endpoint>",
     "provider_type": "<provider_type>",
     "api_key": "<key>" or null
   }
   ```
6. Call `log_classification` with `log_status = "ok"`
7. Return `(StatusCode::OK, json_string)`

**Implementation note**: The handler handles `api_key_env` resolution as `Some(env_name) → std::env::var(env_name).ok()`, which returns `None` if the env var is missing. `serde_json::json!` naturally serializes `Option<String>` as `null` when `None`.

#### 3. classify_and_log stays for classify_handler

**File**: `src/main.rs:126-182` (unchanged except logging extraction)

**Intent**: `classify_handler` continues using `classify_and_log`, which keeps the existing minimal JSON format. Replace its inline logging block with a call to `log_classification`.

**Contract**: In `classify_and_log`, replace lines 155-178 (the `if let Some(log_status)` block) with a call to `log_classification(&state, &classification, body_str, start, log_status)`.

### Success Criteria

#### Automated Verification

- Project compiles: `cargo build`
- All existing tests pass: `cargo test`
- Auth tests pass: `cargo test auth`
- Route auth tests pass: `cargo test routes_auth`

#### Manual Verification

- `POST /v1/chat/completions` with a valid bearer token returns enriched JSON containing `endpoint`, `provider_type`, and `api_key` fields
- `POST /v1/classify` with a valid bearer token returns minimal JSON (no new fields)
- When `api_key_env` points to a valid env var, `api_key` is the resolved value
- When `api_key_env` points to a missing env var, `api_key` is `null`
- When `api_key_env` is absent (None), `api_key` is `null`

---

## Phase 4: Config & Test Fixtures

### Overview

Update `routing.toml.example` to the provider-agnostic format. Update all test `RouteEntry` constructors to include the two new fields.

### Changes Required

#### 1. routing.toml.example

**File**: `routing.toml.example`

**Intent**: Replace the OpenRouter-only example with a provider-agnostic configuration demonstrating all three provider types.

**Contract**: Replace the entire file with:
```toml
# Cerebrum routing configuration
# Maps each classified intent to an upstream model, provider, and auth key.
# Copy to routing.toml and adjust values for your environment.

[COMPLEX_REASONING]
model = "claude-sonnet-4-20250514"
endpoint = "https://api.anthropic.com/v1/messages"
provider_type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[FILE_READING]
model = "deepseek-chat"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"

[SYNTAX_FIX]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"

[CASUAL]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"

[FALLBACK]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"
```

#### 2. Test RouteEntry constructors — intent_classificator.rs

**File**: `src/intent_classificator.rs:540-563`

**Intent**: All test RouteEntry constructors must include the new fields with empty/None defaults.

**Contract**: Add `provider_type: String::new(), api_key_env: None` to every `RouteEntry` in `test_classifier()` (5 entries: FILE_READING, COMPLEX_REASONING, SYNTAX_FIX, CASUAL, fallback).

#### 3. Test RouteEntry constructors — main.rs

**File**: `src/main.rs:428-451`

**Intent**: The test app with classifier constructs RouteEntry entries that now require the new fields.

**Contract**: Add `provider_type: String::new(), api_key_env: None` to the SYNTAX_FIX, CASUAL, and fallback `RouteEntry` constructors in `test_app_with_classifier()`.

#### 4. New test: enriched response fields

**File**: `src/main.rs` (new test in `mod tests`)

**Intent**: Verify that `/v1/chat/completions` returns the enriched fields.

**Contract**: A test that:
- Builds a test app with a classifier whose routing includes `provider_type: "test_provider".to_string(), api_key_env: Some("TEST_API_KEY".to_string())`
- Sets `TEST_API_KEY` env var temporarily
- Sends a request to `/v1/chat/completions`
- Asserts the response contains `"provider_type":"test_provider"`, `"endpoint"`, and `"api_key"`

#### 5. New test: api_key null when env var missing

**File**: `src/main.rs` (new test in `mod tests`)

**Intent**: Verify that a missing env var produces `null` for `api_key`.

**Contract**: A test that:
- Builds a test app with `api_key_env: Some("MISSING_KEY_XYZ")`
- Sends a request to `/v1/chat/completions`
- Asserts the response contains `"api_key":null`

#### 6. New test: /v1/classify does NOT include enriched fields

**File**: `src/main.rs` (new test in `mod tests`)

**Intent**: Verify that `/v1/classify` stays minimal.

**Contract**: A test that sends a request to `/v1/classify` and asserts the response does NOT contain `"provider_type"` or `"api_key"`.

### Success Criteria

#### Automated Verification

- Project compiles: `cargo build`
- All tests pass: `cargo test`
- Auth tests pass: `cargo test auth`
- Route auth tests pass: `cargo test routes_auth`

#### Manual Verification

- `routing.toml.example` is human-readable and demonstrates all three provider types
- All test RouteEntry constructors compile with the new fields
- New tests verify enriched fields, null api_key fallback, and classify endpoint exclusion

---

## Testing Strategy

### Unit Tests (existing, updated):

- `intent_classify_*` tests — verify classification still works (RouteEntry constructors updated with new fields)
- `model_costs_*` tests — verify cost overrides still work (RouteEntry constructors updated)

### Integration Tests (existing, unchanged):

- `test_completion_handler_returns_classification_json` — still passes (response format expanded with new fields, but assertions on `category`, `status`, `tier` remain valid)
- `test_classify_handler_returns_classification_json` — still passes (classify_handler stays minimal)
- `routes_auth_*` — auth layer unchanged

### New Tests:

- `test_completion_enriched_response_fields` — verifies provider_type, endpoint, api_key in response
- `test_completion_api_key_null_when_env_missing` — verifies null fallback
- `test_classify_no_enriched_fields` — verifies classify stays minimal

### Manual Testing Steps:

1. Set up a `routing.toml` with at least one category using `api_key_env` pointing to a valid env var
2. `POST /v1/chat/completions` with a chat message — verify enriched JSON is correct
3. `POST /v1/classify` with a chat message — verify response is minimal
4. Remove the env var referenced by `api_key_env` — verify `api_key` is `null`
5. Remove `routing.toml` entirely — verify hardcoded defaults work (classifier falls back to built-in entries)

## Performance Considerations

- `std::env::var` is called once per request (not in a hot loop) — negligible overhead
- `auth_headers_for` is a static match — not called in this change, only defined for Change 4
- `Routing.toml` parsing happens once at startup — no runtime cost
- No new heap allocations beyond two `String` clones per request (provider_type and api_key_env)

## Migration Notes

- Existing `routing.toml` files without `provider_type` or `api_key_env` continue to work — empty defaults mean `openai_compatible` with no key
- The `UPSTREAM_API_KEY` env var (from Change 2) is no longer the canonical key source — each route entry specifies its own `api_key_env`. A deployment updating from Change 2 to Change 3 should replace `UPSTREAM_API_KEY` with per-provider env vars

## References

- Research: `context/changes/provider-agnostic-config/research.md`
- Master research: `context/changes/upstream-proxy-routing/research.md`

---

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Core Data Model

#### Automated

- [x] 1.1 Project compiles: `cargo build` — cf389c6
- [x] 1.2 All existing tests pass: `cargo test` — cf389c6
- [x] 1.3 Unit tests pass: `cargo test intent_classificator` — cf389c6
- [x] 1.4 Type checking passes: `cargo check` — cf389c6

#### Manual

- [ ] 1.5 Inspect RouteEntry/ClassificationResult — new fields present, all constructors supply them
- [ ] 1.6 Inspect auth_headers_for — all provider_type variants mapped correctly

### Phase 2: TOML Parsing

#### Automated

- [x] 2.1 Project compiles: `cargo build` — cf389c6
- [x] 2.2 All existing tests pass: `cargo test` — cf389c6
- [x] 2.3 Unit tests pass: `cargo test intent_classificator` — cf389c6

#### Manual

- [ ] 2.4 Verify TOML parser reads provider_type and api_key_env correctly
- [ ] 2.5 Verify backward compatibility — TOML without new fields still parses

### Phase 3: Handler Enrichment

#### Automated

- [x] 3.1 Project compiles: `cargo build` — 309622e
- [x] 3.2 All existing tests pass: `cargo test` — 309622e
- [x] 3.3 Auth tests pass: `cargo test auth` — 309622e
- [x] 3.4 Route auth tests pass: `cargo test routes_auth` — 309622e

#### Manual

- [ ] 3.5 POST /v1/chat/completions returns enriched JSON (endpoint, provider_type, api_key)
- [ ] 3.6 POST /v1/classify returns minimal JSON (no new fields)
- [ ] 3.7 Valid api_key_env → resolved key in response
- [ ] 3.8 Missing api_key_env → null in response
- [ ] 3.9 Absent api_key_env → null in response

### Phase 4: Config & Test Fixtures

#### Automated

- [x] 4.1 Project compiles: `cargo build` — 309622e
- [x] 4.2 All tests pass: `cargo test` — 309622e
- [x] 4.3 Auth tests pass: `cargo test auth` — 309622e
- [x] 4.4 Route auth tests pass: `cargo test routes_auth` — 309622e

#### Manual

- [ ] 4.5 routing.toml.example demonstrates all three provider types
- [ ] 4.6 All test RouteEntry constructors compile with new fields
