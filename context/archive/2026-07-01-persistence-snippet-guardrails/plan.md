# Persistence + Snippet Guardrails Implementation Plan

## Overview

Phase 3 of the test plan rollout. Add PII redaction to snippet extraction, make `log_inference`
failure observable with integration tests for unreachable-backend and semaphore-exhaustion
scenarios, and prove cross-backend identity (same input → identical record) across memory,
SQLite, and Postgres. Covers test plan risks #5 (silent logging failure) and #6 (PII leakage).

## Current State Analysis

**Risk #5 (silent failure):** `log_inference` (`src/persistence/mod.rs:40`) spawns a detached
task, acquires a semaphore permit, and calls `backend.insert_inference()`. On failure it emits
`tracing::error!`. However:

- No test verifies the error log actually fires when the DB is unreachable — the existing
  `test_log_classification_failure_does_not_block_response` (`src/persistence/mod.rs:298`) injects
  a `MemoryBackend.fail_next` flag but only asserts the response returns 200; it does not check
  that an error log was emitted.
- No test covers semaphore exhaustion (pool at capacity, semaphore closed).
- No cross-backend test verifies that the same `InferenceRecord` produces identical
  `InferenceLog` output from all three backends (memory, SQLite, Postgres).

**Risk #6 (PII leakage):** Snippet extraction in `enqueue_inference_record`
(`src/proxy/util.rs:343`) is pure truncation: `prompt.chars().take(200).collect()`.
Zero PII redaction exists anywhere in the persistence pipeline. The three extractors
(`extract_last_user_message`, `_anthropic`, `_responses` in `src/persistence/types.rs`)
extract raw user text without filtering. An email, phone number, SSN, or credit card
number in the first 200 characters of the last user message is stored and dashboard-displayed
in full.

**OTel tracing:** An audit of all `#[cfg(feature = "otel")]` instrumentation found zero
prompt-body exposure — all spans and metric attributes record only HTTP metadata
(method/route/status), classification metadata (category/tier), and provider metadata.
The `TraceLayer::new_for_http()` default does not capture request bodies.

**Test infrastructure already present:**

- `testcontainers 0.27` in `Cargo.toml:61` — used by one existing `slow_tests` Postgres test
- `MemoryBackend.fail_next` injection (`src/persistence/mod.rs:303`) — pattern for simulating failure
- `build_app_with_persistence_backend()` (`src/persistence/mod.rs:78`) — test app with custom backend
- `fresh_postgres()` (`src/persistence/sql_backend.rs:1027`) — testcontainers helper that returns `None` gracefully when Docker is absent
- No property-testing crate exists; `proptest` will be added

## Desired End State

- Snippet extraction redacts common structured PII (email, phone, SSN, credit card) from
  the 200-char snippet before persistence, in `enqueue_inference_record` — a single point
  of change catching all three protocol paths.
- Property tests with an adversarial PII corpus prove the redaction holds: zero PII patterns
  in output regardless of where PII appears in the prompt.
- A guard test locks the OTel invariant: no span attribute, event, or error record contains
  prompt body content (verified via span inspection in a request round-trip).
- An unreachable-backend integration test proves that when `SqlBackend` points at a dead
  host, `tracing::error!` fires AND the HTTP response still completes with 200.
- A semaphore-exhaustion test proves that when the task semaphore is at capacity 0,
  `tracing::error!` fires for the blocked task AND other tasks still complete.
- A cross-backend identity test inserts the same `InferenceRecord` into memory, SQLite,
  and Postgres backends, fetches from each, and asserts identical `InferenceLog` output
  for every populated field.

### Key Discoveries

- `enqueue_inference_record` (`src/proxy/util.rs:323-386`) is the single call site for all three
  public logging wrappers — the only place `prompt_snippet` is created
- `MemoryBackend.fail_next` (`src/persistence/memory.rs`) already exists as an injection point for
  simulating insert failures
- `testcontainers` is already wired; the `fresh_postgres()` pattern returns `None` when Docker
  is absent, allowing graceful test skipping
- No `#[instrument]` or custom span attributes exist anywhere — OTel surface is already clean
- `prompt.chars().take(200)` on line 343 is the exact line to insert PII redaction before

## What We're NOT Doing

- PII redaction of `codex_turn_metadata` or other opaque Codex header fields — these are
  not displayed in the dashboard and have unknown schema
- Dashboard UI changes for snippet display (HTML escaping already prevents XSS)
- PII redaction in the three extractors (`extract_last_user_message*`) — the classifier needs
  full user text for accurate routing
- Audit of `src/persistence/sql_backend.rs` double-error-logging (redundant `error!` at
  line 322 + `error!` at `mod.rs:56`) — low severity, not blocking Phase 3
- Adding logging to validation-error/early-return paths in proxy handlers
  (bad Content-Type, invalid UTF-8, probe matches) — these are not persistence failures;
  they are pre-classification exits where no inference happened

## Implementation Approach

Single-point insertion of PII redaction in `enqueue_inference_record` before the 200-char
truncation, with `proptest` property tests in the `src/persistence/` test module. Failure
observability tests use the existing `MemoryBackend.fail_next` injection pattern for the
semaphore case and a `SqlBackend` pointed at an invalid host for the unreachable-backend
case. Cross-backend identity tests use a table-driven struct with three backends, reusing
`test_sql_backend_config()` for SQLite and `fresh_postgres()` for Postgres (in `slow_tests`).

## Critical Implementation Details

**PII regex ordering matters.** Email pattern matches before phone number pattern because
an email address `user@555-1234.com` would otherwise have `555-1234` redacted before the
full email could be matched. Apply redactions in order: email → credit card → SSN → phone.
Each replacement should use a descriptive placeholder like `[email redacted]` rather than
empty string, so the snippet remains readable for debugging.

**Semaphore exhaustion test requires `capacity = 0`.** Create a `Semaphore::new(0)` so the
first `acquire()` blocks; the spawned task never gets a permit and must log the
`"semaphore closed"` error when the semaphore is dropped. The signal path is:
`Semaphore::new(0)` → spawn task → task blocks on `acquire()` → drop the semaphore →
`acquire()` returns `Err(Closed)` → `tracing::error!("semaphore closed")`.

## Phase 1: PII Snippet Guardrails

### Overview

Add regex-based PII redaction to snippet extraction and prove it holds with property tests.
Add `proptest` as a dev-dependency. Lock the OTel prompt-exposure invariant with a guard test.

### Changes Required

#### 1. Add proptest dev-dependency

**File**: `Cargo.toml`

**Intent**: Introduce the `proptest` crate for property-based testing of PII redaction.

**Contract**: Add `proptest = "1"` under `[dev-dependencies]`.

#### 2. PII redaction function

**File**: `src/proxy/util.rs`

**Intent**: Add a `redact_pii(s: &str) -> String` function that applies regex substitutions
for common structured PII patterns (email, phone, SSN, credit card) and returns the
redacted string. Applied in `enqueue_inference_record` before the 200-char truncation.

**Contract**: A public(crate) function `redact_pii(s: &str) -> String` in `src/proxy/util.rs`.
Redaction order: email → credit card → SSN → phone. Each replacement uses a descriptive
placeholder (`[email redacted]`, `[phone redacted]`, `[ssn redacted]`, `[credit card redacted]`).
Apply redaction between the `prompt` extraction and the `prompt.chars().take(200)` on line 343.

Regex patterns (compiled via `lazy_static!` or `std::sync::LazyLock`):

```rust
static PII_PATTERNS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    vec![
        // Email — common TLDs + generic pattern
        (Regex::new(r"(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}").unwrap(), "[email redacted]"),
        // Credit card — 13-19 digits with optional dashes/spaces; catches Visa/MC/Amex/Discover
        (Regex::new(r"\b(?:\d[ -]*?){13,19}\b").unwrap(), "[credit card redacted]"),
        // SSN — xxx-xx-xxxx with word boundaries
        (Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(), "[ssn redacted]"),
        // Phone — US format: (xxx) xxx-xxxx, xxx-xxx-xxxx, xxx.xxx.xxxx, etc.
        (Regex::new(r"(?x)\b(?:\(\d{3}\)|\d{3})[-.\s]?\d{3}[-.\s]?\d{4}\b").unwrap(), "[phone redacted]"),
    ]
});
```

The credit card pattern is intentionally broad (13-19 digit sequences) — it may
false-positive on large numeric IDs. This is acceptable because the snippet is for
observability, not production data; false positives degrade operator visibility
slightly while false negatives leak PII. The email pattern is applied first to
prevent phone-pattern false matches on email address components.

#### 3. Wire redaction into enqueue_inference_record

**File**: `src/proxy/util.rs`

**Intent**: Apply `redact_pii()` to the prompt before truncating to 200 characters.

**Contract**: On line 343 of `src/proxy/util.rs`, change the snippet creation from:
```rust
let snippet: String = prompt.chars().take(200).collect();
```
to:
```rust
let redacted = redact_pii(prompt);
let snippet: String = redacted.chars().take(200).collect();
```

#### 4. Property tests for PII redaction

**File**: `src/persistence/mod.rs`

**Intent**: Add `proptest`-based property tests verifying that `redact_pii()` produces
output free of PII patterns for any valid input, including adversarial placement of
PII within, at the start of, and at the end of naturally-shaped text.

**Contract**: New `#[cfg(test)]` proptest module in `src/persistence/mod.rs` adjacent to
existing snippet tests. Tests:

- `proptest_snippet_free_of_email` — generates random text with emails injected at
  random positions; asserts no `@` + domain pattern in redacted output
- `proptest_snippet_free_of_ssn` — generates random text with SSNs injected; asserts
  no `\d{3}-\d{2}-\d{4}` in output
- `proptest_snippet_free_of_phone` — generates random text with phone numbers injected;
  asserts no phone pattern in output
- `proptest_snippet_free_of_credit_card` — generates random text with card-like digit
  sequences injected; asserts card-digit patterns absent from output
- `proptest_redacted_snippet_still_200_chars_max` — property: redaction never increases
  snippet beyond 200 chars (placeholder tokens may be shorter or longer than original,
  but the final `take(200)` enforces the bound)
- `proptest_redaction_preserves_non_pii_text` — property: text containing no PII passes
  through unmodified (modulo the 200-char truncation)

Each test uses `proptest::prelude::*` strategies to generate realistic-looking text with
adversarial PII placement.

#### 5. OTel prompt-exposure guard test

**File**: `src/persistence/mod.rs`

**Intent**: Lock the invariant that OTel tracing never exposes prompt body content.
This test proves the audit finding stays true as the codebase evolves.

**Contract**: A new `#[cfg(feature = "otel")]` test in the persistence test module that
sends a request through the full app with OTel enabled, inspects the resulting spans
(via `tracing_opentelemetry::OpenTelemetrySpanExt` or the global tracer provider),
and asserts zero span attributes, events, or error records contain the prompt body
or snippet content. Uses a unique marker string in the prompt to detect leaks.

### Success Criteria

#### Automated Verification

- `cargo test` — all existing tests pass (no regressions)
- `cargo test proptest` — all new property tests pass with `proptest` cases
- `cargo test test_snippet_path_truncates_to_200_chars` — existing truncation test still passes with redaction applied
- `cargo test test_snippet_path_does_not_contain_full_prompt` — existing prefix test still passes
- `cargo test test_log_classification_failure_does_not_block_response` — existing failure test still passes
- `cargo build --release` — compiles without warnings

#### Manual Verification

- Send a request with an email address in the first 200 chars of the prompt; verify the
  dashboard inferences page shows `[email redacted]` instead of the raw email
- Send a request with a credit card number; verify the snippet shows `[credit card redacted]`
- Verify non-PII text (code, natural language without PII) appears unchanged in the dashboard

---

## Phase 2: Persistence Failure Observability

### Overview

Add integration tests proving that `log_inference` failure is observable (not silent).
Cover unreachable-backend and semaphore-exhaustion scenarios. Verify `tracing::error!`
fires in both cases while the HTTP response completes normally.

### Changes Required

#### 1. Unreachable-backend test

**File**: `src/persistence/mod.rs`

**Intent**: Prove that when the database backend is unreachable (bad host), the proxy
response still returns 200 and a `tracing::error!` is emitted for the failed insert.

**Contract**: A new `#[tokio::test]` `test_log_inference_failure_unreachable_backend`
in `src/persistence/mod.rs` tests module. Create a `SqlBackend` pointed at a host
that does not exist (e.g., `postgresql://localhost:15432/nonexistent`), build a test
app with it, send a request, and:

- Assert response status is `200 OK` (proxy keeps working)
- Assert a `tracing::error!` containing `"final insert failure"` (from `mod.rs:56`)
  appeared in the tracing subscriber's captured events
- Verify the error log is at error level, not silently swallowed

Uses `tracing_subscriber::fmt().with_test_writer().try_init()` with a custom
`MakeWriter` that captures events into a shared buffer for assertion, or uses
`tracing_test::traced_test` for log capture.

#### 2. Semaphore-exhaustion test

**File**: `src/persistence/mod.rs`

**Intent**: Prove that when the task semaphore is exhausted (capacity zero), the
spawned task fails gracefully with an error log, and the HTTP response still completes.

**Contract**: A new `#[tokio::test]` `test_log_inference_failure_semaphore_exhausted`
in `src/persistence/mod.rs` tests module. Create a `Semaphore::new(0)` (zero permits),
build a test app with a `MemoryBackend`, send a request, drop the semaphore so the
blocked `acquire()` fails, and:

- Assert response status is `200 OK`
- Assert a `tracing::error!` containing `"semaphore closed"` (from `mod.rs:51`)
  was emitted

The signal path: `Semaphore::new(0)` → `log_inference` spawns task → task blocks
on `acquire()` → test drops the semaphore (or its `Arc`) → `acquire()` returns
`Err(Closed)` → `tracing::error!("semaphore closed")` fires.

### Success Criteria

#### Automated Verification

- `cargo test test_log_inference_failure_unreachable_backend` — passes, error log captured
- `cargo test test_log_inference_failure_semaphore_exhausted` — passes, error log captured
- `cargo test test_log_classification_failure_does_not_block_response` — existing test still passes

#### Manual Verification

- Run the unreachable-backend test; confirm the error log message is descriptive enough
  to help an operator diagnose the issue (includes request_id or connection details)
- Confirm the semaphore-exhaustion error log is similarly actionable

---

## Phase 3: Cross-Backend Identity Tests

### Overview

Prove that the same `InferenceRecord` produces identical `InferenceLog` output across
all three backends (memory, SQLite, Postgres). Uses a table-driven test structure
feeding one record to each backend and comparing fetch results.

### Changes Required

#### 1. Cross-backend identity test (memory + SQLite, regular tests)

**File**: `src/persistence/mod.rs`

**Intent**: Prove insert→fetch produces identical output on memory and SQLite backends.

**Contract**: A new `#[tokio::test]` `test_cross_backend_identity_memory_sqlite` in
`src/persistence/mod.rs` tests module. Creates a fully-populated `InferenceRecord`
(all fields set to non-default values including `input_tokens`, `output_tokens`,
`cache_read_tokens`, `cache_creation_tokens`, `client_session_id`,
`previous_response_id`, `codex_*` fields), inserts it into both a `MemoryBackend` and
a `SqlBackend` (in-memory SQLite via `SqlBackend::new_sqlite_in_memory()`), fetches
from each with identical pagination parameters, and asserts every field of the
returned `InferenceLog` is equal across backends.

#### 2. Cross-backend identity test (Postgres, slow_tests)

**File**: `src/persistence/sql_backend.rs`

**Intent**: Extend the cross-backend identity test to include Postgres via testcontainers.

**Contract**: A new `#[tokio::test]` `test_cross_backend_identity_postgres` in
`src/persistence/sql_backend.rs` slow_tests module. Reuses the `fresh_postgres()`
helper (returns `None` when Docker is unavailable — test skips). Creates the same
fully-populated `InferenceRecord`, inserts into the Postgres backend, fetches,
and asserts the `InferenceLog` fields match the expected values from the
memory/SQLite test. Uses the same reference record to avoid drift between test files.

The test uses an extracted test helper `reference_inference_record()` (in
`src/persistence/mod.rs` test module, `pub(crate)`) that returns the canonical
record used by all three cross-backend tests, ensuring they test the same input.

### Success Criteria

#### Automated Verification

- `cargo test test_cross_backend_identity_memory_sqlite` — passes
- `cargo test slow_tests` — Postgres cross-backend test passes (or skips gracefully when Docker absent)

#### Manual Verification

- Run `cargo test slow_tests -- --nocapture` with Docker running; confirm the Postgres
  identity test exercises a real container and passes
- Verify `cargo test slow_tests` skips gracefully on a machine without Docker (no panic,
  no test failure — just a skip message)

---

## Testing Strategy

### Unit Tests

- `redact_pii` unit tests: known email/phone/SSN/credit card inputs produce expected redacted outputs
- `redact_pii` edge case: empty string, no PII present, overlapping patterns (email containing digits that look like a phone number)
- Extractor functions: existing tests in `src/persistence/types.rs` continue to pass

### Integration Tests

- Unreachable-backend: proxy request against dead host → 200 response + error log
- Semaphore exhaustion: proxy request against exhausted semaphore → 200 response + error log
- Cross-backend identity: insert identical record → all three backends → identical fetch output
- Postgres identity in slow_tests: same as above but with real Postgres container

### Property Tests

- `redact_pii` with `proptest`: randomized PII placement in realistic text → zero PII in output
- Non-PII text preservation: randomized non-PII text → identical output (modulo truncation)
- 200-char bound: redacted output never exceeds 200 characters

### Manual Testing Steps

1. Start the app with `DATABASE_URL` pointing to a missing host; send a request; check logs for `"final insert failure"` error
2. Send a request with PII in prompt; refresh dashboard; verify PII replaced with placeholders
3. Send a request without PII; verify snippet appears normally
4. Run `cargo test slow_tests` with Docker running; verify Postgres identity test passes

## Performance Considerations

- `redact_pii()` runs once per request on the extracted prompt (capped at 10,000 chars).
  Four simple regex replacements on a small string are cheap (< 1ms). No measurable
  impact on proxy latency.
- The `LazyLock`-compiled regexes compile once at startup. Reuse the
  `once_cell::sync::Lazy` pattern already present in the codebase (classification regexes
  use it at `src/classification/regex.rs:30`).

## Migration Notes

No schema migration needed. The `prompt_snippet` column in SQL backends stores
redacted text going forward; existing unredacted records remain as-is. No
backfill of historical records is performed — existing snippets predating this
change may contain PII. If backfill is desired, it would be a separate operational
task outside Phase 3.

## References

- Test plan Phase 3: `context/foundation/test-plan.md:71-72`
- Risk response guidance: `context/foundation/test-plan.md:58-60`
- `log_inference`: `src/persistence/mod.rs:40-59`
- `enqueue_inference_record`: `src/proxy/util.rs:323-386`
- Snippet creation: `src/proxy/util.rs:343`
- `MemoryBackend.fail_next`: `src/persistence/mod.rs:303`
- `fresh_postgres()`: `src/persistence/sql_backend.rs:1027`
- `SqlBackend::new_sqlite_in_memory()`: `src/persistence/sql_backend.rs:66`
- `retry_once`: `src/persistence/backend.rs:125-138`
- OTel audit: zero prompt exposure found (all `#[cfg(feature = "otel")]` sites record HTTP/classification/provider metadata only)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: PII Snippet Guardrails

#### Automated

- [x] 1.1 `cargo test` — existing tests pass — 13974a5
- [x] 1.2 `cargo test proptest` — property tests pass — 13974a5
- [x] 1.3 `cargo test test_snippet_path_truncates_to_200_chars` — passes with redaction — 13974a5
- [x] 1.4 `cargo test test_snippet_path_does_not_contain_full_prompt` — passes — 13974a5
- [x] 1.5 `cargo test test_log_classification_failure_does_not_block_response` — passes — 13974a5
- [x] 1.6 `cargo build --release` — compiles without warnings — 13974a5

#### Manual

- [ ] 1.7 Email in prompt → `[email redacted]` in dashboard snippet
- [ ] 1.8 Credit card in prompt → `[credit card redacted]` in dashboard snippet
- [ ] 1.9 Non-PII text appears unchanged in dashboard

### Phase 2: Persistence Failure Observability

#### Automated

- [x] 2.1 `cargo test test_log_inference_failure_unreachable_backend` — passes — 4abc7a8
- [x] 2.2 `cargo test test_log_inference_failure_semaphore_exhausted` — passes — 4abc7a8
- [x] 2.3 `cargo test test_log_classification_failure_does_not_block_response` — still passes — 4abc7a8

#### Manual

- [ ] 2.4 Unreachable-backend error log is actionable (includes request_id or connection context)
- [ ] 2.5 Semaphore-exhaustion error log is actionable

### Phase 3: Cross-Backend Identity Tests

#### Automated

- [x] 3.1 `cargo test test_cross_backend_identity_memory_sqlite` — passes — 511be52
- [x] 3.2 `cargo test slow_tests` — Postgres identity test passes (or skips gracefully) — 511be52

#### Manual

- [ ] 3.3 Postgres identity test exercises real container with Docker running
- [ ] 3.4 Postgres identity test skips gracefully without Docker (no panic)
