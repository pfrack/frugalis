<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Code Structure Reorganization Extension

- **Plan**: context/changes/code-structure-reorg-ext/plan.md
- **Scope**: Full plan (5 phases, all implemented)
- **Date**: 2026-06-29
- **Verdict**: APPROVED
- **Findings**: 0 critical  1 warning  0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | WARNING (1 finding) |
| Success Criteria | PASS (11 manual items pending by design) |

## Summary

All 32 plan-drift verification points across Phases 1, 2, 3a, 3b, 3c resolved as MATCH — the plan's contracts were honored in full. Constant-time auth comparison preserved verbatim through the auth.rs → routing/auth.rs move; cache test helper uses safe defaults; no `unsafe` blocks or new production `unwrap()`; public module surface preserved via glob re-exports in `routing/mod.rs`; no circular dependencies introduced; `include_str!` path correctly bumped to `../../` in `app/cli.rs`; AGENTS.md refreshed for all three extractions.

Single substantive finding (F1) was a `#[allow(unused_imports)]` suppression that hid dead re-exports the repo's own lessons.md rule says to delete. Triage applied the fix.

## Findings

### F1 — `#[allow(unused_imports)]` suppresses warnings the lesson rule says to delete

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/dashboard/mod.rs:12,14
- **Detail**: Two `pub use` re-exports (NavContext, NavItem, NavPage, PAGES, nav_for, CacheTemplate, DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate) carried `#[allow(unused_imports)]`. Removing the attributes surfaced two `unused imports` warnings: there were zero callers anywhere in the codebase for any of these ten items at the `crate::dashboard::*` path. The same items were already reachable via the natural sub-module path (`crate::dashboard::nav::*`, `crate::dashboard::templates::*`) which `dashboard/handlers.rs` actually used via `use super::*`. The repo's own lessons.md rule (lines 48-51) reads: "When a `dead_code` warning fires on code with zero callers, delete it. ... YAGNI: if you aren't using it now, you don't need it." The plan (Phase 3b contract §4) explicitly required these re-exports as part of preserving the public module surface, but the lesson rule post-dates the plan and should shape the call.
- **Fix**: Delete the two `#[allow(unused_imports)] pub use` blocks (lines 12-17) in src/dashboard/mod.rs. `pub(crate) mod nav;` and `pub(crate) mod templates;` (lines 9-10) already expose the items via `crate::dashboard::nav::*` / `crate::dashboard::templates::*` for any future caller. `dashboard::handlers` (the only existing caller) already uses the natural path and is unaffected.
  - Strength: Matches the repo's own YAGNI rule. Zero callers means zero migration cost.
  - Tradeoff: Future external callers must reach into the sub-module path instead of `crate::dashboard::*`. No current caller is affected.
  - Confidence: HIGH — zero callers means zero migration cost.
  - Blind spot: None significant.
- **Decision**: FIXED via Fix (single option). Removed lines 12-17 of src/dashboard/mod.rs; updated AGENTS.md line 27 to drop the stale "re-exports" mention from dashboard/mod.rs description. `cargo build`, `cargo clippy --all-targets`, `cargo test` (365 tests) all clean after the edit.

## Manual verification items still pending

The following 11 manual items in the plan's `## Progress` section remain `- [ ]` by design (awaiting human review). None blocked automated verification or the review verdict:

- 1.4 Comment text is correct, no stale references remain
- 1.5 Multi-provider example is realistic and self-documenting
- 2.7 src/cache.rs is shorter (~420 lines) — verified: 414 lines
- 2.8 src/app.rs test_helpers has the new function
- 3a.8 src/app.rs, src/cli.rs, src/quickstart.rs no longer exist at top level
- 3a.9 AGENTS.md "Naming & File Layout" section documents the `app/` folder
- 3b.7 src/dashboard.rs no longer exists; src/dashboard/ contains 4 files
- 3b.8 AGENTS.md "Dashboard Pages & Auto-Nav" section references new file locations
- 3c.10 src/auth.rs and src/config/routing.rs no longer exist
- 3c.11 src/routing/ contains: auth.rs  mod.rs  routes.rs
- 3c.12 AGENTS.md references updated to new paths
