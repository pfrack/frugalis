---
date: "2026-06-29T20:10:01+02:00"
researcher: kiro
git_commit: 5a5c9b7c2c539dc087c293875d29647fcc252477
branch: sqlx-refactor
repository: pfrack/frugalis
topic: "Extend code-structure-reorg: routing_examples updates, test co-location, remaining cleanup"
tags: [research, codebase, code-structure, routing-examples, test-co-location]
status: complete
last_updated: "2026-06-29"
last_updated_by: kiro
---

# Research: Extend Code-Structure-Reorg

**Date**: 2026-06-29T20:10:01+02:00
**Researcher**: kiro
**Git Commit**: 5a5c9b7c2c539dc087c293875d29647fcc252477
**Branch**: sqlx-refactor
**Repository**: pfrack/frugalis

## Research Question

The original code-structure-reorg (phases 1–6) is archived and complete. What remains for a follow-up extension? Specifically: (1) updating routing_examples/ files, (2) test co-location improvements, and (3) any remaining structural cleanup.

## Summary

The original reorg achieved its goal — main.rs is ~200 lines, all domain code lives in subdirectories (proxy/, classification/, config/, protocol/, persistence/), and tests are co-located in their domain modules. The remaining work is modest:

1. **Routing examples** — 4 files still reference "Cerebrum" (old project name) in comments. They should say "Frugalis." Additionally, they don't demonstrate the multi-provider fallback format that the parser already supports.
2. **Test co-location** — Tests are already co-located everywhere. The only improvement opportunity is `cache.rs` which has a 90-line `test_app_with_cache()` helper that duplicates logic from `app::test_helpers` — this could be consolidated.
3. **Flat files** — None of the 7 flat files (auth.rs, cache.rs, cli.rs, dashboard.rs, quickstart.rs, telemetry.rs, test_util.rs) warrant extraction into subdirectories per the YAGNI principle and the "2-3 files" threshold rule.

## Detailed Findings

### 1. Routing Examples — Stale Comments + Missing Multi-Provider Demo

All 4 files in `routing_examples/` contain the comment:
```
# Cerebrum's merge semantics for [routing] are per-key additive — unmentioned
# routes inherit from the built-in defaults.
```

"Cerebrum" was renamed to "Frugalis" in commit history (archive: `2026-06-27-rename-cerebrum-to-frugalis`). The comment should read "Frugalis's merge semantics..."

The routing parser (`src/config/routing.rs:64-96`) supports two formats:
- **Flat (legacy)**: `model`, `endpoint`, `provider_type`, `api_key_env` at the route level — all 4 examples use this.
- **Multi-provider**: `providers = [{...}, {...}]` with per-provider `timeout_ms` — documented in code but never demonstrated in examples.

**Recommended changes:**
- Update all 4 comments: "Cerebrum" → "Frugalis"
- Add a 5th example file demonstrating multi-provider fallback (e.g., `routing-multi-provider.toml`)
- Optionally update the file header comments to reference the current config docs

### 2. Test Distribution — Already Co-located, One Dedup Opportunity

Tests were distributed in phases 5–6 of the original plan. Current state:

| Module | Test % | Status |
|--------|--------|--------|
| proxy/handlers.rs | 47% (1,521 lines) | ✅ Co-located |
| proxy/streaming.rs | 62% (823 lines) | ✅ Co-located |
| proxy/util.rs | 25% (164 lines) | ✅ Co-located |
| classification/* | 33–82% | ✅ Co-located |
| config/* | 23–67% | ✅ Co-located |
| persistence/* | 33–83% | ✅ Co-located |
| protocol/* | 29–56% | ✅ Co-located |
| cache.rs | **85%** (442 lines) | ✅ Co-located but duplicates test infra |
| dashboard.rs | 43% (308 lines) | ✅ Co-located |
| auth.rs | 22% (62 lines) | ✅ Co-located |

**The cache.rs deduplication opportunity:**

`src/cache.rs:84-175` contains `test_app_with_cache()` — a 90-line function that builds a full AppState with httpmock server, auth config, classifier chain, and routing. This duplicates ~80% of the logic in `src/app.rs:473-544` (`test_app_with_http_client()`). The only difference is that `test_app_with_cache` also configures `response_cache: Some(...)`.

A single additional helper in `app::test_helpers` (e.g., `test_app_with_cache(ttl, max_entries)`) would eliminate this duplication and reduce `cache.rs` tests from 442 to ~350 lines.

**Shared test infrastructure (`test_util.rs`):**

`src/test_util.rs` is a 12-line `EnvGuard` struct used by 5 test modules. It's correctly placed as a top-level utility — no move needed.

`src/app.rs:358-721` contains `pub(crate) mod test_helpers` (364 lines) — the central test infrastructure providing `test_app()`, `make_test_app_state()`, etc. Used by 6 files across the codebase. Already well-structured.

### 3. Flat File Assessment — No Extraction Warranted

Per the lesson rule "group source files into domain-named subdirectories when a module exceeds 2-3 files or crosses subsystem boundaries":

| File | Lines (prod) | Verdict | Rationale |
|------|-------------|---------|-----------|
| auth.rs | 221 | **Keep flat** | Single-concern leaf module, stable interface |
| cache.rs | 75 | **Keep flat** | 3 types + 4 methods — subdirectory would be absurd |
| cli.rs | 137 | **Keep flat** | Self-contained, single consumer |
| dashboard.rs | 416 | **Keep flat** | Cohesive UI module (nav + templates + handlers + routes) |
| quickstart.rs | 290 | **Keep flat** | Zero crate-internal imports, completely standalone |
| telemetry.rs | 192 | **Keep flat** | Feature-gated leaf, no tests, no complexity |
| test_util.rs | 12 | **Keep flat** | 12 lines, utility struct |

None exceeds the "2-3 files" threshold. None crosses subsystem boundaries. YAGNI applies uniformly.

## Code References

- `src/config/routing.rs:14-20` — ProviderEntry struct definition
- `src/config/routing.rs:43-47` — RouteEntry with `providers: Vec<ProviderEntry>`
- `src/config/routing.rs:64-96` — RouteEntryRaw + From impl (dual format support)
- `src/config/routing.rs:25-41` — Doc comment showing multi-provider TOML format
- `src/config/mod.rs:381-386` — Overlay merge logic (per-key additive)
- `src/config/loader.rs:293-356` — `routing_from_value` parsing function
- `src/cache.rs:84-175` — `test_app_with_cache()` helper (dedup candidate)
- `src/app.rs:473-544` — `test_app_with_http_client()` (pattern to follow)
- `src/app.rs:358-721` — `pub(crate) mod test_helpers` (shared test infra)
- `routing_examples/routing-manual-tests.toml:4-5` — Stale "Cerebrum" comment
- `routing_examples/routing_unreachable.toml:4-5` — Stale "Cerebrum" comment
- `routing_examples/routing-openrouter.toml:4-5` — Stale "Cerebrum" comment
- `routing_examples/routing-nvidia-nim.toml:4-5` — Stale "Cerebrum" comment

## Architecture Insights

1. **The routing config parser is forward-compatible.** The `RouteEntryRaw` → `RouteEntry` `From` impl means both flat (`model`/`endpoint` at top level) and array (`providers: [...]`) formats work. Existing configs need no migration — they keep working unchanged.

2. **Test helpers are centralized in `app::test_helpers`.** This is a deliberate design: all modules that need a full AppState for integration testing import from `crate::app::test_helpers`. The one deviation is `cache.rs` which built its own helper pre-reorg and was never consolidated.

3. **The original reorg deliberately left flat files flat.** The plan's "Desired End State" explicitly lists `dashboard.rs`, `auth.rs`, `cache.rs`, `telemetry.rs`, `quickstart.rs` as "unchanged" — this was a conscious decision, not an oversight.

## Historical Context (from prior changes)

- `context/archive/2026-06-28-code-structure-reorg/plan.md` — Original plan with phases 1–6, all completed
- `context/archive/2026-06-28-code-structure-reorg/research.md` — Module coupling matrix that informed extraction decisions
- `context/archive/2026-06-27-rename-cerebrum-to-frugalis/` — Rename that should have updated routing_examples but didn't
- `context/archive/2026-06-28-add-response-cache/` — Introduced cache.rs with its own test infrastructure (before the reorg consolidated test helpers)
- `context/archive/2026-06-24-provider-fallback-cascade/` — Introduced multi-provider `providers: Vec<ProviderEntry>` format

## Related Research

- `context/archive/2026-06-28-code-structure-reorg/research.md` — Original research informing the reorg

## Open Questions

1. **Should routing_examples/ move into a different location?** Currently at project root. Options: keep at root (visible to users), move to `examples/routing/`, or move to `docs/routing/`. Low priority — current location is fine.
2. **Should the multi-provider example use real provider pairs?** E.g., Anthropic primary → OpenRouter fallback. Or should it be a synthetic example with placeholder URLs?
3. **Is the cache.rs test helper dedup worth a phase?** It's ~90 lines of savings. The refactoring is straightforward (add `response_cache: Option<Arc<ResponseCache>>` param to an existing helper) but touches test infrastructure shared across many modules.

## Actionable Work Items (for plan.md)

Based on this research, the extension work breaks into:

**Phase A — Routing Examples Update (low risk, no code changes):**
- Replace "Cerebrum" → "Frugalis" in all 4 file comments
- Add `routing-multi-provider.toml` demonstrating the `providers = [...]` format
- Optionally add `cost_per_1m_input_tokens` field to one example

**Phase B — Test Infrastructure Dedup (low risk, test-only changes):**
- Add `test_app_with_cache(ttl_secs, max_entries)` to `app::test_helpers`
- Refactor `cache.rs` tests to use the shared helper instead of local `test_app_with_cache()`
- Delete the local helper from `cache.rs`
- Verify: `cargo test cache` passes

Both phases are small, independent, and can be done in either order.
