# Code Structure Reorganization Extension — Implementation Plan

## Overview

Extend the completed code-structure-reorg with two independent cleanup tasks: (1) update routing_examples/ to replace stale "Cerebrum" references with "Frugalis" and add a multi-provider example, and (2) deduplicate the cache test infrastructure by consolidating `test_app_with_cache()` into the shared `app::test_helpers` module.

## Current State Analysis

The original reorg (phases 1–6, archived at commit `927df08`) achieved its goals — main.rs is ~200 lines, domain code lives in subdirectories, tests are co-located. Two residual issues remain:

### Key Discoveries:

- `routing_examples/*.toml` (all 4 files) — contain "Cerebrum's merge semantics" in their header comments; should say "Frugalis"
- `src/config/routing.rs:25-41` — documents multi-provider `providers = [...]` format that no example file demonstrates
- `src/cache.rs:175-275` — `test_app_with_cache()` is a 100-line helper that duplicates 95% of `app::test_helpers::test_app_with_http_client()`, differing only by adding `response_cache: Some(...)` to AppState
- `src/app.rs:473-544` — `test_app_with_http_client()` is the established pattern for building test routers with mock servers

## Desired End State

- All routing_examples/ comments reference "Frugalis" (not "Cerebrum")
- A new `routing-multi-provider.toml` demonstrates the providers array format with fallback cascade
- `cache.rs` integration tests use a shared helper from `app::test_helpers` instead of a local 100-line duplicate
- `src/dashboard/` folder with `mod.rs`, `nav.rs`, `templates.rs`, `handlers.rs` (3-way split)
- `src/routing/` folder co-locating `auth.rs` and `routes.rs` (from `config/routing.rs`)
- `src/app/` folder co-locating `mod.rs` (from `app.rs`), `cli.rs`, `quickstart.rs`, `test_helpers.rs`
- AGENTS.md amended to reflect all new file locations
- All tests pass unchanged: `cargo test` (365 tests)

## What We're NOT Doing

- No production code behavior changes — all runtime behavior stays identical
- No config format migration — existing flat format remains valid
- No changes to init_template.toml or config.toml
- No Cargo workspace split — subdirectories only, not separate crates
- No lesson rule amendment — the 2-3-file threshold stands for future modules; these three extractions are user-approved exceptions

## Implementation Approach

Phases 1–2 are independent cleanup (text edits + test refactoring). Phase 3 is structural extraction ordered by ascending risk: 3a (app folder, 8 line edits), 3b (dashboard split, zero external breakage), 3c (routing folder, ~107 edits across 16 files). All extractions preserve the public module surface via re-exports or path-preserving mod.rs structure.

---

## Phase 1: Routing Examples Update

### Overview

Replace the stale "Cerebrum" project name in all 4 routing example comments and add a 5th example demonstrating the multi-provider fallback cascade format.

### Changes Required:

#### 1. Update existing example comments

**Files**: `routing_examples/routing-manual-tests.toml`, `routing_examples/routing_unreachable.toml`, `routing_examples/routing-openrouter.toml`, `routing_examples/routing-nvidia-nim.toml`

**Intent**: Replace "Cerebrum's merge semantics" with "Frugalis's merge semantics" in the header comment of each file.

**Contract**: Line 4 of each file changes from:
`# Cerebrum's merge semantics for [routing] are per-key additive — unmentioned`
to:
`# Frugalis's merge semantics for [routing] are per-key additive — unmentioned`

#### 2. Add multi-provider example

**File**: `routing_examples/routing-multi-provider.toml` (NEW)

**Intent**: Demonstrate the `providers = [...]` array format with fallback cascade, so users can discover this feature from examples rather than only from source code doc-comments.

**Contract**: A valid TOML overlay with at least 2 routes using the `providers` array format (each with 2 providers showing primary + fallback), and remaining routes using flat format. Include `timeout_ms` on at least one provider entry to demonstrate that field. Header comment should explain the fallback behavior.

### Success Criteria:

#### Automated Verification:

- `grep -ri "cerebrum" routing_examples/` returns zero results
- `routing_examples/routing-multi-provider.toml` exists and is valid TOML (parseable by `toml` crate / `cat` + syntax check)
- All 5 routing example files parse correctly when loaded as a config overlay (existing config loader test or manual `cargo run -- --validate` with `CONFIG_PATH` pointed at each)

#### Manual Verification:

- Read through each updated file to confirm comment text is correct and no other stale references remain
- Multi-provider example is realistic and self-documenting

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Cache Test Infrastructure Dedup

### Overview

Move the `test_app_with_cache()` helper from `src/cache.rs` into `src/app.rs::test_helpers` and refactor cache integration tests to use the shared helper. This eliminates 90+ lines of duplicated test setup.

### Changes Required:

#### 1. Add shared helper to app::test_helpers

**File**: `src/app.rs`

**Intent**: Add a `test_app_with_cache()` function to the `test_helpers` module that builds a Router + MockServer + ResponseCache, following the same pattern as `test_app_with_http_client()` but with cache enabled.

**Contract**: Function signature: `pub fn test_app_with_cache(ttl_secs: u64, max_entries: u64) -> (Router, httpmock::MockServer, Arc<crate::cache::ResponseCache>)`. Uses `"TEST_CACHE_PROXY"` as the env var name (matching current behavior). Internally reuses the same routing/classifier/auth setup as `test_app_with_http_client` but sets `response_cache: Some(Arc::new(ResponseCache::new(ttl_secs, max_entries)))` on AppState.

#### 2. Refactor cache.rs tests to use shared helper

**File**: `src/cache.rs`

**Intent**: Delete the local `test_app_with_cache()` function and replace all calls with `crate::app::test_helpers::test_app_with_cache`.

**Contract**: The import line adds `test_app_with_cache` to the existing `use crate::app::test_helpers::{...}` import. The local function definition (lines ~175-275) is deleted. All 5 integration tests that called the local helper (`test_cache_hit_returns_cached_response`, `test_cache_miss_proceeds_to_upstream`, `test_cache_bypass_header_skips_cache`, `test_cache_streaming_not_cached`, `test_cache_error_not_cached`) now call the shared helper with identical arguments.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test cache` — all cache tests pass (same count: 11 tests)
- `cargo test` — full suite passes (365 tests)
- `cargo clippy --all-targets` — no new warnings
- No local `fn test_app_with_cache` exists in `src/cache.rs`

#### Manual Verification:

- Confirm `src/cache.rs` is shorter (should drop from ~517 to ~420 lines)
- Confirm `src/app.rs` test_helpers has the new function

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Domain Subdirectory Extraction

### Overview

User-overridden decision to extract 3 flat `.rs` files into domain subdirectories, reversing the prior "keep flat" verdict from research §3. Three independent sub-phases ordered by ascending risk: app folder (8 edits), dashboard split (1→4 files, zero external breakage), routing folder (~107 edits across 16 files). Each sub-phase is independently verifiable. AGENTS.md amendments are required in all three.

**Conceptual caveat**: None of these files meets the lesson rule threshold ("exceeds 2-3 files") or the precedent mass criterion (~1,800+ LOC). This extraction is a user-approved co-location decision, not a threshold-triggered reorganization. The lesson rule stands for future modules; these are explicit exceptions.

---

### Phase 3a: App Folder Extraction (lowest risk)

#### Changes Required:

##### 1. Move app.rs → app/mod.rs + app/test_helpers.rs

**Files**: `src/app.rs` (delete) → `src/app/mod.rs` (create) + `src/app/test_helpers.rs` (create)

**Intent**: `app/mod.rs` holds AppState struct, ClassifierBuildResult, build_classifiers, build_persistence, build_app (app.rs lines 1–362). `app/test_helpers.rs` holds the `pub(crate) mod test_helpers` body (app.rs lines 364–827, unwrapped from the inline module). mod.rs declares `#[cfg(test)] pub(crate) mod test_helpers;`.

**Contract**: All `crate::app::AppState`, `crate::app::build_app`, `crate::app::test_helpers::*` paths continue resolving unchanged. The 11 consumer files need zero edits.

##### 2. Move cli.rs → app/cli.rs

**File**: `src/cli.rs` (delete) → `src/app/cli.rs` (create)

**Contract**: One critical edit — `include_str!("../init_template.toml")` → `include_str!("../../init_template.toml")` (cli.rs is now one directory deeper, needs extra `../` to reach repo root). All other content moves verbatim.

##### 3. Move quickstart.rs → app/quickstart.rs

**File**: `src/quickstart.rs` (delete) → `src/app/quickstart.rs` (create)

**Contract**: Verbatim move. No `include_str!`, no `crate::` imports — zero edits needed.

##### 4. Update main.rs module declarations and call sites

**File**: `src/main.rs`

**Contract**:
- Delete line 16 (`mod cli;`) and line 22 (`mod quickstart;`)
- `mod app;` (line 12) stays unchanged
- Line 28: `use cli::CliMode;` → `use app::cli::CliMode;`
- Line 33: `let cli::CliResult { mode, force } = cli::parse_args();` → `let app::cli::CliResult { mode, force } = app::cli::parse_args();`
- Line 37: `cli::print_help();` → `app::cli::print_help();`
- Line 42: `cli::run_init(path_opt.as_deref(), force)` → `app::cli::run_init(path_opt.as_deref(), force)`
- Line 52: `quickstart::run_quickstart()` → `app::quickstart::run_quickstart()`

##### 5. Amend AGENTS.md "Naming & File Layout"

**File**: `AGENTS.md` (lines 22–27)

**Contract**: Add `app/` entry describing the new folder. The current section omits `app.rs`, `cli.rs`, `quickstart.rs` entirely (already stale). Replace the block to document `app/{mod.rs, cli.rs, quickstart.rs, test_helpers.rs}`.

#### Success Criteria:

##### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test` — full suite passes (365 tests)
- `cargo clippy --all-targets` — no new warnings
- `rg "^mod (cli|quickstart);" src/main.rs` returns zero results
- `rg 'include_str!\("../init_template' src/app/cli.rs` returns zero results (path updated to `../../`)
- `ls src/app/` shows: `cli.rs  mod.rs  quickstart.rs  test_helpers.rs`

##### Manual Verification:

- `src/app.rs`, `src/cli.rs`, `src/quickstart.rs` no longer exist at top level
- AGENTS.md "Naming & File Layout" section documents the `app/` folder

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation before proceeding.

---

### Phase 3b: Dashboard 3-Way Split (low risk)

#### Changes Required:

##### 1. Create dashboard/nav.rs

**File**: `src/dashboard/nav.rs` (new)

**Contract**: Contains NavPage struct, NavItem struct, NavContext struct, 5 ICON_* consts (ICON_DASHBOARD, ICON_LIST, ICON_CLOCK, ICON_DOLLAR, ICON_CACHE), PAGES static, nav_for fn. All moved verbatim from dashboard.rs lines 17–86. No external imports needed — these are self-contained data types and a pure projection function.

##### 2. Create dashboard/templates.rs

**File**: `src/dashboard/templates.rs` (new)

**Contract**: Contains the `dashboard_page!` macro (dashboard.rs L88–106) + all 5 template structs: DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate, CacheTemplate (L108–153). Imports: `use askama::Template; use askama_web::WebTemplate; use crate::persistence; use super::nav::NavContext;`. The macro stays textually adjacent to its 5 invocations — no `#[macro_export]` needed (macro_rules! is visible by textual order within the same file).

##### 3. Create dashboard/handlers.rs

**File**: `src/dashboard/handlers.rs` (new)

**Contract**: Contains all 5 handler functions: dashboard_handler, inferences_handler, latency_handler, savings_handler, cache_handler (dashboard.rs L155–404). Also contains the entire `#[cfg(test)] mod tests` block (L417–724, 16 tests) — moved as a block. Imports: `use std::collections::HashMap; use std::sync::Arc; use axum::extract::{Query, State}; use axum::response::IntoResponse; use tracing::debug; use crate::app::AppState; use crate::persistence::PersistenceBackend; use super::nav::nav_for; use super::templates::{CacheTemplate, DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate};`

##### 4. Create dashboard/mod.rs

**File**: `src/dashboard/mod.rs` (new)

**Contract**: Contains `pub fn routes(auth_config: Arc<auth::AuthConfig>) -> Router<Arc<AppState>>` (L406–415) + module declarations (`pub(crate) mod handlers; pub(crate) mod nav; pub(crate) mod templates;`) + re-exports (`pub use nav::{nav_for, NavContext, NavItem, NavPage, PAGES}; pub use templates::{CacheTemplate, DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate};`). Imports: `use std::sync::Arc; use axum::{routing::get, Router}; use tower_http::services::ServeDir; use crate::{app::AppState, auth};` (note: `auth` → `routing` after Phase 3c if that phase runs).

##### 5. Delete dashboard.rs

**File**: `src/dashboard.rs` (delete)

**Contract**: Rust's module path lookup automatically resolves `mod dashboard;` (main.rs:18) to `src/dashboard/mod.rs` when the single-file `src/dashboard.rs` is removed.

##### 6. Amend AGENTS.md "Dashboard Pages & Auto-Nav"

**File**: `AGENTS.md` (lines 33–57)

**Contract**: 6 amendments:
- File-layout bullet: `dashboard.rs` → `dashboard/` with 4-file description
- "registered in `src/dashboard.rs`" → "registered in `src/dashboard/nav.rs`"
- `PAGES` location: `src/dashboard.rs:37-42` → `src/dashboard/nav.rs` (drop stale line refs)
- `dashboard_page!` macro location: `src/dashboard.rs:55-68` → `src/dashboard/templates.rs` (drop stale line refs)
- "Adding a new dashboard page requires" 5-step list: update file paths (nav.rs, templates.rs, handlers.rs, mod.rs)
- "Template structs and handlers live in `dashboard.rs`" → "Template structs live in `dashboard/templates.rs`, handlers in `dashboard/handlers.rs`, nav registry in `dashboard/nav.rs`, routes() builder in `dashboard/mod.rs`"

#### Success Criteria:

##### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test dashboard` — all 16 dashboard tests pass (test paths change from `dashboard::tests::` to `dashboard::handlers::tests::`)
- `cargo test` — full suite passes (365 tests)
- `cargo clippy --all-targets` — no new warnings
- `rg "src/dashboard\.rs" AGENTS.md` returns zero results (all references updated)

##### Manual Verification:

- `src/dashboard.rs` no longer exists; `src/dashboard/` contains 4 files
- Test paths changed from `dashboard::tests::*` to `dashboard::handlers::tests::*` (expected, not a regression)
- AGENTS.md "Dashboard Pages & Auto-Nav" section references new file locations

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation before proceeding.

---

### Phase 3c: Routing Folder Extraction (highest risk)

#### Changes Required:

##### 1. Create routing/mod.rs

**File**: `src/routing/mod.rs` (new)

**Contract**:
```rust
pub(crate) mod auth;
pub(crate) mod routes;

pub(crate) use auth::*;
pub(crate) use routes::*;
```
Glob re-export so `crate::routing::AuthConfig` and `crate::routing::RouteEntry` both resolve. No item-name collisions between auth and routes modules (verified).

##### 2. Move auth.rs → routing/auth.rs

**File**: `src/auth.rs` (delete) → `src/routing/auth.rs` (create)

**Contract**: Verbatim move. All 283 lines including `#[cfg(test)] mod tests` (6 tests) move unchanged.

##### 3. Move config/routing.rs → routing/routes.rs

**File**: `src/config/routing.rs` (delete) → `src/routing/routes.rs` (create)

**Contract**: Verbatim move. All 199 lines including `#[cfg(test)] mod tests` (2 tests) move unchanged. Submodule named `routes` (not `routing`) to avoid `routing::routing::` stutter.

##### 4. Update config/mod.rs

**File**: `src/config/mod.rs`

**Contract**:
- Line 7: delete `pub(crate) mod routing;`
- Line 10: `pub(crate) use routing::RouteEntry;` → `pub(crate) use crate::routing::RouteEntry;` (keep re-export so ConfigRoot.routing field at L54 still resolves)

##### 5. Update config/loader.rs

**File**: `src/config/loader.rs`

**Contract**: Line 5: `use super::routing::*;` → `use crate::routing::*;` (CRITICAL — loader.rs uses routing types in hardcoded_routing L152–185, routing_from_value L293+, build_model_costs L407+, and ~15 test sites L722–857).

##### 6. Update main.rs module declaration

**File**: `src/main.rs`

**Contract**: Line 13: `mod auth;` → `mod routing;`

##### 7. Update all import paths (8 `use` lines + ~95 inline qualified paths)

**Files**: `src/app.rs` (or `src/app/mod.rs` after Phase 3a), `src/dashboard.rs` (or dashboard subfiles after Phase 3b), `src/persistence/mod.rs`, `src/proxy/handlers.rs`, `src/proxy/streaming.rs`, `src/classification/{types.rs, chain.rs, regex.rs, fewshot.rs, llm.rs}`, `src/proxy/upstream.rs`, `src/persistence/{types.rs, sql_backend.rs, memory.rs}`

**Contract**: Mechanical prefix swap:
- `use crate::auth;` / `crate::{...auth...}` → `routing` in 8 `use` lines
- `auth::AuthConfig` → `routing::AuthConfig` (13 sites)
- `auth::proxy_auth_layer` / `auth::dashboard_auth_layer` → `routing::proxy_auth_layer` / `routing::dashboard_auth_layer`
- `crate::config::routing::X` → `crate::routing::X` (inline fully-qualified paths, ~30 sites)
- `config::routing::X` → `routing::X` (where `config` is in scope, ~71 sites across app/persistence/proxy test modules)

##### 8. Amend AGENTS.md

**File**: `AGENTS.md`

**Contract**:
- Line 24: `auth.rs` bullet → `routing/auth.rs` (also fix stale fn names: `require_proxy_bearer`/`require_dashboard_basic` → actual `proxy_auth_layer`/`dashboard_auth_layer`)
- Line 61: `[src/auth.rs](src/auth.rs)` link → `[src/routing/auth.rs](src/routing/auth.rs)`
- "Naming & File Layout": add `routing/` entry, update `auth.rs` reference

#### Success Criteria:

##### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test auth` — all 6 auth tests pass (now at `routing::auth::tests::*`)
- `cargo test routes_auth` — route authorization tests pass
- `cargo test` — full suite passes (365 tests)
- `cargo test slow_tests` — slow tests pass
- `cargo clippy --all-targets` — no new warnings
- `rg "crate::auth[^_]|use crate::auth[^_]|mod auth[^_]" src/` returns zero results (no stale auth module refs; `[^_]` avoids false positives on `auth_providers`)
- `rg "config::routing|crate::config::routing" src/` returns zero results (no stale config::routing refs)

##### Manual Verification:

- `src/auth.rs` and `src/config/routing.rs` no longer exist
- `src/routing/` contains: `auth.rs  mod.rs  routes.rs`
- AGENTS.md references updated to new paths

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation before proceeding.

---

## Testing Strategy

### Unit Tests:

- No new unit tests needed — Phases 1–3 are refactoring of test infrastructure and module layout

### Integration Tests:

- All existing cache integration tests (5 async tests using httpmock) must continue passing unchanged (Phase 2)
- All existing dashboard integration tests (16 tests) must continue passing, test paths change to `dashboard::handlers::tests::*` (Phase 3b)
- All existing auth tests (6 tests) must continue passing, test paths change to `routing::auth::tests::*` (Phase 3c)
- `cargo test` full suite must maintain 365 test count across all phases

### Manual Testing Steps:

1. `CONFIG_PATH=routing_examples/routing-multi-provider.toml cargo run -- --validate` should succeed (Phase 1)
2. `cargo test cache` passes all 11 cache tests (Phase 2)
3. Compare test count before/after: `cargo test 2>&1 | grep 'test result'`
4. After Phase 3a: `ls src/app/` shows 4 files; `src/app.rs`/`src/cli.rs`/`src/quickstart.rs` no longer exist at top level
5. After Phase 3b: `ls src/dashboard/` shows 4 files; `src/dashboard.rs` no longer exists
6. After Phase 3c: `ls src/routing/` shows 3 files; `src/auth.rs` and `src/config/routing.rs` no longer exist

## Performance Considerations

None — Phases 1–2 are documentation/test-only. Phase 3 is pure file moves + import path rewrites. Zero runtime behavior changes. All extractions preserve the public module surface via re-exports or path-preserving mod.rs structure.

## References

- Related research: `context/changes/code-structure-reorg-ext/research.md`
- Original plan: `context/archive/2026-06-28-code-structure-reorg/plan.md`
- Multi-provider format: `src/config/routing.rs:25-41`
- Shared test helpers: `src/app.rs:358-721`
- Cache test helper to dedup: `src/cache.rs:175-275`
- Dashboard split precedent: `src/proxy/mod.rs` (mod.rs pattern), `src/config/mod.rs` (re-export pattern)
- Routing folder glob re-export precedent: `src/config/mod.rs:10` (`pub(crate) use routing::RouteEntry;`)
- App folder include_str! path: `src/cli.rs:1` (critical `../` → `../../` edit)
- Dashboard internal cross-refs: `src/dashboard.rs:88-106` (macro → NavContext), `src/dashboard.rs:155-404` (handlers → nav_for + templates)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Routing Examples Update

#### Automated

- [x] 1.1 `grep -ri "cerebrum" routing_examples/` returns zero results — f5ad22b
- [x] 1.2 `routing_examples/routing-multi-provider.toml` exists and is valid TOML — f5ad22b
- [x] 1.3 All 5 routing example files parse correctly as config overlays — f5ad22b

#### Manual

- [ ] 1.4 Comment text is correct, no stale references remain
- [ ] 1.5 Multi-provider example is realistic and self-documenting

### Phase 2: Cache Test Infrastructure Dedup

#### Automated

- [x] 2.1 `cargo build` succeeds — 5b5c085
- [x] 2.2 `cargo build --features otel` succeeds — 5b5c085
- [x] 2.3 `cargo test cache` — all cache tests pass (11 tests) — 5b5c085
- [x] 2.4 `cargo test` — full suite passes (365 tests) — 5b5c085
- [x] 2.5 `cargo clippy --all-targets` — no new warnings — 5b5c085
- [x] 2.6 No local `fn test_app_with_cache` in `src/cache.rs` — 5b5c085

#### Manual

- [ ] 2.7 `src/cache.rs` is shorter (~420 lines)
- [ ] 2.8 `src/app.rs` test_helpers has the new function

### Phase 3a: App Folder Extraction

#### Automated

- [x] 3a.1 `cargo build` succeeds — 5b5c085
- [x] 3a.2 `cargo build --features otel` succeeds — 5b5c085
- [x] 3a.3 `cargo test` — full suite passes (365 tests) — 5b5c085
- [x] 3a.4 `cargo clippy --all-targets` — no new warnings — 5b5c085
- [x] 3a.5 `rg "^mod (cli|quickstart);" src/main.rs` returns zero results — 5b5c085
- [x] 3a.6 `rg 'include_str!("../init_template' src/app/cli.rs` returns zero (path updated to `../../`) — 5b5c085
- [x] 3a.7 `ls src/app/` shows: `cli.rs  mod.rs  quickstart.rs  test_helpers.rs` — 5b5c085

#### Manual

- [ ] 3a.8 `src/app.rs`, `src/cli.rs`, `src/quickstart.rs` no longer exist at top level
- [ ] 3a.9 AGENTS.md "Naming & File Layout" section documents the `app/` folder

### Phase 3b: Dashboard 3-Way Split

#### Automated

- [x] 3b.1 `cargo build` succeeds — 6ba96b4
- [x] 3b.2 `cargo build --features otel` succeeds — 6ba96b4
- [x] 3b.3 `cargo test dashboard` — all 16 tests pass (paths change to `dashboard::handlers::tests::*`) — 6ba96b4
- [x] 3b.4 `cargo test` — full suite passes (365 tests) — 6ba96b4
- [x] 3b.5 `cargo clippy --all-targets` — no new warnings — 6ba96b4
- [x] 3b.6 `rg "src/dashboard\.rs" AGENTS.md` returns zero results — 6ba96b4

#### Manual

- [ ] 3b.7 `src/dashboard.rs` no longer exists; `src/dashboard/` contains 4 files
- [ ] 3b.8 AGENTS.md "Dashboard Pages & Auto-Nav" section references new file locations

### Phase 3c: Routing Folder Extraction

#### Automated

- [x] 3c.1 `cargo build` succeeds — 4945405
- [x] 3c.2 `cargo build --features otel` succeeds — 4945405
- [x] 3c.3 `cargo test auth` — all 6 auth tests pass (now at `routing::auth::tests::*`) — 4945405
- [x] 3c.4 `cargo test routes_auth` — route authorization tests pass — 4945405
- [x] 3c.5 `cargo test` — full suite passes (365 tests) — 4945405
- [x] 3c.6 `cargo test slow_tests` — slow tests pass — 4945405
- [x] 3c.7 `cargo clippy --all-targets` — no new warnings — 4945405
- [x] 3c.8 `rg "crate::auth[^_]|use crate::auth[^_]|mod auth[^_]" src/` returns zero results — 4945405
- [x] 3c.9 `rg "config::routing|crate::config::routing" src/` returns zero results — 4945405

#### Manual

- [ ] 3c.10 `src/auth.rs` and `src/config/routing.rs` no longer exist
- [ ] 3c.11 `src/routing/` contains: `auth.rs  mod.rs  routes.rs`
- [ ] 3c.12 AGENTS.md references updated to new paths
