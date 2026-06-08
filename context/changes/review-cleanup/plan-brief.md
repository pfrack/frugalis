# Code Review Cleanup — Plan Brief

> Full plan: `context/changes/review-cleanup/plan.md`

## What & Why

Address 7 findings from the 2026-06-08 code review of the Cerebrum gateway. The most critical: SSE streaming responses log `"ok"` to the database *before* the stream runs, so mid-stream failures are invisible in inference records. The remaining 6 items are important maintainability improvements: decomposing the oversized `completion_handler`, deduplicating error response construction, simplifying the `QueryError` type, removing an unused config field, fixing test environment variable leaks, and reducing an excessively generous HTTP timeout.

## Starting Point

- `completion_handler` at `src/main.rs:322-698` is 376 lines with 6+ duplicate error JSON patterns. It has been rewritten twice before, both times regressing prior review fixes (`lessons.md:12-17`).
- SSE streaming in `src/main.rs:556-587` spawns a task but logs the outcome before the task runs.
- `QueryError` in `src/persistence.rs:17-20` is a single-variant enum with unnecessary boilerplate.
- `RegexClassifierConfig.timeout_secs` in `src/config.rs:329` is never read.
- `reqwest::Client` uses a 300-second timeout with no connect timeout (`src/main.rs:103-106`).
- Multiple tests in `main.rs` call `set_var`/`remove_var` without panic-safe guards; `config.rs:586-596` already has an `EnvGuard` pattern.

## Desired End State

SSE streaming produces accurate DB records: `"streaming"` before the stream starts, `"ok"` or `"stream_error"` after it ends. `completion_handler` delegates to focused helper functions under 80 lines, with error responses built by two shared helpers. `QueryError` is a transparent struct. `RegexClassifierConfig` has only `enabled`. All test env mutations use panic-safe guards. The HTTP client uses 120s total + 30s connect timeouts.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Scope | 🔴 critical + 🟡 important only (7 items) | High-impact items that are manageable in one session; 3 nice-to-haves deferred. | Plan |
| SSE logging approach | Log "streaming" at start + "ok"/"stream_error" in spawned task | Two records provide an audit trail; fire-and-forget inserts need no schema change. | Plan |
| Handler decomposition depth | Extract helper functions within `main.rs` | Follows existing codebase conventions; avoids new module files per AGENTS.md. | Plan |
| Error response helpers | Two free functions: `upstream_error_json` + `classification_only_json` | Simple, minimal abstraction; covers the two distinct response shapes in the handler. | Plan |
| `QueryError` replacement | Transparent struct `QueryError(String)` | Removes boilerplate without losing the type; DB errors don't need variant discrimination. | Plan |
| reqwest timeout | 120s overall + 30s connect | Reasonable for current Llama 3.1 models; prevents hanging on unreachable hosts. | Plan |
| `timeout_secs` field | Remove | Dead code, never read. YAGNI. | Plan |
| Test env vars | `EnvGuard` in all tests | Precedent exists at `config.rs:586-596`; prevents test ordering failures from leaked state. | Plan |
| Phase ordering | Bug fix first (Phase 1), refactor (Phase 2), cleanup sweep (Phase 3) | Critical fix deploys independently; refactored code benefits from the fix being in place. | Plan |

## Scope

**In scope:** SSE streaming log timing, `completion_handler` decomposition, error response deduplication, `QueryError` struct conversion, unused `timeout_secs` removal, test env var guards, reqwest timeout reduction.

**Out of scope:** New modules, LLMClassifier API key storage, `#[allow]` attribute cleanup, import hygiene, DB schema changes, config format changes.

## Architecture / Approach

All changes stay within existing files. Phase 1 moves `log_classification` into the spawned streaming task, adding a pre-spawn "streaming" record. Phase 2 extracts helper functions from `completion_handler` (request building, buffered response, streaming response, error formatting) — the function shrinks from 376 to ~120 lines. Phase 3 is four independent cleanups applied in a single pass.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Fix SSE Streaming Log Timing | Accurate DB records for streaming outcomes | Spawned task ownership — must move owned data, not borrow |
| 2. Decompose handler + Deduplicate errors | Readable, maintainable `completion_handler` | Prior regression history — must verify all behavior preserved |
| 3. Cleanup items | Struct QueryError, 120s timeout, removed field, safe tests | Test ordering — EnvGuard must cover all set_var calls |

**Prerequisites:** `classifier-config-boundary` change merged to main (avoids merge conflicts on `main.rs`).

**Estimated effort:** ~2-3 sessions across 3 phases.

## Open Risks & Assumptions

- **Handler decomposition regression**: `completion_handler` was rewritten twice before, regressing prior fixes. The test suite provides coverage, but manual smoke testing of all 4 request types (streaming, non-streaming, skip-classify, error) is required after Phase 2.
- **120s timeout may be too short**: Current models (Llama 3.1 8B/70B) complete well within 120s. If larger models are added later, the timeout may need adjustment.
- **Duplicate log inserts**: Phase 1 adds a second DB insert per streaming request. Not a concern given the existing semaphore-bounded task pool, but worth monitoring DB write volume after deploy.

## Success Criteria (Summary)

- SSE streaming requests produce two DB records: "streaming" and "ok"/"stream_error"
- `completion_handler` is under 150 lines, with error responses built by shared helpers
- `cargo test` passes twice in succession (no test ordering failures from env vars)
- All existing integration tests pass without modification
