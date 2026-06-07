<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Cost-Savings Metric Implementation Plan

- **Plan**: context/changes/cost-savings-metric/plan.md
- **Scope**: All 3 phases
- **Date**: 2026-06-01
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

### F1 — Handler short-circuits on missing classifier instead of graceful fallback

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:317-326
- **Detail**: Plan specified that when classifier is None, the handler should pass ModelCosts::empty() and "unknown" as fallbacks. Instead, it returned early with "Cost configuration not available".
- **Fix**: Restructured handler to use `ModelCosts::empty()` and `"unknown"` fallbacks when classifier is None.
- **Decision**: FIXED

### F2 — Missing baseline-model cost silently shows "$0.00 (no savings)"

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:374-378
- **Detail**: When baseline_model is not found in model_costs, baseline_cost defaults to 0.0, making savings = -total_actual_cost. Template showed "$0.00" without surfacing the misconfiguration.
- **Fix A ⭐ Recommended**: Added `baseline_model_unknown: bool` to `SavingsEstimate`. Displayed a warning in the template when set.
- **Decision**: FIXED

### F3 — `savings_usd_formatted` field not in plan contract

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence.rs:52
- **Detail**: `SavingsEstimate` had a `savings_usd_formatted` field not in the plan's contract.
- **Fix**: Renamed to `formatted_savings_usd`.
- **Decision**: FIXED

### F4 — Precision asymmetry between baseline and per-model cost

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:367-378
- **Detail**: Per-model costs rounded to 4 decimal places via prompt_chars_to_cost, but baseline_cost was not rounded.
- **Fix**: Applied same 4-decimal rounding to baseline_cost before computing savings.
- **Decision**: FIXED

### F5 — Very small costs round to zero

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:454-458
- **Detail**: prompt_chars_to_cost rounded to 4 decimal places; costs below $0.00005 got silently zeroed.
- **Fix**: Changed rounding to 6 decimal places.
- **Decision**: FIXED

### F6 — Schema migration failure is non-fatal

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:110-111
- **Detail**: Schema migration error was caught and logged only; app silently continued with degraded schema.
- **Fix**: Changed to return an error on migration failure, forcing operator attention.
- **Decision**: FIXED

### F7 — `fetch_savings_estimate` has type dependency on sibling module

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Architecture
- **Location**: src/persistence.rs:319-324
- **Detail**: `fetch_savings_estimate` directly depended on `intent_classificator::ModelCosts` type.
- **Fix**: Defined `CostProvider` trait in persistence.rs. `ModelCosts` implements it. Method accepts `&impl CostProvider`.
- **Decision**: FIXED

## Triage Summary

| Finding | Decision |
|---------|----------|
| F1 | FIXED — handler uses fallback values per plan |
| F2 | FIXED — added baseline_model_unknown + template warning |
| F3 | FIXED — renamed to formatted_savings_usd |
| F4 | FIXED — baseline_cost now rounded to 4 decimals |
| F5 | FIXED — rounding changed to 6 decimal places |
| F6 | FIXED — migration failure now returns Err |
| F7 | FIXED — CostProvider trait decouples modules |
