# Test Distribution Implementation Plan

## Overview

Distribute 5061 lines of tests from the monolithic `src/tests.rs` into `#[cfg(test)] mod tests` blocks within their respective domain modules. Pure structural move — no test rewriting, no behavior changes.

## Current State Analysis

All 365 tests live in `src/tests.rs` with `use crate::*;` at the top. Tests are organized by section comments (`// ──`) that already indicate their domain. A nested `mod slow_tests` (5 tests) uses `serial_test::serial` for keepalive and shutdown tests.

Shared test helpers (`test_categories`, `make_test_app_state`, `test_app`, etc.) already live in `src/app.rs` under `#[cfg(test)] pub(crate) mod test_helpers`.

### Key Discoveries:

- `src/tests.rs:1` — uses `use crate::*;` which pulls everything in scope
- `src/tests.rs:4639` — `mod slow_tests` contains 5 tests (keepalive × 4, graceful_shutdown × 1)
- Several test-local helper functions exist (e.g., `test_app_with_dead_endpoint`, `test_app_with_cache`, `test_app_with_openai_translation`) that need to move with their tests
- Tests use `httpmock::MockServer` for upstream simulation
- `src/test_util.rs` (12 lines) provides `EnvGuard` — stays as-is

### Test Distribution Map:

| Domain Module | Tests | Lines (approx) |
|---|---|---|
| `proxy/handlers.rs` | completion, messages, classify, feedback, count_tokens, models, upstream routing | ~2200 |
| `proxy/streaming.rs` | SSE streaming, keepalive (slow), mid-stream errors, format_sse_error_event | ~1400 |
| `proxy/util.rs` | sanitize_for_nim, try_optimize_request, json_response contracts, UsageBreakdown | ~350 |
| `dashboard.rs` | dashboard auth, inferences, latency, savings pages | ~400 |
| `classification/chain.rs` | classifier chain tests (regex+fewshot, 3-backend escalation) | ~250 |
| `cache.rs` | cache hit/miss/bypass/streaming/error/dashboard | ~360 |
| `cli.rs` | init template tests | ~100 |

## Desired End State

`src/tests.rs` deleted. Each domain module has its own `#[cfg(test)] mod tests` block. `main.rs` no longer declares `mod tests;`. All 365 tests pass from their new locations.

## What We're NOT Doing

- No test rewriting — tests move as-is with updated imports
- No new test infrastructure — `app.rs::test_helpers` stays
- No test coverage changes — same 365 tests
- No Cargo.toml changes

## Implementation Approach

Move tests bottom-up by dependency: start with modules that have no test-local helpers shared with other groups (util, dashboard, classification, cache, cli), then move the large groups (handlers, streaming) that contain shared helper functions.

## Phase 5: Distribute Tests to Domain Modules

### Overview

Move all test functions from `src/tests.rs` into `#[cfg(test)] mod tests` blocks in their respective domain modules. Remove `src/tests.rs` and the `mod tests;` declaration in `main.rs`.

### Changes Required:

#### 1. proxy/handlers.rs — handler integration tests

**File**: `src/proxy/handlers.rs`

**Intent**: Add `#[cfg(test)] mod tests` block containing all handler-level integration tests: completion, messages, classify, feedback, count_tokens, models, upstream routing, and anthropic translation tests. Include test-local helpers (`test_app_with_dead_endpoint`, `test_app_with_enriched_classifier`, `test_app_with_openai_translation`).

**Contract**: Tests from lines 17–560, 918–1237, 1635–2505, 2964–3275, 3447–3556, 3658–4042, 4043–4283 of current `tests.rs`. Each test uses `use crate::app::test_helpers::*` and `use super::*` as needed.

#### 2. proxy/streaming.rs — streaming and SSE tests

**File**: `src/proxy/streaming.rs`

**Intent**: Add `#[cfg(test)] mod tests` containing SSE streaming tests, format_sse_error_event unit tests, streaming error handling tests, and the keepalive slow tests (formerly `mod slow_tests`).

**Contract**: Tests from lines 2505–2964, 3005–3247 of current `tests.rs`, plus `mod slow_tests` (lines 4639–5061). The slow tests stay in a nested `mod slow_tests` within the `#[cfg(test)]` block and retain `serial_test::serial`.

#### 3. proxy/util.rs — utility function tests

**File**: `src/proxy/util.rs`

**Intent**: Add `#[cfg(test)] mod tests` with tests for `sanitize_for_nim`, `try_optimize_request`, `json_response`, `classification_only_json`, `upstream_error_json`.

**Contract**: Tests from lines 692–918 (sanitize + optimize) and 3447–3556 (JSON contract shape tests) of current `tests.rs`.

#### 4. dashboard.rs — dashboard page tests

**File**: `src/dashboard.rs`

**Intent**: Add `#[cfg(test)] mod tests` with tests for dashboard auth, inferences page, latency page, savings page.

**Contract**: Tests from lines 1466–1635 (inferences/dashboard) and 3275–3447 (latency + savings) of current `tests.rs`.

#### 5. classification/chain.rs — classifier chain tests

**File**: `src/classification/chain.rs`

**Intent**: Add `#[cfg(test)] mod tests` with classifier chain integration tests.

**Contract**: Tests from lines 55–280 of current `tests.rs` (chain_with_regex_and_fewshot, chain_3_backend_escalates_to_llm).

#### 6. cache.rs — cache integration tests

**File**: `src/cache.rs`

**Intent**: Add `#[cfg(test)] mod tests` with cache hit/miss/bypass/streaming/error/dashboard tests. Include the `test_app_with_cache` helper.

**Contract**: Tests from lines 4283–4638 of current `tests.rs`.

#### 7. cli.rs — init template tests

**File**: `src/cli.rs`

**Intent**: Add `#[cfg(test)] mod tests` with init template content assertions and file-writing tests.

**Contract**: Tests from lines 3557–3658 of current `tests.rs`.

#### 8. persistence/mod.rs — snippet path and logging tests

**File**: `src/persistence/mod.rs`

**Intent**: Add `#[cfg(test)] mod tests` with snippet path truncation, log failure isolation, and db-related integration tests.

**Contract**: Tests from lines 1238–1465 of current `tests.rs`.

#### 9. Remove src/tests.rs and update main.rs

**File**: `src/tests.rs` (DELETE), `src/main.rs`

**Intent**: Remove the monolithic test file and the `#[cfg(test)] mod tests;` declaration in main.rs. Also remove the `#[cfg(test)] pub(crate) use cli::{run_init, INIT_TEMPLATE};` re-export (no longer needed since cli.rs has its own tests).

**Contract**: Delete `src/tests.rs`. Remove `mod tests;` line from `main.rs`. Remove the `#[cfg(test)]` re-export block.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test` — 365 tests pass (unchanged count)
- `cargo clippy` — no new warnings
- `src/tests.rs` no longer exists

#### Manual Verification:

- Each domain module has a `#[cfg(test)] mod tests` block
- `cargo test proxy::handlers` runs handler tests
- `cargo test proxy::streaming` runs streaming tests
- `cargo test cache` runs cache tests

**Implementation Note**: This is a large mechanical move. Use a subagent to perform the extraction since it involves reading 5061 lines and distributing across 8 files. After completing this phase and all automated verification passes, pause here for manual confirmation.

---

## Phase 6: Cleanup and Verification

### Overview

Final cleanup — verify no dead imports remain, ensure test isolation is correct (no cross-module test dependencies), and confirm the structure matches the desired end state from the original code-structure-reorg plan.

### Changes Required:

#### 1. Clean up unused imports

**File**: All domain modules that received tests

**Intent**: Remove any unused `use` statements that were artifacts of the monolithic `use crate::*;` pattern. Each module's test block should import only what it needs.

**Contract**: `cargo clippy` should show no `unused_import` warnings. Each test module uses targeted imports (`use super::*` for the module's own items, explicit `use crate::X` for cross-module dependencies).

#### 2. Verify test helpers accessibility

**File**: `src/app.rs`

**Intent**: Confirm `test_helpers` module is accessible from all domain test blocks. Add any missing helper functions if tests fail.

**Contract**: `pub(crate) mod test_helpers` remains in `app.rs`. No new helpers needed if tests were moved correctly.

#### 3. Update plan progress

**File**: `context/changes/code-structure-reorg/plan.md`

**Intent**: Record completion of the test distribution work.

### Success Criteria:

#### Automated Verification:

- `cargo test` — 365 tests pass
- `cargo clippy` — zero warnings
- No `use crate::*` in any test module (each uses targeted imports)

#### Manual Verification:

- `wc -l src/main.rs` decreased (no more `mod tests;` + re-exports)
- `grep -r "mod tests" src/` shows test blocks in each domain module

**Implementation Note**: After completing this phase and all automated verification passes, the test distribution work is complete.

---

## Testing Strategy

### Unit Tests:
- All 365 existing tests move unchanged
- Tests use `use super::*` within their module + explicit cross-module imports

### Integration Tests:
- `test_app()`, `test_app_with_*` helpers from `app.rs::test_helpers` remain the primary integration test infrastructure
- Tests that need a full Router continue using `.oneshot()` pattern

## Performance Considerations

None — compile-time only change. May slightly improve incremental compilation since modifying one test file no longer recompiles all tests.

## References

- Parent plan: `context/changes/code-structure-reorg/plan.md`
- Review finding: F4 in `context/changes/code-structure-reorg/reviews/impl-review.md`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 5: Distribute Tests to Domain Modules

#### Automated

- [x] 5.1 `cargo build` succeeds — 5a182ea
- [x] 5.2 `cargo build --features otel` succeeds — 5a182ea
- [x] 5.3 `cargo test` — 365 tests pass — 5a182ea
- [x] 5.4 `cargo clippy` — no new warnings — 5a182ea
- [x] 5.5 `src/tests.rs` no longer exists — 5a182ea

#### Manual

- [x] 5.6 Each domain module has a `#[cfg(test)] mod tests` block — 5a182ea
- [x] 5.7 `cargo test proxy::handlers` runs handler tests — 5a182ea
- [x] 5.8 `cargo test proxy::streaming` runs streaming tests — 5a182ea

### Phase 6: Cleanup and Verification

#### Automated

- [x] 6.1 `cargo test` — 365 tests pass
- [x] 6.2 `cargo clippy` — zero warnings
- [x] 6.3 No `use crate::*` in any test module

#### Manual

- [x] 6.4 `wc -l src/main.rs` decreased
- [x] 6.5 `grep -r "mod tests" src/` shows test blocks in domain modules
