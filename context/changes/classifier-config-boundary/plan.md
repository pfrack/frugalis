# Classifier Config Boundary Implementation Plan (S-09a)

## Overview

Formalize the generic/specific config boundary for classifier backends with a clean configuration layer. Add global master switch, configurable backend ordering, and extend RegexClassifierConfig with timeout_secs to match LLMClassifier richness.

## Current State Analysis

- Per-backend enable/disable exists via TOML (`[regex_classifier] enabled`, `[llm_classifier] enabled`)
- Order is hardcoded: regex first, then LLM
- No global master switch to disable all classification
- RegexClassifierConfig only has `enabled` field; LLMClassifierConfig is rich (model, endpoint, api_key_env, provider_type, timeout_secs)
- main.rs lines 87-192: ~100 lines of nested if/else with 3× duplicated routing merge logic

## Desired End State

- `[classifiers]` section in config.toml with `enabled` (global switch) and `order` (backend priority)
- Extended RegexClassifierConfig with `timeout_secs` field
- main.rs refactored to ~25-line loop pattern with single routing merge
- Backward compatible: defaults preserve current behavior

### Key Discoveries:

- Config is TOML-based, not env vars — follows established pattern in src/config.rs
- RegexClassifier::from_env takes 4 params: (routing, fallback_entry, short_prompt_len, categories)
- LLMClassifier::new takes (LlmClassifierConfig, client, categories) — does NOT own routing
- ClassifierChain already supports arbitrary vec of backends

## What We're NOT Doing

- Env var overrides for config (TOML-only for simplicity)
- Retry logic for RegexClassifier (just timeout_secs)
- Changing ClassifierChain, IntentClassify trait, or ClassificationResult

## Implementation Approach

Add new [classifiers] TOML section, extend RegexClassifierConfig, refactor main.rs construction logic to loop pattern.

## Phase 1: Add ClassifiersConfig struct and loader

### Overview

Add the [classifiers] section config struct and loader function in src/config.rs.

### Changes Required:

#### 1. src/config.rs

**Intent**: Add ClassifiersConfig struct and loader for global classifier settings.

**Contract**: New struct with `enabled: bool` (default true) and `order: Vec<String>` (default ["regex", "llm"]). Loader function `load_classifiers_config_from_value(root: &toml::Value) -> ClassifiersConfig`.

```rust
/// Configuration for the global classifiers section.
#[derive(Clone, Debug)]
pub(crate) struct ClassifiersConfig {
    pub enabled: bool,
    pub order: Vec<String>,
}

impl Default for ClassifiersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            order: vec!["regex".to_string(), "llm".to_string()],
        }
    }
}

/// Load classifiers config from a parsed toml::Value.
/// Returns default if section is absent.
pub(crate) fn load_classifiers_config_from_value(root: &toml::Value) -> ClassifiersConfig {
    let table = match root.as_table() {
        Some(t) => t,
        None => return ClassifiersConfig::default(),
    };
    let classifiers_section = match table.get("classifiers").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return ClassifiersConfig::default(),
    };
    let enabled = classifiers_section.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let order = classifiers_section
        .get("order")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec!["regex".to_string(), "llm".to_string()]);

    ClassifiersConfig { enabled, order }
}
```

### Success Criteria:

#### Automated Verification:

- [ ] 1.1 Build passes: `cargo build`
- [ ] 1.2 Existing tests pass: `cargo test`

#### Manual Verification:

- [ ] 1.3 Config loader handles missing [classifiers] section (uses defaults)

---

## Phase 2: Extend RegexClassifierConfig with timeout_secs

### Overview

Add timeout_secs field to RegexClassifierConfig to match LLMClassifierConfig richness.

### Changes Required:

#### 1. src/config.rs

**Intent**: Extend RegexClassifierConfig with timeout_secs field.

**Contract**: Add `timeout_secs: u64` field (default 5) to RegexClassifierConfig struct and loader.

```rust
/// Configuration for the regex classifier backend.
#[derive(Clone, Debug)]
pub(crate) struct RegexClassifierConfig {
    pub enabled: bool,
    pub timeout_secs: u64,
}

impl Default for RegexClassifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 5,
        }
    }
}
```

Update `load_regex_classifier_config_from_value` to read the new field:
```rust
let timeout_secs = regex_section
    .get("timeout_secs")
    .and_then(|v| v.as_integer())
    .unwrap_or(5) as u64;
RegexClassifierConfig { enabled, timeout_secs }
```

### Success Criteria:

#### Automated Verification:

- [ ] 2.1 Build passes: `cargo build`
- [ ] 2.2 Existing tests pass: `cargo test`

#### Manual Verification:

- [ ] 2.3 Default timeout_secs is 5 when not specified in config

---

## Phase 3: Refactor main.rs classifier construction

### Overview

Replace the nested if/else tree (lines 87-192) with a clean loop pattern using ClassifiersConfig.

### Changes Required:

#### 1. src/main.rs

**Intent**: Refactor classifier chain construction to use loop pattern with ClassifiersConfig.

**Contract**: Load ClassifiersConfig, iterate over order, build backends vec, single routing merge.

Key changes:
- Load ClassifiersConfig after regex_config
- Replace nested if/else with for loop over classifiers_config.order
- Do routing merge once after loop
- Handle empty backends case → classifier = None

```rust
// Load classifiers config
let classifiers_config = config_root.as_ref()
    .and_then(|root| config::load_classifiers_config_from_value(root))
    .unwrap_or_default();

let (classifier, routing) = if !classifiers_config.enabled {
    info!("All classifiers disabled via config");
    (None, Arc::new(HashMap::new()))
} else {
    let mut backends: Vec<Arc<dyn IntentClassify + Send + Sync>> = Vec::new();

    for name in &classifiers_config.order {
        match name.as_str() {
            "regex" if regex_config.enabled => {
                match IntentClassifier::from_env(
                    routing_map.clone(),
                    fallback_entry.clone(),
                    SHORT_PROMPT_LEN,
                    categories.clone(),
                ) {
                    Ok(c) => backends.push(Arc::new(c)),
                    Err(e) => warn!("RegexClassifier disabled: {e}"),
                }
            }
            "llm" => {
                if let Some(llm_config) = config_root.as_ref()
                    .and_then(|r| config::load_llm_classifier_config_from_value(r))
                {
                    let llm = intent_classifier::LLMClassifier::new(
                        llm_config,
                        http_client.clone(),
                        categories.clone(),
                    );
                    backends.push(Arc::new(llm));
                }
            }
            unknown => warn!("unknown classifier in order: '{unknown}'"),
        }
    }

    if backends.is_empty() {
        warn!("no classifier backends enabled");
        (None, Arc::new(HashMap::new()))
    } else {
        let chain = intent_classifier::ClassifierChain::new(backends);
        let mut merged_routing = HashMap::new();
        for backend in chain.backends().iter() {
            if let Some(r) = backend.get_routing() {
                merged_routing.extend(r.clone());
            }
        }
        (Some(Arc::new(chain)), Arc::new(merged_routing))
    }
};
```

**Note**: Remove the old code blocks that did regex construction (lines ~87-133), LLM addition (lines ~150-192), and the three routing merges.

### Success Criteria:

#### Automated Verification:

- [ ] 3.1 Build passes: `cargo build`
- [ ] 3.2 Existing tests pass: `cargo test`
- [ ] 3.3 New config option documented in config.toml

#### Manual Verification:

- [ ] 3.4 Default behavior unchanged (regex first, then LLM if enabled)
- [ ] 3.5 Can disable all classifiers with `[classifiers] enabled = false`
- [ ] 3.6 Can change order with `[classifiers] order = ["llm", "regex"]`

---

## Testing Strategy

### Unit Tests:

- ClassifiersConfig loader: default values, custom values, missing section
- RegexClassifierConfig: timeout_secs defaults to 5

### Integration Tests:

- Full classifier chain construction with default config
- Full chain with `[classifiers] enabled = false`
- Full chain with custom `[classifiers] order`

### Manual Testing Steps:

1. Start server with default config — verify classification works
2. Add `[classifiers] enabled = false` — verify all requests get CASUAL fallback
3. Add `[classifiers] order = ["llm", "regex"]` with LLM enabled — verify LLM is tried first

## Migration Notes

No migration needed — all new fields have defaults that preserve current behavior.

## References

- Research: `context/changes/classifier-config-boundary/research.md`
- S-07a: `context/archive/2026-06-07-extract-generic-classifier-config/`
- S-07b: `context/archive/2026-06-07-shared-category-config/`
- S-09: `context/archive/2026-06-07-llm-classifier/`

## Progress

### Phase 1: Add ClassifiersConfig struct

#### Automated

- [x] 1.1 Build passes: `cargo build`
- [x] 1.2 Existing tests pass: `cargo test`

#### Manual

- [x] 1.3 Config loader handles missing [classifiers] section

### Phase 2: Extend RegexClassifierConfig

#### Automated

- [ ] 2.1 Build passes: `cargo build`
- [ ] 2.2 Existing tests pass: `cargo test`

#### Manual

- [ ] 2.3 Default timeout_secs is 5

### Phase 3: Refactor main.rs

#### Automated

- [ ] 3.1 Build passes: `cargo build`
- [ ] 3.2 Existing tests pass: `cargo test`
- [ ] 3.3 Config option documented

#### Manual

- [ ] 3.4 Default behavior unchanged
- [ ] 3.5 Can disable all classifiers
- [ ] 3.6 Can change backend order