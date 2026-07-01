<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Replace sqlx with sea-query (re-review after triage fixes)

- **Plan**: context/changes/replace-sqlx/plan.md
- **Scope**: Phases 1–4 of 4 (re-review after commit e1f0d82 applied prior F1–F7 fixes)
- **Date**: 2026-06-29
- **Verdict**: APPROVED (post-triage) — all criticals fixed; refinery path now tested
- **Findings**: 2 critical, 3 warnings, 3 observations — all triaged

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING → PASS (F5 documented; F7 wired) |
| Scope Discipline | PASS |
| Safety & Quality | FAIL → PASS (F1, F2, F3, F4 fixed) |
| Architecture | WARNING → PASS (F8 refinery tests added) |
| Pattern Consistency | PASS |
| Success Criteria | WARNING → PASS (automated green; F8 subsumes manual 4.6 for Postgres) |

## Findings

### F1 — V1 migration uses SQLite-only datetime('now'), breaks fresh Postgres

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: migrations/V1__create_inferences.sql:9
- **Detail**: The prior review's F3 fix changed `created_at TIMESTAMPTZ DEFAULT NOW()` to `created_at TEXT DEFAULT (datetime('now'))`, claiming cross-dialect. It was SQLite-only — Postgres validates DEFAULT at CREATE TABLE time and fails. Fresh Postgres deploy → refinery Err → silent MemoryBackend fallback → data loss on restart.
- **Fix**: Removed the `DEFAULT` clause entirely — the INSERT always supplies `created_at`.
- **Decision**: FIXED

### F2 — V1 declares `id TEXT PRIMARY KEY` that the code never inserts

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: migrations/V1__create_inferences.sql:3
- **Detail**: V1 had `id TEXT PRIMARY KEY` but the INSERT never provides `id` — the Iden enum has no Id variant, and `init_sqlite_schema` uses `request_id TEXT PRIMARY KEY`. On fresh Postgres, INSERT fails with NOT NULL violation.
- **Fix**: Collapsed V1–V5 into a single V1 with `request_id TEXT PRIMARY KEY` and all 15 columns. Deleted V2–V5 (no production DB exists).
- **Decision**: FIXED

### F3 — F4 fix re-introduced format! SQL interpolation the prior F2 banned

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:445-462
- **Detail**: The SQLite p99 window-function query interpolated `cutoff_str` via `format!` — a regression of the prior F2 rule. The other 3 cutoff locations correctly used `Expr::val`.
- **Fix**: Replaced `'{}'` with `?` placeholder and used `.bind(&cutoff_str)` for parameterized execution.
- **Decision**: FIXED

### F4 — Postgres migration connection task leaks on migration failure

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:98-103, 117-122
- **Detail**: The prior F5 fix added `conn_handle.abort()` only on the success path. If `run_async()` returned Err, `?` early-returned, skipping abort. Dropping a JoinHandle detaches, not aborts.
- **Fix**: Capture migration result, `drop(client)` + `conn_handle.abort()` unconditionally, then `?` the result.
- **Decision**: FIXED

### F5 — Plan's parameterized query_with path replaced by AssertSqlSafe + inlined values

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Plan Adherence
- **Location**: src/persistence/sql_backend.rs:222-232, 274-278; Cargo.toml
- **Detail**: Plan specified `sqlx::query_with` with bound params via sea-query-sqlx. Implementation uses `to_string()` + `AssertSqlSafe`. sea-query-sqlx absent from Cargo.toml.
- **Fix B (applied)**: Kept `to_string()` + `AssertSqlSafe` and documented the safety basis. Fix A (restore sea-query-sqlx) was not viable — sea-query-sqlx 0.9.1 has a non-exhaustive-match bug with sea-query 1.0's `with-chrono` feature, which is almost certainly why the original developer dropped it.
- **Decision**: FIXED (Fix B — documented safety basis)

### F6 — Stale doc on percentile_99 claims SQLite uses it

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence/backend.rs:103-110, src/persistence/types.rs:56-57
- **Detail**: `percentile_99` doc claimed the SQLite dialect uses it, but F4 replaced that path with a SQL window function. The function is still used by MemoryBackend.
- **Fix**: Updated doc comments in backend.rs and types.rs to reflect current usage (MemoryBackend only; SQL backends compute p99 in the database).
- **Decision**: FIXED

### F7 — connect() dropped the db_config parameter the plan specified

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence/sql_backend.rs:68
- **Detail**: Plan specified `connect(url: &str, db_config: &DatabaseConfig)`; implementation was `connect(url: &str)` — pool tuning couldn't be passed.
- **Fix**: Re-added `db_config: &DatabaseConfig` parameter; wired `max_connections`, `acquire_timeout_secs`, `idle_timeout_secs` to `PgPoolOptions` and `SqlitePoolOptions`. Updated all call sites.
- **Decision**: FIXED

### F8 — No test exercises the refinery path on a fresh DB (root cause of F1/F2)

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Architecture / Success Criteria
- **Location**: src/persistence/sql_backend.rs:638-1073 (tests)
- **Detail**: Every SqlBackend test used `new_sqlite_in_memory()` → `init_sqlite_schema()` (bypassing refinery). No test exercised the refinery V1 path on a fresh DB — precisely why F1 and F2 passed CI (365 tests green).
- **Fix**: Added `test_sql_backend_connect_file_sqlite_refinery` (fast, file-based SQLite) and `test_sql_backend_connect_postgres_refinery` (slow_tests, testcontainers Postgres). Both immediately caught the migration bugs.
- **Decision**: FIXED

## Bonus fixes discovered during triage

- **V3–V5 `IF NOT EXISTS`**: V3/V4/V5 used `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` which is Postgres-only syntax (SQLite doesn't support it). The new file-based SQLite refinery test caught this immediately. Resolved by collapsing V1–V5 into a single V1.
- **rustls CryptoProvider**: Postgres TLS connections would panic in production because rustls 0.23 requires `CryptoProvider::install_default()` before any TLS operation. Added `rustls::crypto::ring::default_provider().install_default()` in `connect()`.
- **`init_sqlite_schema` simplified**: Removed the ALTER TABLE loop (F6 of prior review) by matching V1's complete schema. Eliminated the `format!` + `AssertSqlSafe` + SAFETY comment pattern entirely.

## Triage Summary

```
═══════════════════════════════════════════════════════════
  TRIAGE COMPLETE
═══════════════════════════════════════════════════════════

  Fixed:     F1, F2, F3, F4, F5 (Fix B), F6, F7, F8  (8)
  Skipped:   —
  Accepted:  —

═══════════════════════════════════════════════════════════
```
