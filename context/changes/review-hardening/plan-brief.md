# Review Hardening — Plan Brief

> Full plan: `context/changes/review-hardening/plan.md`

## What & Why

Harden the codebase against 7 findings from the 2026-06-09 code review: undefined behavior in tests from concurrent env var mutation, DDL running on every boot, stale LLM API keys, a timing oracle in auth comparison, and unsafe JSON construction in SSE error events.

## Starting Point

- 22 tests across 3 files mutate env vars under `#[tokio::test]` (multi-threaded runtime) — UB since Rust 1.66.
- `persistence.rs:from_env()` runs `ALTER TABLE` on every boot instead of using versioned migrations.
- `LLMClassifier` caches the API key as a plain `String` at construction — no rotation support.
- `constant_time_eq_str` in `auth.rs` short-circuits on length mismatch, leaking token length.
- Streaming error path logs a misleading "streaming" record before the actual "upstream_error".
- SSE error events use `format!` for JSON — doesn't escape all control characters.

## Desired End State

All env-mutating tests serialize safely. Migrations are versioned and embedded (Render-compatible). LLM API keys auto-refresh every 60s. Auth comparison eliminates timing oracles via HMAC. Streaming errors produce a clean audit trail and valid JSON.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Test safety approach | `serial_test` crate | Zero refactoring, directly solves the race, 18M downloads. | Plan |
| Migration strategy | `sqlx::migrate!()` embedded | Render has no separate migration step; embedded migrations run at startup, idempotent. | Plan |
| API key refresh | `Arc<RwLock<String>>` + 60s task | Supports rotation without restart; RwLock read is uncontended 99.99% of the time. | Plan |
| Auth comparison | HMAC-SHA256 with per-boot random key | Eliminates both length and content oracles completely. | Plan |
| Streaming error log | Single "upstream_error" | Cleaner audit trail — no misleading "streaming" record for requests that never streamed. | Plan |
| SSE JSON construction | `serde_json::json!` macro | Guaranteed valid JSON; serde handles all escaping. | Plan |
| Scope | Exclude nits | Keeps plan focused and shippable in ~3 sessions. | Plan |

## Scope

**In scope:**
- `serial_test` annotations on 22 tests
- `sqlx` "migrate" feature + migration 003 file + replace inline DDL
- `LLMClassifier` key refresh with `RwLock` + spawned task
- HMAC-based `constant_time_eq_str` replacement
- Remove dual-log on streaming error
- `serde_json::json!` for SSE error events

**Out of scope:**
- Dead `IntentClassifier` type alias removal
- `#[must_use]` annotations
- JSON re-serialization in `handle_buffered_response`
- `fetch_inferences` SQL builder refactor
- Test file reorganization (moving to `tests/`)

## Architecture / Approach

Each phase is a self-contained, independently deployable change. No phase depends on another (though they're ordered by severity). The heaviest architectural addition is the `RwLock` + background task pattern for API key refresh (Phase 3) — a new pattern for this codebase but minimal surface area.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Test Safety | `#[serial]` on 22 tests, eliminates UB | Slightly slower CI (sequential test execution) |
| 2. Embedded Migrations | `sqlx::migrate!()` replaces inline DDL | First boot on existing DB needs idempotent migrations |
| 3. LLM API Key Refresh | 60s periodic refresh via RwLock | New concurrency pattern; must not deadlock classify path |
| 4. Auth Hardening | HMAC-SHA256 comparison | New deps (hmac, sha2, getrandom); must not break auth |
| 5. Streaming & JSON Fixes | Clean audit trail + valid SSE JSON | Integration test assertion change |

**Prerequisites:** Existing `review-cleanup` plan fully implemented (it is — all automated checks are `[x]`).
**Estimated effort:** ~2-3 sessions across 5 phases.

## Open Risks & Assumptions

- Existing databases created via raw SQL (not sqlx) lack `_sqlx_migrations` table — first `sqlx::migrate!()` run will re-apply all migrations, but `IF NOT EXISTS` makes this safe.
- The 60s refresh interval for API keys is a reasonable default but not configurable — future enhancement if needed.
- `getrandom` is assumed available on Render's Linux runtime (it is — uses `/dev/urandom`).

## Success Criteria (Summary)

- `cargo test` passes reliably on multi-threaded runtime with no flaky env var races
- Render deploy applies migrations automatically, health check passes
- Auth tokens of varying lengths produce no observable timing difference
