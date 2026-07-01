# Code Structure Reorganization Extension — Plan Brief

> Full plan: `context/changes/code-structure-reorg-ext/plan.md`
> Research: `context/changes/code-structure-reorg-ext/research.md`

## What & Why

Finish residual cleanup from the archived code-structure-reorg, plus a user-directed extension to extract 3 flat `.rs` files into domain subdirectories. Three concerns: (1) routing example files still reference "Cerebrum" (old project name), (2) `cache.rs` has a 100-line test helper that duplicates shared infrastructure, (3) `dashboard.rs`, `auth.rs`+`config/routing.rs`, and `app.rs`+`cli.rs`+`quickstart.rs` should be co-located into domain folders despite not meeting the 2-3-file threshold (user override of prior research verdict).

## Starting Point

The original reorg (phases 1–6) is complete and archived. The codebase is well-organized into domain subdirectories. The 4 `routing_examples/*.toml` files have correct config format but stale comments. The `src/app.rs::test_helpers` module is the established central test infrastructure, but `cache.rs` predates the consolidation and still has its own local copy. Three flat files (`dashboard.rs`, `auth.rs`, `app.rs`) are user-approved candidates for folder extraction despite the prior research's "keep flat" verdict.

## Desired End State

- All routing examples reference "Frugalis" and a new example demonstrates the multi-provider fallback format that the parser already supports
- Cache integration tests use the shared `app::test_helpers` infrastructure — no duplicated setup code
- `src/dashboard/` folder with `mod.rs`, `nav.rs`, `templates.rs`, `handlers.rs` (3-way split)
- `src/routing/` folder co-locating `auth.rs` (from `src/auth.rs`) and `routes.rs` (from `src/config/routing.rs`)
- `src/app/` folder co-locating `mod.rs` (from `src/app.rs`), `cli.rs` (from `src/cli.rs`), `quickstart.rs` (from `src/quickstart.rs`), `test_helpers.rs`
- Full test suite passes unchanged (365 tests)
- AGENTS.md amended to reflect new file locations

## Key Decisions Made

| Decision | Choice | Why (1 sentence) |
|----------|--------|-------------------|
| Multi-provider example | Add new file | Documents a supported feature users can't discover from existing examples |
| Cache helper approach | New dedicated fn in test_helpers | Keeps existing `test_app_with_http_client` callers untouched, follows established pattern |
| Flat files extraction | Extract dashboard, routing, app folders | User override of "keep flat" verdict — co-location preference over threshold compliance |
| Dashboard split | 3-way (nav/templates/handlers) | Cleanest separation of concerns, matches proxy/ folder precedent |
| Routing folder | New `routing/` with auth + routes | User decision: auth + routing = "same domain" (request-pipeline middleware) |
| App folder | New `app/` with app + cli + quickstart | User decision: cli + quickstart + app = "one domain" (application bootstrap) |
| cost_per_1m_input_tokens in examples | Skip | Orthogonal to reorg goal, would add noise |
| Lesson rule amendment | Pending user decision | Research recommends documenting these as exceptions; user hasn't decided yet |

## Scope

**In scope:**
- Replace "Cerebrum" → "Frugalis" in 4 routing example comments
- Add `routing-multi-provider.toml` example
- Add `test_app_with_cache()` to `app::test_helpers`
- Delete local duplicate from `cache.rs`
- Extract `dashboard.rs` → `dashboard/{mod.rs, nav.rs, templates.rs, handlers.rs}`
- Extract `auth.rs` + `config/routing.rs` → `routing/{mod.rs, auth.rs, routes.rs}`
- Extract `app.rs` + `cli.rs` + `quickstart.rs` → `app/{mod.rs, cli.rs, quickstart.rs, test_helpers.rs}`
- Amend AGENTS.md for all three extractions

**Out of scope:**
- Production code behavior changes
- Config format migration
- init_template.toml or config.toml changes
- Cargo workspace split (subdirectories only, not separate crates)
- Lesson rule amendment (flagged as separate user decision)

## Architecture / Approach

Phases 1–2 are pure cleanup (text edits + test refactoring). Phase 3 is structural extraction ordered by ascending risk: 3a (app folder, 8 line edits), 3b (dashboard split, zero external breakage), 3c (routing folder, ~107 edits across 16 files). All extractions preserve the public module surface via re-exports or path-preserving mod.rs structure.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Routing Examples Update | Corrected comments + multi-provider example | None — text-only |
| 2. Cache Test Infra Dedup | Consolidated test helper, ~90 fewer lines in cache.rs | Low — test-only refactoring |
| 3a. App Folder | `app/{mod.rs, cli.rs, quickstart.rs, test_helpers.rs}` | Lowest — 8 edits, 11 consumers unchanged |
| 3b. Dashboard Split | `dashboard/{mod.rs, nav.rs, templates.rs, handlers.rs}` | Low — zero external breakage, AGENTS.md amendment |
| 3c. Routing Folder | `routing/{mod.rs, auth.rs, routes.rs}` | Highest — ~107 edits across 16 files, high touch count |

**Prerequisites:** Phases 3a–3c are ordered (3a before 3c so routing edits target the new app/mod.rs). Phase 3b is independent.
**Estimated effort:** ~2–3 sessions. Phases 1–2 in one session; Phase 3a–3b in one session; Phase 3c in one session.

## Open Risks & Assumptions

- Assumes `cargo test` count is still 365 (could drift if `replace-sqlx` change adds/removes tests on this branch)
- Phase 3c has ~107 mechanical edits — high touch count means higher chance of missing one; the grep audits (`rg "crate::auth[^_]"` and `rg "config::routing"`) are mandatory
- Phase 3a has one critical `include_str!` path edit (`../` → `../../`) that will fail compilation if missed
- The three extractions contradict the lesson rule threshold and AGENTS.md dashboard cohesion mandate — both are being overridden by user decision; AGENTS.md is amended as part of the work
- The `routing/` folder name is slightly misleading (auth is not a sub-concern of routing) — documented in research, not blocking

## Success Criteria (Summary)

- Zero "Cerebrum" references in `routing_examples/`
- Multi-provider format is discoverable from examples
- Full test suite passes with no duplicated test infrastructure in `cache.rs`
- `src/dashboard.rs`, `src/auth.rs`, `src/config/routing.rs`, `src/app.rs`, `src/cli.rs`, `src/quickstart.rs` no longer exist at top level
- `src/dashboard/`, `src/routing/`, `src/app/` folders exist with the specified structure
- AGENTS.md reflects all new file locations
- Test count unchanged at 365
