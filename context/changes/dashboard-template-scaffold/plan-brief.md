# Dashboard Template Scaffold — Plan Brief

> Full plan: `context/changes/dashboard-template-scaffold/plan.md`

## What & Why

F-03 wires Askama server-side HTML templating into the Axum dashboard route. The current
`dashboard_placeholder` handler returns a raw string literal; this change replaces it with
a compile-time-validated template pipeline using Askama's base/child inheritance model.
Without this scaffold, S-02 (inference log inspection) and S-03 (latency summaries) have
nowhere to render their data — F-03 is the prerequisite layout layer.

## Starting Point

An Axum router at `src/main.rs` exposes `/dashboard/` protected by HTTP Basic auth
middleware wired in F-01. The handler returns `Html("<h1>Dashboard route is protected</h1>")`.
No `templates/` directory exists; `askama` and `askama_axum` are absent from `Cargo.toml`.

## Desired End State

`GET /dashboard/` with valid Basic auth returns a properly structured HTML page titled
"Cerebrum Dashboard" with a "coming soon" placeholder body. The response is produced by
an Askama template struct, not a string literal. A `templates/base.html` layout and
`templates/dashboard/index.html` child template are in place, ready for S-02 to extend
with real inference data.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Template scope | Titled page with "coming soon" body | Proves the full template pipeline (layout, title, body) works and gives S-02 a real shell to populate | Plan |
| Template layout | Base + child inheritance | Avoids duplicating the HTML shell across every future dashboard page; matches standard Askama architecture | Plan |
| Error handling model | Compile-time panic, not runtime 500 | Askama validates templates at build time — runtime errors are structurally impossible; no dead error-handling code needed | Plan |
| Test coverage | Integration test: auth'd GET → 200 + text/html + body substring | Follows existing `test_app()` pattern; verifies the full wiring, not just the auth gate | Plan |

## Scope

**In scope:**
- `askama` + `askama_web` (axum-0.8 feature) dependency addition to `Cargo.toml`
- `templates/base.html` — HTML shell with a named `content` block
- `templates/dashboard/index.html` — child template with placeholder heading + paragraph
- `DashboardIndex` struct with `#[derive(Template)]` in `src/main.rs`
- Handler replacement: `dashboard_placeholder` → `dashboard`
- One integration test: authenticated request returns 200 + HTML body

**Out of scope:**
- Querying or rendering inference records (S-02)
- Latency summaries or metrics (S-03)
- CSS, JavaScript, or client-side tooling
- Navigation router or multiple dashboard sub-pages

## Architecture / Approach

Askama compiles templates from a `templates/` directory at crate root into Rust code at
build time. The handler instantiates a zero-field `DashboardIndex` struct; the `#[derive(Template)]`
macro generates `render()` and (via `askama_web`'s `#[derive(WebTemplate)]`) `IntoResponse`.
Auth stays entirely in the existing `require_dashboard_basic` middleware.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Dependency + Templates | Cargo.toml updated, template files created, `cargo build` clean | ✅ Complete — `askama_web` 0.16.0 with axum-0.8 feature confirmed compatible |
| 2. Handler + Tests | Stub replaced with Askama handler, integration test green | None — template content is compile-time checked |

**Prerequisites:** F-01 complete (dashboard auth middleware already in place)
**Estimated effort:** ~1 session; straightforward scaffolding across 2 focused phases

## Open Risks & Assumptions

- `askama_web` 0.16.0 with `axum-0.8` feature is confirmed compatible (resolved in Phase 1)
- Template directory path (`templates/`) is Askama's default; no custom configuration needed
  unless `Cargo.toml` or `build.rs` overrides it

## Success Criteria (Summary)

- `cargo test` passes, including a new test verifying authenticated dashboard renders HTML
- Browser visit to `/dashboard/` with valid credentials shows "Cerebrum Dashboard" page
- No regressions to existing auth or proxy tests
