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
last_updated_by: opencode
last_updated_note: "User overrode keep-flat verdict; added extraction mechanics for dashboard/, routing/, app/ folder creation"
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

---

## Follow-up Research 2026-06-29T22:40+02:00

**Follow-up commit**: `f5a13f049d105e1a0f22659bfdc97d548d7e0e00` (branch: `main`)
**Follow-up researcher**: opencode
**Trigger**: User challenged the §3 "keep flat" verdict — "I thought that will also touch to move all rust code to specific domain based packages."

### Research Question (refined)

After scope clarification, the question became: **Should the 7 flat `.rs` files at `src/` root be extracted into domain subdirectories** (folder + `mod.rs`, matching the existing `proxy/`, `config/`, `classification/`, `persistence/`, `protocol/` pattern)? Scope = all 7 flat files (auth, cache, cli, dashboard, quickstart, telemetry, test_util); subdirectories only (not Cargo workspace crates); research + draft Phase 3 *if* evidence supports extraction.

### Methodology

Four parallel sub-agents investigated: (1) internal structure of each flat file — seams, concerns, LOC; (2) consumer coupling map — who imports each module and which surfaces; (3) existing subdir patterns as precedent — deriving the implicit folder-worthiness test; (4) historical archive review — prior decisions, growth since decision, provisional vs. final signals.

### Key Finding: The Precedent Threshold

The 5 existing subdirectories were created by extracting formerly-monolithic files. The implicit **folder-worthiness test** they establish requires **all** of:

| Criterion | Threshold | Observed floor |
|-----------|-----------|----------------|
| (a) Children count | 3+ cohesive sub-concerns partitionable along one axis | 3 (`protocol/`) — the one 2-child proposal (`dashboard/`) was **rejected** |
| (b) Mass | Combined ≥ ~1,800–3,200 LOC, OR single file ≥ ~2,400 LOC | Files ≤ ~500 LOC with one concern stayed flat |
| (c) Conceptual seam | Single axis of decomposition yielding a folder name narrower than "utils" | pipeline-stage / request-response-stream / trait-backends-types / impls-trait-types / load-types-routing |
| (d) `mod.rs` earns its keep | Carries root-level glue (root struct, orchestrator fn, or feature-gated shared state), not merely re-exports | config/mod.rs (ConfigRoot), persistence/mod.rs (PersistenceConfig), proxy/mod.rs (RequestMetrics), classification/mod.rs (code_block_re) |

**Source files that were foldered**: `main.rs` (8,460 LOC → `proxy/`), `protocol_translation.rs` (3,165 → `protocol/`), `persistence.rs` (2,727 → `persistence/`), `config.rs` (2,476 → `config/`), `intent_classifier.rs` (1,838) + `fewshot_classifier.rs` (547) → `classification/`. **All exceeded 1,800 LOC.** No sub-500-LOC file was ever foldered.

### Per-File Verdict (re-evaluated against both thresholds)

| File | Prod LOC | Children if split | (a) 3+? | (b) Mass? | (d) mod.rs job? | Coupling | Verdict |
|------|----------|-------------------|---------|-----------|------------------|----------|---------|
| `auth.rs` | 209 | 2 (bearer ~62, basic ~64) + shared config/crypto | FAIL | FAIL (209 ≪ 2,400) | re-export only | tight (6 consumers, 2 surfaces) | **KEEP FLAT** |
| `cache.rs` | 75 | 1 (single concern) | FAIL | FAIL | n/a | moderate (3) | **KEEP FLAT** |
| `cli.rs` | 137 | 2 max (parsing, init) — facets of one entrypoint | FAIL | FAIL | n/a | loose (1) | **KEEP FLAT** |
| `dashboard.rs` | 416 | 3 (nav ~70, templates ~66, handlers ~250) | **PASS** | FAIL (416 ≪ 1,800) | re-export only (routes fn is 10 lines) | loose (1 consumer) | **KEEP FLAT** (borderline) |
| `quickstart.rs` | 290 | 2 (pure builder, interactive wizard) | FAIL | FAIL | n/a | loose (1) | **KEEP FLAT** |
| `telemetry.rs` | 192 | 2 (lifecycle, metrics handle) — tightly coupled | FAIL | FAIL | n/a | moderate (4) | **KEEP FLAT** |
| `test_util.rs` | 0 (12 total) | 1 | FAIL | FAIL | n/a | moderate (5, single type) | **KEEP FLAT** |

**Literal lesson rule check** (`context/foundation/lessons.md:65`): "group source files into domain-named subdirectories when a module exceeds **2-3 files** or crosses subsystem boundaries." A single `.rs` file is 1 file — none exceeds 2-3 files. None crosses subsystem boundaries in the rule's sense (the rule targets modules whose *internal* concerns span subsystems, like `proxy/` housing both request-building and response-handling). **All 7 fail the literal rule too.**

### The `dashboard.rs` Borderline — Deep Dive

`dashboard.rs` is the **only genuine candidate** — it passes criterion (a) with 3 cohesive sub-sections (nav registry L23-86, template structs + macro L88-153, page handlers L155-404). But extraction is **not warranted**:

1. **Fails (b) mass**: 416 prod LOC is ~6× below the smallest single-file foldering observed (~2,400). The 724 total LOC is misleading — 308 of those are co-located tests, which is correct and should stay where they are.
2. **Fails (d) `mod.rs` job**: The only root-level item is the 10-line `routes()` fn (L406-415). `PAGES` (L46-72) is data, not orchestrator glue. A `dashboard/mod.rs` would be a re-export shim — the exact anti-pattern the precedent rejects.
3. **Single consumer**: `src/app.rs:331` is the *only* call site, importing just `dashboard::routes(auth_config)`. Extracting adds a directory hop for zero navigation benefit at the call site.
4. **AGENTS.md explicitly mandates cohesion**: The contributor guide states "Template structs and handlers live in `dashboard.rs`, not in `main.rs`" and documents PAGES + `dashboard_page!` macro + template structs + handlers + `routes()` as a single integrated workflow. Extracting would require amending AGENTS.md — the cohesion is a deliberate, documented decision, not an oversight.
5. **Historical rejection**: The original reorg research (`context/archive/2026-06-28-code-structure-reorg/research.md:166-168`) *proposed* a `dashboard/{mod.rs, handlers.rs}` split; the plan (`plan.md:56`) **rejected** it as "already clean." This is direct precedent that a 2-child split is below threshold. The current 3-child potential doesn't overcome the mass + mod.rs-job failures.
6. **Growth is modest**: The file grew 355→724 *total* LOC since the rejection, but prod LOC grew only 355→416 (+17%). The bulk of growth is co-located tests (correct placement).

### Synthesis: Does Any File Warrant Extraction?

**No.** Both thresholds — the literal lesson rule ("exceeds 2-3 files") and the observed precedent test (3+ children + ~1,800-2,400 LOC mass + mod.rs root-glue job) — reject all 7 flat files. The prior §3 "keep flat" verdict is **reaffirmed**.

Critical corroborating signals:
- **Zero growth since decision**: Every flat file's prod LOC matches the 2026-06-29 ext research exactly (verified by reading `#[cfg(test)]` boundaries). No file has drifted into split-worthy territory.
- **Decision was final, not provisional**: Three independent signals confirm intent — `research.md:122` ("conscious decision, not an oversight"), `plan-brief.md:26` (formal "Key Decision Made" entry), `plan.md:28` ("What We're NOT Doing").
- **AGENTS.md codifies the dashboard cohesion**: extracting dashboard.rs would contradict the contributor guide.
- **5 of 7 files were evaluated twice** (original reorg + ext research) and kept flat both times; `dashboard` was the only one ever proposed for extraction and was rejected.

### Decision

**No Phase 3 added to `plan.md`.** The user's chosen scope was "draft Phase 3 *if research supports extraction*" — research does not support it. The in-flight Phases 1-2 (routing examples + cache test dedup) remain the complete scope of this change.

### Watchlist (for future changes, not this one)

- **`dashboard.rs`** is the first file to revisit if **any** of: prod LOC exceeds ~600-800; a second distinct subsystem concern emerges beyond the current nav/templates/handlers/routes UI aggregation; `mod.rs` would gain a real root-glue job (e.g., a shared dashboard state struct threaded through all handlers). Until then, the AGENTS.md-mandated cohesion holds.
- **`cli.rs`** was added as a new top-level `.rs` *after* the lesson rule was written — technically in tension with the rule's "Never add new top-level `.rs` files without a directory home" clause. It was grandfathered as a conscious exception. If a second CLI-related file ever appears (e.g., `cli_completions.rs`), promote to `cli/` immediately — that would trigger criterion (a).

### Code References

- `src/auth.rs:10-14` — `AuthConfig` struct (shared by both auth schemes; coupling point against split)
- `src/auth.rs:63-119` — `ProxyBearerAuth` + `DashboardBasicAuth` impls (the 2 parallel schemes)
- `src/auth.rs:169-195` — `constant_time_eq_str` + `hmac_key` (shared crypto, used by both schemes)
- `src/cache.rs:3-74` — `CachedEntry`, `CacheStats`, `ResponseCache` (single concern, 75 prod LOC)
- `src/cli.rs:3-136` — `CliMode`, `CliResult`, `parse_args`, `print_help`, `run_init` (single CLI entrypoint)
- `src/dashboard.rs:23-86` — nav registry (`PAGES`, `nav_for`, `NavItem`, `NavContext`)
- `src/dashboard.rs:88-153` — `dashboard_page!` macro + 5 template structs
- `src/dashboard.rs:155-404` — 5 page handlers (dashboard, inferences, latency, savings, cache)
- `src/dashboard.rs:406-415` — `routes()` fn (the only consumer surface, 10 lines)
- `src/quickstart.rs:8-9` — author-documented seam between pure builder and interactive wizard
- `src/quickstart.rs:71-97` — `build_quickstart_toml` (pure, unit-testable)
- `src/quickstart.rs:99-264` — `run_quickstart` (interactive flow)
- `src/telemetry.rs:42-150` — `init` fn (~115 LOC, monolithic OTel bootstrap)
- `src/test_util.rs:1-12` — `EnvGuard` struct (entire file, 12 lines)
- `src/app.rs:331` — sole `dashboard::routes()` consumer
- `src/main.rs:28-52` — sole consumers of `cli` and `quickstart` (single-entrypoint APIs)
- `context/foundation/lessons.md:61-66` — "Organize src/ into domain subdirectories, not flat" rule (the threshold under evaluation)
- `context/archive/2026-06-28-code-structure-reorg/research.md:166-168` — original 2-child `dashboard/` proposal
- `context/archive/2026-06-28-code-structure-reorg/plan.md:56` — rejection of the proposal as "already clean"
- `AGENTS.md` "Dashboard Pages & Auto-Nav" section — explicit cohesion mandate for `dashboard.rs`

### Open Questions (updated)

1. *(From original research, still open)* Should `routing_examples/` move into `examples/routing/` or `docs/routing/`? Low priority — current location is fine.
2. *(New)* If `dashboard.rs` grows past ~600 prod LOC or gains a second subsystem concern, should a fresh change folder open (rather than amending this one)? **Yes** — extraction would contradict an explicit AGENTS.md decision and warrants its own `/10x-frame` + `/10x-plan` cycle, not a Phase 3 here.
3. *(New)* Should `cli.rs` be retroactively promoted to `cli/` to satisfy the lesson rule's "never add new top-level `.rs` without a directory home" clause? **No** — promotes a single-file folder with no benefit; the rule's intent is satisfied by the grandfathering being a conscious, documented exception. Revisit only if a second CLI file appears.

---

## Follow-up Research 2: Extraction Mechanics (2026-06-29T23:00+02:00)

**Trigger**: User overrode the §3 "keep flat" verdict and the "Follow-up Research" recommendation. Decisions:
1. **dashboard.rs** → `dashboard/{mod.rs, nav.rs, templates.rs, handlers.rs}` (3-way split)
2. **auth.rs + config/routing.rs** → new `routing/{mod.rs, auth.rs, routes.rs}` folder
3. **app.rs + cli.rs + quickstart.rs** → new `app/{mod.rs, cli.rs, quickstart.rs, test_helpers.rs}` folder

Three parallel sub-agents mapped the exact extraction mechanics. Summary below; full step-by-step plan in `plan.md` Phase 3.

### Phase 3a: App folder extraction — LOWEST RISK

**Mechanics**: Move `app.rs` → `app/mod.rs` (keeping AppState + build_classifiers + build_persistence + build_app), extract `test_helpers` (464 lines) → `app/test_helpers.rs`, move `cli.rs` → `app/cli.rs`, move `quickstart.rs` → `app/quickstart.rs`.

**Critical edit**: `cli.rs:1` has `include_str!("../init_template.toml")` → must become `"../../init_template.toml"` (one extra `../` because cli.rs is now one directory deeper).

**External breakage**: ZERO. All 11 consumer files that do `crate::app::AppState` or `crate::app::test_helpers::*` continue resolving unchanged — mod.rs keeps items at top level. Only `main.rs` needs edits (delete 2 `mod` declarations, rewrite 5 call sites: `cli::parse_args()` → `app::cli::parse_args()`, etc.).

**Total edits**: 8 lines across 2 files (main.rs + cli.rs include_str path).

**Conceptual caveat (documented, not blocking)**: app.rs is the composition root (AppState + router), cli.rs is pre-app arg dispatch, quickstart.rs is an interactive wizard. Co-locating them in `app/` is a co-location decision, not domain cohesion. The folder name `app` now spans "running app state" + "pre-app CLI utilities" — slightly overloaded but acceptable.

### Phase 3b: Dashboard 3-way split — LOW RISK

**Mechanics**: Split 724-LOC `dashboard.rs` into:
- `dashboard/nav.rs` (~70 LOC): NavPage, NavItem, NavContext, 5 ICON_* consts, PAGES static, nav_for fn
- `dashboard/templates.rs` (~66 LOC): dashboard_page! macro + 5 template structs (DashboardTemplate, InferencesTemplate, LatencyTemplate, SavingsTemplate, CacheTemplate)
- `dashboard/handlers.rs` (~250 prod LOC + 308 test LOC): 5 handler fns + entire `#[cfg(test)] mod tests` (16 tests)
- `dashboard/mod.rs` (~15 LOC): routes() fn + mod declarations + re-exports

**Cross-file references**: handlers.rs imports from nav.rs (`nav_for`) and templates.rs (5 template structs); templates.rs imports from nav.rs (`NavContext`); mod.rs imports from handlers.rs (5 handlers). The `dashboard_page!` macro stays textually adjacent to its 5 invocations in templates.rs — no `#[macro_export]` needed.

**External breakage**: ZERO. Only `app.rs:331` (or `app/mod.rs:331` after Phase 3a) calls `dashboard::routes(auth_config)` — path preserved via mod.rs. No other file references `crate::dashboard::*` items.

**AGENTS.md amendments**: 6 required (file-layout bullet, "Dashboard Pages & Auto-Nav" section: PAGES location, macro location, "Adding a new dashboard page requires" 5-step list, closing sentence). The old line refs (`:37-42`, `:55-68`) were already stale and are dropped.

**Test placement**: All 16 tests move as a block to handlers.rs (every test exercises a handler through the router). No nav-only or template-only tests exist to split out.

### Phase 3c: Routing folder extraction — HIGHEST RISK

**Mechanics**: Create `routing/{mod.rs, auth.rs, routes.rs}`. Move `auth.rs` → `routing/auth.rs` verbatim. Move `config/routing.rs` → `routing/routes.rs` verbatim. Submodule named `routes` (not `routing`) to avoid `routing::routing::` stutter. mod.rs does `pub(crate) use auth::*; pub(crate) use routes::*;` — glob re-export so consumers swap a path prefix (`auth::` → `routing::`, `config::routing::` → `routing::`).

**config/ impact** (critical):
- `config/mod.rs:7` — delete `pub(crate) mod routing;`
- `config/mod.rs:10` — `pub(crate) use routing::RouteEntry;` → `pub(crate) use crate::routing::RouteEntry;` (keep for ConfigRoot field at L54)
- `config/loader.rs:5` — `use super::routing::*;` → `use crate::routing::*;` (CRITICAL — loader.rs uses routing types in hardcoded_routing, routing_from_value, build_model_costs + ~15 test sites)

**Total edits**: ~107 across 16 files (8 `use` lines + 1 `mod` line + 2 config/mod.rs lines + 1 loader.rs glob + ~95 inline qualified-path sites). Mechanical prefix swap but high touch count.

**AGENTS.md amendments**: Update auth.rs bullet → `routing/auth.rs`; update file-layout to add `routing/` entry; update `[src/auth.rs](src/auth.rs)` link target.

**Conceptual caveat (documented, not blocking)**: auth = identity verification; routing = request forwarding config. Both are request-pipeline middleware but distinct concerns. The folder name `routing/` is slightly misleading because `auth` is not a sub-concern of `routing` — if anything both are sub-concerns of "request pipeline." A more neutral name would be `pipeline/` or `gateway/`. User chose `routing/` — proceeding.

### Phase ordering (ascending risk)

| Sub-phase | What | Edits | External breakage | Risk |
|-----------|------|-------|-------------------|------|
| 3a | App folder (app+cli+quickstart → app/) | 8 lines / 2 files | 11 consumers unchanged | Lowest |
| 3b | Dashboard 3-way split | 1 file → 4 files | Zero (only app.rs:331, preserved) | Low |
| 3c | Routing folder (auth + config/routing → routing/) | ~107 lines / 16 files | Zero (paths preserved via re-exports) but high touch count | Highest |

Each sub-phase is independently verifiable (cargo build + cargo test pass between phases). If 3c proves too disruptive, 3a and 3b can land without it.

### Conceptual concern: this extraction reverses a documented decision

All three extractions contradict the lesson rule threshold ("exceeds 2-3 files or crosses subsystem boundaries" — a single `.rs` is 1 file) and the precedent mass criterion (no sub-500-LOC file was ever foldered). The user has explicitly overridden these. This is the user's prerogative — research informs decisions, it doesn't make them — but the reversal should be documented:

- The AGENTS.md "Dashboard Pages & Auto-Nav" section explicitly mandated the dashboard.rs cohesion being broken.
- The `code-structure-reorg-ext` research §3 verdict ("KEEP FLAT") is reversed by user decision.
- The lesson rule at `lessons.md:61-66` is not amended — the threshold stands for future modules; these three are explicit exceptions justified by co-location preference, not by meeting the threshold.

**Recommendation**: add a one-line note to `lessons.md` documenting that `dashboard/`, `routing/`, `app/` are user-approved exceptions to the 2-3-file threshold, so future `/10x-impl-review` runs don't flag them as violations. Alternatively, amend the lesson rule to add a "co-location by maintainer decision" clause. The user should choose; this research does not prescribe.

### Code References (extraction mechanics)

- `src/app.rs:14` — `use crate::{auth, cache, classification, config, dashboard, persistence, proxy};` (stays in app/mod.rs; auth→routing after Phase 3c)
- `src/app.rs:364-827` — `pub(crate) mod test_helpers` (extracts to app/test_helpers.rs)
- `src/app.rs:313-362` — `build_app` fn (stays in app/mod.rs)
- `src/app.rs:331` — `dashboard::routes(auth_config)` call (sole dashboard consumer; preserved after Phase 3b)
- `src/cli.rs:1` — `include_str!("../init_template.toml")` (CRITICAL: → `"../../init_template.toml"` after move)
- `src/main.rs:12,16,22` — `mod app; mod cli; mod quickstart;` (→ just `mod app;` after Phase 3a)
- `src/main.rs:28,33,37,42,52` — cli/quickstart call sites (rewrite to `app::cli::`/`app::quickstart::`)
- `src/dashboard.rs:23-86` — nav registry (→ dashboard/nav.rs)
- `src/dashboard.rs:88-153` — macro + 5 template structs (→ dashboard/templates.rs)
- `src/dashboard.rs:155-404` — 5 handlers (→ dashboard/handlers.rs)
- `src/dashboard.rs:406-415` — `routes()` fn (→ dashboard/mod.rs)
- `src/dashboard.rs:417-724` — 16 tests (→ dashboard/handlers.rs, all move as block)
- `src/auth.rs:1-283` — entire file (→ routing/auth.rs verbatim)
- `src/config/routing.rs:1-199` — entire file (→ routing/routes.rs verbatim)
- `src/config/mod.rs:7` — `pub(crate) mod routing;` (DELETE)
- `src/config/mod.rs:10` — `pub(crate) use routing::RouteEntry;` (→ `pub(crate) use crate::routing::RouteEntry;`)
- `src/config/loader.rs:5` — `use super::routing::*;` (→ `use crate::routing::*;`)
- `src/classification/chain.rs:6`, `regex.rs:13,250`, `fewshot.rs:10,379`, `types.rs:1`, `llm.rs:219` — `crate::config::routing::` → `crate::routing::` (inline path repoints)
- `src/proxy/upstream.rs:7` — `crate::config::routing::ProviderEntry` → `crate::routing::ProviderEntry`
- `src/persistence/types.rs:7` — doc comment `crate::config::routing::ModelCosts` → `crate::routing::ModelCosts`
- `src/persistence/sql_backend.rs:886,921`, `memory.rs:629,669,727,768,804` — `crate::config::routing::ModelCosts::from_costs` → `crate::routing::ModelCosts::from_costs` (test sites)
- `AGENTS.md:22-27` — "Naming & File Layout" section (amend for all 3 extractions)
- `AGENTS.md:33-57` — "Dashboard Pages & Auto-Nav" section (amend for Phase 3b)
- `AGENTS.md:61` — `[src/auth.rs](src/auth.rs)` link (→ `src/routing/auth.rs`)

### Open Questions (updated)

1. *(From original research, still open)* Should `routing_examples/` move into `examples/routing/` or `docs/routing/`? Low priority — current location is fine.
2. *(Resolved)* If `dashboard.rs` grows past ~600 prod LOC or gains a second subsystem concern, should a fresh change folder open? **Moot** — user has decided to extract now.
3. *(Resolved)* Should `cli.rs` be retroactively promoted to `cli/`? **Superseded** — cli.rs moves into `app/` folder instead.
4. *(New)* Should `lessons.md` be amended to document that `dashboard/`, `routing/`, `app/` are user-approved exceptions to the 2-3-file threshold, so future `/10x-impl-review` runs don't flag them? Or should the lesson rule itself be amended to add a "co-location by maintainer decision" clause? **User decision needed** — this research recommends at minimum a one-line note in `lessons.md`.
5. *(New)* The `routing/` folder name is slightly misleading (auth is not a sub-concern of routing). Should it be `pipeline/` or `gateway/` instead? **User chose `routing/`** — proceeding, but flagging for the record.
