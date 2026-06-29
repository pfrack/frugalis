# Repository Guidelines

Frugalis is a Rust/Axum gateway service that provides intent-aware request routing for autonomous agent workflows. It runs on Render, deploys automatically on merge to main, and exposes a health endpoint, proxy routes with bearer-token auth, and a basic-auth dashboard.

## Required Setup Before Testing or Deploying

Set these environment variables: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD` (all non-empty). Server port, log level, and other operational settings come from `config.toml` (or `CONFIG_PATH` overlay). The CI pipeline enforces auth tests before building; any failure blocks the Render webhook.

## Build, Test, and Development

- `cargo build --release` — compile optimized binary for deployment
- `cargo test auth` — run authentication validation tests
- `cargo test routes_auth` — run route authorization tests
- `cargo test` — run all fast unit/integration tests (excludes slow tests)
- `cargo test slow_tests` — run slow tests (e.g., keepalive with real delays)
- `RUST_LOG=info cargo run` — run locally with logging

Tests are organized in two groups in `src/main.rs`: `mod tests` (fast unit/integration tests, run with `cargo test`) and `mod slow_tests` (tests requiring delays or slow mocks, run with `cargo test slow_tests`). Keepalive interval is configurable via config.toml under `[http]` (default: 15s).

## Naming & File Layout

Source files under `src/`:
- `main.rs` — Axum router setup, route definitions, health endpoint, proxy handlers, test harness
- `app/` — Composition root: `mod.rs` (AppState, build_app, build_classifiers, build_persistence), `cli.rs` (CLI arg parsing, init), `quickstart.rs` (interactive setup wizard), `test_helpers.rs` (test utility functions, cfg(test)-gated)
- `auth.rs` — `AuthConfig` struct, middleware implementations (`require_proxy_bearer`, `require_dashboard_basic`), token/credential validation, utility helpers
- `persistence.rs` — `PersistenceConfig` (pool + bounded task semaphore), `InferenceRecord`, async logging API (`log_inference`), snippet extraction. A separate module is justified: persistence is a distinct cross-cutting concern with its own lifecycle, retry policy, and DB driver dependency.
- `dashboard/` — Dashboard sub-module: `nav.rs` (page registry `PAGES`, nav types, `nav_for()`), `templates.rs` (`dashboard_page!` macro, template structs), `handlers.rs` (handler functions, tests), `mod.rs` (`routes()` builder, re-exports)
- `intent_classifier.rs` — Intent classification logic, regex patterns, model cost configuration

Add new authentication schemes or routes to existing modules rather than creating separate files. Keep middleware functions near the config they read.

### Dashboard Pages & Auto-Nav

Dashboard pages are registered in `src/dashboard/nav.rs` via a static registry and a macro.

**`PAGES`** (`src/dashboard/nav.rs`) — the single source of truth for the sidebar navigation. Each entry has `path`, `label`, and inline SVG `icon`. To add a page, add one `NavPage` entry here.

**`dashboard_page!` macro** (`src/dashboard/templates.rs`) — generates the Askama template struct with `nav: NavContext` and `error: Option<String>` pre-populated. Usage:
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
2. Add a `NavPage` entry to `PAGES` in `src/dashboard/nav.rs`
3. Define template struct with `dashboard_page!` macro in `src/dashboard/templates.rs`
4. Write the handler function in `src/dashboard/handlers.rs` (query DB, build struct with `nav_for("name")`)
5. Add `.route("/name", get(name_handler))` in the `routes()` function in `src/dashboard/mod.rs`

Template structs live in `dashboard/templates.rs`, handlers in `dashboard/handlers.rs`, nav registry in `dashboard/nav.rs`, `routes()` builder in `dashboard/mod.rs` — not in `main.rs`.

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

Store all secrets in GitHub Actions or Render environment variables, never in source. Port and logging level are runtime-configured via config.toml; see @render.yaml for the deployment contract.