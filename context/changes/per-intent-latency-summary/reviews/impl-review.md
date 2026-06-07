<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Per-Intent Latency Summary

- **Plan**: context/changes/per-intent-latency-summary/plan.md
- **Scope**: Full Plan (Phases 1–3)
- **Date**: 2026-06-07
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical 1 warning 1 observation

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | WARNING |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Findings

### F1 — Silent data corruption on row parse errors

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:186-211
- **Detail**: The row extraction uses `unwrap_or_else` with defaults on `try_get` failures. Malformed rows are included with placeholder values (None, empty string, default DateTime), masking data quality issues.
- **Fix**: Propagate errors for critical fields OR skip the row while logging an aggregated error count. At minimum, track "corrupted rows" separately and surface it in dashboard statistics.
  - Strength: Prevents any silent data pollution; bad rows fail the whole query
  - Tradeoff: Dashboard shows error instead of partial data; may need user retry
  - Confidence: HIGH — strict data integrity is safer for observability
  - Blind spot: Users see error instead of some rows; but that's desirable
- **Decision**: FIXED via "Propagate errors for critical fields"
  - Changed `fetch_inferences` to use `?` on `try_get()` and collect with `collect::<Result<Vec<_>, sqlx::Error>>()`.
  - Any malformed row now fails the query, surfacing data issues immediately.

### F2 — Unnecessary complexity in dynamic SQL building

- **Severity**: 📝 OBSERVATION
- **Impact**: 🏎️ LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/persistence.rs:136-149, 168-178
- **Detail**: The refactored `fetch_inferences` uses `format!` with dynamic parameter indices (`bind_count + 1`, `bind_count + 2`). While safe, this is more complex than needed and risks off-by-one errors if query structure changes.
- **Fix**: Replace dynamic binding with static query fragments
  - Use hardcoded `$1`, `$2`, `$3` for each filter instead of computing `bind_count + 1`
  - Build WHERE clause by appending conditions to a base query
  - Strength: Improves readability and maintainability without changing functionality
  - Tradeoff: None — pure simplification
  - Confidence: HIGH — matches the explicit style used before refactor
  - Blind spot: None
- **Decision**: FIXED
  - Replaced `bind_count` arithmetic with branch-specific placeholder strings (`"$1"`, `"$2"`, etc.)
  - Simplifies reasoning about parameter binding; no behavior change.

---

═══════════════════════════════════════════════════════════
  TRIAGE COMPLETE
═══════════════════════════════════════════════════════════

  Fixed:     F1, F2       (2)
  Skipped:   —            (0)
  Lessons:   —            (0)

═══════════════════════════════════════════════════════════
