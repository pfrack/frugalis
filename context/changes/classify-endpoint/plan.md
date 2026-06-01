# Classify Endpoint Implementation Plan

## Overview

Add a dedicated `POST /v1/classify` endpoint that extracts the last user message from an OpenAI-compatible body, classifies intent, logs a lightweight classification record, and returns the classification result as JSON. The endpoint is decoupled from the proxy handler and lives under the same bearer auth middleware. This is Change 1 of 4 in the upstream proxy routing sequence.

## Current State Analysis

`completion_handler` at `src/main.rs:119-175` does classification + JSON response + inference logging in one function. The proxy routes sub-router at `src/main.rs:337-342` has only one route (`POST /chat/completions`). `openapi/completions.yaml` documents only `POST /v1/chat/completions`.

Classification logic (`classify()`) at `src/intent_classificator.rs:456-534` is pure and stateless — any handler with `AppState` can call it. `extract_last_user_message()` at `src/persistence.rs:417-447` is a shared utility already imported and used by `completion_handler`.

### Key Discoveries:

- `src/main.rs:337-342` — `proxy_routes` router with `.layer(require_proxy_bearer)` — all routes under `/v1` inherit auth
- `src/main.rs:56-60` — `AppState` carries both `persistence` and `classifier`, both used by classify_handler
- `src/main.rs:362-422` — `test_app()` and `test_app_with_classifier()` constructors provide test state
- `openapi/completions.yaml:1-63` — existing OpenAPI 3.0.3 spec with only `/v1/chat/completions`

## Desired End State

A `POST /v1/classify` with bearer auth and `Content-Type: application/json` body:
```json
{"messages":[{"role":"user","content":"fix this bug"}]}
```
returns:
```json
{"status":"classified","category":"SYNTAX_FIX","model":"gpt-4o-mini","tier":"Regex"}
```

A lightweight classification record is logged to the `inferences` table with `status = "classified"`. The existing `POST /v1/chat/completions` behavior is unchanged.

## What We're NOT Doing

- No changes to `completion_handler` (still returns classification JSON)
- No changes to `Cargo.toml`, `src/intent_classificator.rs`, `src/auth.rs`, `src/persistence.rs`
- No `reqwest` dependency, no upstream HTTP calls, no SSE streaming
- No endpoint/provider info in classify response — classification metadata only
- `test_completion_handler_returns_classification_json` still hits `/v1/chat/completions` (migrates in Change 4)

## Implementation Approach

Extract the classification half of `completion_handler` into a dedicated `classify_handler`. The handler follows the same pattern: validate Content-Type, parse body, extract prompt, classify, log, return JSON. The classify handler logs with `status = "classified"` (distinct from the proxy handler's `"ok"`) to differentiate classification-only records in the dashboard.

## Phase 1: Add Classify Handler + Route + OpenAPI Spec

### Overview

Add `classify_handler` function, register the route in `build_app`, and update the OpenAPI specification to document both endpoints.

### Changes Required:

#### 1. New classify_handler function

**File**: `src/main.rs`

**Intent**: Add a dedicated handler for `POST /v1/classify` that extracts the prompt, classifies intent, logs a lightweight record, and returns classification JSON. Mirrors the first half of `completion_handler` — Content-Type validation, prompt extraction, classification call, and response assembly — plus a fire-and-forget persistence log with `status = "classified"`.

**Contract**: New `async fn classify_handler` inserted after `completion_handler` (after line 175). Signature: `async fn classify_handler(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> (StatusCode, String)`. Logic:

- Guard `Content-Type: application/json` (same pattern as `completion_handler:124-130`)
- Start timer: `let start = std::time::Instant::now();`
- Extract prompt via `persistence::extract_last_user_message(body_str)`
- Classify via `state.classifier.as_ref().map(|c| c.classify(&prompt)).unwrap_or_else(ClassificationResult::fallback)`
- Return JSON `{"status":"classified","category":...,"model":...,"tier":...}`
- Fire-and-forget log: construct `InferenceRecord` with `status = "classified"`, all other fields populated identically to `completion_handler` pattern (duration, snippet, char count, category, model). Skip logging if `persistence` is `None`.

#### 2. Register classify route

**File**: `src/main.rs`

**Intent**: Add `POST /v1/classify` to the existing `proxy_routes` sub-router so it inherits bearer auth middleware.

**Contract**: In `build_app()` at line 338 (inside `proxy_routes` block), add:
```rust
.route("/classify", post(classify_handler))
```

#### 3. Update OpenAPI specification

**File**: `openapi/completions.yaml`

**Intent**: Document the new `POST /v1/classify` endpoint alongside the existing `POST /v1/chat/completions`. Update the title/description to reflect the expanded API surface. Per `lessons.md`, endpoints must be documented via OpenAPI.

**Contract**: Add a `/v1/classify` path entry with the same request body schema as `/v1/chat/completions` (OpenAI-compatible `{"messages": [...]}`), the same `bearerAuth` security, and a 200 response matching the classification JSON schema. Update 401 response to use `application/json` (reflecting the actual `api_unauthorized_response` format) rather than the current `text/plain`. Rename title to "Cerebrum Proxy API" and description to cover both classify and completions.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with new handler and route
- `cargo test auth` — all auth tests pass
- `cargo test routes_auth` — all route authorization tests pass
- `cargo test` — full suite passes, no regressions
- `openapi/completions.yaml` — valid OpenAPI 3.0.3, passes structural validation

#### Manual Verification:

- curl POST to `/v1/classify` with valid bearer token and OpenAI-compatible body returns 200 with classification JSON
- curl POST to `/v1/classify` without bearer token returns 401
- curl POST to `/v1/classify` without `Content-Type: application/json` returns 415
- Dashboard `/dashboard/inferences` shows classify records with `status = "classified"` (visible via category badge)
- Existing `/v1/chat/completions` behavior unchanged (test via curl)

---

## Phase 2: Tests

### Overview

Add an integration test for the new classify endpoint and verify no existing tests break.

### Changes Required:

#### 1. Classify endpoint integration test

**File**: `src/main.rs` (append to `#[cfg(test)] mod tests`)

**Intent**: Verify the classify endpoint returns correct classification JSON for a known prompt pattern.

**Contract**: New `#[tokio::test] async fn test_classify_handler_returns_classification_json` that:
- Uses `test_app_with_classifier()` (classifier available, SYNTAX_FIX routes to "sf-model")
- Sends POST to `/v1/classify` with bearer token, content-type, and `{"messages":[{"role":"user","content":"fix this bug"}]}`
- Asserts 200 OK
- Asserts body contains `"category":"SYNTAX_FIX"`, `"model":"sf-model"`, `"status":"classified"`, `"tier":"Regex"`
- Follows the same pattern as the existing `test_completion_handler_returns_classification_json` at lines 424-453

### Success Criteria:

#### Automated Verification:

- `cargo test test_classify_handler_returns_classification_json` — new test passes
- `cargo test` — full suite passes with no regressions (all 17 existing tests unchanged)

#### Manual Verification:

- Run `cargo test -- --nocapture` and verify classification test output

---

## Testing Strategy

### Unit Tests:
- No unit-level changes — `classify_handler` is integration-tested through the Axum router

### Integration Tests:
- `test_classify_handler_returns_classification_json` — classification JSON with correct category/model/tier
- Existing `routes_auth_proxy_requires_valid_bearer_token` — verifies `/v1/classify` requires auth (uses POST `/v1/chat/completions` but auth middleware applies to all `/v1/*`)
- All existing dashboard/DB tests unchanged

### Manual Testing Steps:
1. Start gateway with `cargo run`, POST to `/v1/classify` with diverse prompts — verify categories match
2. Verify classify endpoint returns 401 without token, 415 without JSON content-type
3. Check dashboard after several classify requests — verify records appear with category/model badges

## Performance Considerations

Classification is CPU-bound but measured in microseconds (~10-50µs per `RegexSet::matches` call). No `spawn_blocking` needed. Fire-and-forget logging is identical to existing pattern — no impact on response latency.

## Migration Notes

No migration needed. The classify endpoint is additive — zero changes to existing routes, handlers, or data schemas.

## References

- Research: `context/changes/upstream-proxy-routing/research.md` (Sections 22-25, 28)
- Prior implementation: `proxy-intent-routing` plan at `context/changes/proxy-intent-routing/plan.md`
- Existing handler pattern: `src/main.rs:119-175` (`completion_handler`)
- Existing test pattern: `src/main.rs:424-453` (`test_completion_handler_returns_classification_json`)
- Existing route pattern: `src/main.rs:336-360` (`build_app`)
- OpenAPI spec: `openapi/completions.yaml`
- Lessons: `context/foundation/lessons.md` (OpenAPI Generator for endpoints)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Add Classify Handler + Route + OpenAPI Spec

#### Automated

- [x] 1.1 `cargo build` compiles with new handler and route
- [x] 1.2 `cargo test auth` — all auth tests pass
- [x] 1.3 `cargo test routes_auth` — all route authorization tests pass
- [x] 1.4 `cargo test` — full suite passes, no regressions
- [x] 1.5 `openapi/completions.yaml` — valid OpenAPI 3.0.3

#### Manual

- [ ] 1.6 curl POST `/v1/classify` with valid auth → 200 with classification JSON
- [ ] 1.7 curl POST `/v1/classify` without auth → 401
- [ ] 1.8 curl POST `/v1/classify` without Content-Type → 415
- [ ] 1.9 Dashboard shows classify records with status "classified"
- [ ] 1.10 `/v1/chat/completions` behavior unchanged

### Phase 2: Tests

#### Automated

- [ ] 2.1 `cargo test test_classify_handler_returns_classification_json` — test passes
- [ ] 2.2 `cargo test` — full suite passes, no regressions

