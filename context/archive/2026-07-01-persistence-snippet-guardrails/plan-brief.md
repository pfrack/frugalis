# Persistence + Snippet Guardrails — Plan Brief

> Full plan: `context/changes/persistence-snippet-guardrails/plan.md`
> Test plan (source): `context/foundation/test-plan.md` (Phase 3, rows #5 and #6)

## What & Why

Phase 3 of the test plan rollout. Two risks: `log_inference` can fail silently (DB unreachable, pool
exhausted) and the operator never knows — dashboard stays empty; and snippet extraction lacks any
PII protection — the raw first 200 characters of the user's last message are stored and displayed
verbatim. We add PII redaction (email, phone, SSN, credit card), test that persistence failures
are observable (error logs fire while the proxy keeps serving), and prove cross-backend consistency
(same record → identical fetch output on memory/SQLite/Postgres).

## Starting Point

- `log_inference` (`src/persistence/mod.rs:40`) spawns a detached task, calls `backend.insert_inference`,
  and emits `tracing::error!` on failure — but no test verifies the log fires or that the proxy survives
  an unreachable DB
- Snippet creation (`src/proxy/util.rs:343`) is `prompt.chars().take(200).collect()` — zero PII redaction
- Three backends exist (memory, SQLite, Postgres) but have never been tested for identical output
- `testcontainers 0.27` already in dev-deps; `MemoryBackend.fail_next` injection pattern exists;
  `fresh_postgres()` helper returns `None` gracefully when Docker is absent
- No PII code exists anywhere; no `proptest` crate; OTel surface is already clean (no prompt exposure)

## Desired End State

Snippet extraction redacts common structured PII before the 200-char truncation, proven by property
tests against adversarial inputs. Unreachable-backend and semaphore-exhaustion failures produce
actionable `tracing::error!` logs while the proxy response completes normally — proven by integration
tests. A cross-backend identity test inserts the same record into all three backends and asserts
identical `InferenceLog` output. The OTel prompt-exposure invariant is locked by a guard test.

## Key Decisions Made

| Decision | Choice | Why | Source |
| --- | --- | --- | --- |
| PII patterns to redact | Common structured PII: email, phone, SSN, credit card | Highest-impact, most-identifiable patterns; regex-based; narrow enough to avoid false positives on code | Plan |
| PII redaction location | `enqueue_inference_record` before truncation, not in extractors | Single point of change; catches all three protocol paths; doesn't affect classifier input quality | Plan |
| Property testing crate | `proptest` | Mature, well-documented, composable strategies, built-in shrinking; better fit than quickcheck for string generation | Plan |
| Observability test scope | Unreachable backend + semaphore exhaustion (2 tests) | Covers both failure modes in risk #5's response guidance; semaphore = 0 is synthetic but catches the error log path | Plan |
| Cross-backend test structure | Table-driven: insert one record to all 3 backends, fetch, compare fields | Tests complete insert→fetch contract per backend; identical record as input; SQLite in regular tests, Postgres in slow_tests | Plan |
| PII in tracing/OTel | Lock invariant with guard test (no prompt content in spans) | Audit found zero prompt exposure already; a test locks it rather than adding redaction where none is needed | Plan |

## Scope

**In scope:**
- PII redaction (email, phone, SSN, credit card) in snippet extraction via `redact_pii()`
- `proptest` dev-dependency + property tests against adversarial PII inputs
- OTel guard test: verify no prompt content in spans
- Unreachable-backend integration test (verify error log + response completes)
- Semaphore-exhaustion integration test (verify error log + response completes)
- Cross-backend identity: memory ↔ SQLite (regular tests) + Postgres (slow_tests via testcontainers)

**Out of scope:**
- PII in Codex header fields (`codex_turn_metadata`, etc.) — opaque client strings, not dashboard-visible
- Dashboard UI changes for snippet display
- PII redaction in classifier extractors — classifier needs full text for routing
- Double-error-logging cleanup in `sql_backend.rs` — low severity noise
- Logging validation/early-return paths in proxy handlers — pre-classification exits, not persistence

## Architecture / Approach

Single insertion point: `redact_pii()` at `src/proxy/util.rs:343` before the `take(200)`.
Four `LazyLock<Regex>` patterns applied in order (email → credit card → SSN → phone)
with descriptive placeholders. Tests live in `src/persistence/mod.rs` (regular) and
`src/persistence/sql_backend.rs` (slow_tests). The `reference_inference_record()` helper
in the test module provides a canonical fully-populated record for all cross-backend tests.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. PII Snippet Guardrails | `redact_pii()` + proptest property tests + OTel guard test | Regex false positives on numeric IDs in code (credit card pattern) — acceptable tradeoff for observability |
| 2. Persistence Failure Observability | Unreachable-backend + semaphore-exhaustion integration tests | Log capture in tests requires `tracing_subscriber` init ordering; may need `#[serial]` |
| 3. Cross-Backend Identity Tests | Table-driven identity test across memory/SQLite/Postgres | Postgres test via testcontainers requires Docker on CI; `fresh_postgres()` already handles skip-gracefully |

**Prerequisites:** `testcontainers 0.27` already installed; Docker available for Postgres slow_tests
**Estimated effort:** ~2-3 sessions across 3 phases (each phase is ~100-150 LOC of test code + small production change in Phase 1)

## Open Risks & Assumptions

- **Credit card regex breadth:** The `\b(?:\d[ -]*?){13,19}\b` pattern matches any 13-19 digit sequence, including large numeric IDs (invoice numbers, timestamps, hash fragments). If false positives degrade snippet readability for operators, we may need to tighten the pattern or add Luhn checksum validation in a follow-up.
- **`tracing_test` crate:** Log capture in Phase 2 tests may need the `tracing_test` or `test-log` crates for assertion on emitted log events. If neither is suitable, we'll use `tracing_subscriber` with a custom `MakeWriter` capturing into a buffer — adds ~20 lines of test infrastructure.
- **CI Docker:** The Postgres identity test (Phase 3) requires Docker on CI. If Docker is unavailable in CI, the test skips gracefully via the existing `fresh_postgres()` pattern, but this means the Postgres path is only verified locally — not in CI.

## Success Criteria (Summary)

- PII in the first 200 chars of a user message is replaced with descriptive placeholders in the dashboard
- Database or semaphore failures produce actionable error logs while the proxy keeps serving
- A single record produces identical output regardless of backend (memory/SQLite/Postgres)
- No prompt content appears in OTel spans ever (guard test enforces this)
