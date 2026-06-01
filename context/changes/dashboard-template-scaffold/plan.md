# Dashboard Template Scaffold Implementation Plan

## Overview

Wire Askama server-side HTML templating into the Axum dashboard route. The current
`dashboard_placeholder` handler returns a raw `Html` literal; this plan replaces it
with a compile-time-checked Askama template using base/child inheritance, so S-02 and
S-03 can add real content by extending the established layout without touching handler
code or duplicating HTML boilerplate.

## Current State Analysis

The `/dashboard/` route is already registered in `main.rs` and protected by the
`require_dashboard_basic` middleware from F-01. The handler returns a static HTML string:

```rust
async fn dashboard_placeholder() -> Html<&'static str> {
    Html("<h1>Dashboard route is protected</h1>")
}
```

No `templates/` directory exists. `askama` and `askama_web` are absent from `Cargo.toml`.

> **Note (post-Phase-1):** Phase 1 landed `askama = "0.16.0"` and `askama_web = { version = "0.16.0", features = ["axum-0.8"] }` in `Cargo.toml`, and created `templates/base.html` + `templates/dashboard/index.html`.

### Key Discoveries:

- Handler lives at `src/main.rs` (inline, near `build_app`)
- Auth middleware already wired at `src/main.rs` `build_app()` — no auth changes needed
- Axum version is `0.8.9` (see `Cargo.toml:7`); `askama_axum` must be version-compatible
- Existing integration tests use `test_app()` + `ServiceExt::oneshot` pattern (`src/main.rs`)
- Askama requires a `templates/` directory at crate root (sibling of `src/`) by default

## Desired End State

`GET /dashboard/` (with valid Basic auth) returns `200 OK` with `Content-Type: text/html`
and an HTML page titled "Cerebrum Dashboard" containing a "Coming soon" body. The response
is produced by an Askama-derived struct, not a string literal. The `templates/` directory
contains a base layout and a dashboard child template ready for S-02 to extend.

Verification: automated tests confirm status code, content type, and body substring.
Manual verification: `curl -u user:password http://localhost:10000/dashboard/` renders the
page in a browser.

### Key Discoveries:

- No frontend build pipeline — Askama compiles templates at Rust build time, no Node.js needed
- `askama_web` with `features = ["axum-0.8"]` provides `IntoResponse` via `#[derive(WebTemplate)]`
- Template file path is determined by the `#[template(path = "...")]` attribute on the struct,
  relative to the `templates/` directory

## What We're NOT Doing

- Querying or displaying inference records (that is S-02)
- Adding latency summaries or metrics (that is S-03)
- CSS framework integration or custom styling beyond plain HTML
- JavaScript / client-side interactivity
- Template hot-reload in development
- Multiple dashboard sub-pages or a navigation router

## Implementation Approach

Add `askama` and `askama_web` (with `axum-0.8` feature) to `Cargo.toml`, create a
`templates/` directory with a base layout (`base.html`) and a dashboard child template
(`dashboard/index.html`). Define a zero-field `DashboardIndex` struct with
`#[derive(Template, WebTemplate)]` in `src/main.rs`. Replace the stub handler with one
that instantiates and returns the struct. Add one integration test.

## Critical Implementation Details

**Axum 0.8 / askama_web:** `askama_axum` 0.5 is deprecated. The correct integration
crate is `askama_web = { version = "0.16.0", features = ["axum-0.8"] }` (already in
`Cargo.toml` after Phase 1). Using `#[derive(Template, WebTemplate)]` with
`use askama_web::WebTemplate` is all that is required — no manual `IntoResponse` impl.

## Phase 1: Dependency, Templates, and Cargo Wiring

### Overview

Add Askama to the project's dependency graph and create the two template files
(`base.html`, `dashboard/index.html`). No Rust source changes yet — confirm
`cargo build` succeeds with the new dependencies before wiring the handler.

### Changes Required:

#### 1. Add askama dependencies to Cargo.toml

**File:** `Cargo.toml`

**Intent:** Pull in Askama and its Axum integration crate so the template engine is available
for the handler wiring in Phase 2.

**Contract:** Add two entries under `[dependencies]`:
- `askama = "0.16.0"`
- `askama_web = { version = "0.16.0", features = ["axum-0.8"] }`

> **Resolved in Phase 1.** `askama_axum` is deprecated; `askama_web` is the correct crate.

#### 2. Create base layout template

**File:** `templates/base.html` *(new file — create `templates/` directory)*

**Intent:** Define the shared HTML shell (doctype, `<html>`, `<head>`, `<body>`) that all
dashboard child templates extend. Contains one Askama block named `content` for child
templates to fill.

**Contract:** Must use Askama's inheritance syntax (`{% block content %}{% endblock %}`).
The `<title>` element should read "Cerebrum Dashboard". No inline CSS required.

#### 3. Create dashboard index child template

**File:** `templates/dashboard/index.html` *(new file — create `templates/dashboard/` directory)*

**Intent:** Minimal placeholder page extending `base.html`. Renders a heading and a one-line
"coming soon" message. S-02 will replace this body with actual inference records.

**Contract:** Must begin with `{% extends "base.html" %}` and fill the `content` block with
an `<h1>Cerebrum Dashboard</h1>` heading and a short placeholder paragraph.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with new dependencies — no version conflict errors

#### Manual Verification:

- `templates/base.html` and `templates/dashboard/index.html` exist at crate root

**Implementation Note:** After completing this phase, pause here for manual confirmation that
`cargo build` is clean before proceeding to Phase 2.

---

## Phase 2: Handler Wiring and Integration Test

### Overview

Define the `DashboardIndex` Askama template struct, replace the `dashboard_placeholder`
handler with one that returns it, and add an integration test that exercises the full
request path (auth → handler → rendered HTML).

### Changes Required:

#### 1. Define DashboardIndex template struct

**File:** `src/main.rs`

**Intent:** Introduce the zero-field struct that Askama's derive macro binds to
`templates/dashboard/index.html`. This is the type the handler returns.

**Contract:** A struct named `DashboardIndex` with `#[derive(Template, WebTemplate)]`
and `#[template(path = "dashboard/index.html")]`. Import `askama_web::WebTemplate` at
the top of `src/main.rs`. The `WebTemplate` derive (from `askama_web` with the
`axum-0.8` feature already in `Cargo.toml`) generates the `IntoResponse` impl
automatically — no manual impl needed. The handler can return `DashboardIndex {}`
directly as `impl IntoResponse`.

#### 2. Replace stub dashboard handler

**File:** `src/main.rs`

**Intent:** Swap the raw `Html` literal handler for one that instantiates `DashboardIndex`
and returns it. Return type changes from `Html<&'static str>` to `impl IntoResponse`.

**Contract:** Handler signature becomes `async fn dashboard() -> impl IntoResponse` (no
`State` needed — this phase has no data access). Body: `DashboardIndex {}`. Remove the old
`dashboard_placeholder` function.

#### 3. Update route registration

**File:** `src/main.rs` — `build_app()`

**Intent:** Point the `/` route inside the `/dashboard` nest at the new `dashboard` handler.

**Contract:** Change `.route("/", get(dashboard_placeholder))` to `.route("/", get(dashboard))`.
No other changes to `build_app`.

#### 4. Add integration test

**File:** `src/main.rs` — `#[cfg(test)]` block

**Intent:** Verify the full wiring: authenticated request reaches the handler, template renders,
and the response has the expected status code, content type, and body content.

**Contract:** Test name `test_dashboard_authenticated_returns_html`. Uses `test_app()`,
sends `GET /dashboard/` with a valid `Authorization: Basic` header. Asserts:
- `StatusCode::OK`
- `Content-Type` header starts with `text/html`
- Response body as UTF-8 string contains `"Cerebrum Dashboard"`

A companion test `test_dashboard_unauthenticated_returns_401` may already be covered by
F-01 tests; add only if not present.

### Success Criteria:

#### Automated Verification:

- `cargo build --release` succeeds (template compilation errors surface here)
- `cargo test` passes, including `test_dashboard_authenticated_returns_html`
- `cargo test auth` still passes (no regression to existing auth tests)

#### Manual Verification:

- `curl -u $DASHBOARD_BASIC_USER:$DASHBOARD_BASIC_PASSWORD http://localhost:10000/dashboard/`
  returns an HTML page with "Cerebrum Dashboard" in a browser
- `curl http://localhost:10000/dashboard/` (no credentials) returns `401`

**Implementation Note:** After completing this phase and all automated verification passes,
pause for manual browser check before marking F-03 complete.

---

## Testing Strategy

### Unit Tests:

- No unit tests required — Askama compiles templates at build time; runtime rendering errors
  are not possible with compile-time template validation.

### Integration Tests:

- `test_dashboard_authenticated_returns_html`: full path auth → handler → rendered HTML
- Auth failure path (`401`) covered by existing F-01 tests; add only if absent

### Manual Testing Steps:

1. Run `PROXY_API_BEARER_TOKEN=x DASHBOARD_BASIC_USER=user DASHBOARD_BASIC_PASSWORD=pw cargo run`
2. Open `http://localhost:10000/dashboard/` in a browser — expect a 401 / Basic auth prompt
3. Enter credentials — expect "Cerebrum Dashboard" page with "coming soon" content
4. `curl http://localhost:10000/health` — expect `200 ok` (public route unaffected)

## Performance Considerations

Askama renders templates at request time from pre-compiled Rust code; there is no file I/O
at runtime. Performance impact is negligible.

## Migration Notes

None. The `dashboard_placeholder` stub is replaced in-place; no data schema changes, no env
var additions, no Render configuration changes.

## References

- Roadmap F-03: `context/foundation/roadmap.md`
- Askama documentation: https://djc.github.io/askama/
- Existing test pattern: `src/main.rs` — `#[cfg(test)]` block, `test_app()` helper
- Auth middleware: `src/auth.rs` — `require_dashboard_basic`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Dependency, Templates, and Cargo Wiring

#### Automated

- [x] 1.1 `cargo build` succeeds with new Askama dependencies

#### Manual

- [x] 1.2 `templates/base.html` and `templates/dashboard/index.html` exist at crate root

### Phase 2: Handler Wiring and Integration Test

#### Automated

- [ ] 2.1 `cargo build --release` succeeds (template compilation clean)
- [ ] 2.2 `cargo test` passes including `test_dashboard_authenticated_returns_html`
- [ ] 2.3 `cargo test auth` passes with no regressions

#### Manual

- [ ] 2.4 Browser: authenticated `GET /dashboard/` renders "Cerebrum Dashboard" HTML page
- [ ] 2.5 `curl` without credentials returns `401`
