# Review Hardening Implementation Plan

## Overview

Address 7 findings from the 2026-06-09 code review: 3 critical (test undefined behavior from `set_var` in concurrent tests, DDL migration on every boot, stale LLM API key) and 4 important (streaming double-log, SSE JSON construction safety, constant-time auth length oracle, fragile dynamic SQL). Nits excluded from scope.

## Current State Analysis

### What exists today

- **Test UB**: 22 tests across 3 files (`main.rs`, `config.rs`, `intent_classifier.rs`) call `std::env::set_var`/`remove_var` under `#[tokio::test]` (multi-threaded). This is UB in Rust ≥1.66. An `EnvGuard` pattern exists but only prevents leaks — it doesn't serialize access.
- **Startup DDL**: `persistence.rs:from_env()` runs `ALTER TABLE inferences ADD COLUMN IF NOT EXISTS prompt_char_count INTEGER` on every boot. Migrations directory has `001` and `002` but `sqlx::migrate!()` is not used; the `migrate` feature is missing from `Cargo.toml`.
- **Stale API key**: `LLMClassifier` stores `api_key: String` resolved once at construction. No refresh mechanism.
- **Auth oracle**: `constant_time_eq_str` in `auth.rs` short-circuits on length mismatch, leaking token length via timing.
- **Streaming double-log**: The error path logs "streaming" then immediately "upstream_error" for requests that never actually streamed.
- **SSE JSON**: The streaming task builds error events with `format!()`, which doesn't escape all JSON-special characters.

### Key constraints

- Render has no separate migration step — migrations must run embedded at app startup via `sqlx::migrate!()`.
- `LLMClassifier` is stored as `Arc<dyn IntentClassify>` in `ClassifierChain` — the refresh task must capture a shared handle to the key.
- `hmac` and `sha2` are already transitive deps in `Cargo.lock` — adding them as direct deps is low-cost.
- `lessons.md` documents regression risk in `completion_handler` — changes to streaming paths need extra verification.

## Desired End State

- All env-mutating tests run sequentially via `#[serial]`, eliminating UB.
- Migrations run via `sqlx::migrate!()` at startup with ordered, versioned SQL files — no raw DDL.
- LLM API key is refreshed periodically from the env var, supporting rotation without restart.
- Auth token comparison uses HMAC-SHA256 with a per-boot random key, eliminating length and content timing oracles.
- Streaming error path logs a single "upstream_error" record (not a misleading "streaming" + "upstream_error" pair).
- SSE error events use `serde_json::json!` for guaranteed-valid JSON.

### Key Discoveries

- **22 tests affected** across `src/main.rs` (16), `src/config.rs` (4), `src/intent_classifier.rs` (4).
- **`sqlx` "migrate" feature** is missing from Cargo.toml — must be added.
- **`tokio::sync::RwLock`** is not used anywhere in the codebase — this is a new pattern.
- **`hmac`/`sha2`** are transitive deps already; adding direct deps won't grow the lockfile.
- **Render deploy** uses `cargo build --release` + `./target/release/cerebrum` — no hook for a separate migration command.

## What We're NOT Doing

- Refactoring tests into `tests/` directory (test file organization is a nit).
- Removing the dead `IntentClassifier` type alias.
- Adding `#[must_use]` annotations.
- Removing the JSON re-serialization in `handle_buffered_response`.
- Refactoring `fetch_inferences` SQL builder (fragile but functional).
- Splitting `main.rs` into smaller modules.

## Implementation Approach

Five sequential phases, each independently shippable and verifiable. Phase 1 eliminates the most urgent correctness issue (UB). Phase 2 fixes the startup DDL. Phase 3 adds key refresh. Phase 4 hardens auth. Phase 5 cleans up streaming behavior.

---

## Phase 1: Test Safety — `serial_test` Crate

### Overview

Add `serial_test` as a dev-dependency and annotate all 22 env-mutating tests with `#[serial]` to serialize their execution and eliminate UB from concurrent `set_var`/`remove_var` calls.

### Changes Required

#### 1. Add `serial_test` dev-dependency

**File**: `Cargo.toml`

**Intent**: Add `serial_test` crate to `[dev-dependencies]` so tests can use the `#[serial]` attribute.

**Contract**: Add `serial_test = "3"` under `[dev-dependencies]`.

#### 2. Annotate tests in `src/main.rs` (mod tests)

**File**: `src/main.rs`

**Intent**: Add `use serial_test::serial;` and `#[serial]` attribute to the 14 env-mutating tests in `mod tests`.

**Contract**: Tests affected: `test_completion_does_not_include_enriched_fields`, `persistence_integration_sse_streaming_success`, `persistence_integration_sse_streaming_error`, `test_upstream_returns_response`, `test_upstream_request_includes_auth_header`, `test_upstream_request_includes_content_type_json`, `test_upstream_unreachable_returns_502`, `test_upstream_skip_classify_via_headers`, `test_streaming_handler_returns_sse_content_type`, `test_streaming_handler_forwards_upstream_bytes`, `test_streaming_handler_non_2xx_returns_sse_error_event`, `test_streaming_true_returns_sse_content`, `test_streaming_false_returns_buffered_json`, `test_streaming_absent_returns_buffered_json`. Each gets `#[serial]` below `#[tokio::test]`.

#### 3. Annotate tests in `src/main.rs` (mod slow_tests)

**File**: `src/main.rs`

**Intent**: Add `use serial_test::serial;` and `#[serial]` to `test_streaming_keepalive_injected`.

**Contract**: Single test annotated with `#[serial]`.

#### 4. Annotate tests in `src/config.rs`

**File**: `src/config.rs`

**Intent**: Add `use serial_test::serial;` and `#[serial]` to the 4 env-mutating tests.

**Contract**: Tests affected: `env_or_default_returns_env_var_when_set`, `env_or_default_returns_default_when_unset`, `hardcoded_routing_respects_nvidia_endpoint_env`, `load_routing_behavior`.

#### 5. Annotate tests in `src/intent_classifier.rs`

**File**: `src/intent_classifier.rs`

**Intent**: Add `use serial_test::serial;` and `#[serial]` to the 4 LLM classifier tests that set `OPENAI_API_KEY`.

**Contract**: Tests affected: `llm_classifier_success`, `llm_classifier_malformed_response`, `llm_classifier_network_error`, `llm_classifier_unknown_category`.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests (run twice consecutively)
- `cargo clippy` zero warnings
- `cargo fmt --check` passes

#### Manual Verification

- Run `cargo test -- --test-threads=1` and `cargo test` (multi-threaded) — both produce identical pass/fail results

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Embedded Migrations

### Overview

Replace the inline `ALTER TABLE` DDL in `persistence.rs:from_env()` with `sqlx::migrate!()` macro invocation. Add the `migrate` feature to sqlx, and create migration `003` for the `prompt_char_count` column.

### Changes Required

#### 1. Add `migrate` feature to sqlx

**File**: `Cargo.toml`

**Intent**: Enable the `sqlx::migrate!()` macro by adding the `migrate` feature.

**Contract**: Change sqlx features list to include `"migrate"`: `sqlx = { version = "0.8", features = ["postgres", "runtime-tokio", "tls-rustls", "macros", "uuid", "chrono", "migrate"] }`.

#### 2. Create migration 003

**File**: `migrations/003_add_prompt_char_count.sql`

**Intent**: Formalize the `prompt_char_count` column addition as a versioned migration.

**Contract**:
```sql
-- Migration 003: Add prompt_char_count column for cost estimation
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS prompt_char_count INTEGER;
```

#### 3. Replace inline DDL with `sqlx::migrate!()`

**File**: `src/persistence.rs`

**Intent**: Replace the ad-hoc `ALTER TABLE` query in `from_env()` with `sqlx::migrate!().run(&pool).await`. This runs all pending migrations in order at startup — idempotent and multi-replica safe.

**Contract**: Remove the `ALTER TABLE` block (lines around the schema migration comment). Replace with `sqlx::migrate!().run(&pool).await.map_err(...)`. Note: `sqlx::migrate!()` embeds SQL at compile time from `./migrations` relative to `CARGO_MANIFEST_DIR` — no filesystem access at runtime. `connect_lazy_with` is still used but `migrate!().run()` will force an eager connection (desired: fail-fast on Render if DB is down).

### Success Criteria

#### Automated Verification

- `cargo build --release` succeeds (migrate macro compiles)
- `cargo test` passes all tests
- Integration tests with `DATABASE_URL` pass (migrations apply cleanly)

#### Manual Verification

- Deploy to a fresh database — all 3 migrations apply in order
- Deploy to existing database (already has prompt_char_count) — no errors, migrations are idempotent
- Verify Render deploy works end-to-end with health check passing

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: LLM API Key Refresh

### Overview

Replace the `api_key: String` field in `LLMClassifier` with `Arc<RwLock<String>>` and spawn a background task that periodically re-reads the env var, supporting key rotation without restart.

### Changes Required

#### 1. Change `api_key` field type

**File**: `src/intent_classifier.rs`

**Intent**: Store the API key behind `Arc<tokio::sync::RwLock<String>>` so it can be updated by a background task while classify calls read it concurrently.

**Contract**: Field changes from `api_key: String` to `api_key: Arc<tokio::sync::RwLock<String>>`. The `new()` constructor initializes it with the current env var value wrapped in `Arc::new(RwLock::new(...))`.

#### 2. Spawn periodic refresh task in `new()`

**File**: `src/intent_classifier.rs`

**Intent**: After constructing the struct, spawn a detached `tokio::spawn` task that re-reads `std::env::var(&api_key_env)` every 60 seconds and updates the RwLock if the value changed.

**Contract**: The task captures `Arc<RwLock<String>>` (clone of the field) and `api_key_env: String` (clone). Loop: `tokio::time::sleep(60s)` → read env → if different from current, acquire write lock → update. Log at `debug!` level on refresh. Task is fire-and-forget (detached).

#### 3. Update `classify_async` to read from RwLock

**File**: `src/intent_classifier.rs`

**Intent**: Read the API key via `.read().await.clone()` instead of borrowing `&self.api_key`.

**Contract**: Replace `let api_key = &self.api_key;` with `let api_key = self.api_key.read().await.clone();`. The rest of the method uses the local `String` as before.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests
- `cargo clippy` zero warnings
- LLM classifier tests (`llm_classifier_*`) still pass

#### Manual Verification

- Start the app with a valid LLM API key env var → classifier works
- While running, change the env var value → within 60s the new key is used (verify via debug log)
- Start with empty env var → warning logged, classifier degrades to fallback

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: Auth Hardening — HMAC Comparison

### Overview

Replace `constant_time_eq_str` (which leaks length via early return) with an HMAC-SHA256 comparison using a per-boot random secret. This eliminates both length and content timing oracles.

### Changes Required

#### 1. Add `hmac` and `sha2` as direct dependencies

**File**: `Cargo.toml`

**Intent**: Add HMAC primitives. Both are already transitive deps in Cargo.lock.

**Contract**: Add `hmac = "0.12"` and `sha2 = "0.10"` under `[dependencies]`.

#### 2. Replace `constant_time_eq_str` with HMAC-based comparison

**File**: `src/auth.rs`

**Intent**: Compute HMAC-SHA256 of both inputs using a per-boot random key, then compare the MACs in constant time. This eliminates the length oracle since HMAC output is always 32 bytes regardless of input length.

**Contract**: Generate a random 32-byte key at process start (e.g., via `rand` or hardcoded from OS entropy). Store it in a module-level `OnceLock<[u8; 32]>`. The `constant_time_eq_str` function computes `HMAC-SHA256(key, left)` and `HMAC-SHA256(key, right)`, then compares the two 32-byte outputs using `subtle::ConstantTimeEq`. The `subtle` crate remains for the final MAC comparison.

#### 3. Initialize HMAC key at startup

**File**: `src/auth.rs`

**Intent**: Generate a cryptographically random 32-byte key once at process startup for HMAC comparisons.

**Contract**: Use `std::sync::OnceLock<[u8; 32]>` initialized via a helper that fills from `getrandom` (already a transitive dep via `uuid`). Alternatively, use `rand::random::<[u8; 32]>()` — but `getrandom` avoids adding `rand` as a direct dep. The simplest approach: add `getrandom = "0.2"` with feature `std`, call `getrandom::getrandom(&mut buf)` once.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests (auth tests specifically)
- `cargo clippy` zero warnings
- Existing auth tests (`auth_validate_*`) pass unchanged

#### Manual Verification

- Auth works end-to-end: valid bearer token → 200, invalid → 401
- Dashboard basic auth works: valid credentials → 200, invalid → 401
- No timing difference observable between wrong-length and right-length-wrong-content tokens

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 5: Streaming & JSON Fixes

### Overview

Fix the dual-log on streaming error path (log single "upstream_error" instead of "streaming" + "upstream_error") and replace `format!`-based JSON in the SSE error event with `serde_json::json!`.

### Changes Required

#### 1. Remove "streaming" log before error response

**File**: `src/main.rs`

**Intent**: In `completion_handler`, when upstream returns non-2xx and client requested streaming, don't log "streaming" before calling `handle_streaming_error`. Only log "upstream_error" after.

**Contract**: Remove the `log_classification(..., "streaming")` call that precedes the `handle_streaming_error` call in the `if !upstream_response.status().is_success()` branch of the streaming path. Keep the `log_classification(..., "upstream_error")` call after. The successful streaming path (which enters `handle_streaming_response`) retains its "streaming" pre-log inside that function.

#### 2. Update integration test expectation

**File**: `src/main.rs`

**Intent**: Update `persistence_integration_sse_streaming_error` test to expect `["upstream_error"]` instead of `["streaming", "upstream_error"]`.

**Contract**: Change the assertion from `vec!["streaming", "upstream_error"]` to `vec!["upstream_error"]`.

#### 3. Replace `format!` JSON with `serde_json::json!` in streaming task

**File**: `src/main.rs`

**Intent**: In the `handle_streaming_response` spawned task, replace the `format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", sanitized)` with proper JSON construction.

**Contract**: Use `serde_json::json!({"error": sanitized}).to_string()` to build the data payload, then wrap it in the SSE frame: `format!("event: error\ndata: {}\n\n", json_payload)`. This guarantees valid JSON regardless of what characters appear in the error message.

### Success Criteria

#### Automated Verification

- `cargo test` passes all tests
- `persistence_integration_sse_streaming_error` passes with updated assertion
- `test_streaming_handler_non_2xx_returns_sse_error_event` still passes
- `cargo clippy` zero warnings

#### Manual Verification

- Send streaming request to dead upstream → single "upstream_error" record in DB
- Send streaming request to working upstream → "streaming" + "ok" records (unchanged)
- Trigger upstream error containing special chars (tabs, null bytes) → valid JSON in SSE event

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests

- Phase 1: No new tests — existing tests gain `#[serial]` for correctness
- Phase 4: Existing `auth_validate_*` tests verify HMAC comparison produces same results
- Phase 5: Existing SSE streaming tests verify behavior

### Integration Tests

- Phase 2: `persistence_integration_*` tests verify migrations apply correctly
- Phase 3: LLM classifier mock tests verify key is read and used
- Phase 5: `persistence_integration_sse_streaming_error` updated expectation

### Manual Testing Steps

1. Run `cargo test` twice consecutively — no flaky failures from env var races
2. Deploy to fresh Render instance — migrations apply, health check passes
3. Rotate LLM API key env var while running — new key picked up within 60s
4. Test auth with valid/invalid tokens of varying lengths — no observable timing difference
5. Trigger streaming error with special-char upstream message — valid JSON in SSE event

## Performance Considerations

- **Phase 1**: Tests with `#[serial]` run sequentially — slightly slower CI. Acceptable tradeoff for correctness.
- **Phase 2**: `sqlx::migrate!()` checks all migrations on every boot but only applies pending ones — negligible overhead (a few ms for the version check query).
- **Phase 3**: RwLock read in `classify_async` is uncontended 99.99% of the time (write happens once per 60s). Cost: one atomic load per classify call.
- **Phase 4**: HMAC-SHA256 adds ~200ns per auth check vs ~50ns for raw byte comparison. At expected traffic (<1000 req/s), this is negligible.
- **Phase 5**: `serde_json::json!` allocation on the error path only — zero impact on happy path.

## Migration Notes

- **Phase 2** requires that existing databases already have the `inferences` table (migration 001) and unique constraint (migration 002) applied. `sqlx::migrate!()` tracks which migrations have run via a `_sqlx_migrations` table it creates automatically. First run on an existing DB: it will detect 001 and 002 as already-applied (checksums match) and only run 003.
- **IMPORTANT**: If the existing database was created by running the raw SQL files manually (not via sqlx), the `_sqlx_migrations` table won't exist. On first boot with `sqlx::migrate!()`, it will attempt to re-run all migrations. Since all use `IF NOT EXISTS` / `ADD CONSTRAINT IF NOT EXISTS` patterns, this is safe and idempotent.

## References

- Code review: 2026-06-09 session (this plan's source)
- Prior change: `context/changes/review-cleanup/plan.md` (already implemented — no overlap)
- Lessons: `context/foundation/lessons.md`
- Render deploy config: `render.yaml`
- Existing migrations: `migrations/001_create_inferences.sql`, `migrations/002_inferences_request_id_unique.sql`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Test Safety

#### Automated

- [x] 1.1 `cargo test` passes all tests (run twice consecutively) — a76f300
- [x] 1.2 `cargo clippy` zero warnings — a76f300
- [x] 1.3 `cargo fmt --check` passes — a76f300

#### Manual

- [x] 1.4 Both `--test-threads=1` and default multi-threaded produce identical results — a76f300

### Phase 2: Embedded Migrations

#### Automated

- [x] 2.1 `cargo build --release` succeeds — 521e00b
- [x] 2.2 `cargo test` passes all tests — 521e00b
- [x] 2.3 Integration tests with DATABASE_URL pass — 521e00b

#### Manual

- [ ] 2.4 Fresh database — all 3 migrations apply in order
- [ ] 2.5 Existing database — no errors, idempotent
- [ ] 2.6 Render deploy works end-to-end with health check passing

### Phase 3: LLM API Key Refresh

#### Automated

- [x] 3.1 `cargo test` passes all tests — 3adf840
- [x] 3.2 `cargo clippy` zero warnings — 3adf840
- [x] 3.3 LLM classifier tests pass — 3adf840

#### Manual

- [ ] 3.4 Valid API key → classifier works
- [ ] 3.5 Key rotation picked up within 60s
- [ ] 3.6 Empty key → warning logged, fallback degradation

### Phase 4: Auth Hardening

#### Automated

- [x] 4.1 `cargo test` passes all tests
- [x] 4.2 `cargo clippy` zero warnings
- [x] 4.3 Auth tests pass unchanged

#### Manual

- [ ] 4.4 Valid bearer token → 200, invalid → 401
- [ ] 4.5 Dashboard basic auth → 200/401 correctly
- [ ] 4.6 No timing difference between wrong-length and right-length tokens

### Phase 5: Streaming & JSON Fixes

#### Automated

- [ ] 5.1 `cargo test` passes all tests
- [ ] 5.2 `persistence_integration_sse_streaming_error` passes with updated assertion
- [ ] 5.3 `test_streaming_handler_non_2xx_returns_sse_error_event` passes
- [ ] 5.4 `cargo clippy` zero warnings

#### Manual

- [ ] 5.5 Streaming to dead upstream → single "upstream_error" record
- [ ] 5.6 Streaming to working upstream → "streaming" + "ok" (unchanged)
- [ ] 5.7 Error with special chars → valid JSON in SSE event
