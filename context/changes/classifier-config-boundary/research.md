---
date: 2026-06-07T14:45:00+02:00
researcher: pfrack
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "Classifier Config Boundary (S-09a) — formalize generic/specific boundary with per-backend enable/disable flags and ordering"
tags: [research, config-boundary, enable-disable, classifier-chain, s-09a, roadmap]
status: complete
last_updated: 2026-06-07
last_updated_by: pfrack
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

After S-07a extracts generic config and S-09 adds the LLM backend, the `main()` classifier construction needs a clean boundary layer. Four env vars control chain construction: a global master switch, per-backend enable flags, and backend ordering. The config separation follows a 4-layer model (generic → shared → backend-specific). Placed after S-09 intentionally — the boundary is validated against two real backends rather than designed from speculation.

## Detailed Findings

### 1. Config Boundary Model

After all slices (S-07a, S-07b, S-09, S-09a), the configuration layers are:

| Layer | What | Owner | Set By |
|---|---|---|---|
| **Generic** | Routing table, `ModelCosts`, `BASELINE_MODEL`, `DEFAULT_MODEL*`, `SHORT_PROMPT_LEN`, enable/disable flags, backend order | `main()` | Env vars |
| **Shared** | `CategoryConfig` (names, descriptions, thresholds, priorities) | `intent_classifier.rs` | Static `CATEGORIES` slice |
| **Regex-specific** | Patterns, weights, negative suppression, dual-threshold logic | `RegexClassifier` | Hardcoded + env |
| **LLM-specific** | Model, endpoint, API key env, prompt template, few-shot examples | `LLMClassifier` | Env vars |

### 2. Env Var Specification

| Var | Type | Default | Purpose |
|---|---|---|---|
| `CLASSIFIERS_ENABLED` | `bool` | `true` | Global master switch. `false` → `classifier = None` in `AppState` — all requests get `ClassificationResult::fallback()` (CASUAL). Useful for testing, debugging, or running cerebrum as a pass-through proxy. |
| `REGEX_CLASSIFIER_ENABLED` | `bool` | `true` | Enable `RegexClassifier` backend. Default `true` preserves existing behavior. |
| `LLM_CLASSIFIER_ENABLED` | `bool` | `false` | Enable `LLMClassifier` backend. Default `false` — opt-in, no surprise latency/cost for existing deployments. |
| `CLASSIFIER_ORDER` | `string` | `regex,llm` | Comma-separated backend names in priority order. Controls `ClassifierChain::new()` vec ordering — first non-Fallback result wins. |

**Allowed values for `CLASSIFIER_ORDER`:** `"regex"`, `"llm"`, `"regex,llm"`, `"llm,regex"`. Unknown entries are skipped with a warning. Empty string → empty chain → `classifier = None`.

### 3. main() Construction Logic

```rust
let mut backends: Vec<Arc<dyn IntentClassify + Send + Sync>> = Vec::new();

let classifiers_enabled = env_bool("CLASSIFIERS_ENABLED", true);
if classifiers_enabled {
    let order = env_str("CLASSIFIER_ORDER", "regex,llm");
    for name in order.split(',').map(|s| s.trim()) {
        match name {
            "regex" if env_bool("REGEX_CLASSIFIER_ENABLED", true) => {
                match RegexClassifier::from_env(routing.clone(), fallback.clone(), short_prompt_len) {
                    Ok(c) => backends.push(Arc::new(c)),
                    Err(e) => warn!("RegexClassifier disabled: {e}"),
                }
            }
            "llm" if env_bool("LLM_CLASSIFIER_ENABLED", false) => {
                match LLMClassifier::from_env(client.clone(), &CATEGORIES, routing.clone(), fallback.clone(), short_prompt_len) {
                    Ok(c) => backends.push(Arc::new(c)),
                    Err(e) => warn!("LLMClassifier disabled: {e}"),
                }
            }
            unknown => warn!("unknown classifier in CLASSIFIER_ORDER: '{unknown}'"),
        }
    }
}

let classifier = if backends.is_empty() {
    None
} else {
    Some(Arc::new(ClassifierChain::new(backends)))
};
```

**Key behaviors:**
- Global `CLASSIFIERS_ENABLED=false` short-circuits everything — no backends are even attempted.
- Per-backend `*_ENABLED` flags gate individual backends.
- `from_env()` failure is non-fatal — the backend is skipped with a warning, other backends still load.
- Empty chain after construction → `classifier = None` → `ClassificationResult::fallback()` for all requests.
- `CLASSIFIER_ORDER` controls priority — `"llm,regex"` would try LLM first, then regex fallback.

### 4. Backward Compatibility

| Scenario | Behavior |
|---|---|
| Existing deployment (no new env vars) | `CLASSIFIERS_ENABLED=true`, `REGEX_CLASSIFIER_ENABLED=true`, `LLM_CLASSIFIER_ENABLED=false`, `CLASSIFIER_ORDER=regex,llm` — regex only, identical to current behavior |
| `CLASSIFIERS_ENABLED=false` | No classification — all requests CASUAL fallback. Equivalent to current behavior when `RegexClassifier::from_env()` fails |
| `REGEX_CLASSIFIER_ENABLED=false`, `LLM_CLASSIFIER_ENABLED=true` | LLM-only classification. Valid configuration for testing LLM classifier in isolation |
| `CLASSIFIER_ORDER=llm,regex` | LLM first, regex fallback. Higher cost (LLM call on every request) but potentially better classification |

### 5. Error Scenarios

| Scenario | Outcome |
|---|---|
| `CLASSIFIERS_ENABLED=true`, both backends disabled | `warn!("no classifier backends enabled")`, `classifier = None` |
| `CLASSIFIERS_ENABLED=true`, regex enabled but `from_env()` fails | Regex skipped with warning, chain has LLM only (if enabled) |
| `CLASSIFIERS_ENABLED=true`, LLM enabled but API key missing | LLM skipped with warning, chain has regex only |
| `CLASSIFIER_ORDER` contains `"unknown"` | Skipped with warning, other ordered backends still load |
| `CLASSIFIER_ORDER` empty string | Empty chain → `classifier = None` |

### 6. What Stays in Each Classifier's Constructor

**RegexClassifier::from_env(** generic_config **)**:
- Receives: `routing: HashMap<String, RouteEntry>`, `fallback_entry: RouteEntry`, `short_prompt_len: usize`
- Owns internally: patterns, weights, negative suppression, RegexSet, scoring logic
- Does NOT read: `ROUTING_CONFIG_PATH`, `BASELINE_MODEL`, `DEFAULT_MODEL*`, `NVIDIA_ENDPOINT`, `ModelCosts`

**LLMClassifier::from_env(** generic_config, llm_specific_config **)**:
- Receives: `client: reqwest::Client`, `categories: &[CategoryConfig]`, `routing: HashMap<String, RouteEntry>`, `fallback_entry: RouteEntry`, `short_prompt_len: usize`
- Owns internally: model name, endpoint, API key, prompt template, few-shot examples
- Reads env: `LLM_CLASSIFIER_MODEL`, `LLM_CLASSIFIER_ENDPOINT`, `LLM_CLASSIFIER_API_KEY`, `LLM_CLASSIFIER_PROVIDER_TYPE`

### 7. Why This Slice Comes After S-09

The boundary between generic and backend-specific config can only be validated when two real backends exist. Doing it earlier risks:

- **Under-extraction**: assuming something is regex-specific when LLM also needs it (e.g., `SHORT_PROMPT_LEN`)
- **Over-extraction**: pulling backend-specific config into generic layer (e.g., trying to make prompt templates "generic")
- **Wrong abstraction**: designing a config trait or interface that doesn't fit either backend's actual needs

By placing S-09a after S-09, the boundary is informed by real constructor signatures from both backends. The config table in Section 1 is a *derived* artifact, not a speculative design.

### 8. Future Backends

The boundary is extensible. Adding a third backend (e.g., `ONNXClassifier`) requires:
1. Add `ONNX_CLASSIFIER_ENABLED` env var (default `false`)
2. Add `"onnx"` to `CLASSIFIER_ORDER` parsing in `main()`
3. Implement `IntentClassify` on the new struct
4. Constructor receives the same generic config (`routing`, `fallback_entry`, `short_prompt_len`, `CategoryConfig`)

No changes to `ClassifierChain`, `AppState`, or handlers.

## Code References

- `src/main.rs:71-100` — current `AppState` construction (destination for new logic)
- `src/main.rs:28-37` — `AppState` struct (`classifier: Option<Arc<ClassifierChain>>`)
- `src/intent_classifier.rs:113-145` — `ClassifierChain` (vec ordering controls fallback priority)
- `src/intent_classifier.rs:78-87` — `IntentClassify` trait (no change)
- `src/intent_classifier.rs:515-524` — `ClassificationResult::fallback()` (used when `classifier = None`)

## Related Research

- `context/changes/extract-generic-classifier-config/research.md` — S-07a (extracts generic config from RegexClassifier)
- `context/changes/shared-category-config/research.md` — S-07b (CategoryConfig shared between backends)
- `context/changes/llm-classifier/research.md` — S-09 (LLMClassifier backend)
