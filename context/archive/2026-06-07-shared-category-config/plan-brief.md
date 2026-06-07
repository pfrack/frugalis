# Shared Category Configuration — Plan Brief

> Full plan: `context/changes/shared-category-config/plan.md`
> Research: `context/changes/shared-category-config/research.md`

## What & Why

Extract intent category definitions (names, descriptions, thresholds, priorities, model defaults) from six hardcoded locations inside `RegexClassifier` into a shared `CategoryConfig` backed by a `config.toml` file with hardcoded Rust fallbacks. This gives the future `LLMClassifier` (S-09) a single source of truth for category descriptions (instead of hardcoding its own copy) and lets operators adjust thresholds and priorities without a recompile.

## Starting Point

Today, `RegexClassifier` in `src/intent_classifier.rs` hardcodes category names (`CAT_FILE_READING`, etc.), pattern counts (`FR_COUNT`, etc.), thresholds (`FR_THRESHOLD`, etc.), pattern arrays, weights, negative suppression, routing entries, and the priority chain — all as independent private constants. `config.rs` mirrors the same four category names in `hardcoded_routing()`. The set of four categories is implicit — no `Vec`, no enum, no unified description. The `LLMClassifier` would need to hardcode its own copy of category descriptions (creating drift risk).

## Desired End State

A `CategoryConfig` struct with `name`, `description`, `threshold`, `priority`, and `model_env_var` fields, definable via `[[categories]]` table array in `config.toml` (renamed from `routing.toml`). When the file is absent, a hardcoded Rust fallback `fn hardcoded_categories()` provides the four defaults. Both `RegexClassifier` and future `LLMClassifier` receive the same config slice at construction time. All existing tests pass identically — no behavioral change, no trait changes, no API contract changes.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Config file name | `config.toml` (renamed from `routing.toml`) | General enough to carry both routing and category config. | User |
| Category definition format | TOML `[[categories]]` table array with all fields | Operator-editable thresholds and priorities without recompile; Rust fallback when absent. | User |
| Fields in shared config | name, description, threshold, priority, model_env_var | LLM needs description; regex needs threshold; both need priority; routing defaults need model_env_var. | User |
| SF dual-threshold handling | Single threshold in config; dual logic stays in `classify()` | Only SYNTAX_FIX has context-dependent threshold (low if FR=0, high otherwise); clean config prevails. | Research |
| Threshold field name | `threshold` (not `regex_threshold`) | Any future classifier (not just regex) could use threshold scoring. | User |
| CategoryConfig lifetime | Owned `String` fields (not `&'static str`) | TOML-loaded strings are heap-allocated; hardcoded fallback constructs Strings once at startup. | Plan |
| Pattern metadata lifetime | `&'static str` (unchanged) | Patterns are always compiled-in; no need to runtime-load regex patterns from TOML. | Research |
| NEGATIVE_META migration | Replace `CAT_*` constant refs with string literals | CAT_* constants are removed; string literals match `CategoryConfig.name` values. | Research |
| Fallback category | Derived from config by highest priority value | Self-correcting — no hardcoded `CAT_CASUAL` in two separate code paths. | Research |
| Route match miss | Add `tracing::warn!` for non-CASUAL HashMap misses | Silent fallback on routing mismatch is undetectable without telemetry. | Research |
| Test routing keys | Index-based `CATEGORIES[n].name` references | Compile-time resilience to renames (tests track automatically). | User |
| External file comments | Single doc comment on `CATEGORIES` listing all 7 consumer files | Centralized inventory for future rename audits. | User |
| `PatternMeta.category` type | `&'static str` (unchanged) | Category names for pattern scoring are static; TOML overrides thresholds/priorities only. | Plan |

## Scope

**In scope:**
- `CategoryConfig` struct definition + hardcoded fallback in `src/intent_classifier.rs`
- `[[categories]]` TOML loading in `src/config.rs`
- `config.toml` rename from `routing.toml` (backward compat: try config.toml, then routing.toml)
- `RegexClassifier` refactor: `build_all_patterns()`, `classify()`, `fallback()`, `from_env()`, NEGATIVE_META
- `hardcoded_routing()` refactor to iterate categories
- `tracing::warn!` in `route_match()` for unknown category
- Fallback category derivation from priority
- Test updates: routing key construction, verification test
- Doc comment on hardcoded categories listing consumer files

**Out of scope:**
- `LLMClassifier` implementation (S-09)
- New intent categories beyond the current four
- TOML-loading of regex patterns or weights (patterns stay compiled-in)
- Changing `ClassifierChain`, `IntentClassify` trait, `ClassificationResult` structure
- `dashboard.rs`, `persistence.rs` changes (categories are opaque strings)
- Renaming existing category names (API-breaking change)

## Architecture / Approach

`CategoryConfig` is a `pub(crate)` struct with owned `String` fields. A `fn hardcoded_categories() -> Vec<CategoryConfig>` provides the compile-time fallback. `config::load_categories()` tries to parse `[[categories]]` from `config.toml`; on failure, returns `hardcoded_categories()`. The loaded `Vec<CategoryConfig>` is passed to `RegexClassifier::from_env()` as an additional parameter.

Inside `RegexClassifier`, `build_all_patterns()` iterates the config, matches each name to its static pattern/weight array, and appends `NEGATIVE` last. `classify()` reads thresholds from config for the scoring check and uses the priority field for the tie-breaking chain. `ClassificationResult::fallback()` derives the catch-all category from `config.max_by_key(|c| c.priority)`.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. CategoryConfig + RegexClassifier refactor | `CategoryConfig` struct, hardcoded fallback, all classifier internals driven by config | NEGATIVE_META compile break if CAT_* refs missed; build_all_patterns ordering must keep NEGATIVE last |
| 2. config.toml support | `[[categories]]` TOML loading, `config.toml` rename, `hardcoded_routing()` refactor | Backward compat with existing `routing.toml` deployments; `key.to_uppercase()` normalization |
| 3. Tests + docs | Test routing keys from config, verification test, consumer-file doc comment | Test assertion strings must exactly match config values |

**Prerequisites:** S-07 (IntentClassify trait — already implemented), S-01a (Regex classifier working)
**Estimated effort:** ~1 session across 3 phases

## Open Risks & Assumptions

- `config.toml` rename is breaking for deployments that use `routing.toml` today; backward-compat fallback (`try config.toml, then routing.toml`) mitigates this
- If TOML `[[categories]]` names don't match the static pattern array names, those categories silently get no patterns — a `tracing::warn!` on mismatch is needed
- `key.to_uppercase()` normalization in the routing loader means category names must stay `[A-Z_]+` — documented in the consumer-files comment
- Test code uses `CATEGORIES[n].name` for routing keys; reordering `CATEGORIES` would shift which name each test inserts — verified by the new verification test

## Success Criteria (Summary)

- All existing `cargo test` and `cargo test slow_tests` pass with no behavioral change
- `config.toml` with `[[categories]]` overrides thresholds/priorities correctly
- Missing `config.toml` falls back to hardcoded defaults with zero errors
- `cargo test auth` and `cargo test routes_auth` pass unchanged
