<!-- PLAN-REVIEW-REPORT -->
# Plan Review: Cost-Savings Metric Implementation Plan

- **Plan**: `context/changes/cost-savings-metric/plan.md`
- **Mode**: Deep
- **Date**: 2026-06-01
- **Verdict**: SOUND
- **Findings**: 0 critical | 1 warning | 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| End-State Alignment | PASS |
| Lean Execution | PASS |
| Architectural Fitness | PASS |
| Blind Spots | WARNING |
| Plan Completeness | PASS |

## Grounding
8/8 paths ✓, 4/4 symbols ✓, brief↔plan ✓, Progress↔Phase ✓

## Findings

### F1 — `prompt_char_count` uses full JSON body instead of extracted user message

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Blind Spots
- **Location**: Phase 1, Change #2 — Caller in `main.rs:completion_handler`
- **Detail**: Plan says `body_str.chars().count() as i32` for `prompt_char_count`, but `body_str` is the full JSON request body (line 126: `std::str::from_utf8(&body).unwrap_or("")`), not the extracted user prompt. The JSON wrapper (`{"messages":[{"role":"user","content":"..."}]}`) adds ~80-100 chars per record that inflate the token estimate. Since `prompt` (the extracted last user message) is already computed on line 127, the fix is trivial.
- **Fix**: Replace `body_str.chars().count()` with `prompt.chars().count()` in the `InferenceRecord` constructor at line ~147 of `main.rs`.
- **Decision**: FIXED — applied to plan.md

### F2 — Negative savings display not addressed

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Blind Spots
- **Location**: Phase 3 — Template
- **Detail**: If the baseline model is cheaper than routed models (operator misconfigures `BASELINE_MODEL` to a value cheaper than actual routed models), savings goes negative. The template contract says "stat card showing the dollar savings figure ($X.XXXX)" without guidance on negative or zero-savings display. A raw "-$0.42" is technically correct but poor UX.
- **Fix**: Show "Estimated savings: $0.00 (no savings — baseline costs less)" when `savings_usd <= 0.0`, and a positive figure otherwise.
- **Decision**: FIXED — applied to plan.md

### F3 — `ModelCosts` merge from `routing.toml` needs explicit mechanism

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Completeness
- **Location**: Phase 1, Change #4
- **Detail**: Plan says to parse `cost_per_1m_input_tokens` from TOML entries and merge into `ModelCosts`, but doesn't specify the mechanism. `load_routing_from_file` currently returns `HashMap<String, RouteEntry>` — it has no way to carry cost data. The implementer needs to decide: extend `RouteEntry`, or parse costs in a separate pass? Given the codebase patterns, the natural approach is to add the field to `RouteEntry`.
- **Fix**: Add `pub cost_per_1m_input_tokens: Option<f64>` to `RouteEntry` (line ~10), parse it from TOML, then have `IntentClassifier::from_env` build `ModelCosts` by iterating the routing HashMap and extracting this field (falling back to the hardcoded cost table for models without a TOML override).
- **Decision**: FIXED — applied to plan.md
