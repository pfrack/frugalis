# Per-Intent Latency Summary Implementation Plan

## Overview

Add a per-intent latency summary to the Cerebrum dashboard. Two delivery points: a summary card on `/dashboard` showing total inferences and per-category average latency, and a dedicated `/dashboard/latency` page with configurable time window (`?hours=`), AVG + P99 per category, and an unclassified-count footnote. Both views degrade gracefully when persistence is unavailable.

## Current State Analysis

All prerequisites are implemented:
- **F-03**: Askama template system with `base.html` inheritance, working `/dashboard` and `/dashboard/inferences` routes
- **S-01**: Intent classifier wired into `completion_handler`, records populated with `category`, `upstream_model`, `duration_ms` columns
- **S-02**: `PersistenceConfig::fetch_inferences()` with parameterized SQL, filtering, pagination, and manual Row mapping
- **Inferences table**: `category TEXT`, `duration_ms INTEGER`, `created_at TIMESTAMPTZ` — all needed for GROUP BY aggregation

The dashboard index handler (`main.rs:152`) is currently standalone — does not accept `State` or query the database. The navigation bar is defined per-template in `{% block nav %}` — adding a new tab requires edits to `index.html` and `inferences.html`.

## Desired End State

1. **`/dashboard`** index page shows a latency summary card below the welcome section:
   - "Last 24 hours" heading
   - Total inference count for the window
   - Per-category mini-table: category name, count, average duration
   - "Unclassified records: N" footnote when category is null
   - "No data" message when no inferences exist in window
   - Link to the full latency breakdown page
   - Degrades to current welcome-only page when persistence is disabled

2. **`/dashboard/latency`** page shows a full latency breakdown:
   - Time-window form with configurable `?hours=` parameter (default 24, min 1, max 720)
   - Full table: Category, Count, Avg Duration, P99 Duration
   - "Unclassified records excluded: N" footnote below the table
   - Consistent empty-state message matching `inferences.html` style
   - Degrades to error message when persistence is disabled

3. Navigation bar updated in all dashboard templates to include "Latency" tab.

### Key Discoveries:

- PostgreSQL `PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms)` is a standard ordered-set aggregate — available without extensions on all PostgreSQL versions the `inferences` table is deployed on (`src/persistence.rs:1`)
- The existing `fetch_inferences` method uses raw `sqlx::query()` + manual `Row` mapping — the aggregation query follows the same pattern (`src/persistence.rs:83-207`)
- The dashboard index handler `async fn dashboard() -> impl IntoResponse { DashboardIndex {} }` at `src/main.rs:152` must be modified to accept `State<Arc<AppState>>` for the summary card (`src/main.rs:152`)
- Navigation links are defined per-template in `{% block nav %}` — both `index.html` and `inferences.html` need a new `<a href="/dashboard/latency">Latency</a>` link (`templates/dashboard/index.html:3-6`, `templates/dashboard/inferences.html:3-6`)

## What We're NOT Doing

- No time-series charts or graphical visualizations — pure table view
- No real-time streaming / WebSocket updates — page refreshes on reload
- No per-model latency breakdown — grouped by category only per the PRD spec
- No date-range picker UI — just a simple `?hours=` integer parameter
- No export or CSV download
- No retention-based data pruning logic

## Implementation Approach

Add a new SQL aggregation query to `PersistenceConfig`, build two presentation layers on top of it (index card + full page), and wire a new route. The aggregation query runs two statements: the GROUP BY breakdown and a separate COUNT for NULL-category records. Both queries share the same time-window WHERE clause. The `hours` parameter is parsed in the handler with validation (1–720 range, default 24).

The dashboard index handler gains a `State` parameter and queries the same aggregation method (with `hours=24`). If persistence is `None`, the handler renders the current static welcome page unchanged. The latency page gets a dedicated route `GET /dashboard/latency` under the existing dashboard router that already has the Basic Auth middleware layer.

## Critical Implementation Details

- **Timing & lifecycle**: The `hours` parameter bound check (1–720) prevents DoS via absurd ranges. The max range of 720 hours (30 days) aligns with the planned 90-day retention.

## Phase 1: Aggregation Query — Persistence Layer

### Overview

Add the `fetch_latency_summary` method to `PersistenceConfig` with a new `LatencySummary` return type. This is the single data source for both the index card and the full latency page.

### Changes Required:

#### 1. New structs for latency summary data

**File**: `src/persistence.rs`

**Intent**: Define the data types returned by the aggregation query so callers (handlers) receive structured, typed data.

**Contract**: Two new public structs added after `InferenceLog` (around line 34):

- `LatencySummaryRow` — one row per category: `category: String`, `request_count: i64`, `avg_duration_ms: Option<i32>`, `p99_duration_ms: Option<i32>`
- `LatencySummary` — container: `rows: Vec<LatencySummaryRow>`, `unclassified_count: i64`, `total_classified_count: i64`, `hours: u32`

#### 2. New `fetch_latency_summary` method

**File**: `src/persistence.rs`

**Intent**: Execute the GROUP BY aggregation query against the `inferences` table filtered by the time window, plus a separate COUNT for NULL-category records. Returns `Result<LatencySummary, QueryError>` matching the module's error convention.

**Contract**: Add a public method `fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError>` to `impl PersistenceConfig`. Two SQL statements, run sequentially on `self.pool`:

1. **Grouped aggregation:**

```sql
SELECT category,
       COUNT(*)::BIGINT,
       ROUND(AVG(duration_ms))::INTEGER,
       ROUND(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms))::INTEGER
FROM inferences
WHERE created_at >= NOW() - ($1 || ' hours')::INTERVAL
  AND category IS NOT NULL
GROUP BY category
ORDER BY count DESC
```

Bind `hours as i64` for the single parameter `$1`. Map each row to `LatencySummaryRow` manually via `row.try_get::<&str, _>("category")`, `try_get::<i64, _>("count")`, etc.

2. **Unclassified count:**

```sql
SELECT COUNT(*)
FROM inferences
WHERE created_at >= NOW() - ($1 || ' hours')::INTERVAL
  AND category IS NULL
```

Bind the same `hours` parameter. Map the scalar count with `row.try_get(0)`.

Return `LatencySummary` with the collected rows, unclassified count, `total_classified_count` as sum of all row counts, and the `hours` parameter value.

#### 3. No migration needed

**File**: `migrations/`

**Intent**: Existing `category TEXT`, `duration_ms INTEGER`, and `created_at TIMESTAMPTZ` columns are sufficient. The `inferences_created_at_idx` index (created in 001_create_inferences.sql) serves the time-window WHERE clause.

**Contract**: No migration. No new indexes needed at MVP scale.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles the new structs and method
- DB integration tests pass: the method returns correct aggregation when run against a live database

#### Manual Verification:

- Method returns empty rows and zero counts when no data exists in the window
- Method correctly groups by category with proper AVG and P99 values
- NULL category count is accurate

---

## Phase 2: Dashboard Index Summary Card

### Overview

Modify the `/dashboard` index page to display a latency summary card below the welcome message. The handler now queries persistence for a 24-hour latency summary and passes it to the template. When persistence is disabled, the current static page renders unchanged.

### Changes Required:

#### 1. Modify `DashboardIndex` template struct

**File**: `src/main.rs` (around line 20)

**Intent**: Extend the `DashboardIndex` struct to carry optional summary data and an optional error string, mirroring the `InferencesTemplate` pattern.

**Contract**: Add two fields:
- `summary: Option<persistence::LatencySummary>`
- `error: Option<String>`

#### 2. Modify `dashboard()` handler

**File**: `src/main.rs` (around line 152)

**Intent**: Accept `State<Arc<AppState>>`, query the persistence layer for a 24h latency summary when available, and pass the result to the template.

**Contract**: Change signature to `async fn dashboard(State(state): State<Arc<AppState>>) -> impl IntoResponse`. Inside:
- If `state.persistence` is `None`, return `DashboardIndex { summary: None, error: None }` — current behavior preserved
- If `state.persistence` is `Some(p)`, call `p.fetch_latency_summary(24).await` and map result to `DashboardIndex { summary: Some(s), error: None }` on success, or `DashboardIndex { summary: None, error: Some(e.to_string()) }` on error

#### 3. Update dashboard index template

**File**: `templates/dashboard/index.html`

**Intent**: Add a summary card after the welcome section when summary data is available, or show an error banner on failure.

**Contract**: Below the welcome card, add a conditional block:
- `{% if let Some(err) = error %}` — `<div class="error-banner">`
- `{% if let Some(s) = summary %}`
  - Card with heading "Latency Summary — Last 24 hours"
  - Show `s.total_classified_count` as "Total classified inferences: N"
  - Mini-table of categories: one row per `s.rows`, showing category badge, request count, average duration
  - If `s.unclassified_count > 0`, show footnote: "s.unclassified_count unclassified records excluded."
  - If `s.rows.is_empty()`, show `<div class="empty-state">` with "No inference data yet"
  - Link button to `/dashboard/latency?hours=24` — "View full latency breakdown →"
- `{% endif %}`

The existing welcome card and link to `/dashboard/inferences` remain above the new card.

#### 4. Update navigation bar

**File**: `templates/dashboard/index.html` (`{% block nav %}`)

**Intent**: Add a "Latency" tab link so the operator can reach the full latency page.

**Contract**: Add `<a href="/dashboard/latency">Latency</a>` after the "Inference Logs" link.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles the updated handler and template
- `cargo test` — existing `test_dashboard_authenticated_returns_html` still passes (response still contains "Cerebrum Dashboard")
- New test: dashboard index with persistence=None returns 200 with welcome content, no crash

#### Manual Verification:

- When persistence is active and data exists: summary card shows with correct counts and averages
- When persistence is active but no data: "No inference data yet" empty state
- When persistence is disabled (no DATABASE_URL): page renders as before, no error
- Navigation bar includes the "Latency" link

---

## Phase 3: Full Latency Breakdown Page

### Overview

Create a dedicated `/dashboard/latency` page with a configurable time-window form, a full latency table showing AVG and P99 per category, and the unclassified-count footnote. Follows the same template and handler patterns as the existing `inferences` route.

### Changes Required:

#### 1. New `LatencyTemplate` struct

**File**: `src/main.rs` (after `InferencesTemplate`, around line 33)

**Intent**: Template struct carrying the latency summary data, error state, and current hours parameter for the form's pre-filled value.

**Contract**:

```rust
#[derive(Template, WebTemplate)]
#[template(path = "dashboard/latency.html")]
struct LatencyTemplate {
    summary: Option<persistence::LatencySummary>,
    hours: u32,
    error: Option<String>,
}
```

#### 2. New `latency()` handler

**File**: `src/main.rs` (after `inferences()` handler, around line 220)

**Intent**: Parse the `?hours=` query parameter with validation, fetch the latency summary, and render the template. Follows the same structure as the `inferences()` handler (lines 156-220).

**Contract**: `async fn latency(State(state): State<Arc<AppState>>, Query(params): Query<HashMap<String, String>>) -> impl IntoResponse`

- Parse `hours` from query params: default 24, clamp to 1..720 range on parse failure or out-of-bounds
- If `state.persistence` is None → return `LatencyTemplate { summary: None, hours, error: Some("Database not configured".into()) }`
- Call `p.fetch_latency_summary(hours).await` → map to `LatencyTemplate { summary: Some(s), hours, error: None }` on success or `{ summary: None, hours, error: Some(e.to_string()) }` on error

#### 3. New latency template

**File**: `templates/dashboard/latency.html`

**Intent**: Render the full latency breakdown with time-window selector, category table, and footnote.

**Contract**: Extends `base.html`. Structure:

- `{% block nav %}` — same three links as the updated `index.html`: Dashboard, Inference Logs, Latency
- `{% block content %}`:
  - Page heading "Latency Summary" with subtitle "Grouped by intent category"
  - Time-window form (GET to `/dashboard/latency`): label "Time window (hours)", input type="number" name="hours" with value pre-filled from template, min=1 max=720, submit button "Update"
  - Error banner for `error`
  - If `summary` is `Some(s)`:
    - If `s.rows` is empty: `<div class="empty-state"><p>No inference data in the selected window</p>...</div>`
    - Otherwise: table with columns Category (badge), Count, Avg Duration (ms), P99 Duration (ms). One row per `s.rows`.
    - Below table: if `s.unclassified_count > 0`, footnote `<p class="muted" style="font-size:12px;">s.unclassified_count unclassified records excluded from this summary.</p>`
    - Summary line: `<p class="muted">s.total_classified_count total classified inferences in the selected window.</p>`
  - Link back to dashboard: `<a href="/dashboard" class="btn btn-ghost">← Back to Dashboard</a>`

All CSS classes (`card`, `error-banner`, `badge`, `muted`, `empty-state`, `btn`, `btn-ghost`, `btn-primary`, `filters`, `form`) are already defined in `base.html` and reused.

#### 4. Wire new route

**File**: `src/main.rs` (in `build_app()`, around line 234)

**Intent**: Add the `/latency` GET route to the dashboard router so it inherits the Basic Auth middleware layer.

**Contract**: Add `.route("/latency", get(latency))` to the `dashboard_routes` Router builder, after the existing `.route("/inferences", get(inferences))` line.

#### 5. Update inferences template navigation

**File**: `templates/dashboard/inferences.html` (`{% block nav %}`)

**Intent**: Add the "Latency" tab link so all dashboard pages have consistent navigation.

**Contract**: Add `<a href="/dashboard/latency">Latency</a>` after the "Inference Logs" link, matching the same pattern added to `index.html` in Phase 2.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles the new route, handler, and template
- `cargo test routes_auth` — new `test_latency_unauthenticated_returns_401` passes (auth gate works)
- `cargo test routes_auth` — new `test_latency_authenticated_returns_html` passes (200 + text/html Content-Type)
- `cargo test` — new `test_latency_empty_state` passes (persistence=None → renders error message without crash)
- New tests for parameter parsing: invalid hours → defaults to 24, out-of-range → clamped

#### Manual Verification:

- Navigating to `/dashboard/latency` (authenticated) renders the page with correct data
- Changing the `hours` parameter and submitting the form updates the table
- Categories display with correct counts, average durations, and P99 values
- The unclassified-count footnote appears when unclassified records exist
- Empty state renders cleanly when no data exists in the window
- Navigation links are consistent across all three dashboard pages
- Page works when persistence is disabled (shows error message, no crash)

---

## Testing Strategy

### Unit Tests (in `src/main.rs` `#[cfg(test)] mod tests`):

- `test_latency_unauthenticated_returns_401` — verifies Basic Auth gate on `/dashboard/latency`
- `test_latency_authenticated_returns_html` — verifies 200 + Content-Type text/html with valid auth
- `test_latency_invalid_hours_defaults` — `?hours=abc` and `?hours=0` both render with default hours=24
- `test_latency_out_of_range_clamped` — `?hours=99999` clamps to 720
- `test_latency_empty_state` — persistence=None returns error template, no crash
- `test_dashboard_index_without_persistence` — ensure existing test still passes; verify no crash when persistence is None
- `test_dashboard_index_renders_latency_card_structure` — verifies HTML contains expected class names when summary data is mocked

### Integration Tests (in `src/persistence.rs` `#[cfg(test)] mod tests`):

- `test_fetch_latency_summary_empty` — no data in window returns empty rows and zero counts
- `test_fetch_latency_summary_with_data` — inserts test records with known durations, verifies correct categories/counts/avg/p99
- `test_fetch_latency_summary_unclassified_count` — inserts records with NULL category, verifies unclassified_count
- `test_fetch_latency_summary_time_filter` — inserts records with old timestamps (manual `created_at` override), verifies they're excluded

### Manual Testing Steps:

1. Send several classified proxy requests (`POST /v1/chat/completions`) with varying intents
2. Navigate to `/dashboard` — verify summary card appears with correct counts and averages
3. Navigate to `/dashboard/latency` — verify full table with AVG and P99 values
4. Change hours parameter to 1 — verify only recent records appear
5. Change hours parameter to 720 — verify all records appear
6. Verify navigation links work between all three dashboard pages
7. Disable DATABASE_URL — verify graceful degradation on both index and latency pages

## Performance Considerations

- The aggregation query runs a sequential scan on `inferences_created_at_idx` for the time filter, then a hash aggregate on category. At MVP scale (hundreds or thousands of rows), this is sub-millisecond.
- Two separate queries (grouped + unclassified count) rather than a single `FILTER` clause to keep the SQL readable and avoid sqlx compatibility concerns.
- The semaphore for background logging (`task_semaphore`) is unrelated to dashboard reads — no contention.

## Migration Notes

No schema changes required. The `inferences` table already has `category TEXT`, `duration_ms INTEGER`, and `created_at TIMESTAMPTZ`. The `inferences_created_at_idx` index from migration 001 serves the time-window filter.

## References

- Related research: `context/changes/proxy-intent-routing/research.md` (classifier design, category constants)
- Similar implementation: `PersistenceConfig::fetch_inferences` at `src/persistence.rs:83`
- Similar handler: `inferences()` at `src/main.rs:156`
- Roadmap spec: `context/foundation/roadmap.md:135-146` (S-03 definition)
- PRD: `context/foundation/prd.md:39` (secondary success criterion)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Aggregation Query — Persistence Layer

#### Automated

- [x] 1.1 `cargo build` compiles new structs and method — a1b16eb
- [x] 1.2 DB integration tests pass for fetch_latency_summary — a1b16eb

#### Manual

- [ ] 1.3 Method returns correct aggregations against live DB

### Phase 2: Dashboard Index Summary Card

#### Automated

- [x] 2.1 `cargo build` compiles updated handler and template — b92acc8
- [x] 2.2 Existing `test_dashboard_authenticated_returns_html` still passes — b92acc8
- [x] 2.3 New test: dashboard index with persistence=None returns 200, no crash — b92acc8

#### Manual

- [ ] 2.4 Summary card renders with correct data when DB is active
- [ ] 2.5 Empty state renders when no data exists
- [ ] 2.6 Page renders unchanged when persistence is disabled
- [ ] 2.7 Navigation includes Latency link

### Phase 3: Full Latency Breakdown Page

#### Automated

- [x] 3.1 `cargo build` compiles new route, handler, and template
- [x] 3.2 `test_latency_unauthenticated_returns_401` passes
- [x] 3.3 `test_latency_authenticated_returns_html` passes
- [x] 3.4 `test_latency_invalid_hours_defaults` and out-of-range clamping pass
- [x] 3.5 `test_latency_empty_state` passes (no crash with persistence=None)

#### Manual

- [ ] 3.6 Full table renders with correct AVG + P99 per category
- [ ] 3.7 Time-window form changes update the table correctly
- [ ] 3.8 Unclassified-count footnote appears when applicable
- [ ] 3.9 Empty state renders cleanly
- [ ] 3.10 Navigation consistent across all three dashboard pages
- [ ] 3.11 Graceful degradation when persistence is disabled
