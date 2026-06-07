---
date: 2026-06-07T14:45:00+02:00
researcher: pfrack
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "Extract Generic Classifier Config (S-07a) — move routing, costs, defaults from RegexClassifier to main()"
tags: [research, generic-config, routing, model-costs, baseline-model, s-07a, roadmap]
status: complete
last_updated: 2026-06-07
last_updated_by: pfrack
---

# Research: Extract Generic Classifier Config (S-07a)

**Date**: 2026-06-07T14:45:00+02:00
**Researcher**: pfrack
**Git Commit**: 792b2618299caf26c5adabf28293e2eb1bafc836
**Branch**: provider-url-derivation
**Repository**: cerebrum

## Research Question

What generic configuration currently leaks from `RegexClassifier::from_env()` that should be lifted to `main()` so it's available to all classifier backends (current `RegexClassifier` and future `LLMClassifier`)?

## Summary

Seven configuration items are currently parsed or populated inside `RegexClassifier::from_env()` (or the functions it calls) that have nothing to do with regex classification. These are generic config consumed by `AppState` fields or needed by any classifier backend. Moving them to `main()` simplifies `RegexClassifier` to a pure classification constructor (patterns + weights + thresholds) and makes the same config available to `LLMClassifier` without duplication.

## Detailed Findings

### 1. Routing Loading (ROUTING_CONFIG_PATH + TOML parsing + hardcoded fallback)

**Current:** `src/intent_classifier.rs:487-508` (`load_routing()`) and `434-485` (`load_routing_from_file()`) and `297-351` (`hardcoded_routing()`)

Called from `RegexClassifier::from_env()` at line 536. Returns `(HashMap<String, RouteEntry>, RouteEntry)` which is stored in the classifier's `routing` and `fallback_entry` fields — then cloned into `AppState.routing` and used by `completion_handler` for category-to-endpoint mapping.

**After extraction:** `main()` calls `load_routing()` directly, stores the `HashMap` and fallback in local variables, passes them to both `RegexClassifier` and `LLMClassifier` constructors and to `AppState.routing`.

### 2. BASELINE_MODEL

**Current:** `src/intent_classifier.rs:538-539`
```rust
let baseline_model = std::env::var("BASELINE_MODEL")
    .unwrap_or_else(|_| DEFAULT_MODEL_COMPLEX.to_string());
```
Stored in `RegexClassifier.baseline_model` → cloned to `AppState.baseline_model`. Used only by the dashboard savings calculation.

**After extraction:** `main()` reads `BASELINE_MODEL` directly, stores in `AppState.baseline_model`. The classifier no longer needs to know about it.

### 3. ModelCosts (hardcoded defaults + routing.toml overrides)

**Current:** `src/intent_classifier.rs:541-547`
```rust
let mut costs = hardcoded_model_costs();
for (_category, entry) in &routing {
    if let Some(override_cost) = entry.cost_per_1m_input_tokens {
        costs.insert(entry.model.clone(), override_cost);
    }
}
```
Stored in `RegexClassifier.model_costs` → cloned to `AppState.model_costs`. Used only by the dashboard savings calculation. Also used as `crate::persistence::CostProvider` for log enrichment.

`hardcoded_model_costs()` is at lines 53-60 and defines costs for `claude-3.5-sonnet` ($3.00), `gpt-4o` ($2.50), `gpt-4o-mini` ($0.15), `deepseek-chat` ($0.14).

**After extraction:** `main()` builds `ModelCosts` from hardcoded defaults + routing overrides, stores in `AppState.model_costs`. Function `hardcoded_model_costs()` stays in `intent_classifier.rs` (it's not classifier-specific, just a data table).

### 4. DEFAULT_MODEL / DEFAULT_MODEL_COMPLEX / DEFAULT_MODEL_READING

**Current:** `src/intent_classifier.rs:161-163` (constants) + `env_or_default()` calls in `hardcoded_routing()` lines 297-351

Used as fallback values when `routing.toml` is missing or entries lack a `model` field. The constants are:
- `DEFAULT_MODEL = "meta/llama-3.1-8b-instruct"` — used for SYNTAX_FIX, CASUAL, and general fallback
- `DEFAULT_MODEL_COMPLEX = "meta/llama-3.3-70b-instruct"` — used for COMPLEX_REASONING and BASELINE_MODEL default
- `DEFAULT_MODEL_READING = "meta/llama-3.1-70b-instruct"` — used for FILE_READING

Also used in `ClassificationResult::fallback()` at line 518 for the CASUAL fallback model.

**After extraction:** `main()` reads env vars, passes to routing builder. `ClassificationResult::fallback()` continues to read `DEFAULT_MODEL` from env directly (it's a static method, not tied to any classifier instance). The constants stay in `intent_classifier.rs` as public defaults.

### 5. NVIDIA_ENDPOINT

**Current:** `src/intent_classifier.rs:298-301`
```rust
let endpoint = env_or_default("NVIDIA_ENDPOINT",
    "https://integrate.api.nvidia.com/v1/chat/completions");
```
Used as the hardcoded fallback endpoint when no `routing.toml` is present. All hardcoded routing entries use this endpoint.

**After extraction:** `main()` reads `NVIDIA_ENDPOINT`, passes to the routing builder as the default endpoint. The hardcoded `nvidia_nim` provider type in default routing is a separate concern — it's a routing default, not a classifier concern.

### 6. SHORT_PROMPT_LEN (30 chars)

**Current:** `src/intent_classifier.rs:191` — used in `classify()` at line 613:
```rust
if sanitized.len() < SHORT_PROMPT_LEN && all_zero {
    return self.route_fallback(CAT_CASUAL);
}
```

**After extraction:** This is a generic threshold — any classifier should shortcut very short prompts. Move to generic config in `main()` or keep as a constant in `intent_classifier.rs` but remove from `RegexClassifier` struct. The `classify()` method receives it as a parameter or reads from a shared source.

**Edge case:** If `LLMClassifier` is the only backend and a prompt is <30 chars, it should also skip the LLM call and return CASUAL directly. This confirms it's generic, not regex-specific.

### 7. ClassificationResult::fallback() default model

**Current:** `src/intent_classifier.rs:515-524`
```rust
pub fn fallback() -> Self {
    ClassificationResult {
        category: CAT_CASUAL.to_string(),
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        ...
    }
}
```

**After extraction:** Keep reading `DEFAULT_MODEL` from env at the call site. This is a static method — it has no access to `AppState` and shouldn't. The env var is a globally available fallback, consistent with how `PORT` or `RUST_LOG` work. No change needed.

## What Stays in RegexClassifier

After S-07a extraction, `RegexClassifier` retains only classifier-specific concerns:

| Concern | Reason |
|---|---|
| Pattern arrays (`FILE_READING`, `COMPLEX_REASONING`, etc.) | Regex-specific — the LLM classifier doesn't use them |
| Weight arrays (`FR_WEIGHTS`, `CR_WEIGHTS`, etc.) | Regex-specific — score weighting is a regex heuristic |
| Negative pattern arrays + `NegativeMeta` | Regex-specific — suppression is a regex heuristic |
| `build_all_patterns()` | Regex-specific — assembles regex patterns with metadata |
| `RegexSet` compilation | Regex-specific — the core regex matching engine |
| `sanitize()` function | Regex-specific — text preprocessing for regex matching |
| `classify()` scoring + threshold logic | Regex-specific — the classification algorithm |
| SF dual-threshold logic | Regex-specific — interaction rule between SYNTAX_FIX and FILE_READING |

## Constructor Signature After Extraction

**Before (current):**
```rust
RegexClassifier::from_env() -> Result<Self, String>
// Internally reads: ROUTING_CONFIG_PATH, BASELINE_MODEL, DEFAULT_MODEL*,
// NVIDIA_ENDPOINT, builds patterns, builds routing, builds costs
```

**After:**
```rust
RegexClassifier::from_env(
    routing: HashMap<String, RouteEntry>,
    fallback_entry: RouteEntry,
    short_prompt_len: usize,
) -> Result<Self, String>
// Only reads env for regex-specific concerns (none currently)
// Builds patterns, weights, RegexSet internally
```

**Test constructor** (`from_values`) already takes injected `routing` and `fallback_entry` — add `short_prompt_len` parameter. Tests that don't care about prompt length can pass `30`.

## What main() Gains

`main()` currently does this after `RegexClassifier::from_env()` succeeds:
```rust
let model_costs = regex_classifier.model_costs.clone();
let baseline_model = regex_classifier.baseline_model.clone();
let chain = ClassifierChain::new(vec![Arc::new(regex_classifier)]);
let merged_routing = /* extract from chain backends */;
```

After S-07a, `main()` does the work directly:
```rust
let (routing, fallback_entry) = load_routing();  // was inside from_env()
let model_costs = build_model_costs(&routing);    // was inside from_env()
let baseline_model = env_or_default("BASELINE_MODEL", &DEFAULT_MODEL_COMPLEX);
let short_prompt_len = 30;  // constant, kept in intent_classifier.rs

let regex_classifier = RegexClassifier::from_env(
    routing.clone(), fallback_entry.clone(), short_prompt_len,
)?;
let chain = ClassifierChain::new(vec![Arc::new(regex_classifier)]);
```

`AppState` construction is identical — the same fields are populated from the same sources, just from `main()` variables instead of classifier fields.

## Impact on LLMClassifier (S-09)

After S-07a, the `LLMClassifier` constructor receives the same generic config that `RegexClassifier` receives:

- `routing: HashMap<String, RouteEntry>` — to map classified categories to endpoints
- `fallback_entry: RouteEntry` — for fallback routing
- `short_prompt_len: usize` — to shortcut short prompts

This eliminates any need for `LLMClassifier` to parse routing files or env vars for these settings — they come from `main()` like everything else.

## Code References

- `src/intent_classifier.rs:487-508` — `load_routing()` (move to `main()` or a shared `config.rs`)
- `src/intent_classifier.rs:434-485` — `load_routing_from_file()` (move to `main()`)
- `src/intent_classifier.rs:297-351` — `hardcoded_routing()` (move to `main()`)
- `src/intent_classifier.rs:538-539` — `BASELINE_MODEL` read (move to `main()`)
- `src/intent_classifier.rs:541-547` — `ModelCosts` build (move to `main()`)
- `src/intent_classifier.rs:53-60` — `hardcoded_model_costs()` (stay — data table)
- `src/intent_classifier.rs:161-163` — `DEFAULT_MODEL*` constants (stay — public defaults)
- `src/intent_classifier.rs:298-301` — `NVIDIA_ENDPOINT` default (move to `main()`)
- `src/intent_classifier.rs:191` — `SHORT_PROMPT_LEN` (move to generic config)
- `src/intent_classifier.rs:515-524` — `ClassificationResult::fallback()` (unchanged)
- `src/main.rs:71-100` — current `AppState` construction (consolidates after extraction)

## Related Research

- `context/changes/shared-category-config/research.md` — S-07b (next slice, depends on this)
- `context/changes/llm-classifier/research.md` — S-09 (depends on S-07b)
- `context/archive/2026-06-07-provider-agnostic-config/research.md` — routing.toml format
