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

**Phase 1: Query layer** — Add a method to `PersistenceConfig` that fetches recent inferences with optional pagination (offset, limit) and filters (category, model). Returns both a `Vec<InferenceLog>` and total record count for pagination metadata.

**Phase 2: Template & handler** — Create a new `InferencesTemplate` struct with fields (`records: Vec<InferenceLog>`, `page: u32`, `total_pages: u32`, `error: Option<String>`, etc.). Create `templates/dashboard/inferences.html` with Askama loops, conditionals for empty state, optional fields for category/model, and truncation filter for snippet. Write handler that extracts state, validates URL parameters, calls the query method, and returns the template (or error message).

**Phase 3: Integration & testing** — Wire the handler into the dashboard router; add comprehensive tests for auth gate, happy path (records render), empty state, filtering, pagination, invalid parameters, and database error handling.

## Critical Implementation Details

**Pagination contract fix:** The handler needs total record count to compute pagination metadata (`total_pages`). Phase 1 query method must return both the record vec AND total_count (via tuple or struct), allowing the handler to compute `total_pages = (total_count + limit - 1) / limit` without a second query. This prevents N+1 queries.

**Error handling pattern:** Unlike existing background tasks (which silently swallow errors), the inferences handler should return a `Result` type and gracefully pass errors to the template. This is a new pattern for the codebase. Handler should not panic; if the database query fails, catch the error, pass an error message to the template, and return HTTP 200 with the error block rendered (not HTTP 500). This keeps the dashboard responsive and prevents crashes.

**Custom error type:** Define a `QueryError` enum with variants for different failure modes (database vs. invalid filter), allowing type-safe error handling in the handler and more meaningful error messages.

**Parameter validation:** URL parameters (offset, limit) must be validated and safely parsed. Invalid params (non-numeric, out-of-range) should be silently replaced with defaults, not cause errors or crashes.

**Template context struct:** `InferencesTemplate` will have fields for `records`, `page`, `total_pages`, `error: Option<String>`, and query parameters like `filter_category: Option<String>`. This is the first context-bound template struct in the codebase (DashboardIndex is zero-field static content). Pass data from handler to template via struct fields, not query string parameters.

**Timestamp formatting:** Database stores `created_at` as `TIMESTAMPTZ`; Rust handler should format as a human-readable string (e.g., "2026-06-01 14:30:45 UTC") before passing to template. Askama has no datetime formatting filters; compute in Rust.

---

## Phase 1: Query Layer

### Overview

Add a custom error type and a public query method to `PersistenceConfig` that fetches recent inferences from the database with pagination and optional filtering. The method returns both records and total count.

### Changes Required:

#### 1.1 Define QueryError enum

**File**: `src/persistence.rs`

**Intent**: Define a custom error type for query failures, allowing the handler to distinguish database errors from invalid filter values.

**Contract**: Add an enum named `QueryError`:
```rust
#[derive(Debug, Clone)]
pub enum QueryError {
    Database(String),      // Connection, query, or pool error
    InvalidFilter(String), // Invalid filter value
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Database(msg) => write!(f, "Database error: {}", msg),
            Self::InvalidFilter(msg) => write!(f, "Invalid filter: {}", msg),
        }
    }
}
```

Use `QueryError::Database(e.to_string())` for DB errors and `QueryError::InvalidFilter(msg)` for filter validation.

#### 1.2 Define InferenceLog struct

**File**: `src/persistence.rs`

**Intent**: Define a struct that represents one row from the `inferences` table, with all fields needed for the dashboard, pre-formatted for display.

**Contract**: Add a struct named `InferenceLog` with fields:
- `id: String` (UUID formatted as string)
- `timestamp: String` (formatted "2026-06-01 14:30:45 UTC" or similar)
- `prompt_snippet: String`
- `category: Option<String>` (None renders as "—" in template)
- `upstream_model: Option<String>` (None renders as "—")
- `duration_ms: Option<i32>` (formatted as "42 ms" or None renders as "—")

#### 1.3 Implement fetch_inferences method

**File**: `src/persistence.rs`

**Intent**: Provide a method that the handler calls to fetch recent inferences. Returns both records and total count.

**Contract**: Add an async method named `fetch_inferences` with signature:
```rust
pub async fn fetch_inferences(
    &self,
    offset: u32,
    limit: u32,
    filter_category: Option<&str>,
    filter_model: Option<&str>,
) -> Result<(Vec<InferenceLog>, i64), QueryError>
```

The method:
- Builds a dynamic SQL query: `SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms FROM inferences`
- Appends `WHERE category = $X` if `filter_category` is provided
- Appends `WHERE upstream_model = $X` if `filter_model` is provided (use AND if both filters are present)
- Appends `ORDER BY created_at DESC LIMIT $limit OFFSET $offset`
- Executes via `self.pool.query(...).fetch_all(...).await`
- Converts each `PgRow` to `InferenceLog`, formatting timestamps and durations
- **Returns both records AND total count** (via a separate COUNT(*) query with same WHERE filters, or single query with COUNT(*) OVER())
- Returns `Ok((records, total_count))` on success, `Err(QueryError::Database(...))` on failure

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- Unit test `test_fetch_inferences_empty_list` passes (query returns empty Vec + count=0 when no records exist)
- Unit test `test_fetch_inferences_with_records` passes (query returns records with correct fields and accurate count)
- Unit test `test_fetch_inferences_filter_by_category` passes (WHERE clause filters correctly)
- Unit test `test_fetch_inferences_returns_total_count` passes (total_count in return tuple is correct)

#### Manual Verification:

- `sqlx prepare` or manual inspection confirms query is syntactically valid
- Query executes without timeouts or connection errors

**Implementation Note**: After completing this phase, pause for code review before proceeding to Phase 2. Ensure query is correct, pagination contract is sound, and error handling is solid.

---

## Phase 2: Template & Handler

### Overview

Create the dashboard view template (`templates/dashboard/inferences.html`) and a handler (`inferences()`) that fetches data and renders it. Wire state, query results, and error handling into the template.

### Changes Required:

#### 2.1 Create InferencesTemplate struct

**File**: `src/main.rs`

**Intent**: Define a template context struct that holds query results and metadata for the template to render.

**Contract**: Add a struct named `InferencesTemplate` with `#[derive(Template, WebTemplate)]`:
```rust
#[derive(Template, WebTemplate)]
#[template(path = "dashboard/inferences.html")]
pub struct InferencesTemplate {
    pub records: Vec<InferenceLog>,
    pub page: u32,
    pub total_pages: u32,
    pub error: Option<String>,
    pub filter_category: Option<String>,
    pub filter_model: Option<String>,
}
```

#### 2.2 Create inferences.html template

**File**: `templates/dashboard/inferences.html` *(new file)*

**Intent**: Render the inference logs table with pagination, filters, and empty state.

**Contract**: Template must:
- Extend `base.html` and fill the `content` block
- Use `{% if let Some(error) = error %}<div class="error">{{ error | escape }}</div>{% endif %}` to display errors at the top
- Use `{% if records.is_empty() %}<p>No inference records yet...</p>{% else %}<table>...</table>{% endif %}` for empty state
- Inside `<table>`: loop over `records` with `{% for record in records %}<tr>...{{ record.timestamp }}...{{ record.prompt_snippet | truncate(200) }}...{% endfor %}`
- Use `{% if let Some(cat) = category %}{{ cat }}{% else %}—{% endif %}` for optional fields
- Include a filter form with input fields for category and model (submit reloads page with query params)
- Include a "Refresh" button that reloads the page
- Include pagination links (prev/next) that adjust offset in URL based on `page` and `total_pages`
- Use semantic HTML (`<table>`, `<thead>`, `<tbody>`, `<button>`) for accessibility

#### 2.3 Create inferences handler

**File**: `src/main.rs`

**Intent**: Extract auth state and query parameters, call the Phase 1 query method, and return the template.

**Contract**: Define an async handler named `inferences` with signature:
```rust
async fn inferences(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse
```

The handler:

1. **Parse and validate URL params** with safe defaults:
   ```rust
   let offset = params.get("offset")
       .and_then(|o| o.parse::<u32>().ok())
       .unwrap_or(0);
   let limit = params.get("limit")
       .and_then(|l| l.parse::<u32>().ok())
       .map(|l| l.min(100))  // clamp limit to max 100
       .unwrap_or(20);
   let filter_category = params.get("filter_category").map(|c| c.as_str());
   let filter_model = params.get("filter_model").map(|m| m.as_str());
   ```
   - If parsing fails, silently use defaults (no error)

2. **Fetch inferences**:
   - If `state.persistence` is None: return error template with message "Database not configured"
   - Call `state.persistence.fetch_inferences(offset, limit, filter_category, filter_model).await`

3. **On success** (returns `(records: Vec<InferenceLog>, total_count: i64)`):
   - Compute `page = offset / limit` (zero-indexed)
   - Compute `total_pages = (total_count + limit - 1) / limit` (ceiling division)
   - Return `InferencesTemplate { records, page, total_pages, error: None, filter_category, filter_model }`
   - HTTP 200

4. **On error** (returns `QueryError`):
   - Extract error message from QueryError
   - Return `InferencesTemplate { records: vec![], page: 0, total_pages: 0, error: Some(error.to_string()), filter_category, filter_model }`
   - HTTP 200 (not 500) — render error in template instead

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds (template macro generates InferencesTemplate correctly)
- `cargo test test_inferences_authenticated_returns_html` passes (request with auth header returns 200, body contains table header or "No inference records yet")
- `cargo test test_inferences_empty_state` passes (when no records exist, body contains friendly message)
- `cargo test test_inferences_filter_by_category` passes (filter query param is passed to query method, results filter correctly)
- `cargo test test_inferences_pagination_offset` passes (offset/limit params are parsed correctly and pagination metadata is accurate)
- `cargo test test_inferences_invalid_params` passes (offset="abc" and limit=999999 are handled gracefully, defaults applied) **NEW**
- `cargo test test_inferences_db_error` passes (database error is caught, error message renders in template, returns HTTP 200 not 500)

#### Manual Verification:

- Browser with auth credentials: navigate to `/dashboard/inferences` → table renders (if S-01 is logging data) or empty state message appears
- Click filter input and submit → page reloads with filtered results
- Try invalid params in URL (e.g., `/dashboard/inferences?offset=abc&limit=999`) → page renders with defaults, no errors
- Click pagination buttons → offset adjusts in URL and correct page displays
- Without auth: HTTP 401 challenge

**Implementation Note**: After completing this phase, pause for manual verification before proceeding to Phase 3.

---

## Phase 3: Integration & Testing

### Overview

Wire the handler into the dashboard router, ensure middleware chains correctly, and finalize test coverage.

### Changes Required:

#### 3.1 Add inferences route to dashboard nest

**File**: `src/main.rs` — in `build_app()` function

**Intent**: Register the inferences handler at `/dashboard/inferences` so it's protected by the same auth middleware as the dashboard.

**Contract**: In the `dashboard_routes` nest (after the `dashboard()` handler for `/`), add:
```rust
.route("/inferences", get(inferences))
```

This route is protected by `require_dashboard_basic` middleware (applied to the entire `dashboard_routes` nest) and will render the inference logs table.

#### 3.2 Add comprehensive test suite

**File**: `src/main.rs` — in `#[cfg(test)]` block

**Intent**: Test all success paths, error paths, and edge cases.

**Contract**: Add the following tests (reuse `test_app()` helper and `ServiceExt::oneshot()` pattern):

- `test_inferences_authenticated_returns_html` — GET `/dashboard/inferences` with valid Basic auth → 200 status, body contains table header or "No inference records yet"
- `test_inferences_unauthenticated_returns_401` — GET `/dashboard/inferences` without auth → 401 status
- `test_inferences_empty_state` — When database has no records → body contains "No inference records yet" or similar message
- `test_inferences_filter_by_category` — GET `/dashboard/inferences?filter_category=COMPLEX_REASONING` → query filters correctly (requires test data in database or mock)
- `test_inferences_pagination_offset` — GET `/dashboard/inferences?offset=20&limit=10` → correct page of results
- `test_inferences_invalid_params` — GET `/dashboard/inferences?offset=abc&limit=999999` → returns 200 with defaults applied, no errors
- `test_inferences_db_error` — Simulate database error (e.g., pool is None) → handler returns HTTP 200 with error message in template, no panic

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes all (including all new tests in Phase 3)
- `cargo test inferences` passes (filtered by test name)

#### Manual Verification:

- All three routes work: `GET /dashboard` (static), `GET /dashboard/inferences` (with data), HTTP Basic auth gates both
- Filter and pagination controls work correctly
- Error handling is graceful (no crashes, friendly message)

**Implementation Note**: This phase finalizes the feature. After all tests pass and manual verification is complete, the slice is ready for PR and deployment.

---

## Testing Strategy

### Unit Tests:

- Query method (`fetch_inferences`): empty list with correct count, records with all fields populated, records with None fields, filter correctness, pagination offset/limit, total_count accuracy
- Handler parameter extraction: offset, limit, filter params are parsed correctly from URL, invalid params use defaults
- Handler error handling: database error is caught and error message passes to template

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
5. Test invalid params: `http://localhost:10000/dashboard/inferences?offset=abc&limit=999999` → expect page with defaults
6. Click a category filter → expect results to filter
7. Click pagination buttons → expect page to change
8. Without auth → expect 401 challenge
9. Verify server doesn't crash if database is unavailable

## Performance Considerations

- Query uses index on `created_at DESC` for efficient recent-record retrieval
- Limit default to 20 rows per page; pagination prevents unbounded result sets
- **Single COUNT query** (not N+1) — both records and total_count fetched in Phase 1 method
- Template rendering happens in-memory; no file I/O per request
- No full-text search (simple WHERE filters only) for MVP

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

- [x] 1.1 `cargo build` succeeds with QueryError enum defined
- [x] 1.2 `cargo build` succeeds with InferenceLog struct defined
- [x] 1.3 Unit test `test_fetch_inferences_empty_list` passes
- [x] 1.4 Unit test `test_fetch_inferences_with_records` passes
- [x] 1.5 Unit test `test_fetch_inferences_filter_by_category` passes
- [x] 1.6 Unit test `test_fetch_inferences_returns_total_count` passes

#### Manual

- [ ] 1.7 Manual inspection confirms SQL query is syntactically valid and executes without errors

### Phase 2: Template & Handler

#### Automated

- [ ] 2.1 `cargo build` succeeds (template macro generates no errors)
- [ ] 2.2 `cargo test test_inferences_authenticated_returns_html` passes
- [ ] 2.3 `cargo test test_inferences_empty_state` passes
- [ ] 2.4 `cargo test test_inferences_filter_by_category` passes
- [ ] 2.5 `cargo test test_inferences_pagination_offset` passes
- [ ] 2.6 `cargo test test_inferences_invalid_params` passes
- [ ] 2.7 `cargo test test_inferences_db_error` passes

#### Manual

- [ ] 2.8 Browser: `/dashboard/inferences` with auth renders table or empty message
- [ ] 2.9 Browser: Filter controls work correctly
- [ ] 2.10 Browser: Pagination works
- [ ] 2.11 Browser: Invalid params handled gracefully

### Phase 3: Integration & Testing

#### Automated

- [ ] 3.1 `cargo build` succeeds
- [ ] 3.2 `cargo test` passes all tests (no regressions from F-01, F-02, F-03)
- [ ] 3.3 `cargo test inferences` passes (all inferences-specific tests)

#### Manual

- [ ] 3.4 All three dashboard routes work: `/dashboard`, `/dashboard/inferences`; auth gates both
- [ ] 3.5 Error handling is graceful (no crashes, friendly messages on DB errors)
