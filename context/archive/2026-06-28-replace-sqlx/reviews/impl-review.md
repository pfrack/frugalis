<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Replace sqlx with sea-query

- **Plan**: context/changes/replace-sqlx/plan.md
- **Scope**: Phases 1–4 of 4 (manual verification pending)
- **Date**: 2026-06-29
- **Verdict**: APPROVED (post-triage)
- **Findings**: 1 critical, 5 warnings, 1 observation — all triaged

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING → PASS (drifts fixed during triage) |
| Scope Discipline | PASS |
| Safety & Quality | FAIL → PASS (all critical/warning findings fixed) |
| Architecture | WARNING → PASS (SQLite now uses refinery) |
| Pattern Consistency | WARNING → PASS (p99 computed in SQL, not in Rust) |
| Success Criteria | WARNING (manual items pending) |

## Findings

### F1 — Latency summary test passes by accident (string comparison mismatch)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:347, 820-858
- **Detail**: INSERT omitted created_at (DB default used), and cutoff used RFC 3339 format while SQLite stores `YYYY-MM-DD HH:MM:SS`. String comparison accidentally produced correct test results.
- **Fix**: Added created_at to INSERT columns; changed cutoff format to `%Y-%m-%d %H:%M:%S` to match SQLite storage format.
- **Decision**: FIXED

### F2 — Cutoff timestamps interpolated via format! instead of parameterized binding

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:347,406,436,465,503
- **Detail**: Five locations used `Expr::cust(format!(...))` which bypasses sea-query's value escaping.
- **Fix**: Replaced all with `Expr::col(Inferences::CreatedAt).gte(Expr::val(...))`.
- **Decision**: FIXED

### F3 — SQLite skips refinery; hand-maintained schema path

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Architecture
- **Location**: src/persistence/sql_backend.rs:110-167
- **Detail**: Postgres used refinery; SQLite used hand-rolled CREATE TABLE + ALTER TABLE loops. Two maintenance paths could silently diverge.
- **Fix**: Added rusqlite-bundled to refinery features. V1 migration made cross-dialect (TEXT for UUID, datetime('now') for NOW()). File-based SQLite now runs refinery via rusqlite in spawn_blocking. In-memory (test) databases keep hand-rolled schema.
- **Decision**: FIXED (Fix A)

### F4 — SQLite p99 fetches all durations into memory

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:430-458
- **Detail**: SQLite fetch_latency_summary loaded all (category, duration_ms) rows into a Vec to compute p99 in Rust.
- **Fix**: Replaced with SQL window function (ROW_NUMBER + COUNT) that computes p99 in the database. Single query, no data leaves SQLite.
- **Decision**: FIXED

### F5 — Postgres migration connection leaked after drop

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:70-83
- **Detail**: Spawned tokio_postgres connection task continued running after drop(client).
- **Fix**: Store JoinHandle and call .abort() after drop(client).
- **Decision**: FIXED

### F6 — ALTER TABLE uses format! + AssertSqlSafe

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:158-159
- **Detail**: SQLite schema migration used format! + AssertSqlSafe for column names.
- **Fix**: Added SAFETY comment documenting that col/typ must be compile-time constants.
- **Decision**: FIXED

### F7 — INSERT omits created_at; DB default used (plan-intentional)

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence/sql_backend.rs:188-203
- **Detail**: Plan intentionally omitted created_at. Behavioral divergence with MemoryBackend.
- **Fix**: Resolved by F1 fix (created_at now included in INSERT).
- **Decision**: FIXED (via F1)

## Triage Summary

```
═══════════════════════════════════════════════════════════
  TRIAGE COMPLETE
═══════════════════════════════════════════════════════════

  Fixed:     F1, F2, F3 (Fix A), F4, F5, F6, F7  (7)
  Skipped:   —
  Accepted:  —

═══════════════════════════════════════════════════════════
```
