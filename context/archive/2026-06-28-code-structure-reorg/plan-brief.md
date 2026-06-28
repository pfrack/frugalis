# Code Structure Reorganization — Plan Brief

> Full plan: `context/changes/code-structure-reorg/plan.md`
> Research: `context/changes/code-structure-reorg/research.md`

## What & Why

Reorganize the flat `src/` directory into domain-grouped subdirectories. The 8,460-line `main.rs` and 12 sibling files are hard to navigate and hide 7 distinct domains in a single level. This is pure structural refactoring — zero behavior changes.

## Starting Point

All 20,691 lines of Rust source live flat in `src/`. `main.rs` contains handlers, streaming logic, utilities, AppState, router assembly, and ~4,900 lines of tests. Modules are declared directly in main.rs. A tight coupling triangle (routing ↔ intent_classifier ↔ config) makes naive one-at-a-time extraction risky.

## Desired End State

`main.rs` shrinks to ~250 lines (mod declarations + CLI bootstrap). Code is grouped into 6 directories (`proxy/`, `classification/`, `protocol/`, `config/`, `persistence/`, plus existing `dashboard.rs`) with exposed submodule paths. Tests live inline with the code they exercise. Dead code (`translate/mod.rs`) is deleted.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Test organization | Inline `#[cfg(test)] mod tests` | Rust convention; tests stay next to code, cargo discovers automatically. | Plan |
| Phase granularity | 4 phases by coupling risk | Each phase is a compilable milestone without being so large it's unrecoverable. | Plan |
| AppState handling | Keep monolithic, move to `app.rs` | Zero Axum boilerplate changes; simplest path for a pure reorg. | Plan |
| Import paths | Expose submodule paths | Explicit — consumers see exactly which subfile a type lives in. | Plan |
| Feature flag (otel) | Move `RequestMetrics` into `proxy/` | Metrics are only used by proxy handlers; natural home. | Plan |
| routing.rs fate | Absorb into `config/routing.rs` | It's 159 lines of pure config types (RouteEntry, ProviderEntry). | Research |

## Scope

**In scope:**
- Delete dead `translate/` directory
- Split `protocol_translation.rs` → `protocol/` (3 subfiles)
- Split `persistence.rs` → `persistence/` (5 subfiles)
- Merge `config.rs` + `routing.rs` → `config/` (3 subfiles)
- Merge `intent_classifier.rs` + `fewshot_classifier.rs` → `classification/` (5 subfiles)
- Extract proxy handlers/streaming/upstream from main.rs → `proxy/` (4 subfiles)
- Move AppState + `build_app()` to `app.rs`
- Relocate ~4,900 lines of tests to their domain modules

**Out of scope:**
- Behavior changes, new features, API changes
- AppState decomposition into sub-states
- Adding `lib.rs` or changing crate type
- Dependency/Cargo.toml changes
- Test rewriting (tests move as-is)

## Architecture / Approach

Bottom-up extraction ordered by coupling risk. Leaf modules (zero dependencies) move first. The tight coupling cluster (config + classification) moves together in one phase. Main.rs proxy code moves next. Finally, AppState + tests redistribute. Each phase must produce a compiling codebase with all tests green.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Dead code + leaf extraction | `protocol/` and `persistence/` directories; dead `translate/` gone | Low — zero-coupling modules |
| 2. Config + classification cluster | `config/` and `classification/` directories; coupling triangle resolved | Medium — bidirectional type sharing requires simultaneous move |
| 3. Proxy extraction | `proxy/` directory; main.rs loses ~2,170 lines of handlers | Medium-high — largest single extraction, many function cross-references |
| 4. AppState + tests | `app.rs`; tests co-located; main.rs at ~250 lines | Low-medium — mechanical redistribution after handlers are out |

**Prerequisites:** All tests green on current branch. No in-flight changes to the same files.
**Estimated effort:** ~3-4 implementation sessions across 4 phases.

## Open Risks & Assumptions

- Phase 3 (proxy extraction) touches the most code — if handlers have unexpected private-function dependencies in main.rs, the split boundaries may need adjustment
- Exposed submodule paths mean every consumer's imports change — grep-and-replace must be exhaustive per phase
- `cache.rs` (165 lines, leaf) stays as a single file — if it grows, it can be extracted later

## Success Criteria (Summary)

- `cargo build && cargo test && cargo clippy` green after every phase
- `main.rs` ≤ 300 lines at completion
- Test count identical before and after (zero regressions)
