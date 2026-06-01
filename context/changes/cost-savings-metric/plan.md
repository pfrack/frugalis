# Cost-Savings Metric Implementation Plan

## Overview

Add a new `/dashboard/savings` page showing an estimated cost-savings indicator: the inferred savings from routing prompts to cheaper models vs. sending everything to a configurable baseline model. Uses hardcoded model costs (overridable via `routing.toml`) and input-token estimation from prompt character counts. This is FR-007 (nice-to-have) per the PRD — directional savings, not billing-grade precision.

## Current State Analysis

The dashboard already has three pages with server-rendered Askama templates:
- `/dashboard/` — latency summary card (24h default)
- `/dashboard/inferences` — paginated inference log table with category/model filters
- `/dashboard/latency` — per-category latency breakdown with configurable time window

Persistence layer (`persistence.rs:101-286`) has rich SQL query patterns (`fetch_inferences` with filter variants, `fetch_latency_summary` with GROUP BY aggregation). The inferences table schema stores `prompt_snippet` (200 chars) but has no `prompt_char_count` column — current records can't produce accurate token estimates.

Navigation links are embedded in each template's `{% block nav %}` — four templates (index, inferences, latency, plus the new savings page) will each need the "Savings" tab added.

Cost routing lives in `intent_classificator.rs:174-209` (hardcoded defaults) with optional overrides from `routing.toml`. Model names are already available in inference records via `upstream_model`.

### Key Discoveries:

- `persistence.rs:240-250`: `fetch_latency_summary` uses `created_at >= NOW() - interval '1 hour' * $1` — same pattern for savings time filtering
- `persistence.rs:410-425`: `insert_once` binds exactly the current 6 columns — adding `prompt_char_count` is a 7th bind
- `intent_classificator.rs:278-301`: `load_routing_from_file` parses `[category].model` and `[category].endpoint` from TOML — adding an optional `cost_per_1m_input_tokens` field is a one-line extension
- `main.rs:289-311`: `build_app` routes use `.nest("/dashboard", dashboard_routes)` — adding a new handler is a one-liner `.route("/savings", get(savings))`
- Templates extend `base.html` which has `{% block nav %}{% endblock %}` — each template independently defines nav links, so 4 files get the new "Savings" tab

## Desired End State

The operator visits `/dashboard/savings`, sees an estimated dollar savings figure for the last 24 hours (e.g., "Estimated savings: $2.47"), a brief explanation of what the comparison means, and — if any records lacked cost data — a warning count. The calculation compares actual routing costs against what the baseline model would have cost for the same prompts. A model-costs-hardcoded table provides defaults; `routing.toml` entries can override individual model costs. The operator can change the baseline model via `BASELINE_MODEL` env var.

## What We're NOT Doing

- Per-category savings breakdown (single total only)
- Configurable time window on the savings page (fixed 24h)
- Including classification/fallback costs in the calculation
- Tracking or estimating output tokens (input tokens only — 4 chars ≈ 1 token)
- Backfilling `prompt_char_count` for historical records (they'll produce estimates from snippet length with a note)
- Billing-grade precision — directional only, per PRD FR-007

## Implementation Approach

Three phases, ordered by dependency: data model change must land before the query that reads the new column. Cost configuration is the foundation for both the query and the display.

Phase 1 introduces the DB column and updates the insert path — old records get NULL/0 in the new column and the query falls back gracefully. Phase 2 adds the server-side aggregation. Phase 3 adds the user-facing template.

## Phase 1: Data Model Extension + Cost Configuration

### Overview

Add `prompt_char_count` to the inferences table schema, update the insert path to populate it, and wire up the model-cost configuration (hardcoded defaults + `routing.toml` overrides + `BASELINE_MODEL` env var).

### Changes Required:

#### 1. Database schema migration

**File**: `src/persistence.rs` (or a standalone migration file)

**Intent**: Add `prompt_char_count INTEGER` column to the `inferences` table so each stored record carries the full prompt character count. Existing records get NULL — the query in Phase 2 falls back to `LENGTH(prompt_snippet)` when `prompt_char_count IS NULL`.

**Contract**: `ALTER TABLE inferences ADD COLUMN IF NOT EXISTS prompt_char_count INTEGER;` — executed either via a SQL migration file or at startup in `PersistenceConfig::from_env`. Follow the same migration approach the codebase uses (check existing migration strategy — if none exists yet, run the DDL in `from_env` after connection).

#### 2. Extend InferenceRecord and insert path

**File**: `src/persistence.rs`

**Intent**: Add `prompt_char_count: Option<i32>` to `InferenceRecord` and bind it in `insert_once` as a 7th parameter. Update the caller in `main.rs:completion_handler` to compute and pass the character count.

**Contract**: 
- `InferenceRecord` gets a new field: `pub prompt_char_count: Option<i32>` (line ~70)
- `insert_once` SQL gains a 7th bind parameter: `prompt_char_count` (line ~412)
- Caller in `main.rs:142-158` passes `Some(prompt.chars().count() as i32)` in the `InferenceRecord` constructor (uses the already-extracted user message at line 127, not the full JSON body)

#### 3. Cost display utility

**File**: `src/persistence.rs`

**Intent**: Add a `prompt_chars_to_cost(chars: i32, cost_per_1m: f64) -> f64` utility that converts character count → estimated token count (chars / 4) → dollar cost (tokens × cost_per_1m / 1_000_000). Shared between the savings query and the template.

**Contract**: `pub fn prompt_chars_to_cost(char_count: i32, cost_per_1m_input_tokens: f64) -> f64` — rounds to 4 decimal places.

#### 4. Model cost configuration

**File**: `src/intent_classificator.rs`

**Intent**: Add a hardcoded `HashMap<&str, f64>` of known model → cost per 1M input tokens, overridable by an optional `cost_per_1m_input_tokens` field in each routing.toml entry. Expose the merged cost map as a public method on `IntentClassifier`.

**Contract**:
- New `ModelCosts` struct with a `HashMap<String, f64>` and a `get(model: &str) -> Option<f64>` method
- Hardcoded defaults:
  ```
  "claude-3.5-sonnet" → 3.00,
  "gpt-4o" → 2.50,
  "gpt-4o-mini" → 0.15,
  "deepseek-chat" → 0.14,
  ```
- `RouteEntry` (line ~10) gains `pub cost_per_1m_input_tokens: Option<f64>` — parsed from the TOML `cost_per_1m_input_tokens` field when present
- `load_routing_from_file` parses `cost_per_1m_input_tokens` from each TOML entry (optional float), storing it in the `RouteEntry`
- `IntentClassifier::from_env` builds `ModelCosts` by iterating the completed routing HashMap: for each entry, extracts `cost_per_1m_input_tokens` from the `RouteEntry` (falling back to the hardcoded cost table for models without an override)
- `IntentClassifier` gains a `pub fn model_costs(&self) -> &ModelCosts` accessor
- `BASELINE_MODEL` env var (default `"claude-3.5-sonnet"`) is read in `IntentClassifier::from_env` and stored as `pub baseline_model: String`

### Success Criteria:

#### Automated Verification:

- Migration applies without error against a test DB: column exists with type INTEGER
- `insert_once` correctly binds 7 parameters (no SQL error on insert)
- `prompt_char_count` is stored and retrievable after insert
- Hardcoded model costs are accessible via `classifier.model_costs().get("gpt-4o-mini") == Some(0.15)`
- `BASELINE_MODEL=claude-3.5-sonnet` is read and stored; unset falls back to default
- `routing.toml` with `[COMPLEX_REASONING].cost_per_1m_input_tokens = 5.0` overrides the hardcoded value
- Existing tests continue to pass: `cargo test`

#### Manual Verification:

- Insert a test inference record; verify `prompt_char_count` column shows the correct character count in the database
- Set `BASELINE_MODEL=gpt-4o` and confirm the classifier stores it
- Add a `cost_per_1m_input_tokens` override in `routing.toml` and confirm it's reflected in `model_costs()`

---

## Phase 2: Savings Query + Handler + Route

### Overview

Add a `fetch_savings_estimate` method to `PersistenceConfig` that aggregates last-24h inference records by model, computes actual and baseline costs in Rust, and returns a `SavingsEstimate` struct. Wire a new `/dashboard/savings` GET handler and template struct in `main.rs`.

### Changes Required:

#### 1. SavingsEstimate struct and fetch method

**File**: `src/persistence.rs`

**Intent**: Add a `SavingsEstimate` struct carrying the computed savings figure, the baseline model used, the record count, a warning count for records with unknown model costs, and a `has_historical_fallback` flag indicating whether any records used snippet-length fallback (missing `prompt_char_count`). Add a `fetch_savings_estimate` method that runs a SQL GROUP BY on `upstream_model`, sums chars per model, applies costs in Rust, and returns the struct.

**Contract**:
```rust
pub struct SavingsEstimate {
    pub savings_usd: f64,
    pub baseline_model: String,
    pub classified_count: i64,
    pub unknown_cost_count: i64,
    pub has_historical_fallback: bool,
}
```

The SQL query groups by `upstream_model` for the last 24 hours, filtering out NULL categories and NULL models:

```sql
SELECT
    upstream_model,
    COUNT(*)::BIGINT AS count,
    COALESCE(SUM(prompt_char_count), 0)::BIGINT AS total_chars,
    COALESCE(SUM(LENGTH(prompt_snippet)), 0)::BIGINT AS total_fallback_chars,
    COALESCE(SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END), 0)::BIGINT AS fallback_count
FROM inferences
WHERE created_at >= NOW() - interval '1 hour' * $1
  AND category IS NOT NULL
  AND upstream_model IS NOT NULL
GROUP BY upstream_model
```

Rust-side computation:
- For each row: look up model cost via `classifier.model_costs().get(upstream_model)`
- If cost is known: compute actual cost from `total_chars` (or `total_fallback_chars` if `total_chars == 0`) using `prompt_chars_to_cost`, sum into `total_actual_cost`
- If cost is unknown: add `count` to `unknown_cost_count`, skip the model
- After all models: `total_tokens_est = sum of all chars / 4`, `baseline_cost = total_tokens_est * baseline_model_cost / 1_000_000`, `savings = baseline_cost - total_actual_cost`
- If baseline model itself has no cost configured, `baseline_cost` is 0 and savings is negative `total_actual_cost` (operator misconfiguration — show a warning in the template)

**Method signature** on `PersistenceConfig`:
```rust
pub async fn fetch_savings_estimate(
    &self,
    hours: u32,
    model_costs: &ModelCosts,
    baseline_model: &str,
) -> Result<SavingsEstimate, QueryError>
```

This takes `model_costs` and `baseline_model` from the caller (the handler in main.rs reads them from `classifier`), keeping persistence decoupled from classification.

#### 2. Handler, template struct, and route

**File**: `src/main.rs`

**Intent**: Add a `SavingsTemplate` struct for Askama, a `savings` async handler, and register the route in `build_app`.

**Contract**:
- New template struct:
```rust
#[derive(Template, WebTemplate)]
#[template(path = "dashboard/savings.html")]
struct SavingsTemplate {
    estimate: Option<persistence::SavingsEstimate>,
    error: Option<String>,
    baseline_model: String,
}
```
- Handler `async fn savings(State(state): State<Arc<AppState>>) -> impl IntoResponse` follows the same pattern as the `latency` handler (lines 254-287): guards on `state.persistence` being `Some`, calls `fetch_savings_estimate(24, &model_costs, &baseline_model)`, returns `SavingsTemplate` with the result or error
- Route: add `.route("/savings", get(savings))` inside `dashboard_routes` in `build_app` (line ~300)

The handler extracts `model_costs` and `baseline_model` from `state.classifier`, passing `ModelCosts::empty()` and `"unknown"` as fallbacks when the classifier is `None` (graceful degradation — shows an error message in the template).

### Success Criteria:

#### Automated Verification:

- `fetch_savings_estimate` returns a `SavingsEstimate` with correct counts for test data in the DB
- Query filters NULL categories and NULL models correctly
- Records with unknown model cost are counted in `unknown_cost_count` and excluded from the savings total
- Handler returns HTTP 200 with HTML content for authenticated requests
- Handler returns HTTP 401 for unauthenticated requests
- Handler gracefully handles `state.classifier == None` (shows an error template, no panic)
- `cargo test` — all existing and new tests pass
- `cargo build --release` — compiles without warnings

#### Manual Verification:

- Send several proxy requests with different prompts; visit `/dashboard/savings` and verify savings figure appears
- Verify the warning count increments when a model in the inference records has no cost configured
- Change `BASELINE_MODEL` to a cheaper model and verify savings decrease or go negative

---

## Phase 3: Template + Navigation

### Overview

Create the `savings.html` template with the savings display and integrate the "Savings" navigation tab into all existing dashboard templates.

### Changes Required:

#### 1. Savings template

**File**: `templates/dashboard/savings.html` (new file)

**Intent**: Render the savings estimate as a card with the dollar figure, baseline model name, record count, and optional warnings for unknown-cost records or historical fallback records.

**Contract**:
- Extends `base.html`
- `{% block nav %}` includes the full set of tabs: Dashboard, Inference Logs, Latency, Savings (with Savings as active)
- Content renders:
  - **With estimate**: A stat card showing the dollar savings figure (`$X.XXXX`), the baseline model used, classified record count. When `savings_usd <= 0.0`, show "$0.00 (no savings — baseline costs less)" instead of a negative figure. If `unknown_cost_count > 0`, a muted note: "N records excluded — unknown model cost". If `has_historical_fallback`, a note: "Includes older records estimated from snippet length (less accurate)."
  - **With error**: Error banner with the message
  - **Without data/classifier**: Empty state: "No inference data yet" or "Cost configuration not available"

Follows the same visual patterns as `templates/dashboard/latency.html` (stat-row, card, empty-state, error-banner CSS classes from `base.html`).

#### 2. Navigation updates

**Files**: `templates/dashboard/index.html:3-7`, `templates/dashboard/inferences.html:3-7`, `templates/dashboard/latency.html:3-7`

**Intent**: Add the "Savings" tab link to the nav block in each existing template so the operator can navigate to the new page from anywhere in the dashboard.

**Contract**: Each `{% block nav %}` gains an additional `<a href="/dashboard/savings">Savings</a>` line (before the closing `{% endblock %}`). The savings.html template's nav block marks Savings with `class="active"`.

### Success Criteria:

#### Automated Verification:

- `cargo build --release` compiles the new template without Askama parsing errors
- Template renders without panic when `estimate` is `Some`, `None`, or when `error` is set

#### Manual Verification:

- Visit `/dashboard/savings` with a web browser; verify the page renders with the savings figure
- Click "Savings" tab from each of the other 3 dashboard pages; verify navigation works
- Verify the savings page shows appropriate empty state when no data exists
- Verify the HTML is well-formed and renders correctly on desktop and mobile viewports

---

## Testing Strategy

### Unit Tests:

- `persistence::prompt_chars_to_cost` — test with known input (1000 chars → 250 tokens → $0.00075 for gpt-4o-mini at $0.15/1M)
- `ModelCosts::get` — returns Some for hardcoded models, None for unknown
- `ModelCosts` merge behavior — routing.toml override takes precedence over hardcoded
- `SavingsEstimate` computation — test with known model costs and char counts, verify correct savings

### Integration Tests:

- End-to-end: insert inference records with known char counts and models → call `fetch_savings_estimate` → verify correct savings figure
- Unknown model cost: insert a record with a model not in the cost table → verify it's counted in `unknown_cost_count` and excluded from savings
- Historical fallback: insert a record with NULL `prompt_char_count` → verify `has_historical_fallback` is true and snippet length is used
- Authenticated access to `/dashboard/savings` returns 200; unauthenticated returns 401

### Manual Testing Steps:

1. Send 5+ proxy requests with varying prompts through the gateway
2. Visit `/dashboard/savings` and verify a savings figure is displayed
3. Verify the "Savings" tab appears on all 4 dashboard pages
4. Set `BASELINE_MODEL` to a cheap model (e.g., `gpt-4o-mini`), restart, and verify savings decrease or go negative
5. Remove a model's cost from `routing.toml`, send requests routed to that model, verify the warning count appears

## Performance Considerations

- The savings SQL aggregation runs one GROUP BY query over the last 24h of records — same cost profile as `fetch_latency_summary` which is already acceptable
- Model cost lookups are HashMap O(1) per unique model in the result set (at most 4 models given current routing)
- Template rendering is server-side, single page, no client-side computation

## Migration Notes

- The `prompt_char_count` column is NULLable — no backfill required, existing records continue to work with snippet-length fallback
- No breaking changes to existing API or dashboard pages
- `BASELINE_MODEL` env var is optional; omitting it preserves current behavior (effectively, savings are computed against claude-3.5-sonnet)
- `routing.toml` entries without `cost_per_1m_input_tokens` fall back to hardcoded defaults — existing `routing.toml` files continue to work unchanged

## References

- Related PRD requirement: FR-007 (nice-to-have) in `context/foundation/prd.md:86-87`
- Roadmap entry: S-04 in `context/foundation/roadmap.md:148-160`
- Cost config pattern: `intent_classificator.rs:278-301` (TOML routing loader)
- Query pattern: `persistence.rs:233-287` (`fetch_latency_summary`)
- Handler pattern: `main.rs:254-287` (`latency` handler)
- Template pattern: `templates/dashboard/latency.html`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Data Model Extension + Cost Configuration

#### Automated

- [x] 1.1 Migration applies cleanly against a test DB: column exists with type INTEGER — 01241bb
- [x] 1.2 `insert_once` correctly binds 7 parameters (no SQL error on insert) — 01241bb
- [x] 1.3 `prompt_char_count` is stored and retrievable after insert — 01241bb
- [x] 1.4 Hardcoded model costs are accessible via `classifier.model_costs().get("gpt-4o-mini") == Some(0.15)` — 01241bb
- [x] 1.5 `BASELINE_MODEL=claude-3.5-sonnet` is read and stored; unset falls back to default — 01241bb
- [x] 1.6 `routing.toml` with `[COMPLEX_REASONING].cost_per_1m_input_tokens = 5.0` overrides the hardcoded value — 01241bb
- [x] 1.7 Existing tests continue to pass: `cargo test` — 01241bb

#### Manual

- [ ] 1.8 Insert a test inference record; verify `prompt_char_count` column shows the correct character count in the database
- [ ] 1.9 Set `BASELINE_MODEL=gpt-4o` and confirm the classifier stores it

### Phase 2: Savings Query + Handler + Route

#### Automated

- [x] 2.1 `fetch_savings_estimate` returns a `SavingsEstimate` with correct counts for test data
- [x] 2.2 Query filters NULL categories and NULL models correctly
- [x] 2.3 Records with unknown model cost are counted in `unknown_cost_count` and excluded from savings total
- [x] 2.4 Handler returns HTTP 200 with HTML content for authenticated requests
- [x] 2.5 Handler returns HTTP 401 for unauthenticated requests
- [x] 2.6 Handler gracefully handles `state.classifier == None` (no panic)
- [x] 2.7 `cargo test` — all existing and new tests pass
- [x] 2.8 `cargo build --release` — compiles without warnings

#### Manual

- [ ] 2.9 Send several proxy requests; visit `/dashboard/savings` and verify savings figure appears
- [ ] 2.10 Verify the warning count when a model in inference records has no cost configured
- [ ] 2.11 Change `BASELINE_MODEL` to a cheaper model and verify savings decrease or go negative

### Phase 3: Template + Navigation

#### Automated

- [ ] 3.1 `cargo build --release` compiles template without Askama parsing errors
- [ ] 3.2 Template renders without panic when `estimate` is `Some`, `None`, or when `error` is set

#### Manual

- [ ] 3.3 Visit `/dashboard/savings` with a web browser; verify the page renders with the savings figure
- [ ] 3.4 Click "Savings" tab from each of the other 3 dashboard pages; verify navigation works
- [ ] 3.5 Verify the savings page shows appropriate empty state when no data exists
- [ ] 3.6 Verify the HTML is well-formed and renders correctly on desktop and mobile viewports
