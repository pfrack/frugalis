# Repository Guidelines

Cerebrum is a Rust/Axum gateway service that provides intent-aware request routing for autonomous agent workflows. It runs on Render, deploys automatically on merge to main, and exposes a health endpoint, proxy routes with bearer-token auth, and a basic-auth dashboard.

## Required Setup Before Testing or Deploying

Set these environment variables: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD` (all non-empty), and `PORT` (defaults to `10000`). The CI pipeline enforces auth tests before building; any failure blocks the Render webhook.

## Build, Test, and Development

- `cargo build --release` — compile optimized binary for deployment
- `cargo test auth` — run authentication validation tests
- `cargo test routes_auth` — run route authorization tests
- `cargo test` — run all tests
- `RUST_LOG=info cargo run` — run locally with logging

Tests are co-located in source modules using `#[cfg(test)]` blocks (see [src/main.rs](src/main.rs) and [src/auth.rs](src/auth.rs) for examples).

## Naming & File Layout

Source files under `src/`:
- `main.rs` — Axum router setup, route definitions, health endpoint, proxy handlers, test harness
- `auth.rs` — `AuthConfig` struct, middleware implementations (`require_proxy_bearer`, `require_dashboard_basic`), token/credential validation, utility helpers
- `persistence.rs` — `PersistenceConfig` (pool + bounded task semaphore), `InferenceRecord`, async logging API (`log_inference`), snippet extraction. A separate module is justified: persistence is a distinct cross-cutting concern with its own lifecycle, retry policy, and DB driver dependency.
- `dashboard.rs` — Dashboard page registry (`PAGES`), `dashboard_page!` macro, template structs, handler functions, and `routes()` builder for the `/dashboard/*` sub-router
- `intent_classificator.rs` — Intent classification logic, regex patterns, model cost configuration

Add new authentication schemes or routes to existing modules rather than creating separate files. Keep middleware functions near the config they read.

### Dashboard Pages & Auto-Nav

Dashboard pages are registered in `src/dashboard.rs` via a static registry and a macro.

**`PAGES`** (`src/dashboard.rs:37-42`) — the single source of truth for the sidebar navigation. Each entry has `path`, `label`, and inline SVG `icon`. To add a page, add one `NavPage` entry here.

**`dashboard_page!` macro** (`src/dashboard.rs:55-68`) — generates the Askama template struct with `nav: NavContext` and `error: Option<String>` pre-populated. Usage:
```rust
dashboard_page! {
    struct MyPageTemplate for "dashboard/my-page.html" {
        records: Vec<SomeType>,
        count: u32,
    }
}
```
The generated struct has `#[derive(Template, WebTemplate)]` and the correct `#[template(path = ...)]` attribute.

**Nav auto-generation** — `templates/base.html` renders the entire sidebar by iterating `nav.pages` (a `Vec<NavItem>`), using `item.active` to highlight the current page. Each child template only provides `{% block content %}` — the nav block was removed from all dashboard templates.

**Adding a new dashboard page requires:**
1. Create `templates/dashboard/{name}.html` (extends `base.html`, only `{% block content %}`)
2. Add a `NavPage` entry to `PAGES` in `src/dashboard.rs`
3. Define template struct with `dashboard_page!` macro
4. Write the handler function (query DB, build struct with `nav_for("name")`)
5. Add `.route("/name", get(name_handler))` in the `routes()` function

Template structs and handlers live in `dashboard.rs`, not in `main.rs`.

## Coding Conventions

- Use **constant-time comparison** for all security-sensitive string matching (imported from `subtle` crate; see `constant_time_eq_str` in [src/auth.rs](src/auth.rs))
- Pass `Arc<AuthConfig>` via Axum state to middleware; never hardcode secrets
- Middleware receives `State(config): State<Arc<AuthConfig>>`, `headers: HeaderMap`, `request: Request<Body>`, and `next: Next`
- Tests construct `AuthConfig::from_values()` with plaintext test credentials; production uses `AuthConfig::from_env()`

## Testing This Module

Write tests inline with the code they test. Follow the pattern in [src/main.rs](src/main.rs): use `#[tokio::test]` for async tests, construct a test app via `test_app()`, and make requests via `Request::builder()`. Test names follow `test_<route_or_component>_<case>`.

## Deployment & CI

Every push to main triggers the pipeline: check out, install Rust, run tests, build release, and trigger Render webhook. Test failure blocks deployment; missing webhook secret halts the deploy step with an explicit error.

## Secrets & Sensitive Configuration

Store all secrets in GitHub Actions or Render environment variables, never in source. Port and logging level are runtime-configured via env vars; see @render.yaml for the deployment contract.