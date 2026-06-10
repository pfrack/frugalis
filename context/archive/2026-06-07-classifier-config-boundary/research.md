---
date: 2026-06-07T14:45:00+02:00
researcher: pfrack
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "Classifier Config Boundary (S-09a) — formalize generic/specific boundary with per-backend enable/disable flags and ordering"
tags: [research, config-boundary, enable-disable, classifier-chain, s-09a, roadmap]
status: complete
last_updated: 2026-06-08
last_updated_by: pfrack
last_updated_note: "Validation pass — corrected line references, constructor signatures, config mechanism (TOML not env vars), identified already-implemented features vs genuine gaps"
validated_against_commit: 309ffc02e1fa7c5b08bfe36e0694b70a10cf7519
validated_branch: llm-classifier
---

# Research: Classifier Config Boundary (S-09a)

**Date**: 2026-06-07T14:45:00+02:00
**Researcher**: pfrack
**Git Commit**: 792b2618299caf26c5adabf28293e2eb1bafc836
**Branch**: provider-url-derivation
**Repository**: cerebrum

## Research Question

With both `RegexClassifier` and `LLMClassifier` backends operational (after S-09), how should the generic/specific config boundary be formalized, and how should per-backend enable/disable and ordering be controlled?

## Summary

After S-07a extracts generic config and S-09 adds the LLM backend, the `main()` classifier construction needs a clean boundary layer. ~~Four env vars control chain construction~~ **[CORRECTION]**: The codebase uses TOML config (`config.toml`), not env vars, for structural configuration. Per-backend enable/disable already exists. What's genuinely new: a global master switch, configurable ordering, and code cleanup (collapsing ~100 lines of nested if/else into a ~15-line loop).

## Validation Summary (2026-06-08)

### Prerequisites Status: ✅ All Complete

| Prerequisite | Status | Archived |
|---|---|---|
| S-07a (extract-generic-classifier-config) | ✅ archived | 2026-06-07 |
| S-07b (shared-category-config) | ✅ archived | 2026-06-08 |
| S-09 (llm-classifier) | ✅ archived | 2026-06-08 |

### Critical Corrections

| Issue | Original Claim | Reality |
|---|---|---|
| **Config mechanism** | Env vars (`CLASSIFIERS_ENABLED`, `REGEX_CLASSIFIER_ENABLED`, etc.) | **TOML config** (`config.toml` sections `[regex_classifier]`, `[llm_classifier]`) |
| **RegexClassifier::from_env signature** | 3 params: `(routing, fallback, short_prompt_len)` | **4 params**: `(routing, fallback_entry, short_prompt_len, categories)` |
| **LLMClassifier constructor** | `from_env(client, categories, routing, fallback, short_prompt_len)` | **`new(config: LlmClassifierConfig, client, categories)`** — no routing/fallback/short_prompt_len |
| **LLMClassifier routing ownership** | Receives routing + fallback | **Does NOT own routing** — returns `None` from `get_routing()`, only classifies |
| **AppState construction size** | ~30 lines (71-100) | **~116 lines** (87-203) due to S-09 LLM addition |
| **Per-backend enable/disable** | Proposed as NEW | **ALREADY EXISTS** in TOML config |

### Line Reference Drift

All line references have drifted forward +16 to +29 lines due to code inserted by S-07b and S-09:

| Reference | Research Claims | Actual (309ffc02) | Drift |
|---|---|---|---|
| AppState struct | `src/main.rs:28-37` | `src/main.rs:28-39` | +2 (fields added) |
| AppState construction | `src/main.rs:71-100` | `src/main.rs:87-203` | +16 start, 4× longer |
| ClassifierChain | `src/intent_classifier.rs:113-145` | `src/intent_classifier.rs:131-158` | +18 |
| IntentClassify trait | `src/intent_classifier.rs:78-87` | `src/intent_classifier.rs:95-105` | +17 |
| ClassificationResult::fallback() | `src/intent_classifier.rs:515-524` | `src/intent_classifier.rs:539-553` | +24 |

### What's Already Implemented vs Genuinely New

| Feature | Status | Evidence |
|---|---|---|
| Per-backend regex enable/disable | ✅ Already exists, needs extension | `[regex_classifier] enabled` in config.toml, but should add more proper config options (e.g., timeout, fallback behavior, priority) |
| Per-backend LLM enable/disable | ✅ Already exists, needs extension | `[llm_classifier] enabled` in config.toml, but already has rich config (model, endpoint, api_key_env, provider_type, timeout_secs) — this is the model to follow for regex |
| Regex disabled + LLM-only path | ✅ Already exists | `src/main.rs:170-179` handles this case |
| Both disabled → classifier=None | ✅ Already exists | Falls through to `(None, Arc::new(HashMap::new()))` |
| **Global master switch** | ❌ NEW | No single toggle to disable all classification |
| **Configurable backend ordering** | ❌ NEW | Order is hardcoded regex→llm (`src/main.rs:107,162`) |
| **Extended per-classifier config** | ❌ NEW | RegexClassifier only has `enabled` flag; should get richer config like LLMClassifier (timeout, retry, priority) |
| **Clean construction loop** | ❌ NEW (refactor) | Current: 100-line nested if/else; proposed: 15-line loop |
| **Extensibility pattern** | ❌ NEW (design) | Currently adding a 3rd backend requires structural surgery |

## Detailed Findings

### 1. Config Boundary Model (CORRECTED)

| Layer | What | Owner | Set By |
|---|---|---|---|
| **Generic** | Routing table, `ModelCosts`, `BASELINE_MODEL`, `DEFAULT_MODEL*`, `SHORT_PROMPT_LEN`, enable/disable flags, backend order | `main()` | **TOML config** (`config.toml`) |
| **Shared** | `CategoryConfig` (names, descriptions, thresholds, priorities) | `intent_classifier.rs` | TOML `[[categories]]` section (fallback: hardcoded `CATEGORIES` slice) |
| **Regex-specific** | Patterns, weights, negative suppression, dual-threshold logic | `RegexClassifier` | Hardcoded |
| **LLM-specific** | Model, endpoint, API key env, prompt template, few-shot examples, timeout | `LLMClassifier` | TOML `[llm_classifier]` section |

### 2. Config Specification (CORRECTED — TOML-based)

The implementation should use TOML sections, consistent with the existing pattern. Proposed addition to `config.toml`:

```toml
[classifiers]
enabled = true              # Global master switch (NEW). Default: true.
order = ["regex", "llm"]   # Backend priority order (NEW). Default: ["regex", "llm"].

[regex_classifier]
enabled = true              # ALREADY EXISTS. Default: true.
# Extended config (NEW):
timeout_secs = 5            # Match timeout for classification
retry_count = 2             # Number of retries on transient failure

[llm_classifier]
enabled = false             # ALREADY EXISTS. Default: false.
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
provider_type = "openai_compatible"
timeout_secs = 3
```

**Key design decision**: Structural config (on/off, ordering) belongs in TOML. Only secrets and runtime overrides (`PORT`, API keys) use env vars. This is the established pattern throughout the codebase (`src/config.rs`).

### 3. main() Construction Logic (CORRECTED)

```rust
// Load classifier config
let classifiers_config = config_root.as_ref()
    .and_then(|root| load_classifiers_config_from_value(root))
    .unwrap_or_default();

let classifier = if !classifiers_config.enabled {
    info!("All classifiers disabled via config");
    None
} else {
    let mut backends: Vec<Arc<dyn IntentClassify + Send + Sync>> = Vec::new();

    for name in &classifiers_config.order {
        match name.as_str() {
            "regex" if regex_config.enabled => {
                match RegexClassifier::from_env(
                    routing_map.clone(), fallback_entry.clone(),
                    SHORT_PROMPT_LEN, categories.clone(),
                ) {
                    Ok(c) => backends.push(Arc::new(c)),
                    Err(e) => warn!("RegexClassifier disabled: {e}"),
                }
            }
            "llm" => {
                if let Some(llm_config) = config_root.as_ref()
                    .and_then(|r| load_llm_classifier_config_from_value(r))
                {
                    let llm = LLMClassifier::new(llm_config, http_client.clone(), categories.clone());
                    backends.push(Arc::new(llm));
                }
            }
            unknown => warn!("unknown classifier in order: '{unknown}'"),
        }
    }

    if backends.is_empty() {
        None
    } else {
        Some(Arc::new(ClassifierChain::new(backends)))
    }
};
```

**Key corrections from original research:**
- Uses TOML config, not env vars
- `RegexClassifier::from_env` takes 4 params (includes `categories`)
- `LLMClassifier::new` takes `(LlmClassifierConfig, client, categories)` — no routing/fallback
- LLM enabled check is via `load_llm_classifier_config_from_value` returning `Some` (already incorporates `enabled` field)

### 4. Backward Compatibility

| Scenario | Behavior |
|---|---|
| No `[classifiers]` section in config.toml | Default: `enabled=true`, `order=["regex","llm"]` — identical to current behavior |
| `[classifiers] enabled = false` | No classification — all requests get CASUAL fallback |
| Only `[regex_classifier] enabled = false` | LLM-only (if configured). Already works today. |
| `order = ["llm", "regex"]` | LLM first, regex fallback. Higher cost but potentially better classification. |

### 5. What the Implementation Actually Needs to Do

1. **Add `ClassifiersConfig` struct** in `src/config.rs`:
   - `enabled: bool` (default `true`)
   - `order: Vec<String>` (default `["regex", "llm"]`)
   - `load_classifiers_config_from_value(root: &toml::Value) -> ClassifiersConfig`

2. **Extend `RegexClassifierConfig`** — add fields like `timeout_secs`, `retry_count` to match `LlmClassifierConfig` richness. Model: `[llm_classifier]` section in config.rs.

3. **Refactor `main.rs` lines 87-192** — replace the nested if/else tree with the loop pattern above. This is the core of S-09a: ~100 lines collapsed into ~25 lines.

4. **Merge routing once** after the loop, not 3× as currently done (lines 114-118, 160-164, 175-179 are nearly identical).

5. **No changes needed to**:
   - `ClassifierChain` (already works with any vec of backends)
   - `IntentClassify` trait
   - `RegexClassifier` or `LLMClassifier` (constructors stay the same)
   - `ClassificationResult::fallback()`

### 6. What Stays in Each Classifier's Constructor (CORRECTED)

**RegexClassifier::from_env(routing, fallback_entry, short_prompt_len, categories)**:
- Receives: `HashMap<String, RouteEntry>`, `RouteEntry`, `usize`, `Vec<CategoryConfig>`
- Owns internally: patterns, weights, negative suppression, RegexSet, scoring logic
- Returns: `Result<Self, String>`

**LLMClassifier::new(config, client, categories)**:
- Receives: `LlmClassifierConfig`, `reqwest::Client`, `Vec<CategoryConfig>`
- Owns internally: model, endpoint, api_key, provider_type, prompt_template, timeout
- Does NOT own: routing table (returns `None` from `get_routing()`)
- Note: routing for LLM results is resolved at the chain level, not within the classifier

### 7. Why This Slice Comes After S-09

The boundary between generic and backend-specific config can only be validated when two real backends exist. By placing S-09a after S-09, the boundary is informed by real constructor signatures from both backends:
- RegexClassifier owns routing — it implements `get_routing()`
- LLMClassifier does NOT own routing — it only classifies prompts into categories

This asymmetry means the config boundary is NOT symmetric: routing lives in the generic layer and is merged from backends that have it, not passed to all backends.

### 8. Future Backends

Adding a third backend (e.g., `ONNXClassifier`) requires:
1. Add `[onnx_classifier]` section in config.toml with `enabled` field
2. Add `"onnx"` arm in the loop in `main()`
3. Implement `IntentClassify` on the new struct
4. If it owns routing, implement `get_routing()` (otherwise default `None`)

No changes to `ClassifierChain`, `AppState`, or handlers.

## Code References (CORRECTED — commit 309ffc02)

- `src/main.rs:87-203` — current AppState construction (target for refactor)
- `src/main.rs:28-39` — `AppState` struct
- `src/main.rs:107-126` — regex classifier construction + chain creation
- `src/main.rs:150-192` — LLM classifier conditional addition to chain
- `src/intent_classifier.rs:131-158` — `ClassifierChain` (vec ordering controls fallback priority)
- `src/intent_classifier.rs:95-105` — `IntentClassify` trait
- `src/intent_classifier.rs:539-553` — `ClassificationResult::fallback()`
- `src/intent_classifier.rs:163-185` — `LLMClassifier::new()` constructor
- `src/intent_classifier.rs:558-567` — `RegexClassifier::from_env()` (4 params)
- `src/config.rs:225-237` — `load_regex_classifier_config_from_value()` (TOML-based enable/disable)
- `src/config.rs:270-310` — `load_llm_classifier_config_from_value()` (TOML-based enable/disable)
- `src/config.rs:7` — `CONFIG_DEFAULT = "config.toml"`

## Historical Context

- `context/archive/2026-06-07-extract-generic-classifier-config/` — S-07a extracted config module, established TOML pattern
- `context/archive/2026-06-07-shared-category-config/` — S-07b added `Vec<CategoryConfig>` param to `from_env`
- `context/archive/2026-06-07-llm-classifier/` — S-09 added `LLMClassifier::new()` with `LlmClassifierConfig` struct, established the asymmetric pattern (LLM doesn't own routing)

## Related Research

- `context/archive/2026-06-07-extract-generic-classifier-config/research.md` — S-07a
- `context/archive/2026-06-07-shared-category-config/research.md` — S-07b
- `context/archive/2026-06-07-llm-classifier/research.md` — S-09

## Open Questions

1. **Should the global switch also be exposed as an env var override?** — TOML for default, but `CLASSIFIERS_ENABLED=false` env var could override for quick debugging. This is the only case where dual config might be justified.
2. **Should ordering support weight/priority instead of just position?** — Current chain semantics are "first non-fallback wins". Position-based ordering is simpler and sufficient for 2 backends.
3. **What about the 3× duplicated routing merge?** — The loop-based refactor eliminates this by doing a single post-loop merge. This is the biggest code quality win.
