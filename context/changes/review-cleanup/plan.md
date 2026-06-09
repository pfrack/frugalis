# Code Review Cleanup Implementation Plan

## Overview

Address 7 findings from the 2026-06-08 code review: 1 critical bug fix (SSE streaming logs "ok" before streaming completes) and 6 important improvements (oversized `completion_handler`, duplicate error construction, single-variant `QueryError`, unused config field, test env pollution, generous HTTP timeout). The changes span `main.rs`, `persistence.rs`, and `config.rs`.

## Current State Analysis

### What exists today

- `completion_handler` (`src/main.rs:322-698`) is ~376 lines handling classification, API key resolution, upstream proxying, streaming, buffered responses, error construction, and logging in a single function. Prior reviews have been regressed by two subsequent rewrites (see `lessons.md:12-17`).
- SSE streaming (`src/main.rs:556-587`) calls `log_classification` at line 602 with status `"ok"` *before* the streaming loop runs. Mid-stream failures produce SSE error events but the DB record stays `"ok"`.
- The same `serde_json::json!({"error":"upstream_error",...}).to_string()` pattern is repeated 6+ times in `completion_handler`.
- `QueryError` in `persistence.rs:17-20` is an enum with a single variant `Database(String)`, adding boilerplate without value.
- `RegexClassifierConfig.timeout_secs` (`config.rs:329`) is declared, documented as "unused", and never read.
- Tests in `main.rs` call `std::env::set_var()` / `remove_var()` without panic-safe guards; `config.rs:586-596` already has an `EnvGuard` pattern.
- `reqwest::Client` is built with `.timeout(300)` and no connect timeout (`main.rs:103-106`).

### Key constraints

- `completion_handler` has a documented regression history — every change must preserve existing behavior per `lessons.md:12-17`.
- The spawned streaming task must satisfy `'static` — any data it captures must be owned.
- The `EnvGuard` pattern already exists in `config.rs`, providing a precedent to follow.
- All changes are additive or replacement — no API contract changes.

## Desired End State

- SSE streaming records reflect actual stream outcomes: a `"streaming"` record is logged before the response starts, and `"ok"` or `"stream_error"` is logged after the stream ends.
- `completion_handler` delegates to focused helper functions, each under ~80 lines, with error response bodies constructed via shared helpers.
- `QueryError` is a transparent struct, not a single-variant enum.
- `RegexClassifierConfig` contains only `enabled`.
- Every test that mutates environment variables uses panic-safe guards.
- `reqwest::Client` has a 120s total timeout and 30s connect timeout.

### Key Discoveries

- **EnvGuard pattern exists** at `src/config.rs:586-596` — use it, don't invent a new one.
- **Prior review regression** — `lessons.md:12-17` documents that `completion_handler` was rewritten twice after fixes were applied. The plan must include a verification checkpoint.
- **Log at start + update pattern** — two separate `log_classification` calls (one before spawn, one in spawned task) is the simplest approach that gives an audit trail without requiring DB UPDATE support.

## What We're NOT Doing

- Creating a new `src/proxy.rs` module — decomposition stays within `main.rs`.
- Adding new error variants or `thiserror` derives to `QueryError`.
- Implementing regex timeout support for `RegexClassifierConfig`.
- Addressing the 3 🟢 nice-to-have items (stored API key in LLMClassifier, `#[allow]` attributes, import hygiene).
- Changing the DB schema or adding UPDATE support for inference records.

## Implementation Approach

Three sequential phases, each independently shippable. Phase 1 fixes the critical bug first so it can be deployed immediately. Phase 2 refactors `completion_handler` — the riskiest change — while the fix from Phase 1 is already in place. Phase 3 is a cleanup sweep of the remaining items.

All phases preserve existing behavior. The test suite (`cargo test` + `cargo test slow_tests`) acts as the regression safety net at each phase boundary.

## Phase 1: Fix SSE Streaming Log Timing

### Overview

Move the final status logging for SSE streaming responses from before the response headers are sent (current behavior) to after the streaming loop completes inside the spawned task. Add a "streaming" status log before spawning so there is always at least one record for every streaming request.

### Changes Required

#### 1. Move final log into spawned streaming task

**File**: `src/main.rs`

**Intent**: In `completion_handler`, the spawned streaming task currently does not log anything — `log_classification` runs at line 602 before the spawn. Move the final status log inside the task so it captures the actual stream outcome. Log `"streaming"` before spawning to ensure traceability even if the process crashes mid-stream.

**Contract**:
- Before the `tokio::spawn` call (line 556): call `log_classification` with status `"streaming"`.
- Capture `state: Arc<AppState>`, `classification: ClassificationResult`, `body_str: String`, `start: Instant` for the spawned task.
- Inside the spawned task, track stream outcome: default `"ok"`, set to `"stream_error"` when the loop breaks on a chunk read error.
- After the loop exits (all break paths), call `log_classification` with the tracked status.
- Keep the existing SSE error event formatting and channel error handling unchanged.

#### 2. Update routing unit tests for streaming

**File**: `src/main.rs`

**Intent**: Existing tests (`test_streaming_handler_returns_sse_content_type`, `test_streaming_handler_forwards_upstream_bytes`, etc.) do not check DB logging. Add assertions or test-only hooks to verify that logging fires with correct status codes when a DB is present.

**Contract**: No test changes required for existing test suite if `test_app()` has `persistence: None` (logging is a no-op). The behavior change is verified by manual inspection of DB records in staging, plus a new test in `slow_tests` that uses a real DB connection and verifies both "streaming" and "ok" records exist after a successful SSE request.

### Success Criteria

#### Automated Verification

- `cargo test` passes all fast tests
- `cargo test slow_tests` passes the keepalive test (existing)
- New integration test: with `DATABASE_URL` set, a successful SSE streaming request produces exactly two inference records with statuses "streaming" and "ok"
- New integration test: with `DATABASE_URL` set, a failed SSE streaming request (upstream error) produces records with "streaming" and "stream_error"

#### Manual Verification

- Deploy to staging, send a streaming request, verify DB has "streaming" + "ok" records
- Simulate upstream failure mid-stream, verify DB has "stream_error" record

---

## Phase 2: Decompose completion_handler + Deduplicate Errors

### Overview

Extract two error response helper functions to replace 6+ duplicate `serde_json::json!({...}).to_string()` patterns. Split `completion_handler` into focused sub-functions: request construction, buffered response handling, and streaming response handling. The main dispatch logic (classification, API key resolution) stays in `completion_handler`.

### Changes Required

#### 1. Add error response helper functions

**File**: `src/main.rs`

**Intent**: Replace all inline `serde_json::json!({"error":...,"status":...,"message":...}).to_string()` constructions with two shared functions, reducing duplication and making error format changes single-point.

**Contract**:
```rust
fn upstream_error_json(status: u16, message: &str) -> String
```
Returns `{"error":"upstream_error","status":<status>,"message":<escaped message>}`. Used in all upstream failure paths (bad gateway, unreachable, too large, non-2xx).

```rust
fn classification_only_json(result: &ClassificationResult) -> String
```
Returns `{"status":"classified","category":...,"model":...,"tier":...}`. Used in all degradation paths where upstream proxying is skipped (no client, no API key, header skip-classify with unknown category).

**Implementation note**: `upstream_error_json` must sanitize the message (escape backslashes, quotes, newlines) since it's embedded in JSON string context. Follow the escaping pattern already used at line 571.

#### 2. Replace inline error JSON with helpers

**File**: `src/main.rs`

**Intent**: Replace all 6+ inline `serde_json::json!({...}).to_string()` calls in `completion_handler` with calls to `upstream_error_json` or `classification_only_json`. This is a mechanical find-and-replace — no behavior change.

**Contract**: Every call site that constructs an error or classification-only response body is replaced. The function signatures match the existing JSON shape exactly. Verify by running the full test suite — all assertions on response body content must still pass.

#### 3. Extract build_upstream_request helper

**File**: `src/main.rs`

**Intent**: Move upstream request construction (parse JSON body, inject model field, attach auth headers) into a separate function, reducing `completion_handler` length and isolating the JSON manipulation logic.

**Contract**:
```rust
fn build_upstream_request(
    client: &reqwest::Client,
    classification: &ClassificationResult,
    body: &Bytes,
    api_key: &str,
) -> Result<(bool, reqwest::RequestBuilder), String>
```
Returns a tuple of `(streaming_flag, RequestBuilder)` or an error string. The streaming flag is extracted from the request body to avoid re-parsing; the RequestBuilder is configured with the model field and auth headers. The caller sends the request. Auth header lookup uses `auth_headers_for` from `intent_classifier.rs`.

**Why the tuple return**: Avoids duplicate parsing of the request body's "stream" field after it has been consumed into the RequestBuilder. This optimization keeps caller code clean and prevents re-serialization overhead.

#### 4. Extract handle_buffered_response helper

**File**: `src/main.rs`

**Intent**: Move the buffered (non-streaming) upstream response handling into a separate function. This covers: reading chunks, enforcing `MAX_UPSTREAM_BODY`, handling non-2xx status codes, reading error bodies, and returning the final JSON response.

**Contract**:
```rust
async fn handle_buffered_response(
    upstream_response: reqwest::Response,
) -> (StatusCode, String)
```
Returns the HTTP status code and JSON body string for the Axum response. Handles all error modes (non-2xx, too large, chunk read error) internally. The caller wraps the result in `json_response()`.

#### 5. Extract handle_streaming_response helper

**File**: `src/main.rs`

**Intent**: Move the SSE streaming setup (channel creation, spawn, response header construction) into a separate function. The caller provides the upstream response bytes stream and receives the Axum `Response`.

**Contract**:
```rust
fn handle_streaming_response(
    state: Arc<AppState>,
    classification: ClassificationResult,
    body_str: String,
    start: Instant,
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
) -> Response<Body>
```
Sets up the mpsc channel, spawns the streaming task (with keepalive), constructs the SSE response with correct headers. Logging is handled inside the spawned task (from Phase 1).

**Note on Unpin**: The `Unpin` bound is required because the byte_stream is moved into a spawned task, which must own all captured data. Trait objects in async contexts require `Unpin` for safe pinning during task execution.

#### 6. Simplify completion_handler dispatch

**File**: `src/main.rs`

**Intent**: After extraction, `completion_handler` becomes a dispatcher: validate content-type, parse body, classify (or use X-Cerebrum-* headers), resolve API key, call `build_upstream_request`, send request, and delegate to `handle_buffered_response` or `handle_streaming_response` based on the `stream` field.

**Contract**: The function shrinks from ~376 lines to ~120 lines. Every code path preserved. All existing tests pass without modification.

### Success Criteria

#### Automated Verification

- `cargo test` passes all fast tests (classification, auth, routing, dashboard)
- `cargo test routes_auth` passes
- `cargo test auth` passes
- `cargo test slow_tests` passes (SSE streaming, keepalive)
- `cargo clippy` produces zero warnings on changed files
- `cargo fmt --check` passes

#### Manual Verification

- Send a non-streaming completion request → verify upstream proxying still works
- Send a streaming completion request → verify SSE output is unchanged
- Send a request with X-Cerebrum-Category/X-Cerebrum-Model headers → verify skip-classify still works
- Simulate upstream returning 503 → verify error JSON format is unchanged
- Simulate upstream returning oversize response → verify 502 with "too large" message

---

## Phase 3: Cleanup Items

### Overview

Four independent cleanups: replace `QueryError` enum with a struct, reduce reqwest timeout, remove unused `timeout_secs` field, and add `EnvGuard` to all tests.

### Changes Required

#### 1. Replace QueryError enum with struct

**File**: `src/persistence.rs`

**Intent**: The single-variant `QueryError::Database(String)` adds unnecessary boilerplate (match arm in Display, enum construction at call sites). Replace with a transparent struct.

**Contract**:
- Replace `pub enum QueryError { Database(String) }` with `pub struct QueryError(pub String)`.
- Update `impl Display` to use the inner string directly (no match arm needed).
- Update all construction sites: `QueryError::Database(e.to_string())` → `QueryError(e.to_string())`.
- Update all pattern matches: `QueryError::Database(msg)` → `QueryError(msg)` or just access `.0`.
- All call sites are in `persistence.rs` and `dashboard.rs` (dashboard passes errors as `.to_string()` to template `error` field — unchanged since Display output is identical).

#### 2. Reduce reqwest timeout and add connect timeout

**File**: `src/main.rs`

**Intent**: The 300-second overall timeout is excessively long. A 120s total + 30s connect timeout is reasonable for LLM upstream responses while preventing indefinite hangs.

**Contract**:
- In `main()` at line 103: change `.timeout(Duration::from_secs(300))` to `.timeout(Duration::from_secs(120))`.
- Add `.connect_timeout(Duration::from_secs(30))` to the builder chain.
- Existing tests that use dead endpoints (`test_upstream_unreachable_returns_502`) already use 1s timeouts on their per-test clients — no impact.

#### 3. Remove unused timeout_secs from RegexClassifierConfig

**File**: `src/config.rs`

**Intent**: `RegexClassifierConfig.timeout_secs` is never read. Remove the field and its initializers.

**Contract**:
- Remove `timeout_secs: u64` from `RegexClassifierConfig` struct.
- Remove `timeout_secs: 5` from `Default` impl — struct becomes `{ enabled: true }`.
- Remove `timeout_secs` reading from `load_regex_classifier_config_from_value` — the function loads only `enabled`.
- Update tests that assert `cfg.timeout_secs == 5` — remove that assertion (or replace with `cfg.enabled` check).

#### 4. Add EnvGuard to all test env var mutations

**File**: `src/main.rs`

**Intent**: Tests that call `std::env::set_var()` without panic-safe cleanup risk leaking environment state to subsequent tests. Apply the `EnvGuard` pattern already used in `config.rs:586-596`.

**Contract**:
- Define `EnvGuard` struct (or re-export from config) with a `Drop` impl that calls `remove_var`.
- Apply to every test in `main.rs` that calls `set_var` / `remove_var`:
  - `test_completion_does_not_include_enriched_fields`
  - `test_completion_no_enriched_fields_with_missing_env`
  - `test_classify_no_enriched_fields`
  - `test_upstream_returns_response`
  - `test_upstream_request_includes_auth_header`
  - `test_upstream_request_includes_content_type_json`
  - `test_upstream_unreachable_returns_502`
  - `test_upstream_skip_classify_via_headers`
  - All SSE streaming tests (6 tests)
- Replace `std::env::remove_var(env)` cleanup lines with guard drops.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests
- `cargo test routes_auth` passes
- `cargo test slow_tests` passes
- `cargo clippy` zero warnings on all changed files
- `cargo fmt --check` passes

#### Manual Verification

- Run `cargo test` twice in succession — no test ordering failures from leaked env vars
- Deploy and verify upstream requests complete within 120s (no regression for slow models)

---

## Testing Strategy

### Unit Tests

- Existing test suite (~50 tests across `tests` and `slow_tests` modules) provides regression coverage for all changed code paths.
- Phase 1 adds 2 new integration tests (requires `DATABASE_URL`): verify "streaming" → "ok" and "streaming" → "stream_error" record sequences.
- Phase 3 removes assertions on `timeout_secs` field, replaces with `enabled` assertions.

### Integration Tests

- `test_upstream_*` tests (Phase 2) verify upstream proxying still works after handler decomposition.
- `test_streaming_*` tests (Phase 2) verify SSE streaming output is byte-identical after refactoring.
- `test_*_enriched_fields` tests (Phase 3) continue to verify no sensitive field leakage with `EnvGuard` protection.

### Manual Testing Steps

1. After Phase 1 deploy: send streaming request, check DB for two records with correct statuses.
2. After Phase 2 deploy: send all 4 request types (streaming, non-streaming, skip-classify, error upstream), verify responses unchanged.
3. After Phase 3 deploy: check that a slow upstream model (>60s, <120s) still completes successfully.

## Performance Considerations

- **Timeout reduction**: The 120s timeout may affect very slow models (e.g., complex reasoning over 2 mins). This is an intentional tradeoff — operators can use per-request overrides via X-Cerebrum headers for such cases. Not expected to be an issue given current model selection (Llama 3.1 8B/70B).
- **Duplicate log inserts**: Phase 1 adds a second DB insert per streaming request. The existing semaphore (max 100 concurrent) already bounds task concurrency; the additional insert is fire-and-forget with no latency impact on the client response path.

## Migration Notes

No schema changes. No config format changes (`RegexClassifierConfig.timeout_secs` is already unused — removing it from the struct is backwards-compatible since config files that set it will silently ignore the value). No API contract changes.

## References

- Code review findings: session 2026-06-08
- Lessons: `context/foundation/lessons.md` (rules: re-run review after follow-up, document guard points, dynamic WHERE clauses, log before falling back)
- EnvGuard precedent: `src/config.rs:586-596`
- Prior change: `context/changes/classifier-config-boundary`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Fix SSE Streaming Log Timing

#### Automated

- [x] 1.1 `cargo test` passes all fast tests
- [x] 1.2 `cargo test slow_tests` passes keepalive test
- [x] 1.3 New integration test: streaming success produces "streaming" + "ok" records
- [x] 1.4 New integration test: streaming failure produces "stream_error" record
- [x] 1.5 `cargo clippy` zero warnings
- [x] 1.6 `cargo fmt --check` passes

#### Manual

- [ ] 1.7 Deploy to staging, verify DB has "streaming" + "ok" records for successful SSE request
- [ ] 1.8 Simulate upstream error mid-stream, verify DB has "stream_error" record

### Phase 2: Decompose completion_handler + Deduplicate Errors

#### Automated

- [x] 2.1 `cargo test` passes all fast tests
- [x] 2.2 `cargo test routes_auth` passes
- [x] 2.3 `cargo test auth` passes
- [x] 2.4 `cargo test slow_tests` passes
- [x] 2.5 `cargo clippy` zero warnings
- [x] 2.6 `cargo fmt --check` passes

#### Manual

- [ ] 2.7 Non-streaming completion request → upstream proxying works
- [ ] 2.8 Streaming completion request → SSE output unchanged
- [ ] 2.9 X-Cerebrum-Category/Model headers → skip-classify works
- [ ] 2.10 Upstream 503 → error JSON format unchanged
- [ ] 2.11 Upstream oversize response → 502 with "too large" message

### Phase 3: Cleanup Items

#### Automated

- [x] 3.1 `cargo test` passes all tests (run twice to verify no env leak)
- [x] 3.2 `cargo test routes_auth` passes
- [x] 3.3 `cargo test slow_tests` passes
- [x] 3.4 `cargo clippy` zero warnings
- [x] 3.5 `cargo fmt --check` passes

#### Manual

- [ ] 3.6 Slow upstream model (>60s, <120s) completes successfully
