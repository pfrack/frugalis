<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Code Structure Reorganization

- **Plan**: context/changes/code-structure-reorg/plan.md
- **Scope**: All phases (1–4 + Test Distribution Addendum)
- **Date**: 2026-06-29
- **Verdict**: APPROVED (after triage)
- **Findings**: 2 critical, 4 warnings, 2 observations — all triaged and applied

## Verdicts (post-triage)

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ (F1, F2 fixed) |
| Scope Discipline | PASS ✅ (F2 fixed) |
| Safety & Quality | PASS ✅ |
| Architecture | PASS ✅ (F4 fixed) |
| Pattern Consistency | PASS ✅ (F3, F6, F7 fixed) |
| Success Criteria | PASS ✅ (clippy clean, 365 tests pass) |

► Overall: **APPROVED** ✅

## Success Criteria Verification (final)

| Check | Result |
|-------|--------|
| `cargo build` | ✅ PASS (0 errors, 0 warnings) |
| `cargo build --features otel` | ✅ PASS (0 errors, 0 warnings) |
| `cargo test` | ✅ PASS (365 passed, 60.06s) |
| `cargo clippy --all-targets` | ✅ PASS (No issues found) |
| `src/translate/` gone | ✅ PASS |
| `src/tests.rs` gone | ✅ PASS |
| `src/persistence/sql_backend.rs` gone | ✅ PASS (orphan eliminated) |
| `src/main.rs` ≤ 300 lines | ✅ PASS (263) |
| Module directory layout | ✅ PASS (no strays, no orphans) |
| No `use crate::*` | ✅ PASS |
| `src/classification/types.rs` uses live-only types | ✅ PASS (dead code removed) |

## Findings (post-triage)

### F1 — Dead-code duplication: classification types never deleted

- **Severity**: ❌ CRITICAL
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence / Success Criteria
- **Location**: src/classification/types.rs (was lines 4-77)
- **Detail**: The impl-review-full F3 fix moved CategoryConfig, NegativePatternConfig, PatternEntry, DualThreshold to src/config/types.rs:425-498, but the original definitions at classification/types.rs:6-77 were not deleted. All production code used config::types::X (zero callers of classification::types::X for these 4 types). Result: 9 dead_code warnings from cargo clippy --all-targets.
- **Fix Applied**: Deleted classification/types.rs lines 4-77. The file still holds the live ClassificationResult, ClassificationTier, PatternMeta, FewShotExample types and the DEFAULT_MODEL-using `fallback()` impl.
- **Decision**: FIXED

### F2 — Orphaned module: src/persistence/sql_backend.rs (482 lines)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence / Scope Discipline
- **Location**: src/persistence/sql_backend.rs (entire file, deleted)
- **Detail**: persistence/sql_backend.rs defined a SqlBackend PersistenceBackend impl using sea_query, but it was NOT declared in persistence/mod.rs and had zero external callers. The file sat on disk, never compiled, invisible to cargo build. Not in the plan's "Changes Required" for any phase. Per lessons.md: dead code is technical debt.
- **Fix Applied (Fix A)**: Deleted src/persistence/sql_backend.rs. Build and clippy remain clean.
- **Decision**: FIXED (Fix A)

### F3 — Move-breadcrumb comment in src/config/types.rs:424

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Pattern Consistency
- **Location**: src/config/types.rs:424
- **Detail**: Banner comment "Classification config types (moved from classification::types)" leaked refactor history.
- **Fix Applied**: Changed to `// ── Classification config types ──`.
- **Decision**: FIXED

### F4 — RequestMetrics lives in handlers.rs, plan said proxy/mod.rs

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Architecture
- **Location**: src/proxy/handlers.rs:14 → src/proxy/mod.rs
- **Detail**: Plan Phase 3 contracts RequestMetrics to live in src/proxy/mod.rs behind `#[cfg(feature = "otel")]`. The implementation was in src/proxy/handlers.rs:14 with no otel cfg gate.
- **Fix Applied**: Moved RequestMetrics struct + its 3 impl blocks to src/proxy/mod.rs behind `#[cfg(feature = "otel")]`. Updated imports in src/proxy/handlers.rs and src/proxy/util.rs.
- **Decision**: FIXED

### F5 — F2 cross-reference

- **Severity**: ⚠️ WARNING (alias to F2)
- **Detail**: Cross-referenced from F2 (orphaned sql_backend.rs).
- **Decision**: FIXED via F2 (file deleted)

### F6 — Function-scoped use statements (style)

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Pattern Consistency
- **Location**: src/persistence/postgres.rs (production), src/persistence/mod.rs (tests), src/proxy/handlers.rs (production + tests), src/config/mod.rs + src/config/loader.rs (tests)
- **Detail**: Use statements inside function bodies (`use sha2::...`, `use sqlx::Row;`, `use std::io::Write;`, `use std::collections::HashMap;`, `use std::str::FromStr;`, `use sqlx::postgres::PgConnectOptions;`, `use std::collections::HashMap;`).
- **Fix Applied**: Hoisted all to module-level (production) or mod tests top (tests). Removed shadowed inner `use std::io::Write;` lines in config files.
- **Decision**: FIXED

### F7 — Comment in streaming.rs still references F1-F4 by review ID

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Pattern Consistency
- **Location**: src/proxy/streaming.rs:111-115
- **Detail**: Comment referenced "F1–F4 review fixes" by ID. Borderline violation of lesson "Document guard points with self-describing comments, not review cross-references".
- **Fix Applied**: Replaced "F1–F4 review fixes" with a reference to the lesson title ("Re-run review after a follow-up change touches the same handler").
- **Decision**: FIXED

## Notes

- Total changes during this triage: 0 externally observable behavior changes. 365 tests pass before and after. `cargo clippy --all-targets` went from 9 warnings to 0.
- Plan's Progress section commit SHAs (239d48d, e2416f9, 3d1e6a3, 0ef70aa) were not found in the current git log — those SHAs appear to predate the squash merge history. The actual implementation commits are `4ec8ad1` (p5), `2743651` (p6), `fdfdca1` (review fixes), `5bfa9a6` (review fixes), `9b6675b` (archive), `cccdf11` (un-archive). Plan doc would benefit from SHA update, but is out of scope for the refactor itself.
