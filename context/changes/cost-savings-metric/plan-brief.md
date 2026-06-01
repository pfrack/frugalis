# Cost-Savings Metric — Plan Brief

> Full plan: `context/changes/cost-savings-metric/plan.md`
> Related: Roadmap S-04 in `context/foundation/roadmap.md`

## What & Why

The gateway routes prompts to cheaper models based on intent, but the operator has no visibility into how much money that routing actually saves. This adds a `/dashboard/savings` page that shows a single estimated dollar figure: the difference between what the operator paid with intent-aware routing vs. what they would have paid sending every prompt to an expensive baseline model. FR-007 (nice-to-have) — directional only, not billing-grade.

## Starting Point

The dashboard already serves three pages (`/dashboard/`, `/dashboard/inferences`, `/dashboard/latency`) with server-rendered Askama templates and SQL aggregation queries in `persistence.rs`. Inference records store `upstream_model` (the model actually used) and `prompt_snippet` (200 chars). The `IntentClassifier` already manages model routing config — we extend it with per-model cost data. What's missing: no cost config, no prompt character count on records, no savings query or page.

## Desired End State

The operator visits `/dashboard/savings`, sees a card with "Estimated savings: $X.XX over the last 24 hours" and the baseline model name. A warning counter appears if some records used unknown models. Navigation tabs include "Savings" on all 4 dashboard pages. Model costs are hardcoded for known models and overridable in `routing.toml`.

## Key Decisions Made

| Decision                       | Choice                                                   | Why (1 sentence)                                       | Source |
| ------------------------------ | -------------------------------------------------------- | ------------------------------------------------------ | ------ |
| Baseline model                 | `BASELINE_MODEL` env var, default `claude-3.5-sonnet`    | Most expensive model represents the "send-everything-to-best" default that agents do today | Plan   |
| Classification cost            | Exclude — gross routing savings only                     | Simpler calculation; classification is cheap and fallback is rare, so the error is negligible | Plan   |
| Display scope                  | Single total estimate on a dedicated page               | Minimal UI work; new page leaves room for future per-category breakdown | Plan   |
| Cost configuration             | Hardcoded defaults, overridable per model in `routing.toml` | Works out of the box with no mandatory config; matches existing hardcoded-fallback pattern in classifier | Plan   |
| Time window                    | Fixed 24 hours                                           | Consistent with dashboard home page latency summary default | Plan   |
| Missing model cost handling    | Omit from calculation, show warning count               | Transparent — operator sees what's incomplete; no false precision | Plan   |
| Token estimation               | Input tokens only — 4 chars ≈ 1 token                   | Only data we have (we don't store output tokens); matches PRD "directional" non-goal | Plan   |
| Prompt length storage          | Add `prompt_char_count INTEGER` column (future records only) | Accurate per-record cost estimation forward-looking; historical records fall back to snippet length | Plan   |

## Scope

**In scope:**
- New `prompt_char_count` column on inferences table
- Model cost lookup table (hardcoded + `routing.toml` overrides)
- `BASELINE_MODEL` env var
- `fetch_savings_estimate` SQL aggregation + Rust cost computation
- `/dashboard/savings` route, handler, and template
- "Savings" nav tab on all dashboard pages

**Out of scope:**
- Per-category savings breakdown
- Configurable time window (fixed 24h)
- Output token estimation
- Backfilling historical `prompt_char_count`
- Classification/fallback cost accounting
- Billing-grade precision

## Architecture / Approach

```
[proxy] → insert inference record (with prompt_char_count + upstream_model)
                      ↓
               inferences table (PostgreSQL)
                      ↓
[fetch_savings_estimate] — SQL GROUP BY upstream_model, sum char counts
                      ↓
[Rust compute] — apply per-model costs, subtract from baseline
                      ↓
[SavingsEstimate struct] → [Askama template] → HTML response
```

Model costs live in `IntentClassifier` (co-located with routing config). The handler reads costs from `AppState.classifier`, passes them to the persistence query. Persistence stays decoupled from classification — it receives a `ModelCosts` reference and a baseline model name.

## Phases at a Glance

| Phase                              | What it delivers                                        | Key risk                                               |
| ---------------------------------- | ------------------------------------------------------- | ------------------------------------------------------ |
| 1. Data Model + Cost Config        | DB column, insert path, hardcoded costs, routing.toml extension, BASELINE_MODEL | Migration failure if the inferences table doesn't exist yet (PersistenceConfig handles this — existing `from_env` is fallible and the app degrades gracefully) |
| 2. Savings Query + Handler + Route | fetch_savings_estimate, SavingsEstimate struct, handler, route registration | Baseline model has no cost configured → savings shows negative or zero (acceptable: operator misconfiguration, not a crash) |
| 3. Template + Nav                  | savings.html template, nav tabs on all 4 pages          | Askama template parse failures if the struct field names don't match (caught at compile time by `cargo build`) |

**Prerequisites:** S-01 + S-02 must be complete (inference records being logged, dashboard rendering working). The existing codebase already has this.
**Estimated effort:** ~2-3 sessions across 3 phases.

## Open Risks & Assumptions

- Model pricing hardcoded in source will drift over time — acceptable for "directional" metric; operators can override in `routing.toml`
- 4 chars ≈ 1 token is a rough heuristic that varies by model tokenizer — within acceptable range for directional estimates; real tokenizer integration is post-MVP
- New column is NULLable and requires no data migration — safe to deploy without downtime

## Success Criteria (Summary)

- Operator sees a dollar savings figure on `/dashboard/savings` based on last 24h of inference records
- Unknown-model records are excluded with a visible warning count (no false precision)
- Navigation to the savings page works from all dashboard tabs
- Baseline model is configurable via env var; model costs are overridable via `routing.toml`
