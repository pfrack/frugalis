# Shared Category Configuration Implementation Plan

## Overview

Extract the four intent categories from six hardcoded locations inside `RegexClassifier` into a shared `CategoryConfig` struct, backed by an optional `config.toml` `[[categories]]` table array with hardcoded Rust fallbacks. This creates a single source of truth for category names, descriptions, thresholds, priorities, and model defaults — consumed by both the existing `RegexClassifier` and the future `LLMClassifier` (S-09).

## Current State Analysis

Category knowledge is scattered across `src/intent_classifier.rs` and `src/config.rs` with no shared definition:

- **6 definition points** in `intent_classifier.rs`: name constants (`CAT_FILE_READING` etc., lines 121–125), pattern count constants (`FR_COUNT` etc., lines 129–133), weight arrays (lines 137–140), threshold constants (`FR_THRESHOLD` etc., lines 145–149), pattern arrays (lines 153–214), and the `classify()` priority chain (lines 420–429).
- **Hardcoded routing** in `config.rs:19–57` mirrors the same four category name strings in four identical `routing.insert()` calls.
- **NEGATIVE_META** (lines 218–235) references the `CAT_*` constants that will be removed — a compile blocker if missed.
- **Two independent** fallback-to-CASUAL code paths (`ClassificationResult::fallback()` at line 323 and the short-prompt shortcut at line 400) both hardcode `CAT_CASUAL`.
- **7 external files** (TOML examples, OpenAPI spec, shell script, HTML template) hardcode category name strings with no link to the Rust source of truth.
- **`route_match()`** (line 433) silently falls back on HashMap miss — no telemetry for "classifier produced unknown category."

The `LLMClassifier` (S-09) needs category descriptions for its prompt template. Without a shared config, it would hardcode its own copy of descriptions — creating drift risk with the regex patterns.

## Desired End State

A `CategoryConfig` struct that is the single source of truth for all four categories. Operators can override thresholds, priorities, and descriptions via `[[categories]]` in `config.toml`. When the file is absent or invalid, hardcoded Rust defaults take over. Both classifiers consume the same config slice at construction time. All existing tests pass unchanged — zero behavioral difference.

### Key Discoveries:

- `NEGATIVE_META` references `CAT_COMPLEX_REASONING`, `CAT_SYNTAX_FIX`, `CAT_FILE_READING` — must switch to string literals (`src/intent_classifier.rs:218–235`)
- SF dual-threshold in `classify()` (`>= 4` OR `>= 3 AND FR == 0`) is the only cross-category coupling — stays as implementation detail (`src/intent_classifier.rs:404–407`)
- `build_all_patterns()` order must keep `NEGATIVE` last for `negative_idx` range to remain correct (`src/intent_classifier.rs:269–314`)
- `key.to_uppercase()` normalization in routing loader means category names must stay `[A-Z_]+` (`src/config.rs:109`)
- `route_match()` HashMap miss = silent fallback; needs `tracing::warn!` (`src/intent_classifier.rs:433`)

## What We're NOT Doing

- New intent categories (staying with the current four)
- TOML-loading of regex patterns or weights (patterns are compiled-in, not config-driven)
- Adding `description` to `RouteEntry`, `ClassificationResult`, or `InferenceRecord`
- Changing the `IntentClassify` trait, `ClassifierChain`, or `ClassificationResult` structure
- Renaming existing category strings (API-breaking change)
- Updating `dashboard.rs`, `persistence.rs`, or `auth.rs`

## Implementation Approach

Define `CategoryConfig` with owned `String` fields (compatible with TOML deserialization). A `fn hardcoded_categories() -> Vec<CategoryConfig>` provides compile-time defaults. `config::load_categories()` tries `config.toml` `[[categories]]` then falls back to hardcoded. The loaded `Vec<CategoryConfig>` is passed to `RegexClassifier::from_env()` at construction time.

Inside `RegexClassifier`, `build_all_patterns()` iterates the config, matches each name to its static `&[&str]` pattern + `&[u8]` weight array via a match or lookup, appending `NEGATIVE` last. `classify()` reads `threshold` and `priority` from the stored config. `ClassificationResult::fallback()` derives the catch-all from the config by max priority. `route_match()` emits `tracing::warn!` on non-CASUAL HashMap miss.

Pattern metadata (`PatternMeta.category`) and negative suppression (`NEGATIVE_META.suppressed`) stay `&'static str` — their values are always the static category name strings, regardless of TOML overrides.

---

## Phase 1: CategoryConfig struct + RegexClassifier internals

### Overview

Define `CategoryConfig`, the hardcoded fallback, and refactor all classifier internals from hardcoded constants to config-driven logic. This is the core of the refactor — everything else (TOML loading, test updates) builds on this.

### Changes Required:

#### 1. CategoryConfig struct definition

**File**: `src/intent_classifier.rs`

**Intent**: Add a `pub(crate)` struct that holds category identity + classifier-agnostic properties. Both `RegexClassifier` and `LLMClassifier` receive a `&[CategoryConfig]` at construction time.

**Contract**: Insert after the `RouteEntry` re-export block (after line 9), before `ClassificationResult`:

```rust
/// Single source of truth for intent category definitions.
/// Consumed by RegexClassifier (patterns, thresholds, routing) and
/// LLMClassifier (prompt template descriptions).
///
/// External files hardcoding category name strings:
/// - routing_examples/routing-*.toml (4 files) — section names
/// - openapi/completions.yaml — enum constraint values (line 44, 111)
/// - manual-test/run.sh — x-cerebrum-category header (line 179)
/// - templates/dashboard/inferences.html — placeholder text (line 19)
/// Category names are a PUBLIC API contract. Renaming any value here
/// is a breaking change requiring updates to all listed consumers.
/// Names must stay [A-Z_]+ for compatibility with key.to_uppercase()
/// normalization in the routing config loader.
#[derive(Clone, Debug)]
pub(crate) struct CategoryConfig {
    pub name: String,
    pub description: String,
    pub threshold: u32,
    pub priority: u8,
    pub model_env_var: Option<String>,
}

pub(crate) fn hardcoded_categories() -> Vec<CategoryConfig> {
    vec![
        CategoryConfig {
            name: "FILE_READING".to_string(),
            description: "Reading, viewing, inspecting, searching, or navigating files or code".to_string(),
            threshold: 3,
            priority: 1,
            model_env_var: Some("DEFAULT_MODEL_READING".to_string()),
        },
        CategoryConfig {
            name: "SYNTAX_FIX".to_string(),
            description: "Fixing bugs, errors, typos, compilation issues, or broken code".to_string(),
            threshold: 3,
            priority: 2,
            model_env_var: Some("DEFAULT_MODEL".to_string()),
        },
        CategoryConfig {
            name: "COMPLEX_REASONING".to_string(),
            description: "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization".to_string(),
            threshold: 3,
            priority: 3,
            model_env_var: Some("DEFAULT_MODEL_COMPLEX".to_string()),
        },
        CategoryConfig {
            name: "CASUAL".to_string(),
            description: "Simple questions, greetings, general conversation, or short prompts".to_string(),
            threshold: 1,
            priority: 4,
            model_env_var: Some("DEFAULT_MODEL".to_string()),
        },
    ]
}
```

#### 2. Remove category name constants

**File**: `src/intent_classifier.rs`

**Intent**: Delete `CAT_FILE_READING`, `CAT_COMPLEX_REASONING`, `CAT_SYNTAX_FIX`, `CAT_CASUAL`, `CAT_NEG` (lines 121–125). All references to these constants switch to either `config.name.as_str()` or string literals.

**Contract**: Remove lines 121–125.

#### 3. Remove pattern count constants, keep NEG_COUNT

**File**: `src/intent_classifier.rs`

**Intent**: Delete `FR_COUNT`, `CR_COUNT`, `SF_COUNT`, `CA_COUNT` (lines 129–132). Keep `NEG_COUNT` (line 133). Pattern counts are derived from array `.len()` at use sites.

**Contract**: Remove lines 129–132. Keep line 133 (`const NEG_COUNT: usize = 4;`).

#### 4. Remove threshold constants

**File**: `src/intent_classifier.rs`

**Intent**: Delete `FR_THRESHOLD`, `SF_THRESHOLD_HIGH`, `SF_THRESHOLD_LOW`, `CR_THRESHOLD`, `CA_THRESHOLD` (lines 145–149). Thresholds come from `CategoryConfig.threshold`.

**Contract**: Remove lines 145–149. Keep `SHORT_PROMPT_LEN` (line 144) — it's category-agnostic.

#### 5. Update NEGATIVE_META to string literals

**File**: `src/intent_classifier.rs`

**Intent**: Replace `CAT_COMPLEX_REASONING`, `CAT_SYNTAX_FIX`, `CAT_FILE_READING` references with the corresponding string literals. This is a compile blocker — if missed, the build fails with "cannot find value."

**Contract**: Replace lines 218–235:

```rust
const NEGATIVE_META: &[NegativeMeta] = &[
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "SYNTAX_FIX",         penalty: 2 },
    NegativeMeta { suppressed: "FILE_READING",       penalty: 2 },
];
```

#### 6. Refactor build_all_patterns() to iterate config

**File**: `src/intent_classifier.rs`

**Intent**: Replace five hardcoded per-category blocks (FR → CR → SF → CA → NEG) with a config-driven iteration that matches category name to static pattern/weight arrays. NEGATIVE must always be appended last.

**Contract**: New signature takes `categories: &[CategoryConfig]` and returns `(Vec<&'static str>, Vec<PatternMeta>, Range<usize>)`. The negative_idx range is computed from the positive count and returned directly — eliminating the need for `from_env()` to compute `negative_start` from removed constants.

```rust
fn build_all_patterns(categories: &[CategoryConfig]) -> (Vec<&'static str>, Vec<PatternMeta>, Range<usize>) {
    let mut patterns = Vec::new();
    let mut metadata = Vec::new();

    for config in categories {
        match config.name.as_str() {
            "FILE_READING" => {
                for (i, p) in FILE_READING.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "FILE_READING", weight: FR_WEIGHTS[i] });
                }
            }
            "COMPLEX_REASONING" => {
                for (i, p) in COMPLEX_REASONING.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "COMPLEX_REASONING", weight: CR_WEIGHTS[i] });
                }
            }
            "SYNTAX_FIX" => {
                for (i, p) in SYNTAX_FIX.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "SYNTAX_FIX", weight: SF_WEIGHTS[i] });
                }
            }
            "CASUAL" => {
                for (i, p) in CASUAL.iter().enumerate() {
                    patterns.push(*p);
                    metadata.push(PatternMeta { category: "CASUAL", weight: CA_WEIGHTS[i] });
                }
            }
            unknown => {
                tracing::warn!(category = %unknown, "CategoryConfig name has no pattern array");
            }
        }
    }

    let positive_count = metadata.len();
    let negative_start = positive_count;

    for p in NEGATIVE.iter() {
        patterns.push(*p);
        metadata.push(PatternMeta { category: "NEG", weight: 0 });
    }
    let negative_idx = negative_start..(negative_start + NEG_COUNT);

    (patterns, metadata, negative_idx)
}
```

#### 7. Add categories field to RegexClassifier

**File**: `src/intent_classifier.rs`

**Intent**: Store the config slice so `classify()` can read thresholds and priorities. Pattern metadata stays with static strings — the config provides scoring parameters.

**Contract**: Add to `RegexClassifier` struct (after `short_prompt_len`):
```rust
pub categories: Vec<CategoryConfig>,
```

#### 8. Refactor from_env() and from_values()

**File**: `src/intent_classifier.rs`

**Intent**: Accept `categories: Vec<CategoryConfig>` parameter. Call the new three-return `build_all_patterns(categories)`. Remove `FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT` computation (lines 340, 357).

**Contract**: 
- `from_env()` signature gains `categories: Vec<CategoryConfig>` parameter (fourth positional arg)
- `from_values()` signature gains `categories: Vec<CategoryConfig>` parameter
- Both call `let (patterns, metadata, negative_idx) = build_all_patterns(&categories);`
- Both store `categories` in the returned struct

#### 9. Refactor classify() scoring and priority

**File**: `src/intent_classifier.rs`

**Intent**: Replace hardcoded constants (`FR_THRESHOLD`, `CAT_FILE_READING`, etc.) and hardcoded priority chain (`if fr { } if sf { } if cr { }`) with config-driven lookups.

**Contract**: 

The scoring section (lines 404–429) becomes:

```rust
// Find config for each category by name (helper closure or inline)
let cfg = |name: &str| self.categories.iter().find(|c| c.name == name);

// Check thresholds
let mut met: Vec<(&CategoryConfig, bool)> = self.categories.iter()
    .map(|c| {
        let score = *scores.get(c.name.as_str()).unwrap_or(&0);
        (c, score >= c.threshold)
    })
    .collect();

// SF dual-threshold special case (SYNTAX_FIX only)
let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);

// Update the met flag for SYNTAX_FIX
if let Some(entry) = met.iter_mut().find(|(c, _)| c.name == "SYNTAX_FIX") {
    entry.1 = sf_met;
}

let met_count = met.iter().filter(|(_, m)| *m).count();

if met_count == 0 {
    return self.route_fallback(fallback_category(&self.categories));
}
if met_count >= 2 {
    return self.route_fallback(fallback_category(&self.categories));
}

// Sort by priority (lower = higher), pick first that met
met.sort_by_key(|(c, _)| c.priority);
for (config, is_met) in &met {
    if *is_met {
        return self.route_match(&config.name);
    }
}
// Unreachable (met_count >= 1 ensures at least one hit)
self.route_fallback(fallback_category(&self.categories))
```

Helper:
```rust
fn fallback_category(categories: &[CategoryConfig]) -> &str {
    categories.iter()
        .max_by_key(|c| c.priority)
        .map(|c| c.name.as_str())
        .unwrap_or("CASUAL")
}
```

Short-prompt shortcut (lines 397–401): replace `CAT_CASUAL` with `fallback_category(&self.categories)`.

#### 10. Add tracing::warn! in route_match()

**File**: `src/intent_classifier.rs`

**Intent**: Catch "classifier produced unknown category" at runtime instead of silently falling back. Non-CASUAL HashMap misses indicate a config mismatch.

**Contract**: In `route_match()` (line 432–442), after the `routing.get()` call:

```rust
fn route_match(&self, category: &str) -> ClassificationResult {
    if category != "CASUAL" && !self.routing.contains_key(category) {
        tracing::warn!(%category, "route_match: category not in routing table — falling back");
    }
    let route = self.routing.get(category).unwrap_or(&self.fallback_entry);
    // ... rest unchanged
}
```

#### 11. Refactor ClassificationResult::fallback()

**File**: `src/intent_classifier.rs`

**Intent**: Derive the catch-all category from the config by max priority instead of hardcoding `CAT_CASUAL`. This is the second of two fallback-to-CASUAL locations — both must agree.

**Contract**: Replace line 323. Since `ClassificationResult::fallback()` is called when no classifier is configured (before any config exists), keep the hardcoded `"CASUAL"` string here as a safe default. The `classify()` method's own fallback paths use `fallback_category(&self.categories)`.

### Success Criteria:

#### Automated Verification:

- Compiles: `cargo build`
- All classifier unit tests pass: `cargo test intent_classify`
- All auth tests pass: `cargo test auth`
- All route auth tests pass: `cargo test routes_auth`

#### Manual Verification:

- Classification output is identical to pre-refactor for representative prompts (read file, fix bug, architect system, hello)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation before proceeding to Phase 2.

---

## Phase 2: config.toml support

### Overview

Rename `routing.toml` → `config.toml` with backward compat, add `[[categories]]` TOML loading, refactor `hardcoded_routing()` to iterate categories, and wire category loading into `main()`.

### Changes Required:

#### 1. Rename default config path constant

**File**: `src/config.rs`

**Intent**: Change the default config file from `routing.toml` to `config.toml`. Backward compat: if `config.toml` isn't found, try `routing.toml`.

**Contract**: Update line 6:
```rust
pub(crate) const CONFIG_DEFAULT: &str = "config.toml";
pub(crate) const ROUTING_CONFIG_LEGACY: &str = "routing.toml";
```

Keep `ROUTING_CONFIG_DEFAULT` as deprecated alias or remove it after updating all call sites.

#### 2. Add load_categories() and load_categories_from_file()

**File**: `src/config.rs`

**Intent**: Load `Vec<CategoryConfig>` from `config.toml` `[[categories]]` section. On any failure, return `hardcoded_categories()`.

**Contract**: New functions:
```rust
use crate::intent_classifier::{CategoryConfig, hardcoded_categories};

pub(crate) fn load_categories() -> Vec<CategoryConfig> {
    let path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| CONFIG_DEFAULT.to_string());
    match load_categories_from_file(&path) {
        Ok(cats) => cats,
        Err(e) => {
            tracing::warn!("{e}; using hardcoded category defaults");
            hardcoded_categories()
        }
    }
}

fn load_categories_from_file(path: &str) -> Result<Vec<CategoryConfig>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {path}: {e}"))?;
    let root: toml::Value = toml::from_str(&content)
        .map_err(|e| format!("Invalid TOML in {path}: {e}"))?;
    let table = root.as_table()
        .ok_or_else(|| format!("Root must be a table in {path}"))?;

    let cats_array = match table.get("categories") {
        Some(toml::Value::Array(arr)) => arr,
        _ => return Err("No [[categories]] section found".to_string()),
    };

    let mut categories = Vec::new();
    for (i, cat) in cats_array.iter().enumerate() {
        let t = cat.as_table()
            .ok_or_else(|| format!("categories[{i}] must be a table"))?;
        let name = t.get("name").and_then(|v| v.as_str())
            .ok_or_else(|| format!("categories[{i}]: missing 'name'"))?
            .to_string();
        let description = t.get("description").and_then(|v| v.as_str())
            .unwrap_or("").to_string();
        let threshold = t.get("threshold").and_then(|v| v.as_integer())
            .unwrap_or(1) as u32;
        let priority = t.get("priority").and_then(|v| v.as_integer())
            .unwrap_or(99) as u8;
        let model_env_var = t.get("model_env_var").and_then(|v| v.as_str())
            .map(|s| s.to_string());

        categories.push(CategoryConfig { name, description, threshold, priority, model_env_var });
    }

    if categories.is_empty() {
        return Err("[[categories]] is empty".to_string());
    }
    Ok(categories)
}
```

#### 3. Update load_routing() for config.toml with backward compat

**File**: `src/config.rs`

**Intent**: Try `config.toml` first, then `routing.toml` for legacy deployments. Skip the `categories` key when parsing routing sections.

**Contract**: Update `load_routing()` (line 122):

```rust
pub(crate) fn load_routing() -> (HashMap<String, RouteEntry>, RouteEntry) {
    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| CONFIG_DEFAULT.to_string());

    // Try config.toml first, then routing.toml for backward compat
    let path = if std::path::Path::new(&config_path).exists() {
        config_path
    } else if std::path::Path::new(ROUTING_CONFIG_LEGACY).exists() {
        tracing::info!("Using legacy routing.toml; consider renaming to config.toml");
        ROUTING_CONFIG_LEGACY.to_string()
    } else {
        tracing::warn!("No config.toml or routing.toml found; using hardcoded routing defaults");
        return hardcoded_routing(&hardcoded_categories());
    };

    // ... rest of existing logic with the resolved path
}
```

Update `load_routing_from_file()` to also skip the `"categories"` key (line 79):

```rust
if key == "fallback" || key == "categories" {
    continue;
}
```

#### 4. Refactor hardcoded_routing() to iterate categories

**File**: `src/config.rs`

**Intent**: Replace four identical `routing.insert()` calls (lines 19–57) with a loop over categories. Use `model_env_var` from config to determine which env var provides the default model.

**Contract**: New signature takes `categories: &[CategoryConfig]`:

```rust
pub(crate) fn hardcoded_routing(categories: &[CategoryConfig]) -> (HashMap<String, RouteEntry>, RouteEntry) {
    let endpoint = env_or_default("NVIDIA_ENDPOINT", NVIDIA_ENDPOINT_DEFAULT);
    let mut routing = HashMap::new();

    for cat in categories {
        let model = match &cat.model_env_var {
            Some(env_var) => env_or_default(env_var, DEFAULT_MODEL),
            None => DEFAULT_MODEL.to_string(),
        };
        routing.insert(
            cat.name.clone(),
            RouteEntry {
                model,
                endpoint: endpoint.clone(),
                cost_per_1m_input_tokens: None,
                provider_type: "nvidia_nim".to_string(),
                api_key_env: Some("NVIDIA_API_KEY".to_string()),
            },
        );
    }

    let fallback = RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint,
        cost_per_1m_input_tokens: None,
        provider_type: "nvidia_nim".to_string(),
        api_key_env: Some("NVIDIA_API_KEY".to_string()),
    };
    (routing, fallback)
}
```

#### 5. Wire category loading into main()

**File**: `src/main.rs`

**Intent**: Load categories before constructing the classifier, pass to `from_env()`. Unchanged otherwise.

**Contract**: In `main()` (lines 73–101), add before the classifier construction block:

```rust
let categories = config::load_categories();
```

Update `RegexClassifier::from_env()` call to include `categories` as a parameter.

### Success Criteria:

#### Automated Verification:

- Compiles: `cargo build`
- All tests pass: `cargo test`
- Works without config file (hardcoded fallback): `cargo test`

#### Manual Verification:

- With a `config.toml` containing `[[categories]]` with overridden thresholds, classification respects the override
- With `routing.toml` present but no `config.toml`, legacy path works and logs info message
- With neither file present, hardcoded defaults take over with zero errors

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation before proceeding to Phase 3.

---

## Phase 3: Tests + docs

### Overview

Update test helpers to reference `CategoryConfig` values instead of raw string literals, add a verification test for routing key consistency, and document consumer files in the category config block.

### Changes Required:

#### 1. Update test_classifier() helper

**File**: `src/intent_classifier.rs`

**Intent**: Use `CATEGORIES` names for routing keys instead of raw `"FILE_READING"` etc. string literals. This gives compile-time tracking if a category name changes.

**Contract**: Update `test_classifier()` (line 460) to accept a `Vec<CategoryConfig>` and use config-driven names:

```rust
fn test_classifier() -> RegexClassifier {
    let cats = hardcoded_categories();
    let mut routing = HashMap::new();
    routing.insert(cats[0].name.clone(), RouteEntry { model: "fr-model".to_string(), endpoint: String::new(), cost_per_1m_input_tokens: None, provider_type: String::new(), api_key_env: None });
    routing.insert(cats[1].name.clone(), RouteEntry { model: "sf-model".to_string(), endpoint: String::new(), cost_per_1m_input_tokens: None, provider_type: String::new(), api_key_env: None });
    routing.insert(cats[2].name.clone(), RouteEntry { model: "cr-model".to_string(), endpoint: String::new(), cost_per_1m_input_tokens: None, provider_type: String::new(), api_key_env: None });
    routing.insert(cats[3].name.clone(), RouteEntry { model: "ca-model".to_string(), endpoint: String::new(), cost_per_1m_input_tokens: None, provider_type: String::new(), api_key_env: None });
    let fallback = RouteEntry { model: "fallback-model".to_string(), endpoint: String::new(), cost_per_1m_input_tokens: None, provider_type: String::new(), api_key_env: None };
    RegexClassifier::from_values(routing, fallback, 30, cats)
}
```

**Note**: The ordering `cats[0]` = FILE_READING (priority 1), `cats[1]` = SYNTAX_FIX (priority 2), `cats[2]` = COMPLEX_REASONING (priority 3), `cats[3]` = CASUAL (priority 4) depends on `hardcoded_categories()` insertion order. A verification test guards this.

#### 2. Add routing-key verification test

**File**: `src/intent_classifier.rs`

**Intent**: Assert that all test routing keys exist as CategoryConfig names, and vice versa. Catches ordering shifts or missing keys at test time.

**Contract**: New test:

```rust
#[test]
fn hardcoded_categories_match_test_routing_keys() {
    let classifier = test_classifier();
    let cats = hardcoded_categories();
    let routing_keys: std::collections::HashSet<&str> = classifier.routing.keys().map(|s| s.as_str()).collect();
    let cat_names: std::collections::HashSet<&str> = cats.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(routing_keys, cat_names, "test_classifier routing keys must match hardcoded_categories names");
}
```

#### 3. Update test_app_with_classifier() and friends

**File**: `src/main.rs`

**Intent**: Replace raw string keys (`"SYNTAX_FIX"`, `"CASUAL"`) in test routing HashMaps with `hardcoded_categories()`-derived names.

**Contract**: In `test_app_with_classifier()` (line 744), `test_app_with_enriched_classifier()` (line ~830), `test_app_with_http_client()` (line ~1370), `test_app_with_dead_endpoint()` (line ~1420), `test_streaming_keepalive_injected()` (line ~2120): replace routing.insert string keys with config-driven values, and pass `hardcoded_categories()` to `from_values()`.

The `from_values()` already gained the `categories` parameter in Phase 1.

#### 4. Update hardcoded_routing test

**File**: `src/config.rs`

**Intent**: The existing test `hardcoded_routing_produces_expected_defaults` (line 241) checks 4 specific keys and model values. Update to use `hardcoded_categories()` for expected values.

**Contract**: Update the test to call `hardcoded_routing(&hardcoded_categories())` and assert against config-derived expectations.

### Success Criteria:

#### Automated Verification:

- All tests pass: `cargo test` (includes fast tests), `cargo test slow_tests`
- CI pipeline checks pass: `cargo test auth`, `cargo test routes_auth`
- New verification test catches key mismatches

#### Manual Verification:

- `cargo test intent_classify` output shows all classification tests pass unchanged
- No test assertion strings (e.g. `"SYNTAX_FIX"`) produce string mismatch failures

---

## Testing Strategy

### Unit Tests:

- `intent_classify_file_reading` — FILE_READING classification unchanged
- `intent_classify_complex_reasoning` — COMPLEX_REASONING classification unchanged
- `intent_classify_syntax_fix` — SYNTAX_FIX classification unchanged
- `intent_classify_casual` — CASUAL classification unchanged
- `intent_classify_empty_prompt` — empty prompt → CASUAL fallback
- `intent_classify_fallback_on_ambiguous` — 2+ thresholds met → CASUAL fallback
- `intent_classify_negative_suppression` — NEGATIVE_META penalty still suppresses false positives
- `chain_returns_first_regex_match`, `chain_falls_through_to_next`, `chain_returns_last_on_all_fallback`, `chain_handles_empty_backends` — ClassifierChain unchanged
- `hardcoded_categories_match_test_routing_keys` — new: routing keys match config names
- `hardcoded_routing_produces_expected_defaults` — updated: uses config-driven expectations
- `load_routing_from_file_success` — updated: skips `categories` key
- All `auth_headers_*` tests — unchanged

### Integration Tests:

- `test_completion_handler_returns_classification_json` — SYNTAX_FIX routing via test_app_with_classifier
- `test_classify_endpoint_returns_category` — classification endpoint output
- `test_proxy_bearer_required_for_completions` — auth gating unchanged
- `test_inferences_filter_by_category` — dashboard filter unchanged

### Manual Testing Steps:

1. Run without any config file: `cargo run` — confirms hardcoded fallback works, no errors
2. Create `config.toml` with `[[categories]]` overriding SYNTAX_FIX threshold to 5 — verify "fix this bug" no longer matches SYNTAX_FIX (falls to CASUAL)
3. Create `config.toml` with only 2 categories — verify only those categories get routing entries
4. Keep existing `routing.toml` (no `config.toml`) — verify legacy path works with info log
5. Test with `routing_examples/routing-openrouter.toml` renamed to `config.toml` + `[[categories]]` — verify routing + categories both load

## Performance Considerations

- CategoryConfig is a `Vec` created once at startup — zero per-request overhead
- `classify()` HashMap lookups remain `O(1)` by category name string
- Priority-chain iteration (4 elements) is constant time
- `hardcoded_categories()` allocates 4 `String`s once at startup if config file is absent — negligible

## Migration Notes

- **Existing `routing.toml` deployments**: the `load_routing()` function tries `config.toml` first, then falls back to `routing.toml` with an info log. No breakage.
- **`ROUTING_CONFIG_PATH` env var**: still works. Controls the path for `load_routing()`. A new `CONFIG_PATH` env var controls `load_categories()`. If both point to the same `config.toml`, categories and routing are loaded from one file.
- **`ROUTING_CONFIG_DEFAULT` constant**: becomes `CONFIG_DEFAULT`; `ROUTING_CONFIG_DEFAULT` removed after updating call sites.
- **External files** (`routing_examples/*.toml`, `openapi/completions.yaml`, `manual-test/run.sh`, `templates/dashboard/inferences.html`): no content changes. The doc comment on `CategoryConfig` inventories them as consumers.

## References

- Research: `context/changes/shared-category-config/research.md`
- Trait design background: `context/archive/2026-06-06-intent-classifier-trait/plan.md`
- Original classification research: `context/archive/2026-06-07-proxy-intent-routing/research.md`
- LLMClassifier prerequisite: `context/changes/llm-classifier/research.md`
- `src/intent_classifier.rs:121–125` — CAT_* constants to remove
- `src/intent_classifier.rs:145–149` — threshold constants to remove
- `src/intent_classifier.rs:218–235` — NEGATIVE_META to update
- `src/intent_classifier.rs:269–314` — build_all_patterns to refactor
- `src/intent_classifier.rs:397–429` — classify() scoring to refactor
- `src/config.rs:13–67` — hardcoded_routing to refactor

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: CategoryConfig struct + RegexClassifier internals

#### Automated

- [x] 1.1 Compiles: `cargo build` — ce916fd
- [x] 1.2 All classifier unit tests pass: `cargo test intent_classify` — ce916fd
- [x] 1.3 All auth tests pass: `cargo test auth` — ce916fd
- [x] 1.4 All route auth tests pass: `cargo test routes_auth` — ce916fd

#### Manual

- [ ] 1.5 Classification output identical to pre-refactor for representative prompts

### Phase 2: config.toml support

#### Automated

- [x] 2.1 Compiles: `cargo build` — 02b6a73
- [x] 2.2 All tests pass: `cargo test` — 02b6a73
- [x] 2.3 Works without config file (hardcoded fallback): `cargo test` — 02b6a73

#### Manual

- [ ] 2.3 config.toml with overridden thresholds: classification respects override
- [ ] 2.4 Legacy routing.toml without config.toml: works with info log
- [ ] 2.5 Neither file present: hardcoded defaults with zero errors

### Phase 3: Tests + docs

#### Automated

- [x] 3.1 All tests pass: `cargo test` — 13ae23b
- [x] 3.2 Slow tests pass: `cargo test slow_tests` — 13ae23b
- [x] 3.3 CI checks: `cargo test auth && cargo test routes_auth` — 13ae23b
- [x] 3.4 Verification test catches key mismatches — 13ae23b

#### Manual

- [ ] 3.5 Classification test output shows all tests pass unchanged
- [ ] 3.6 No string mismatch failures in test assertions
