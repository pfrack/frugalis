# Test Distribution — Plan Brief

> Full plan: `context/changes/code-structure-reorg/plan-tests.md`

## What & Why

Distribute 5061 lines of tests from the monolithic `src/tests.rs` into `#[cfg(test)] mod tests` blocks within their respective domain modules. This was identified as F4 in the impl review — tests were moved to a single file as a user-approved shortcut during Phase 4, and now need proper co-location with the code they exercise.

## Starting Point

All 365 tests live in `src/tests.rs` using `use crate::*;`. Test helpers already extracted to `app.rs::test_helpers`. Domain modules (proxy/, classification/, config/, persistence/, protocol/) have no inline tests.

## Desired End State

`src/tests.rs` deleted. Each domain module has its own `#[cfg(test)] mod tests` block with targeted imports. Running `cargo test proxy::handlers` executes only handler tests. 365 tests still pass.

## Key Decisions Made

| Decision | Choice | Why |
|----------|--------|-----|
| slow_tests | Distribute into proxy/streaming.rs | They test keepalive/streaming — belong with that code |
| Shared helpers | Keep in app.rs::test_helpers | Already factored; generic across modules |
| Phase count | 2 phases (move + cleanup) | Move is mechanical; cleanup ensures targeted imports |

## Scope

**In scope:** Move all tests, delete tests.rs, update imports

**Out of scope:** No test rewriting, no new tests, no Cargo.toml changes

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 5. Distribute | All tests moved to domain modules, tests.rs deleted | Import resolution — tests may reference items not in scope via `use super::*` |
| 6. Cleanup | Targeted imports, no `use crate::*` in tests | May uncover hidden dependencies between test groups |

**Prerequisites:** Phase 4 complete (code-structure-reorg), `src/app.rs::test_helpers` in place
**Estimated effort:** ~1 session, mostly mechanical

## Success Criteria (Summary)

- 365 tests pass from their new module locations
- `src/tests.rs` no longer exists
- `cargo test <module>` runs only that module's tests
