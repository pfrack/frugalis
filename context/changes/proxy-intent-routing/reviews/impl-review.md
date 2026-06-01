<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Intent Classification

- **Plan**: context/changes/proxy-intent-routing/plan.md
- **Scope**: All 3 phases
- **Date**: 2026-06-01
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical, 5 warnings, 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING |
| Scope Discipline | PASS |
| Safety & Quality | WARNING |
| Architecture | WARNING |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Findings

### F1 — Missing extract_last_user_message tests per Phase 3

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: N/A (tests not written)
- **Detail**: Phase 3 contract specifies two tests that were not added: test_extract_prompt_text_extracts_last_user_message and test_extract_prompt_text_returns_empty_on_invalid_json. The persistence_snippet_* tests cover this indirectly through extract_snippet, but no direct tests for extract_last_user_message exist.
- **Fix**: Add #[test] functions in persistence.rs testing extract_last_user_message directly with valid JSON, invalid JSON, missing messages, and empty body.
- **Decision**: FIXED

### F3 — Dropped JoinHandle silences background panics

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — worth pausing; real tradeoff or non-trivial edit
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:138-142
- **Detail**: log_inference returns a JoinHandle that is discarded. If the spawned task panics, the panic is silently swallowed.
- **Fix A ⭐ Recommended**: Document in code that the JoinHandle is intentionally detached — panics in the logging task are isolated from the response path (current behavior).
  - Strength: Zero change to code; documented intent.
  - Tradeoff: Panics are silently lost.
  - Confidence: HIGH — this is the standard "fire-and-forget" pattern used across Rust async codebases.
  - Blind spot: No visibility into logging-task panics.
- **Fix B**: Wrap log_inference body in std::panic::catch_unwind and log panics.
  - Strength: Panics are visible in logs.
  - Tradeoff: More complex code; edge cases with UnwindSafe.
  - Confidence: MED — std::panic::catch_unwind has subtle constraints.
  - Blind spot: Haven't verified the closure captures are UnwindSafe.
- **Decision**: FIXED (Fix A — documented intentional detach)

### F4 — Dual category key naming (abbr vs full names)

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — worth pausing; real tradeoff or non-trivial edit
- **Dimension**: Architecture
- **Location**: src/intent_classificator.rs:38, 128-140, 341-346, 398-406
- **Detail**: scores HashMap uses abbreviated keys ("FR", "CR", "SF", "CA") but route_match() uses full names ("FILE_READING", etc.). NEGATIVE_META.suppressed also uses abbreviations.
- **Fix A ⭐ Recommended**: Use full category names in PatternMeta.category and NEGATIVE_META.suppressed, eliminating the abbreviation layer entirely.
  - Strength: Single source of truth; no fragile mapping.
  - Tradeoff: Slightly more memory per PatternMeta.
  - Confidence: HIGH — all call sites are within the same module.
  - Blind spot: None significant.
- **Fix B**: Define a const MAP between abbreviations and full names.
  - Strength: Keeps pattern rows compact.
  - Tradeoff: Adds a second mapping; same fragility.
  - Confidence: MED — still two things to keep in sync.
  - Blind spot: Const MAP needs its own tests.
- **Decision**: FIXED (Fix A — full category names)

### F5 — Test naming doesn't match existing convention

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classificator.rs:461-498
- **Detail**: auth.rs uses auth_<function>_<case>, persistence.rs uses persistence_<function>_<case>, but intent_classificator.rs uses test_<action>_<case> — missing the module prefix.
- **Fix**: Rename to intent_classify_file_reading, intent_classify_complex_reasoning, etc.
- **Decision**: FIXED

### F6 — Missing Content-Type validation

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:103
- **Detail**: Non-JSON bodies silently produce "" → CASUAL with 200.
- **Decision**: FIXED

### F7 — sanitize return type drifts from plan

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious
- **Dimension**: Plan Adherence
- **Location**: src/intent_classificator.rs:171
- **Detail**: Plan says `-> &str` but allocation requires `-> String`. Necessary drift.
- **Decision**: PENDING
