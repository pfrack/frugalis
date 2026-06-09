# Post-Review Cleanup, Hardening, and Production Reliability Plan

## Overview

This plan consolidates three review tracks—`review-cleanup`, `review-hardening`, and `prod-hardening-reliability`—into a single 12-phase implementation. The goal is to move the codebase from a functional but rough state to a production-ready, well-tested, observable, and secure gateway service. Phases are ordered from most urgent (correctness) to nice-to-have (observability).

## Current State Analysis

- **SSE Streaming**: Log timing is unreliable; logs may fire before data is actually flushed.
- **Handler Architecture**: `completion_handler` is a large, monolithic function that mixes routing, auth, classification, proxying, and logging, making testing and reasoning hard.
- **Error Handling**: Error types are duplicated between `persistence.rs` and callers; `QueryError` is inconsistent and underused.
- **Tests**: Not truly parallel-safe; concurrent tests may share mutable global state (env vars), risking undefined behavior (UB).
- **Migrations**: Run manually or are assumed to be in place; no embedded migration at startup.
- **LLM Keys**: Static for the process lifetime; no refresh mechanism.
- **Auth**: String comparison is not constant-time; streaming and JSON deserialization have edge-case bugs; dead code persists across the codebase.
- **Production**: No graceful shutdown; no slow test suite; no DB validation at startup; hardcoded limits; fragile env parsing; no internal metrics; health endpoint is minimal; no operator docs.

## Desired End State

- Logs exactly reflect when bytes were actually sent.
- Handlers are decomposed into testable, single-purpose units.
- A single, canonical error type exists with no duplication.
- Tests run in parallel safely without UB.
- Database schema is versioned and applied automatically.
- LLM keys can be rotated without redeploy.
- Auth uses constant-time comparison and hardened token handling.
- Streaming and JSON edge cases are handled correctly.
- Dead code is removed and prevented from re-entering.
- Server shuts down gracefully, validates DB, supports configurable limits, parses env robustly, exposes metrics, and provides useful health/readiness checks.

## What We Are NOT Doing

- Rewriting the classification model or changing inference semantics.
- Replacing the web framework (Axum).
- Adding external observability (Sentry, OTel) beyond internal metrics and structured logs.
- Implementing a full config file; we keep env-based configuration.

## Implementation Approach

Implement the twelve phases in strict order. Each phase includes the files to change, the intent behind each change, the public contract (how it affects tests and callers), and explicit success criteria. Code review is required between phases where noted.

---

### Phase 1: SSE Streaming Log Timing Fix

**Overview**
Fix the timing of SSE streaming logs so that the logged `elapsed_ms` represents the time from request start until the final byte is actually flushed to the client, rather than an internal proxy completion event.

**Changes Required**
- `src/main.rs` (SSE streaming handler)
  - Move log emission until after the `Stream` has fully yielded and the response body is complete.
  - Ensure the timer starts exactly once at request entry and stops exactly once after stream termination.
- `src/persistence.rs`
  - `log_inference` signature must accept the precomputed `elapsed_ms: u64` and not derive it internally.

**Success Criteria**
- SSE streaming tests assert that the logged `elapsed_ms` is strictly greater than or equal to the artificial delay injected in the stream.
- No `elapsed_ms` is logged before the body stream completes.

---

### Phase 2: Decompose `completion_handler` and Deduplicate Errors

**Overview**
Split the monolithic `completion_handler` into discrete, testable stages (classify, build request, stream, log). Unify error handling by removing duplicated error definitions across `persistence.rs` and callers.

**Changes Required**
- `src/main.rs`
  - Extract `classify_and_route(request: Request) -> RoutedRequest`.
  - Extract `proxy_and_stream(request: RoutedRequest) -> impl Stream`.
  - Keep `completion_handler` as a thin orchestrator that wires the above together.
- `src/persistence.rs`
  - Remove duplicate error variants already defined in other modules.
- All error sites previously returning duplicated errors now return the canonical `QueryError` (or a unified crate error type).

**Success Criteria**
- `completion_handler` is no more than 50 lines and delegates to named sub-functions.
- Only one definition of each error variant exists in the crate.
- All existing tests compile and pass.

---

### Phase 3: Cleanup Items (`QueryError`, `timeout`, `timeout_secs`, `EnvGuard`)

**Overview**
Clean up remaining naming and consistency issues: unify timeout configuration names, finalize `QueryError` usage, and replace the ad-hoc `EnvGuard` with a safer pattern.

**Changes Required**
- `src/persistence.rs`
  - Rename internal `timeout` to `timeout_secs` (or vice versa) and update all references.
  - Ensure `QueryError` is used for every persistence failure path.
- `src/main.rs` (tests)
  - Remove `EnvGuard` or replace it with `serial_test` + explicit env restoration (see Phase 4).

**Success Criteria**
- No occurrence of the old timeout field name remains in the codebase.
- `QueryError` covers 100% of persistence failure paths.
- `EnvGuard` is removed from the source tree.

---

### Phase 4: Test Safety (`serial_test`)

**Overview**
Prevent test UB caused by concurrent mutation of `env::set_var`/`env::remove_var` by forcing env-dependent tests to run serially.

**Changes Required**
- `Cargo.toml`
  - Add `serial_test = "3"` (or latest) under `[dev-dependencies]`.
- `src/main.rs` (tests)
  - Import `serial_test::serial`.
  - Annotate every test that mutates env vars with `#[serial]`.

**Success Criteria**
- `cargo test` passes reliably under `cargo test -- --test-threads=16`.
- Miri (if available) does not flag a data race on env mutation.

---

### Phase 5: Embedded Migrations

**Overview**
Guarantee that the application starts only if the database schema is at the expected version, applying pending migrations automatically.

**Changes Required**
- `Cargo.toml`
  - Add `sqlx` feature `migrate`.
- `src/persistence.rs`
  - On pool creation, run `sqlx::migrate!("./migrations").run(&pool).await`.

**Success Criteria**
- Deleting/recreating the database and starting the app results in a fully migrated schema.
- Startup fails fast with a clear error if migrations cannot run.

---

### Phase 6: LLM API Key Refresh

**Overview**
Support rotating LLM API keys without restarting the service by reloading the key from env (or a future config source) on a configurable interval or on-demand.

**Changes Required**
- `src/auth.rs` (or new `src/llm_config.rs`)
  - Replace static key string with a refreshable wrapper: an `Arc<RwLock<String>>` or similar.
  - Expose `refresh_llm_key() -> Result<(), EnvError>`.
- `src/main.rs`
  - Wire a background tokio task that calls `refresh_llm_key()` every `LLM_KEY_REFRESH_SECS` (env var, default 300).

**Success Criteria**
- Changing the env var `LLM_API_KEY` and waiting one refresh cycle causes subsequent requests to use the new key.
- A manual trigger endpoint (e.g., `POST /internal/refresh-key`, behind auth) forces immediate refresh.

---

### Phase 7: Auth Hardening (Constant-Time Comparison)

**Overview**
Eliminate timing side channels in bearer token and basic auth validation by replacing direct string equality with constant-time comparison.

**Changes Required**
- `src/auth.rs`
  - Import a constant-time comparison (e.g., `subtle::ConstantTimeEq`).
  - Replace all `==` on secret strings with `ct_eq` or equivalent.
- `src/main.rs`
  - Ensure auth middleware passes tokens through the hardened comparison path.

**Success Criteria**
- A simple timing test (many requests with wrong prefix vs wrong suffix) shows no statistically significant difference.

---

### Phase 8: Streaming and JSON Fixes

**Overview**
Fix edge cases in streaming and JSON deserialization: handle empty chunks, missing trailing newlines, and partial JSON that deserializers may reject.

**Changes Required**
- `src/main.rs` (SSE stream)
  - Skip zero-length chunks before parsing.
  - Append a newline terminator if the final chunk lacks one before feeding the deserializer.
- Any JSON utility
  - Use `serde_json::from_reader` with a buffered reader where appropriate; handle `UnexpectedEof` explicitly.

**Success Criteria**
- A stream ending without a final newline still parses correctly.
- A stream with empty `data:` events does not panic.

---

### Phase 9: Dead Code Cleanup

**Overview**
Remove all unused imports, variables, functions, and modules. Add CI linting to prevent reintroduction.

**Changes Required**
- Entire `src/` tree
  - Run `cargo clippy --all-targets -- -D warnings` and fix every dead_code/unused warning.
- `.github/workflows/ci.yml`
  - Add a clippy step that fails on warnings.

**Success Criteria**
- `cargo clippy --all-targets -- -D warnings` exits zero.
- CI blocks merges that reintroduce warnings.

---

### Phase 10: Production Resilience

**Overview**
Add graceful shutdown, split slow tests, and validate the database connection at startup.

**Changes Required**
- `src/main.rs`
  - Install a `tokio::signal` handler for SIGTERM/SIGINT that triggers a graceful shutdown with a bounded drain period.
- Tests
  - Move tests with real sleeps/delays into `tests/slow/` or annotate with a `slow_tests` feature.
- `src/persistence.rs`
  - On startup, run a lightweight `SELECT 1` (or equivalent) and fail fast on error.

**Success Criteria**
- Sending SIGTERM to the running binary allows in-flight requests to complete within a configurable drain window.
- `cargo test` is fast; `cargo test --features slow_tests` includes the slow suite.
- App startup fails immediately with a clear error if the DB is unreachable.

---

### Phase 11: Configurability (Limits, Env Parsing)

**Overview**
Replace hardcoded limits (body size, timeouts, connection pool size) with robust env parsing and sensible defaults.

**Changes Required**
- `src/main.rs` / new `src/config.rs`
  - Define a `Config` struct using ` envy ` or manual parsing with clear error messages.
  - Fields: `max_body_size`, `timeout_secs`, `db_pool_size`, `log_level`, etc.
- Cleanup `.env.example` or `README.md` to list every config knob.

**Success Criteria**
- Every previously hardcoded limit is now configurable via env.
- Invalid env values produce human-friendly errors at startup.

---

### Phase 12: Observability (Metrics, Health, Docs)

**Overview**
Expose internal metrics, enhance the health endpoint, and write operator-facing documentation.

**Changes Required**
- `src/main.rs`
  - Add `/metrics` using `metrics-exporter-prometheus` (or a lightweight impl) for: request count, latency histogram, active connections, classification outcomes, LLM proxy errors.
- `src/main.rs` (health)
  - `/health` returns `{ "status": "ok", "db": "ok" }` including a DB connectivity check.
- `README.md` or `docs/ops.md`
  - Document deployment, env vars, health/metrics endpoints, and alert thresholds.

**Success Criteria**
- `/metrics` returns valid Prometheus text.
- `/health` fails (non-200) when DB is unreachable.
- A new operator can deploy and monitor the service using only the provided docs.

## References

- `subtle` crate docs for constant-time comparison.
- `serial_test` crate docs for test isolation.
- `sqlx migrate` macros for embedded migrations.
- Axum graceful shutdown examples (SIGTERM handling).
- Prometheus text format specification.

## Performance Considerations

- Constant-time comparison is slightly slower than `==`; it is acceptable for auth paths.
- Embedded migrations run once at startup; negligible steady-state overhead.
- Metrics exposition (`/metrics`) should use an atomic counter/histogram to avoid locks on the hot path.
- Graceful shutdown should cap the drain window to prevent hanging indefinitely.

## Migration Notes

- Phases 1–9 are code-only and safe to ship incrementally.
- Phase 10 introduces a new test split: update CI to run both fast and slow test suites.
- Phase 11 adds new env vars; operators should review their configuration before deploying.
- Phase 12 adds new endpoints; ensure these are not exposed publicly if they carry sensitive counters.
