# Dashboard MVP Rewrite Implementation Plan

## Overview

Transform the dashboard from a basic POC scaffold (F-03) into a full-featured, production-ready observability UI. This consolidation rewrite integrates the incremental dashboard slices (S-02, S-03, S-04) into a cohesive experience with proper navigation, styling, and a dedicated module architecture.

## Current State Analysis

The dashboard prior to this rewrite consisted of:
- `src/main.rs` inline template structs and handlers
- Basic `templates/base.html` with minimal styling
- Separate pages for inferences, latency, and savings with inconsistent layouts
- No navigation system (hardcoded links in base template)
- Rudimentary CSS (`static/dashboard.css`) with about 200 lines

The rewrite extracts all dashboard logic into a dedicated `src/dashboard.rs` module and significantly expands the UI/UX.

## Desired End State

A professional dashboard with:
- **Sidebar navigation** with auto-generated menu items and active state highlighting
- **Homepage** (`/dashboard`) aggregating key metrics: total requests, classification rate, savings estimate, recent activity
- **Inferences page** (`/dashboard/inferences`) with pagination, filtering by category/model, and expandable prompt snippets
- **Latency page** (`/dashboard/latency`) with configurable time window and per-intent statistics
- **Savings page** (`/dashboard/savings`) showing cost comparison vs baseline model
- **Theme toggle** (dark/light) with localStorage persistence
- **Consistent error handling** with user-friendly banners
- **Status indicators** showing gateway and database connection state

## Implementation Approach

The rewrite is executed in three phases:

### Phase 1: Module Extraction & Navigation Infrastructure

Extract dashboard code from `main.rs` into new `src/dashboard.rs` module. Define:
- `NavPage`, `NavItem`, `NavContext` structs
- `PAGES` static registry and `nav_for()` helper
- `dashboard_page!` macro for template struct generation
- Four template struct definitions (DashboardIndex, InferencesTemplate, LatencyTemplate, SavingsTemplate)
- Handler functions for all four routes
- `routes()` builder that installs routes with dashboard auth layer

**Files touched:**
- `src/main.rs` — remove dashboard handlers/structs, import `dashboard::routes`, use it in `build_app()`
- `src/dashboard.rs` — new file (314 lines)
- `src/modules.rs` — add `pub mod dashboard;`

### Phase 2: Template & CSS Overhaul

Update Askama templates to use new navigation system and extended data:

**templates/base.html**
- Remove hardcoded nav links; use `{% for item in nav.pages %}` loop
- Add sidebar structure with logo, navigation, theme toggle button
- Add theme switcher JavaScript (toggleTheme, localStorage)

**templates/dashboard/index.html**
- Extend to include status bar, quick stats cards, latency summary table, recent activity table
- Use new data fields: `summary`, `savings`, `recent`, `db_connected`, `classifier_active`, `baseline_model`

**templates/dashboard/inferences.html**
- Add pagination controls, filter UI (dropdowns for category/model)
- Show error banner when database unavailable

**templates/dashboard/latency.html**
- Add time window selector (hours parameter)
- Show hours in UI, allow adjustment via links

**templates/dashboard/savings.html**
- Display baseline model name and savings estimate in friendly format

**static/dashboard.css** (new/expanded)
- Complete responsive design with CSS variables for theming
- Sidebar, cards, tables, badges, status indicators, pagination, filters, empty states
- Dark mode color scheme
- Approx. 570 lines total

### Phase 3: Integration & Testing

Wire everything together and verify:

1. **Route registration** in `main.rs` uses `dashboard::routes(auth_config)` instead of inline router builder
2. **Auth** remains unchanged; `require_dashboard_basic` layer still applied
3. **State access** — handlers use `State<Arc<AppState>>` to reach `persistence` and `classifier`
4. **Parallel queries** on homepage using `tokio::join!` for performance
5. **Error handling** — all handlers return `Option`/`Result` fields mapped to `error: Option<String>`
6. **Pagination safety** — limit capped at 100, page calculation uses `saturating_add`
7. **Tests** — update existing integration tests to reflect new HTML structure and content

**Test updates:**
- `test_dashboard_authenticated_returns_html` — check for sidebar, theme toggle, status bar
- `test_inferences_authenticated_returns_html` — check for pagination, filters
- `test_latency_authenticated_returns_html` — check for hours selector
- `test_savings_authenticated_returns_html` — check for baseline model display
- Add tests for unauthenticated access (401) on all routes

## Critical Implementation Details

**askama_web integration:** Use `#[derive(Template, WebTemplate)]` with `use askama_web::WebTemplate` to get Axum 0.8 `IntoResponse` support.

**Macro hygiene:** The `dashboard_page!` macro expands to a struct with `nav: NavContext`, `error: Option<String>`, plus user fields. All templates must extend `base.html` and fill the `content` block.

**Navigation auto-generation:** `PAGES` registry is the single source of truth; sidebar in `base.html` iterates it. Adding a new page requires:
1. Create template file
2. Define struct with `dashboard_page!`
3. Add `NavPage` entry to `PAGES`
4. Add route to `routes()`

**Theme persistence:** Theme is read from localStorage on page load; toggle button switches `data-theme` attribute on `<html>` and saves to localStorage.

**Graceful degradation:** Handlers check `state.persistence` and `state.classifier` and return empty data with appropriate error messages rather than failing when DB is misconfigured.

## Testing Strategy

### Unit Tests

None required — Askama compile-time validation covers templates; runtime errors are impossible with concrete structs.

### Integration Tests

Full path coverage for each route:
- Authenticated → 200 + HTML content checks
- Unauthenticated → 401 with `WWW-Authenticate: Basic` challenge
- Invalid query params (non-numeric offset/limit/hours) → 200 with clamped defaults
- Database unavailable → 200 with error banner in response

### Manual Verification

1. Start server with `DASHBOARD_BASIC_USER`/`DASHBOARD_BASIC_PASSWORD` set
2. Navigate to `/dashboard` → expect sidebar, status bar, homepage content
3. Click each nav item → verify page loads and active state highlights
4. Toggle theme → page switches between light/dark, refreshes persist
5. Navigate to `/dashboard/inferences?offset=10&limit=5` → pagination works
6. Apply filters → table updates (backend supports category/model filters)
7. Navigate to `/dashboard/latency?hours=48` → hours selector shows 48
8. Verify `/dashboard/savings` shows baseline model and savings value

## Performance Considerations

- Homepage uses `tokio::join!` to run three DB queries concurrently → response time = max(query time), not sum
- Pagination limits prevent unbounded result sets (max 100 per page)
- CSS is ~570 lines but served as single static file; browser caches aggressively
- No client-side JS framework overhead; only ~100 lines of vanilla JS for theme toggle

## Migration Notes

This is a **breaking change only for deployed instances** if they have customized the old dashboard UI. The routes remain the same (`/dashboard`, `/dashboard/inferences`, `/dashboard/latency`, `/dashboard/savings`) and auth requirements unchanged. The HTML structure and CSS classes are completely different — any external tools scraping the dashboard will need updates.

For local development: simply pull the change and restart Cerebrum. Templates compile at build time; no database schema changes.

## Rollback Plan

If critical issues emerge:
1. Revert `src/dashboard.rs` → restore handlers/structs in `src/main.rs`
2. Restore old `templates/` files from Git history
3. Revert `static/dashboard.css` to previous version
4. Update `main.rs` to use old inline router builder

The old implementation is preserved in commit `f19fc07^` (parent of rewrite).

## References

- **Commit:** f19fc07 — "Dashboard rewrite"
- **Changed files:** `src/dashboard.rs` (new), `src/main.rs` (refactor), `templates/` (rewritten), `static/dashboard.css` (expanded)
- **Roadmap:** S-05 — Dashboard MVP rewrite

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands.

- [x] Phase 1: Module extraction and navigation infrastructure — f19fc07
- [x] Phase 2: Template and CSS overhaul — f19fc07
- [x] Phase 3: Integration and testing — f19fc07
- [x] Manual verification: all routes render correctly with auth — f19fc07
