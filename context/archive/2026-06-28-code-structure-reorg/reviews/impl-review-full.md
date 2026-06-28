<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Code Structure Reorganization

- **Plan**: context/changes/code-structure-reorg/plan.md
- **Scope**: All phases (1–6)
- **Date**: 2026-06-28
- **Verdict**: NEEDS ATTENTION
- **Findings**: 1 critical, 4 warnings

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | FAIL |
| Scope Discipline | PASS |
| Safety & Quality | WARNING |
| Architecture | WARNING |
| Pattern Consistency | WARNING |
| Success Criteria | WARNING |

## Findings

### F1 — main.rs is 513 lines, not ≤300 as planned

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; the plan's Phase 4 goal of a "thin entry point" was only partially achieved
- **Dimension**: Plan Adherence / Success Criteria
- **Location**: src/main.rs
- **Detail**: Phase 4 planned main.rs as ~250 lines: mod declarations, main(), run_init(), shutdown_signal(). Actual main.rs is 513 lines. The main() function (lines 31–498) contains all config loading, classifier construction, persistence setup, and AppState assembly inline. The run_init() function was never extracted. Tests were moved out and AppState was extracted to app.rs, but the bootstrap logic remains monolithic. Automated check `wc -l src/main.rs ≤ 300` fails (513 lines).
- **Fix A ⭐ Recommended**: Extract bootstrap logic into a dedicated function (e.g. run_server() or init_app()) in src/main.rs or a new src/bootstrap.rs, leaving main() as CLI parsing + function call.
  - Strength: Matches the plan's intent; main.rs becomes a thin entry point. Low risk — pure extraction, no behavior change.
  - Tradeoff: Requires moving ~200 lines of init code into a new function and handling the async boundary.
  - Confidence: HIGH — straightforward function extraction.
  - Blind spot: The init code uses early-return patterns (config validation) that need careful async/Result handling.
- **Fix B**: Accept 513 lines as the new baseline and update the plan's success criteria to ≤550.
  - Strength: Acknowledges that bootstrap logic has legitimate complexity.
  - Tradeoff: The "thin entry point" goal is abandoned.
  - Confidence: MEDIUM — depends on team preference.
- **Decision**: FIXED — Applied Fix A. Extracted `build_classifiers()` and `build_persistence()` into `src/app.rs`. main.rs reduced from 513 to 263 lines.

### F2 — SQL string interpolation in persistence tests

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/mod.rs:324-325, 339, 391, 406
- **Detail**: Test code uses format!() for SQL queries. While test-only, this habit can propagate to production code. The lesson "Favor dynamic WHERE clause building" in lessons.md applies.
- **Fix**: Replace with parameterized queries using sqlx::query().bind() even in test code.
- **Decision**: FIXED — Replaced all 4 format!() SQL queries with parameterized `sqlx::query().bind()` calls.

### F3 — Bidirectional dependency: config ↔ classification

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; affects module boundary clarity
- **Dimension**: Architecture
- **Location**: src/config/mod.rs:10-12, src/classification/types.rs:1
- **Detail**: config/mod.rs re-exports types from classification::types, while classification/types.rs imports from config::routing. Bidirectional dependency between sibling modules.
- **Fix A ⭐ Recommended**: Move shared types (CategoryConfig, NegativePatternConfig, PatternEntry) into config::types and have classification import from there.
  - Strength: Unidirectional dependency. Matches plan's intent that config is the "lower" module.
  - Tradeoff: Requires updating imports in classification/ files.
  - Confidence: HIGH — types are data structs, no behavior to move.
  - Blind spot: config/mod.rs may have other re-exports from classification.
- **Fix B**: Document as accepted coupling with comments in both mod.rs files.
  - Strength: No code change.
  - Tradeoff: Dependency remains and may grow.
  - Confidence: MEDIUM — acceptable if types are truly stable.
- **Decision**: FIXED — Applied Fix A. Moved CategoryConfig, NegativePatternConfig, PatternEntry, DualThreshold to config::types. Updated all imports. Bidirectional dependency eliminated.

### F4 — config/mod.rs uses pub mod instead of pub(crate) mod

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/config/mod.rs:6-18
- **Detail**: config/mod.rs declares `pub mod` while all other domain modules use `pub(crate) mod`. Since config modules are only used within the crate, this is a visibility inconsistency.
- **Fix**: Change to `pub(crate) mod loader; pub(crate) mod routing; pub(crate) mod types;`.
- **Decision**: FIXED — Changed to pub(crate) mod for consistency with sibling modules.

### F5 — completion_handler and messages_handler share ~90% logic

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; both handlers are ~800/600 lines with duplicated provider-retry loops
- **Dimension**: Pattern Consistency
- **Location**: src/proxy/handlers.rs:155-948, 949-1550
- **Detail**: Pre-existing design debt surfaced by the extraction. Both handlers share nearly identical logic for provider retry loop, cache check, classification, logging, error handling. Only protocol translation differs.
- **Fix**: Extract shared provider-retry-loop logic into a generic handler function parameterized by protocol type. Consider filing as a follow-up change.
- **Decision**: FOLLOW-UP — Create separate change for handler deduplication. Too large for inline triage fix.

## Success Criteria Verification

| Check | Result |
|-------|--------|
| `cargo build` | ✅ PASS |
| `cargo build --features otel` | ✅ PASS |
| `cargo test` (365 passed) | ✅ PASS |
| `cargo clippy` (No issues found) | ✅ PASS |
| `src/translate/` gone | ✅ PASS |
| `src/protocol/` has 4 files | ✅ PASS |
| `src/persistence/` has 6 files | ✅ PASS |
| `src/config/` has 4 files | ✅ PASS |
| `src/classification/` has 6 files | ✅ PASS |
| `src/proxy/` has 5 files | ✅ PASS |
| Old files removed | ✅ PASS |
| `wc -l src/main.rs` ≤ 300 | ❌ FAIL (513) |
| `cargo run -- --help` | ✅ PASS |
| 365 tests (matches target) | ✅ PASS |
