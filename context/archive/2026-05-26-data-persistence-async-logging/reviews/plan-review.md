<!-- PLAN-REVIEW-REPORT -->
# Plan Review: Data Persistence Async Logging Pipeline

- **Plan**: context/changes/data-persistence-async-logging/plan.md
- **Mode**: Deep
- **Date**: 2026-05-31
- **Verdict**: REVISE → SOUND (after triage)
- **Findings**: 2 critical | 3 warnings | 0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| End-State Alignment | PASS |
| Lean Execution | PASS |
| Architectural Fitness | WARNING |
| Blind Spots | WARNING |
| Plan Completeness | FAIL |

## Grounding

5/5 paths confirmed (2 greenfield new — expected, 3 existing ✓), 3/3 symbols confirmed (build_app:51, proxy_placeholder:43, from_env in auth.rs:16 ✓), brief↔plan ✓

## Findings

### F1 — PostgreSQL crate unspecified; sqlx compile-time checks would break CI

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Architectural Fitness / Blind Spots
- **Location**: Phase 1 — Persistence dependency contract (Cargo.toml)
- **Detail**: Phase 1 named no PostgreSQL crate. Choosing sqlx (natural Axum-ecosystem pick) with compile-time query macros breaks `cargo build --release` in CI — no DATABASE_URL is set at build time in the existing workflow.
- **Fix A ⭐ Recommended**: sqlx with offline mode — `cargo sqlx prepare` locally, commit `.sqlx` cache, set `SQLX_OFFLINE=true` in CI.
  - Strength: Most idiomatic; full type safety; sqlx migrate for schema management.
  - Tradeoff: `.sqlx` files must be regenerated on SQL changes.
  - Confidence: HIGH — documented sqlx approach for CI without a live DB.
  - Blind spot: Confirm Supabase connection string format works with sqlx PgPool.
- **Fix B**: tokio-postgres (no compile-time macros) — simpler CI, less type safety, more verbose.
- **Decision**: FIXED via Fix A — Plan updated to name sqlx with offline mode; CI contract updated to set SQLX_OFFLINE=true and use `sqlx migrate run`.

### F2 — Progress section missing Phase 3 manual verification items

- **Severity**: ❌ CRITICAL
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Completeness
- **Location**: ## Progress — Phase 3 section
- **Detail**: Phase 3 body has 3 Manual Verification items (DB write, snippet privacy, failure isolation) but Progress section had no `#### Manual` block — /10x-implement would silently skip them.
- **Fix**: Add `#### Manual` block with 3.4, 3.5, 3.6 to Phase 3 Progress.
- **Decision**: FIXED — 3.4, 3.5, 3.6 added to Progress section.

### F3 — Migration runner mechanism unspecified

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Completeness
- **Location**: Phase 1 (migration contract) + Phase 3 (CI gate)
- **Detail**: Phase 1 created migration SQL but no phase named how to apply it. Phase 3 CI said "migration bootstrap" with no concrete command.
- **Fix A ⭐ Recommended**: `sqlx migrate run` + sqlx-cli in CI.
- **Fix B**: Plain psql script — no extra tooling, no version tracking.
- **Decision**: FIXED via Fix A — Migration contract and manual verification updated to reference `sqlx migrate run`; CI contract already updated via F1 fix.

### F4 — Snippet extraction from request body is unscoped

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Blind Spots
- **Location**: Phase 3 — App state and lifecycle integration (snippet capture)
- **Detail**: Plan said snippet extracted "at handler level from request JSON" but proxy_placeholder (main.rs:43) ignores the body entirely. No JSON field path, truncation limit, or malformed-body fallback was specified.
- **Fix A ⭐ Recommended**: Add extraction contract: last user message from `messages[]`, first 200 chars of `content`, empty string + WARN log on error.
- **Fix B**: Leave a comment in Phase 3, trust the implementer.
- **Decision**: FIXED via Fix A — Phase 2, item 2 contract updated with field path, 200-char limit, and error fallback.

### F5 — test_app() not listed as affected by build_app signature change

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Completeness
- **Location**: Phase 3 — App state and lifecycle integration
- **Detail**: Phase 3 expands `build_app` signature but didn't mention `test_app()` (main.rs:88) or the 3 route auth tests that call it — all break at compile time without a matching update.
- **Fix**: Add note to Phase 3 item 1 that `test_app()` must be updated with a no-op persistence state.
- **Decision**: FIXED — Phase 3 item 1 contract updated to call out test_app() explicitly.
