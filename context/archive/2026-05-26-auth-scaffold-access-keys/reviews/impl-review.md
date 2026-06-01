<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Auth Scaffold Access Keys

- **Plan**: context/changes/auth-scaffold-access-keys/plan.md
- **Scope**: Phases 1-3 of 3
- **Date**: 2026-05-29
- **Verdict**: APPROVED
- **Findings**: 0 critical, 3 warnings, 0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS |

## Automated Verification

- cargo build --release: PASS
- cargo test auth: PASS
- cargo test routes_auth: PASS
- cargo clippy --all-targets --all-features -- -D warnings: PASS
- cargo test: PASS
- cargo fmt -- --check: PASS (after triage fix)

## Findings

### F1 - Unsafe JSON payload construction pattern in API 401 response

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW - quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/auth.rs:137
- **Detail**: JSON body was built with string interpolation, which is a risky pattern if message content ever becomes dynamic.
- **Fix**: Replaced string formatting with structured JSON serialization.
- **Decision**: FIXED

### F2 - Formatting gate regressed against plan criterion

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW - quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/auth.rs:137, src/main.rs:75
- **Detail**: `cargo fmt -- --check` failed in current working tree.
- **Fix**: Ran `cargo fmt` and re-verified formatting gate.
- **Decision**: FIXED

### F3 - Manual deployment checks marked done without code-evidence trail

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM - real tradeoff; pause to reason through it
- **Dimension**: Success Criteria
- **Location**: context/changes/auth-scaffold-access-keys/plan.md:217
- **Detail**: Phase 3 manual checks were marked complete without explicit audit trail in repository artifacts.
- **Fix A ⭐ Recommended**: Keep completion state and add deployment evidence trail to change notes.
  - Strength: Preserves progress while improving auditability.
  - Tradeoff: Requires short documentation pass.
  - Confidence: HIGH - minimal code risk.
  - Blind spot: External platform state can drift after documentation.
- **Decision**: FIXED via Fix A

## Triage Summary

- Fixed: F1, F2, F3 (Fix A)
- Skipped: none
- Accepted without fix: none
- Lessons recorded: none
