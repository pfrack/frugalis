# Inference Log Inspection Implementation Plan

## Overview

Add a dashboard view (S-02) that displays recent inference records from the PostgreSQL logging pipeline. The operator can browse a paginated table of inferences with prompt snippet, intent category, upstream model, and duration. Table supports filtering by category and model, and pagination with prev/next navigation.

This slice depends on F-02 (data pipeline with async logging) and F-03 (template rendering infrastructure). It is unblocked by S-01 (proxy routing) for infrastructure purposes — the handler and queries can be wired now, and the table will populate as soon as S-01 starts logging real data.

## Current State Analysis

- **F-02 (Data persistence)** is impl-reviewed: PostgreSQL `inferences` table exists with schema (id, request_id, status, category, upstream_model, duration_ms, created_at, prompt_snippet); `PersistenceConfig` and `InferenceRecord` already in `src/persistence.rs`
- **F-03 (Dashboard templates)** is complete: `/dashboard` route exists, renders Askama template, protected by HTTP Basic auth
- **Current dashboard** (`templates/dashboard/index.html`) is static placeholder; no database queries yet
- **Logging** — F-02 enqueues async logging after response completes; `completion_handler` in `src/main.rs` shows the pattern
- **Query patterns** — Existing codebase uses `sqlx::query()` with parameterized bindings; no ORM
- **Handler patterns** — State extraction via `State(state): State<Arc<AppState>>` and `impl IntoResponse` return types are established

### Key Discoveries:

- [src/persistence.rs:1-40] — `PersistenceConfig` provides `pool: Arc<PgPool>` and `task_semaphore` for background tasks; `.from_env()` loads `DATABASE_URL`
- [src/main.rs:73-105] — `completion_handler` shows fire-and-forget pattern; `AppState` carries `persistence: Option<PersistenceConfig>`
- [src/main.rs:14-17] — Template struct pattern uses `#[derive(Template, WebTemplate)]` and `#[template(path = "...")]`; currently zero-field (no context data)
- [migrations/001_create_inferences.sql] — Schema is ready; index on `created_at DESC` for efficient recent-record queries
- [templates/dashboard/index.html] — Extends base.html; can be split into sub-templates or replaced with a new inference-logs template

## Desired End State

A `/dashboard/inferences` endpoint (or modal/panel within `/dashboard`) that:
- Authenticates via HTTP Basic auth (reuses `require_dashboard_basic` middleware from F-01)
- Queries recent inferences from the `inferences` table, with pagination (offset/limit)
- Renders a table with columns: timestamp, prompt snippet (first 200 chars with ellipsis), intent category, request duration
- Shows upstream_model and other fields in an expandable row detail (click-to-expand)
- Supports filtering by `category` and `upstream_model` via optional URL query parameters
- Shows a friendly empty-state message if no inferences exist
- Gracefully handles database errors (renders error message in template, doesn't crash with HTTP 500)
- Includes a "Refresh" button that reloads the page to fetch latest data (manual refresh; no auto-polling yet)

### Verification:

Automated:
- `cargo build` succeeds (no type errors or missing imports)
- `cargo test` passes, including new tests for authenticated access, empty state, filtering, pagination, and error handling
- SQL queries execute without errors against the test database

Manual:
- Browser: navigate to `/dashboard/inferences` with valid Basic auth → table renders
- Browser: no inferences yet → shows friendly message ("No inferences yet...")
- Browser: click a row → expandable detail shows upstream_model
- Browser: filter by category dropdown → query updates and results filter correctly
- Browser: pagination works (prev/next buttons, or offset in URL)
- Browser: without auth → HTTP 401 challenge

## What We're NOT Doing

- Real-time auto-refresh (no WebSocket or polling; manual refresh only)
- Full-text search in prompts (offset/limit pagination on recent records is sufficient for MVP)
- Exporting inference records to CSV/JSON (dashboard view only)
- Cost calculation or savings metric (that's S-04, parked)
- Per-intent latency summaries (that's S-03, depends on S-02 query infrastructure)
- Multi-user filtering (single operator, simple filters only)

## Implementation Approach

**Phase 1: Query layer** — Add a method to `PersistenceConfig` that fetches recent inferences with optional pagination (offset, limit) and filters (category, model). Returns `Vec<InferenceRecord>` or a custom struct with additional computed fields (e.g., formatted timestamp).

**Phase 2: Template & handler** — Create a new `InferencesTemplate` struct with fields (`records: Vec<InferenceRecord>`, `page: u32`, `error: Option<String>`, etc.). Create `templates/dashboard/inferences.html` with Askama loops, conditionals for empty state, optional fields for category/model, and truncation filter for snippet. Write handler that extracts state, calls the query method, and returns the template (or error message).

**Phase 3: Integration & testing** — Wire the handler into the dashboard router; add comprehensive tests for auth gate, happy path (records render), empty state, filtering, pagination, and database error handling.

## Critical Implementation Details

**Error handling pattern:** Unlike existing background tasks (which silently swallow errors), the inferences handler should return a `Result` type and gracefully pass errors to the template. This is a new pattern for the codebase. Handler should not panic; if the database query fails, catch the error, pass an error message to the template, and return HTTP 200 with the error block rendered (not HTTP 500). This keeps the dashboard responsive and prevents crashes.

**Template context struct:** `InferencesTemplate` will have fields for `records`, `page`, `total_pages` (optional, for pagination UI), `error: Option<String>`, and query parameters like `filter_category: Option<String>`. This is the first context-bound template struct in the codebase (DashboardIndex is zero-field static content). Pass data from handler to template via struct fields, not query string parameters.

**Timestamp formatting:** Database stores `created_at` as `TIMESTAMPTZ`; Rust handler should format as a human-readable string (e.g., "2026-06-01 14:30:45 UTC") before passing to template. Askama has no datetime formatting filters; compute in Rust.

## Phase 1: Query Layer

### Overview

Add a public method to `PersistenceConfig` that fetches recent inferences from the database with pagination and optional filtering. This method will be called by the handler in Phase 2.

### Changes Required:

#### 1. Query result struct (optional but recommended)

**File**: `src/persistence.rs`

**Intent**: Define a struct that represents one row from the `inferences` table, with all fields needed for the dashboard. `InferenceRecord` (used for logging) has `Option` fields and is write-focused; a new `InferenceLog` struct can be read-focused with pre-formatted fields (e.g., `timestamp: String` instead of `TIMESTAMPTZ`).

**Contract**: Add a struct named `InferenceLog` (or similar) with fields:
- `id: String` (UUID formatted as string)
- `timestamp: String` (formatted "2026-06-01 14:30:45 UTC" or similar)
- `prompt_snippet: String`
- `category: Option<String>` (None renders as "—" in template)
- `upstream_model: Option<String>` (None renders as "—")
- `duration_ms: Option<i32>` (formatted as "42 ms" or None renders as "—")

Alternatively, keep the database row generic and format fields in the handler before passing to template. Either approach works; the recommendation is to compute formatting in Rust (not in template) for clarity.

#### 2. Query method on PersistenceConfig

**File**: `src/persistence.rs`

**Intent**: Provide a method that the handler calls to fetch recent inferences. Method signature should accept pagination parameters (offset, limit) and optional filters (category, model), and return a Vec of results.

**Contract**: Add an async method named `fetch_inferences` with signature:
```rust
pub async fn fetch_inferences(
    &self,
    offset: u32,
    limit: u32,
    filter_category: Option<&str>,
    filter_model: Option<&str>,
) -> Result<Vec<InferenceLog>, String>
```

The method:
- Builds a dynamic SQL query: `SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms FROM inferences`
- Appends `WHERE category = $X` if `filter_category` is provided
- Appends `WHERE upstream_model = $X` if `filter_model` is provided (use AND if both filters are present)

#### Automated Verification:

- `cargo build` succeeds
#### 1. Define QueryError enum
- Unit test `test_fetch_inferences_empty_list` passes (query returns empty Vec when no records exist)
- Unit test `test_fetch_inferences_with_records` passes (query returns records with correct fields if DB has data)
- Unit test `test_fetch_inferences_filter_by_category` passes (WHERE clause filters correctly)

#### Manual Verification:

- `sqlx prepare` or manual inspection confirms query is syntactically valid
- Query executes without timeouts or connection errors

**Implementation Note**: After completing this phase, pause for code review before proceeding to Phase 2. Ensure query is correct and error handling is solid.

---

## Phase 2: Template & Handler

### Overview

Create the dashboard view template (`templates/dashboard/inferences.html`) and a handler (`inferences()`) that fetches data and renders it. Wire state, query results, and error handling into the template.

### Changes Required:

#### 1. Create InferencesTemplate struct

**File**: `src/main.rs`

**Intent**: Define a template context struct that holds query results and metadata for the template to render.

**Contract**: Add a struct named `InferencesTemplate` with `#[derive(Template, WebTemplate)]`:
- `records: Vec<InferenceLog>` (from Phase 1 query)
- `page: u32` (current page number for pagination UI)
- `total_pages: u32` (computed from query or passed from handler)
- `error: Option<String>` (error message if query failed)
**File**: `templates/dashboard/inferences.html` *(new file)*

**Intent**: Render the inference logs table with pagination, filters, and empty state.

**Contract**: Template must:
- Extend `base.html` and fill the `content` block
- Use `{% if error.is_some() %}<div class="error">{{ error | escape }}</div>{% endif %}` to display errors at the top
- Use `{% if records.is_empty() %}<p>No inference records yet...</p>{% else %}<table>...</table>{% endif %}` for empty state
- Inside `<table>`: loop over `records` with `{% for record in records %}<tr>...{{ record.timestamp }}...{{ record.prompt_snippet | truncate(200) }}...{% endfor %}`
- Use `{% if let Some(cat) = record.category %}{{ cat }}{% else %}—{% endif %}` for optional fields
- Include a filter form with dropdowns for category and model (submit reloads page with query params)
- Include a "Refresh" button that reloads the page
- Include pagination links (prev/next) that adjust offset in URL
- Use semantic HTML (`<table>`, `<thead>`, `<tbody>`, `<button>`) for accessibility

#### 3. Create inferences handler

**File**: `src/main.rs`

**Intent**: Extract auth state and query parameters, call the Phase 1 query method, and return the template.

- Extracts `offset`, `limit`, `filter_category`, `filter_model` from `params` (with defaults: offset=0, limit=20)

- `cargo build` succeeds (template macro generates InferencesTemplate correctly)
- `cargo test test_inferences_authenticated_returns_html` passes (request with auth header returns 200, body contains "Cerebrum Dashboard" or table header)
- `cargo test test_inferences_empty_state` passes (when no records exist, body contains friendly message)
- `cargo test test_inferences_filter_by_category` passes (filter query param is passed to query method, results filter correctly)

- Browser with auth credentials: navigate to `/dashboard/inferences` → table renders (if S-01 is logging data) or empty state message appears
- Click filter dropdown and select a category → page reloads with filtered results
- Without auth: HTTP 401 challenge

**Implementation Note**: After completing this phase, pause for manual verification before proceeding to Phase 3.

---

## Phase 3: Integration & Testing

### Overview

Wire the handler into the dashboard router, ensure middleware chains correctly, and finalize test coverage.

### Changes Required:

#### 1. Add inferences route to dashboard nest

**File**: `src/main.rs` — in `build_app()` function

**Intent**: Register the inferences handler at `/dashboard/inferences` so it's protected by the same auth middleware as the dashboard.

**Contract**: In the `dashboard_routes` nest (after the `dashboard()` handler for `/`), add:
```rust
.route("/inferences", get(inferences))
```

This route is protected by `require_dashboard_basic` middleware (applied to the entire `dashboard_routes` nest) and will render the inference logs table.

#### 2. Add comprehensive test suite

**File**: `src/main.rs` — in `#[cfg(test)]` block

**Intent**: Test all success paths, error paths, and edge cases.

**Contract**: Add the following tests (reuse `test_app()` helper and `ServiceExt::oneshot()` pattern):

- `test_inferences_authenticated_returns_html` — GET `/dashboard/inferences` with valid Basic auth → 200 status, body contains table or empty message
- `test_inferences_unauthenticated_returns_401` — GET `/dashboard/inferences` without auth → 401 status
- `test_inferences_empty_state` — When database has no records → body contains "No inference records yet" or similar message
- `test_inferences_filter_by_category` — GET `/dashboard/inferences?filter_category=COMPLEX_REASONING` → query filters correctly (requires test data in database or mock)
- `test_inferences_pagination_offset` — GET `/dashboard/inferences?offset=20&limit=10` → correct page of results
- `test_inferences_db_error` — Simulate database error (e.g., pool is None) → handler returns HTTP 200 with error message in template, no panic

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes all (including all new tests in Phase 3)
- `cargo test dashboard` and `cargo test inferences` passes (filtered by test name)

#### Manual Verification:

- All three routes work: `GET /dashboard` (static), `GET /dashboard/inferences` (with data), HTTP Basic auth gates both
- Filter and pagination controls work correctly
- Error handling is graceful (no crashes, friendly message)

**Implementation Note**: This phase finalizes the feature. After all tests pass and manual verification is complete, the slice is ready for PR and deployment.

---

## Testing Strategy

### Unit Tests:

- Query method (`fetch_inferences`): empty list, records with all fields populated, records with None fields, filter correctness, pagination offset/limit
- Handler parameter extraction: offset, limit, filter params are parsed correctly from URL

### Integration Tests:

- Full request path: auth → handler → query → template rendering
- HTTP status codes: 200 for success, 401 for unauthed, error message in 200 response for DB errors
- Template rendering: table rows appear for records, empty message appears when no records, optional fields render as "—"
- Filters and pagination: query params propagate to query method and affect results

### Manual Testing Steps:

1. Start the server: `PROXY_API_BEARER_TOKEN=x DASHBOARD_BASIC_USER=user DASHBOARD_BASIC_PASSWORD=pw cargo run`
2. Open `http://localhost:10000/dashboard/inferences` in browser → expect 401 / Basic auth prompt
3. Enter credentials → expect "No inferences yet" message (because S-01 hasn't run yet)
4. (Once S-01 is logging data) Refresh → expect table with inference records
5. Click a category filter → expect results to filter
6. Click pagination buttons → expect page to change
7. Without auth → expect 401 challenge
8. Verify server doesn't crash if database is unavailable

## Performance Considerations

- Query uses index on `created_at DESC` for efficient recent-record retrieval
- Limit default to 20-50 rows per page; pagination prevents unbounded result sets
- No N+1 queries (single SELECT per request)
- Template rendering happens in-memory; no file I/O per request
- No full-text search (simple LIKE filters only) for MVP

## Migration Notes

No schema changes needed. Phase 1 query method works with existing `inferences` table from F-02. If schema changes later (e.g., new columns), update query and `InferenceLog` struct accordingly.

## References

- Roadmap: `context/foundation/roadmap.md` (S-02 slot)
- PRD: `context/foundation/prd.md` (FR-006)
- F-02 plan: `context/changes/data-persistence-async-logging/plan.md` (schema, logging pattern)
- F-03 plan: `context/changes/dashboard-template-scaffold/plan.md` (template infrastructure)
- Askama docs: https://djc.github.io/askama/ (template syntax, filters)
- Axum State pattern: [src/main.rs](src/main.rs#L73-L78) (existing handler pattern)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Query Layer

#### Automated

- [ ] 1.1 `cargo build` succeeds (no missing imports or type errors)
- [ ] 1.2 Unit test `test_fetch_inferences_empty_list` passes
- [ ] 1.3 Unit test `test_fetch_inferences_with_records` passes
- [ ] 1.4 Unit test `test_fetch_inferences_filter_by_category` passes

#### Manual

- [ ] 1.5 Manual inspection confirms SQL query is syntactically valid and executes without errors

### Phase 2: Template & Handler

#### Automated

- [ ] 2.1 `cargo build` succeeds (template macro generates no errors)
- [ ] 2.2 `cargo test test_inferences_authenticated_returns_html` passes
- [ ] 2.3 `cargo test test_inferences_empty_state` passes
- [ ] 2.4 `cargo test test_inferences_filter_by_category` passes
- [ ] 2.5 `cargo test test_inferences_pagination` passes
- [ ] 2.6 `cargo test test_inferences_db_error` passes

#### Manual

- [ ] 2.7 Browser: `/dashboard/inferences` with auth renders table or empty message
- [ ] 2.8 Browser: Filter controls work correctly
- [ ] 2.9 Browser: Pagination works

### Phase 3: Integration & Testing

#### Automated

- [ ] 3.1 `cargo build` succeeds
- [ ] 3.2 `cargo test` passes all tests (no regressions from F-01, F-02, F-03)
- [ ] 3.3 `cargo test inferences` passes (all inferences-specific tests)

#### Manual

- [ ] 3.4 All three dashboard routes work: `/dashboard`, `/dashboard/inferences`; auth gates both
- [ ] 3.5 Error handling is graceful (no crashes, friendly messages on DB errors)
