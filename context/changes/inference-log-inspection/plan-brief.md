# Inference Log Inspection — Plan Brief

> Full plan: `context/changes/inference-log-inspection/plan.md`
> Roadmap: `context/foundation/roadmap.md` (S-02 slot)

## What & Why

Dashboard table view for recent inference records: operator can browse prompt snippets, intent categories, upstream models, and request durations. This completes the observability loop begun in F-02 (async logging pipeline) and F-03 (template infrastructure) by making inference data visible and queryable. Unblocks S-03 (per-intent latency summary) and validates that S-01 (proxy routing) is logging correctly.

## Starting Point

- **F-02 (Data persistence)** impl-reviewed: PostgreSQL `inferences` table exists with schema; `PersistenceConfig` and async logging task are wired into the application
- **F-03 (Dashboard templates)** complete: Askama templates + HTTP Basic auth middleware in place; `/dashboard` route renders static placeholder
- **Current state**: No database queries yet; dashboard is static HTML only; handler pattern exists but not context-bound templates

## Desired End State

A `/dashboard/inferences` endpoint that:
- Fetches recent inferences from PostgreSQL with offset/limit pagination
- Renders a table with: timestamp, prompt snippet (200 chars), intent category, duration, expandable row for upstream_model
- Supports filtering by category and model via URL query params
- Shows friendly empty-state message if no records exist
- Handles database errors gracefully (renders error message, no HTTP 500)
- Includes pagination controls and a "Refresh" button
- Authenticates via HTTP Basic auth (reuses existing middleware)

Verification: automated tests for auth gate, happy path, empty state, filtering, pagination, and error handling; manual browser testing for UX.

## Key Decisions Made

| Decision | Choice | Why | Source |
|----------|--------|-----|--------|
| Pagination | Offset/limit (not cursor or fixed window) | Allows browsing older records; simple SQL with standard navigation UI | Plan |
| Snippet display | 200 chars + click-to-expand tooltip | Balances table readability with available context; tooltip shows full snippet | Plan |
| Table columns | Core fields (timestamp, snippet, category, duration) + expandable detail | Keeps table width reasonable; operator can expand rows to see model | Plan |
| Empty state | Friendly message ("No inferences yet...") | Acknowledges S-01 isn't running; clear next step | Plan |
| Error handling | Graceful message in template (HTTP 200, not 500) | Keeps dashboard responsive; operator sees problem without crashes | Plan |
| Filtering | Optional filters by category and model | Enables tuning inspection; supports S-03 aggregation by category | Plan |
| Refresh | Manual "Refresh" button (no auto-polling) | Simple, server-side only; no JavaScript or WebSocket needed for MVP | Plan |

## Scope

**In scope:**
- Query method for recent inferences (with pagination, optional filters)
- Template struct + Askama template for table rendering
- Handler that fetches data and returns template
- Router integration (wire into `/dashboard` nest)
- Test coverage (6+ test scenarios)

**Out of scope:**
- Real-time auto-refresh (manual refresh only)
- Full-text search in prompts (offset/limit pagination on recent records sufficient)
- Export to CSV/JSON
- Cost calculation or savings metric (S-04, parked)
- Per-intent latency summaries (S-03, depends on this query infrastructure)

## Architecture / Approach

**Three layers:**
1. **Query layer** (`src/persistence.rs`): Add `fetch_inferences()` method to `PersistenceConfig`. Accepts pagination (offset, limit) and optional filters (category, model). Returns `Vec<InferenceLog>` with formatted fields (timestamp as String, duration as "N ms"). Uses `sqlx::query()` with parameterized WHERE clauses.

2. **Template + Handler** (`src/main.rs` + `templates/dashboard/inferences.html`): Create `InferencesTemplate` struct with context fields (records, page, error, filters). Write handler that extracts state + URL params, calls query method, returns template. On error: pass error message to template (HTTP 200, not 500).

3. **Integration** (`src/main.rs` router): Wire handler at `/dashboard/inferences`, protected by existing `require_dashboard_basic` middleware. All three dashboard routes (`/dashboard`, `/dashboard/inferences`) authenticate the same way.

**Data flow:**
```
HTTP GET /dashboard/inferences?offset=0&limit=20&filter_category=COMPLEX_REASONING
  ↓ auth middleware (require_dashboard_basic)
  ↓ handler extracts params
  ↓ handler calls state.persistence.fetch_inferences(0, 20, Some("COMPLEX_REASONING"), None)
  ↓ query executes: SELECT * FROM inferences WHERE category = $1 ORDER BY created_at DESC LIMIT 20 OFFSET 0
  ↓ handler returns InferencesTemplate { records: [...], page: 0, error: None, ... }
  ↓ Askama renders template
  ↓ HTTP 200 with HTML table
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|------------------|----------|
| 1. Query Layer | `fetch_inferences()` method; handles pagination + filtering | SQL correctness; parameterized queries must be safe |
| 2. Template & Handler | `InferencesTemplate` struct, template file, handler that wires them | Template compile-time error if struct fields don't match template usage |
| 3. Integration & Testing | Router integration, comprehensive test suite (6+ tests) | Test coverage gaps; missing edge cases |

**Prerequisites:** F-02 (inferences table exists), F-03 (template infrastructure), Axum + sqlx + Askama already in dependencies
**Estimated effort:** ~3-4 sessions across 3 phases (research + implementation + manual testing)

## Open Risks & Assumptions

- **S-01 dependency**: Plan assumes S-01 (proxy routing) will start logging inferences soon after this is merged. Until then, table will be empty (but infrastructure is correct).
- **Database error patterns**: Assumes database errors are transient or operator-visible (connection timeout, quota exceeded). No retry logic; single attempt per request.
- **Template context complexity**: First context-bound template struct in codebase (DashboardIndex is zero-field). Pattern is straightforward but new—watch for template macro errors.
- **Pagination scale**: Assumes query load will be low (last 100 records, < 100 req/sec). If traffic grows, may need query optimization (caching, indexes, etc.).

## Success Criteria (Summary)

- Automated: All 6+ tests pass; `cargo build` and `cargo test` clean
- Manual: `/dashboard/inferences` loads with auth, shows empty state initially, renders table once S-01 logs data, filters and pagination work, error handling is graceful
- Integration: Coexists with F-01 (auth), F-02 (data pipeline), F-03 (templates) with no regressions
