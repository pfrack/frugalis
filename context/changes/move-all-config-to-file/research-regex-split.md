---
date: 2026-06-10T22:39:05+02:00
researcher: Kiro
git_commit: 23fbef7
branch: main
repository: cerebrum
topic: "Split RegexClassifier into engine vs data — move all patterns/categories/costs/thresholds to config"
tags: [research, codebase, regex-classifier, config-driven, refactoring, intent-classification]
status: complete
last_updated: 2026-06-10
last_updated_by: Kiro
---

# Research: Split RegexClassifier into Engine + Config-Driven Data

**Date**: 2026-06-10T22:39:05+02:00
**Researcher**: Kiro
**Git Commit**: `23fbef7`
**Branch**: `main`
**Repository**: cerebrum

## Research Question

Move all settings of the RegexClassifier to config.toml (embedded or user-provided via `CONFIG_PATH`). Nothing related to categories, costs, patterns, or thresholds should be hardcoded. The RegexClassifier should be transparent — it works with any data provided. Additionally, split `intent_classifier.rs` into two files.

## Summary

The `src/intent_classifier.rs` file (45,595 bytes) mixes two concerns:

1. **Engine** — The `RegexClassifier` struct, `IntentClassify` trait, `ClassifierChain`, `LLMClassifier`, scoring algorithm, sanitization, routing logic
2. **Data** — Hardcoded regex patterns (44 positive + 4 negative), weight arrays (4), category definitions (4), model costs (4 entries), `SHORT_PROMPT_LEN`, `NEGATIVE_META` suppression rules

The proposed split:
- **`src/intent_classifier.rs`** — Pure engine: structs, traits, algorithms. Accepts any `CategoryConfig` (now extended with patterns/weights) and builds a classifier from it. Zero knowledge of FILE_READING, SYNTAX_FIX, etc.
- **`src/regex_defaults.rs`** — Default pattern data that gets embedded into `config.toml`. Provides compile-time defaults that are *replaced* entirely when a user supplies their own config.

After this change, a user who provides a `CONFIG_PATH` with their own `[[categories]]` section (including patterns and weights) gets a completely custom classifier with no trace of the built-in FILE_READING/SYNTAX_FIX/etc. categories.

## Detailed Findings

### 1. Current Architecture — What Lives in `intent_classifier.rs`

| Component | Lines | Type | Destination |
|-----------|-------|------|-------------|
| `hardcoded_model_costs()` | 16–23 | DATA | `config.toml` `[model_costs]` section (already exists, just remove hardcoded seed) |
| `CategoryConfig` struct | 41–48 | SCHEMA | stays in `intent_classifier.rs` (extended with patterns/weights) |
| `hardcoded_categories()` | 52–79 | DATA | `config.toml` `[[categories]]` (already exists, extend with patterns) |
| `ClassificationResult` struct | 81–89 | ENGINE | stays |
| `ClassificationTier` enum | 91–95 | ENGINE | stays |
| `IntentClassify` trait | 97–106 | ENGINE | stays |
| `RegexClassifier` struct | 108–116 | ENGINE | stays |
| `ClassifierChain` struct | 121–150 | ENGINE | stays |
| `LLMClassifier` struct + impl | 153–310 | ENGINE | stays |
| `PatternMeta` struct | 278–281 | ENGINE | stays |
| `NegativeMeta` struct | 283–287 | ENGINE/DATA | struct stays, instances → config |
| `NEG_COUNT` | 291 | DATA | derived from config at runtime |
| `FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS` | 295–298 | DATA | → config `[[categories]].patterns[].weight` |
| `SHORT_PROMPT_LEN` | 302 | DATA | → config `[regex_classifier].short_prompt_len` |
| `FILE_READING` patterns | 306–330 | DATA | → config `[[categories]].patterns` |
| `COMPLEX_REASONING` patterns | 320–350 | DATA | → config `[[categories]].patterns` |
| `SYNTAX_FIX` patterns | 338–362 | DATA | → config `[[categories]].patterns` |
| `CASUAL` patterns | 351–358 | DATA | → config `[[categories]].patterns` |
| `NEGATIVE` patterns | 358–366 | DATA | → config `[[negative_patterns]]` |
| `NEGATIVE_META` | 366–380 | DATA | → config `[[negative_patterns]].suppressed` + `.penalty` |
| `build_all_patterns()` | 325–380 | ENGINE | stays — but refactored to read from `CategoryConfig.patterns` |
| `fallback_category()` | 380–386 | ENGINE | stays — already generic (highest priority) |
| `classify_internal()` | 413–461 | ENGINE | stays — remove hardcoded "SYNTAX_FIX" dual-threshold |
| `route_match()` / `route_fallback()` | 463–484 | ENGINE | stays |
| `sanitize()` | 318–324 | ENGINE | stays |
| `code_block_re()` | 312–316 | ENGINE | stays |
| `auth_headers_for()` | 335–348 | ENGINE | stays |
| `build_llm_classifier_prompt()` | 268–276 | ENGINE | stays (already uses `categories` dynamically) |

### 2. What `CategoryConfig` Needs to Become

**Current** (intent_classifier.rs:41–48):
```rust
pub(crate) struct CategoryConfig {
    pub name: String,
    pub description: String,
    pub threshold: u32,
    pub priority: u8,
}
```

**Proposed** — extend with patterns, weights, and optional dual-threshold:
```rust
pub(crate) struct CategoryConfig {
    pub name: String,
    pub description: String,
    pub threshold: u32,
    pub priority: u8,
    pub patterns: Vec<PatternEntry>,
    pub dual_threshold: Option<DualThreshold>,
}

pub(crate) struct PatternEntry {
    pub regex: String,
    pub weight: u8,
}

pub(crate) struct DualThreshold {
    pub alt_score: u32,           // e.g. 4 for SYNTAX_FIX
    pub suppress_if_present: String, // category name whose score must be 0
}
```

### 3. What `[[negative_patterns]]` Looks Like in Config

Currently hardcoded as two parallel arrays (`NEGATIVE` + `NEGATIVE_META`). Config-driven version:

```toml
[[negative_patterns]]
regex = '(?i)\b(?:read|show|display|cat|view|open)\s+(?:the|this|my|a)\s+\w*(?:architecture|design|system|pattern|refactor)'
suppressed = "COMPLEX_REASONING"
penalty = 2

[[negative_patterns]]
regex = '(?i)\b(?:fix|correct|repair)\s+(?:the|this|my)\s+(?:compile|syntax|typo|lint|warning|error)'
suppressed = "COMPLEX_REASONING"
penalty = 2

[[negative_patterns]]
regex = '(?i)\b(?:design|architect|refactor|rearchitect|restructure)\s+(?:a|the|an)\s+(?:fix|solution|remedy|patch|workaround)'
suppressed = "SYNTAX_FIX"
penalty = 2

[[negative_patterns]]
regex = '(?i)\b(?:explain|describe|tell\s+me\s+about|what\s+do\s+you\s+think\s+about)\s+(?:the|this|that)\s+(?:file|code|class|module)'
suppressed = "FILE_READING"
penalty = 2
```

### 4. What `build_all_patterns()` Becomes

**Current** — a giant `match` on category name to select hardcoded pattern arrays:
```rust
match config.name.as_str() {
    "FILE_READING" => { for (i, p) in FILE_READING.iter().enumerate() { ... } }
    "COMPLEX_REASONING" => { ... }
    "SYNTAX_FIX" => { ... }
    "CASUAL" => { ... }
    unknown => { tracing::warn!(...); }
}
```

**New** — generic iteration over `CategoryConfig.patterns`:
```rust
fn build_all_patterns(
    categories: &[CategoryConfig],
    negative_patterns: &[NegativePatternConfig],
) -> (Vec<String>, Vec<PatternMeta>, Range<usize>) {
    let mut patterns = Vec::new();
    let mut metadata = Vec::new();

    for config in categories {
        for entry in &config.patterns {
            patterns.push(entry.regex.clone());
            metadata.push(PatternMeta {
                category: config.name.clone(), // now String, not &'static str
                weight: entry.weight,
            });
        }
    }

    let negative_start = patterns.len();
    for neg in negative_patterns {
        patterns.push(neg.regex.clone());
        metadata.push(PatternMeta {
            category: "NEG".to_string(),
            weight: 0,
        });
    }
    let negative_idx = negative_start..patterns.len();

    (patterns, metadata, negative_idx)
}
```

This eliminates the name-matching entirely. The engine doesn't care what categories are called.

### 5. What `classify_internal()` Changes

**Remove the hardcoded SYNTAX_FIX dual-threshold** (lines 718–726):
```rust
// BEFORE: hardcoded category name references
let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);
```

**After: config-driven dual-threshold loop**:
```rust
for (config, met_flag) in met.iter_mut() {
    if let Some(dt) = &config.dual_threshold {
        let score = *scores.get(config.name.as_str()).unwrap_or(&0);
        let suppress_score = *scores.get(dt.suppress_if_present.as_str()).unwrap_or(&0);
        *met_flag = score >= dt.alt_score || (score >= config.threshold && suppress_score == 0);
    }
}
```

**Remove hardcoded "CASUAL" references** (lines 619, 749):
- `ClassificationResult::fallback()` already uses `DEFAULT_MODEL` const → replace with fallback_entry's model
- `route_match()` warning skip for "CASUAL" → remove the special case, warn for all missing routes

### 6. Changes to `PatternMeta`

Currently uses `&'static str` for category:
```rust
pub struct PatternMeta {
    pub category: &'static str,
    pub weight: u8,
}
```

Must change to owned `String` since patterns come from config at runtime:
```rust
pub struct PatternMeta {
    pub category: String,
    pub weight: u8,
}
```

### 7. The File Split

#### `src/intent_classifier.rs` (engine only — keeps):
- `IntentClassify` trait
- `ClassificationResult` + `ClassificationTier`
- `RegexClassifier` struct + `from_env()` / `from_values()` / `classify_internal()`
- `ClassifierChain` struct + impl
- `LLMClassifier` struct + impl
- `PatternMeta`, `NegativePatternConfig` (new struct replacing `NegativeMeta`)
- `CategoryConfig` (extended with `patterns` + `dual_threshold`)
- `PatternEntry`, `DualThreshold` structs
- `build_all_patterns()` (refactored to be generic)
- `classify_internal()` (refactored — no hardcoded names)
- `fallback_category()` (already generic)
- `sanitize()`, `code_block_re()`
- `auth_headers_for()`
- `build_llm_classifier_prompt()` (already dynamic)
- `route_match()`, `route_fallback()`
- Tests that exercise the engine generically

#### `src/regex_defaults.rs` (default data — new file):
- `default_categories() -> Vec<CategoryConfig>` — returns the 4 default categories with their patterns and weights
- `default_negative_patterns() -> Vec<NegativePatternConfig>` — returns the 4 negative suppression rules
- `default_model_costs() -> HashMap<String, f64>` — returns the 4 hardcoded costs (or empty, since config.toml has `[model_costs]`)
- `DEFAULT_SHORT_PROMPT_LEN: usize = 30`

This file is only used as a **fallback** when config.toml parsing fails. In normal operation, all data comes from config.toml.

### 8. Config.toml Schema Extension

The existing `[[categories]]` section gets `patterns` and optional `dual_threshold`:

```toml
[[categories]]
name = "FILE_READING"
description = "Reading, viewing, inspecting, searching, or navigating files or code"
threshold = 3
priority = 1
patterns = [
    { regex = '(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file)\b', weight = 3 },
    { regex = '(?i)\b(?:show|display|print|cat)\s+(?:me\s+)?(?:the\s+)?(?:content|output)(?:\s+of)?', weight = 3 },
    # ... all 12 patterns with weights
]

[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs, errors, typos, compilation issues, or broken code"
threshold = 3
priority = 2
patterns = [
    { regex = '(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo)\b', weight = 3 },
    # ... all 11 patterns with weights
]

[SYNTAX_FIX.dual_threshold]
alt_score = 4
suppress_if_present = "FILE_READING"
```

The `[regex_classifier]` section also gets `short_prompt_len`:
```toml
[regex_classifier]
enabled = true
short_prompt_len = 30
```

### 9. Changes to `config.rs` — New Loaders

#### `load_categories_from_value()` — Extended

Currently parses 4 fields. Needs to also parse:
- `patterns` — TOML array of inline tables `{ regex = "...", weight = N }`
- `dual_threshold` — optional sub-table under `[CATEGORY_NAME.dual_threshold]`

**Problem**: Dual threshold is currently inside `classify_internal()` not on the category config. It needs to be lifted to the config layer.

**Alternative approach**: The dual_threshold could be a field on `[[categories]]`:
```toml
[[categories]]
name = "SYNTAX_FIX"
# ...
dual_threshold_alt_score = 4
dual_threshold_suppress_if_present = "FILE_READING"
```

Or as an inline table:
```toml
[[categories]]
name = "SYNTAX_FIX"
dual_threshold = { alt_score = 4, suppress_if_present = "FILE_READING" }
```

#### `load_negative_patterns_from_value()` — New

Parses `[[negative_patterns]]` array from TOML:
```rust
pub(crate) struct NegativePatternConfig {
    pub regex: String,
    pub suppressed: String,
    pub penalty: u8,
}
```

#### `RegexClassifierConfig` — Extended

```rust
pub(crate) struct RegexClassifierConfig {
    pub enabled: bool,
    pub short_prompt_len: usize,  // NEW
}
```

### 10. Changes to `hardcoded_model_costs()`

Currently seeds the cost map with 4 vendor-specific entries. After this change:
- Remove the function entirely (or make it return empty `HashMap`)
- `build_model_costs()` in config.rs starts from empty and only uses `[model_costs]` + per-route overrides

This means `config.toml` must define all model costs explicitly. The embedded `config.toml` already has a `[model_costs]` section — just populate it with the same 4 entries that were hardcoded.

### 11. Removal of `hardcoded_categories()`

After the split:
- `hardcoded_categories()` is replaced by `regex_defaults::default_categories()`
- It only fires if `config.toml` has no `[[categories]]` section (which the embedded default always has)
- Since embedded `config.toml` always exists (include_str!), the fallback is effectively dead code in production — but kept for robustness

### 12. Impact on Tests

Tests in `intent_classifier.rs` currently use `hardcoded_categories()` and `test_classifier()` helper to construct a `RegexClassifier`. After the split:
- `test_classifier()` calls `regex_defaults::default_categories()` (or constructs test-specific categories with patterns)
- Tests can also construct completely custom categories to verify engine generality
- The engine tests stay in `intent_classifier.rs`
- No test should depend on specific category names being FILE_READING etc.

### 13. `build_llm_classifier_prompt()` — Few-Shot Examples

Lines 377–380 hardcode:
```
- "read the file src/main.rs" -> FILE_READING
- "fix this compile error" -> SYNTAX_FIX
- "design a distributed system" -> COMPLEX_REASONING
- "hello how are you" -> CASUAL
```

After the change, these should be generated from the first pattern of each category (or removed entirely, since the `prompt_template_path` config already allows full override). The simplest fix: remove hardcoded few-shot examples from the auto-generated prompt. Users who want them can provide a `prompt_template_path`.

## Code References

- `src/intent_classifier.rs:16–23` — `hardcoded_model_costs()` (→ remove, use config)
- `src/intent_classifier.rs:41–79` — `CategoryConfig` + `hardcoded_categories()` (→ extend struct, move data to config)
- `src/intent_classifier.rs:108–116` — `RegexClassifier` struct (→ stays, but `PatternMeta.category` becomes `String`)
- `src/intent_classifier.rs:278–287` — `PatternMeta` + `NegativeMeta` (→ PatternMeta.category becomes String, NegativeMeta data → config)
- `src/intent_classifier.rs:291–302` — Weight arrays + constants (→ all to config)
- `src/intent_classifier.rs:306–380` — All pattern const arrays (→ to config)
- `src/intent_classifier.rs:325–380` — `build_all_patterns()` (→ refactor to generic)
- `src/intent_classifier.rs:413–461` — `classify_internal()` (→ remove hardcoded "SYNTAX_FIX" dual-threshold)
- `src/intent_classifier.rs:617–627` — `ClassificationResult::fallback()` (→ remove hardcoded "CASUAL")
- `src/intent_classifier.rs:749` — `route_match()` "CASUAL" special case (→ remove)
- `src/config.rs:369–415` — `load_categories_from_value()` (→ extend to parse patterns/weights/dual_threshold)
- `src/config.rs:417–434` — `build_model_costs()` (→ remove hardcoded seed)
- `src/config.rs:447–450` — `RegexClassifierConfig` (→ add `short_prompt_len`)
- `config.toml:48–71` — `[[categories]]` section (→ add patterns/weights inline tables)

## Architecture Insights

1. **The engine/data split is clean**: All hardcoded data lives in `const` arrays and `fn hardcoded_*()` functions. The engine already consumes `CategoryConfig` generically via `build_all_patterns()` — only the `match` arms inside that function tie it to specific names.

2. **The dual-threshold is the trickiest part**: It's a special-case rule embedded in the scoring algorithm that references category names by string. Making it config-driven requires a small DSL (alt_score + suppress_if_present). This is the only place where the engine "knows" about specific category names.

3. **PatternMeta lifetime change is unavoidable**: Currently `&'static str` because patterns are `const`. With config-driven patterns, ownership moves to the `RegexClassifier` instance. This requires changing `PatternMeta.category` from `&'static str` to `String`.

4. **The embedded config.toml IS the default data**: After this change, the file `config.toml` at the repo root (compiled in via `include_str!`) contains all default patterns, weights, categories, costs, and thresholds. `regex_defaults.rs` exists only as a last-resort fallback if TOML parsing completely fails — which shouldn't happen since the embedded file is validated at compile time.

5. **TOML array-of-inline-tables for patterns**: TOML supports `patterns = [{ regex = "...", weight = 3 }, ...]` which maps cleanly to `Vec<PatternEntry>`. This is readable and overridable per-category.

## Historical Context

- `context/archive/2026-06-07-shared-category-config/` — Prior change that introduced `CategoryConfig` as a shared struct
- `context/archive/2026-06-07-extract-generic-classifier-config/` — Attempted to extract classifier config (earlier iteration)
- `context/archive/2026-06-07-classifier-config-boundary/` — Drew the boundary between hardcoded and configurable
- `context/archive/2026-06-09-in-memory-config-filesystem/` — Established the embedded config.toml + overlay pattern
- `context/changes/move-all-config-to-file/plan.md` — Current plan (Phase 1–3 complete: env vars moved to config.toml)

## Open Questions

1. **TOML patterns readability**: 44 regex patterns inline in config.toml will make the file large (~200+ lines for patterns alone). Alternative: keep patterns in a separate `patterns.toml` referenced by path in `[regex_classifier].patterns_file`. But this adds complexity.
2. **Should `regex_defaults.rs` exist at all?** If the embedded config.toml always has all patterns, the Rust-level defaults are dead code. Counter-argument: the embedded config could have an invalid `[[categories]]` section if someone edits it wrong — the Rust fallback catches that.
3. **Should negative patterns be per-category or global?** Currently global (they suppress across categories). The proposed `[[negative_patterns]]` global array mirrors this. An alternative: `suppress = [{ regex = "...", penalty = 2 }]` field on each category that suppresses THAT category.
4. **LLM few-shot examples**: Remove from auto-generated prompt entirely, or generate one example per category from `description`?
