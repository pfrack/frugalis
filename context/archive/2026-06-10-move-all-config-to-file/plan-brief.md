# Split RegexClassifier into Engine + Config-Driven Data — Plan Brief

> Full plan: `context/changes/move-all-config-to-file/plan.md`
> Research: `context/changes/move-all-config-to-file/research-regex-split.md`

## What & Why

The `src/intent_classifier.rs` file (1232 lines) mixes two concerns: a generic regex classification engine and hardcoded category data (44 patterns, 4 weight arrays, model costs, negative suppression rules, dual-threshold logic referencing category names by string). We're splitting them so the engine becomes fully transparent — it works with any category config provided via `config.toml`. A user who supplies a custom `CONFIG_PATH` with their own `[[categories]]` (including patterns and weights) gets a completely custom classifier with zero trace of built-in FILE_READING/SYNTAX_FIX/etc. categories.

## Starting Point

Phases 1–3 of the existing plan are complete: all env-var-based config (`PORT`, `LOG_FORMAT`, `ALLOWED_ORIGINS`, `DEFAULT_MODEL`, etc.) has been moved to `config.toml`. The config loading pipeline (embedded `include_str!` + `CONFIG_PATH` overlay + per-section loader functions) is established and stable. What remains: the 48 hardcoded data items in `intent_classifier.rs` still bypass the config system entirely.

## Desired End State

- `CategoryConfig` carries `patterns: Vec<PatternEntry>` and optional `dual_threshold: Option<DualThreshold>` — all read from `config.toml`
- `config.toml` contains every pattern, weight, negative suppression rule, model cost, and threshold that was previously hardcoded
- `build_all_patterns()` iterates categories generically — zero `match` on category name
- `classify_internal()` drives dual-threshold logic from `DualThreshold` config, not from hardcoded `"SYNTAX_FIX"` / `"FILE_READING"` string lookups
- `PatternMeta.category` is `String` (owned) since patterns come from runtime config
- `hardcoded_model_costs()`, `hardcoded_categories()`, all pattern/weight arrays, `NEGATIVE_META`, and `SHORT_PROMPT_LEN` const are removed
- No `regex_defaults.rs` file — the embedded `config.toml` IS the default data
- Existing tests pass against config-driven categories; new tests verify engine works with fully custom category sets

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Patterns location | Inline in `config.toml` | Single source of truth, no extra path resolution, matches existing overlay model. | Plan |
| Rust defaults fallback | None — embedded config.toml is sufficient | `include_str!` bakes it into the binary; it can't be missing or corrupt at runtime. | Plan |
| Negative pattern scope | Global `[[negative_patterns]]` (current behavior) | Least behavior change risk; expressions like "explain this file" cross-suppress across categories. | Research |
| LLM few-shot examples | Generated dynamically from category config | Adapts to custom categories without hardcoding names; users who want hand-tuned examples provide `prompt_template_path`. | Plan |
| Dual-threshold TOML format | Inline table `dual_threshold = { alt_score, suppress_if_present }` | Self-contained within the category entry; clean TOML syntax. | Plan |
| Plan integration | Extend existing `plan.md` (Phase 4+) | One change-id, coherent progress tracking across all config migration work. | Plan |

## Scope

**In scope:**
- Extend `CategoryConfig` with `patterns`, `PatternEntry`, `DualThreshold`
- Add `NegativePatternConfig` struct and `[[negative_patterns]]` TOML loader
- Populate `config.toml` with all 44 positive patterns, 4 negative patterns, model costs
- Change `PatternMeta.category` from `&'static str` to `String`
- Refactor `build_all_patterns()` to be generic
- Refactor `classify_internal()` for config-driven dual-threshold
- Remove CASUAL special cases in `route_match()`, `fallback()`, `fallback_category()`
- Generate LLM few-shot examples dynamically
- Remove all hardcoded arrays and constants
- Remove `hardcoded_model_costs()`, `hardcoded_categories()`
- Update tests and add engine-generality tests

**Out of scope:**
- Creating `regex_defaults.rs` (embedded config serves this role)
- Changing the `merge_toml_values()` mechanism
- Adding serde-based deserialization (stays manual TOML extraction)
- Changing how `RegexSet` is compiled or how the scoring algorithm works
- Moving `[[categories]].patterns` to a separate file

## Architecture / Approach

```
Before:                                     After:
intent_classifier.rs (1232 lines)           intent_classifier.rs (~900 lines)
├── hardcoded_model_costs() [DATA]          ├── Engine only: structs, traits,
├── hardcoded_categories() [DATA]               algorithm, routing, tests
├── CategoryConfig (bare) [SCHEMA]          ├── CategoryConfig (extended with
├── 44+4 pattern arrays [DATA]                  patterns, dual_threshold)
├── 4 weight arrays [DATA]                  └── PatternEntry, DualThreshold,
├── NEGATIVE_META [DATA]                        NegativePatternConfig
├── build_all_patterns() (match-based)
├── classify_internal() (hardcoded SF)      config.toml (~350 lines)
├── engine: ClassifierChain, LLMClassifier  ├── [[categories]] with patterns,
│   routing, traits                             weights, dual_threshold
└── tests                                   ├── [[negative_patterns]]
                                            ├── [model_costs] (complete)
config.rs                                   ├── [regex_classifier].short_prompt_len
├── build_model_costs() seeds from          └── all existing sections
│   hardcoded_model_costs() [REMOVED]
├── load_categories_from_value()            config.rs
│   4 fields [EXTENDED: +patterns,           ├── build_model_costs() starts empty
│   +dual_threshold]                        ├── load_categories_from_value()
│                                           │   parses patterns, dual_threshold
│                                           ├── load_negative_patterns_from_value() [NEW]
│                                           └── RegexClassifierConfig
│                                               +short_prompt_len
```

The data flows: `config.toml` → TOML parser → loader functions → `CategoryConfig` (with patterns) / `NegativePatternConfig` → `build_all_patterns()` (generic) → `RegexSet` + `Vec<PatternMeta>` → scoring algorithm.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 4. Config Schema & Data Migration | Extended structs, TOML loaders, populated config.toml, PatternMeta.category→String, removed hardcoded_model_costs | Schema changes break existing callers; must update all construction sites simultaneously |
| 5. Engine Refactor | Generic build_all_patterns(), config-driven classify_internal(), CASUAL de-hardcoding, dynamic LLM examples, all hardcoded arrays removed | classify_internal() scoring behavior must match current output exactly — regression risk |
| 6. Tests & Cleanup | Updated test helpers, engine-generality tests, dead code removal, full suite green | Test changes are extensive; some tests assert on specific category names |

**Prerequisites:** Phases 1–3 complete (done). `config.toml` has `[model_costs]`, `[[categories]]`, `[regex_classifier]` sections (done).
**Estimated effort:** ~2–3 sessions across 3 phases.

## Open Risks & Assumptions

- **Dual-threshold regression**: The current SYNTAX_FIX dual-threshold is the only production user of this feature. Changing it to config-driven must produce identical scoring for the default config. The dual-threshold loop in `classify_internal()` must be verified against the current hardcoded logic before removing it.
- **TOML regex escaping**: The 44 regex patterns use `\b`, `\d`, `\w`, `\s`, `(?i)`, etc. TOML basic strings (`'...'`) don't interpret backslash escapes, which is correct for regex. But some patterns contain `'` (single quotes) — those must use TOML literal strings or escape properly.
- **CASUAL as lowest-priority fallback**: Currently `fallback_category()` hardcodes `"CASUAL"` as the `unwrap_or`. After the change, the fallback is the highest-priority (lowest `priority` number) category whose name sorts first — which with default config is FILE_READING (priority=1). This may change fallback behavior. Mitigation: keep "CASUAL" semantics by using the **lowest**-priority category (highest `priority` value) as fallback.
- **`config.toml` merge semantics for `[[categories]]`**: The plan assumes `[[categories]]` arrays in an overlay config completely replace the base (established in Phase 1.4). Verify this holds — a partial categories overlay would be confusing.

## Success Criteria (Summary)

- `cargo build --release` compiles with zero hardcoded category name references in engine code
- `cargo test` — full test suite passes with config-driven categories
- A user-provided `CONFIG_PATH` with custom `[[categories]]` (different names, patterns, thresholds) produces correct classification output
- The service starts and classifies correctly using only the embedded `config.toml` (no env vars beyond secrets)
