<!-- IMPL-REVIEW-REPORT -->
# Implementation Review (v2): Cost-Savings Metric Implementation Plan

- **Plan**: context/changes/cost-savings-metric/plan.md
- **Scope**: All 3 phases (post prior review b9e7249)
- **Date**: 2026-06-07
- **Verdict**: APPROVED
- **Findings**: 0 critical | 2 warnings | 5 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS |

## Findings

### F1 — BASELINE_MODEL default differs from plan specification

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/intent_classificator.rs:513-514
- **Detail**: Plan specifies default "claude-3.5-sonnet". Production uses DEFAULT_MODEL_COMPLEX ("meta/llama-3.3-70b-instruct"). Test at line 916-918 tests from_values path only.
- **Fix A ⭐ Recommended**: Document the change in plan as accepted drift.
- **Decision**: FIXED via Fix A — plan updated.

### F2 — Unused `model_costs()` accessor on IntentClassifier

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/intent_classificator.rs:535-537
- **Detail**: Planned method produced dead-code warning. Handler reads costs from AppState directly.
- **Fix**: Removed method; updated test references to use field access `classifier.model_costs.get(...)`.
- **Decision**: FIXED — method removed, tests pass.

### F3 — Plan states 4dp rounding; code rounds to 6dp

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence.rs:460
- **Detail**: Prior review F5 changed rounding from 4dp to 6dp. Plan still says 4dp.
- **Fix**: Update plan line 80 to reflect 6dp internal with 4dp display.
- **Decision**: FIXED — plan updated.

### F4 — Plan says handler in main.rs; actual is dashboard.rs

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/dashboard.rs:269-301
- **Detail**: Implementation follows AGENTS.md convention (dashboard.rs). Plan says main.rs.
- **Fix**: Update plan Phase 2 to reference dashboard.rs.
- **Decision**: FIXED — plan updated.

### F5 — Nav uses PAGES auto-generation, not per-template blocks

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/dashboard.rs:36-41
- **Detail**: Plan assumed {% block nav %} blocks. Project uses PAGES + base.html auto-nav.
- **Fix**: Update plan Phase 3 to reference PAGES registry.
- **Decision**: FIXED — plan updated.

### F6 — Unplanned "Est. Savings" quick-stat on dashboard index

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: templates/dashboard/index.html:51-59
- **Detail**: Dashboard index includes savings quick-stat — not in plan scope.
- **Fix**: Document as accepted scope addition.
- **Decision**: ACCEPTED — plan addendum added.

### F7 — 5 pre-existing build warnings

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Success Criteria
- **Location**: src/main.rs:17, src/persistence.rs:20/35/70, src/intent_classificator.rs:584
- **Detail**: After F2 fix, 5 warnings remain — all pre-date cost-savings. No new regressions.
- **Fix**: Note scoped criterion in plan.
- **Decision**: FIXED — plan updated.

## Prior Review Regression Check

All 7 fixes from prior review (b9e7249) verified intact at HEAD after intervening commit d6f7d35 (classify-endpoint review fixes).

| Prior Fix | Status |
|-----------|--------|
| F1 — Handler fallback values | INTACT |
| F2 — baseline_model_unknown | INTACT |
| F3 — formatted_savings_usd rename | INTACT |
| F4 — baseline_cost 4dp rounding | INTACT |
| F5 — 6dp rounding | INTACT |
| F6 — Migration returns Err | INTACT |
| F7 — CostProvider trait | INTACT |

## Triage Summary

| Finding | Decision |
|---------|----------|
| F1 | FIXED via Fix A — plan doc updated |
| F2 | FIXED — method removed, tests pass |
| F3 | FIXED — plan updated |
| F4 | FIXED — plan updated |
| F5 | FIXED — plan updated |
| F6 | ACCEPTED as addendum |
| F7 | FIXED — plan updated |
