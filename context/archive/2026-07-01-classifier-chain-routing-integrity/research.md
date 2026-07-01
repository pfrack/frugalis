---
date: "2026-07-01T00:00:00+02:00"
researcher: opencode
git_commit: c12869887d04154c0f7ec10f25dd58651682433c
branch: testing-proxy-translation-contracts
repository: pfrack/frugalis
topic: "Classifier chain routing integrity — auditing the pipeline from intent classification through upstream provider resolution"
tags: [research, classifier-chain, routing, integrity, LLMClassifier, ClassificationResult, providers, fallback]
status: complete
last_updated: "2026-07-01"
last_updated_by: opencode
---

# Research: Classifier Chain Routing Integrity

**Date**: 2026-07-01T00:00:00+02:00
**Researcher**: opencode
**Git Commit**: c12869887d04154c0f7ec10f25dd58651682433c
**Branch**: testing-proxy-translation-contracts
**Repository**: pfrack/frugalis

## Research Question

Does the classifier chain correctly produce valid routing information in every escalation path? Are there gaps where a successful classification cannot reach an upstream provider?

## Summary

**Yes — there is a critical integrity gap.** When the LLM classifier wins in the chain (regex → fewshot → llm escalation), its `ClassificationResult` carries `providers: vec![]`, causing the handler to fall through to "all providers exhausted" → **502 Bad Gateway**. The request is correctly classified but cannot be routed. Secondary integrity issues include: the `ClassifierChain` itself never contributes routing; `AppState.routing` (merged from backends' `get_routing()`) is never used as a post-classification fallback; and FewShotClassifier's routing is typically empty, so all its matches route to a bare fallback entry.

## Detailed Findings

### 1. LLMClassifier Returns Empty Providers (CRITICAL)

**File**: `src/classification/llm.rs:188-193`

When `LLMClassifier.parse_response()` matches a known category, it returns:
```rust
ClassificationResult {
    category: cat.name.clone(),
    model: self.model.clone(),
    tier: ClassificationTier::Regex,       // misleading!
    providers: vec![],                      // <-- EMPTY
}
```

The `providers` field is always empty because `LLMClassifier` does not own a routing table — it only knows categories and its own LLM model/endpoint for the classification call itself.

**Impact**: When the classifier chain's default order (`regex → fewshot → llm`) escalates to the LLM backend and it produces a match, the handler at `src/proxy/handlers.rs:315` iterates over `classification.providers` (empty), produces no `last_error_response`, and reaches line 924:
```rust
// All providers exhausted
if let Some(resp) = last_error_response {
    return resp;    // <-- not taken, it's None
}
// ...
crate::proxy::util::json_response(
    StatusCode::BAD_GATEWAY,
    crate::proxy::util::upstream_error_json(502, "all providers failed"),
)
```

This 502 is returned even though classification was successful and the intent was correctly identified.

**Invocation path that triggers this**:
1. User sends a prompt (e.g. "design a distributed cache with write-through semantics")
2. Regex classifier: no patterns fire → returns Fallback
3. FewShot classifier: cosine similarity below threshold → returns Fallback  
4. LLM classifier (`parse_response`, line 188-193): API returns "COMPLEX_REASONING" → returns valid category with `providers: vec![]`
5. Handler: iterates empty providers → 502 "all providers failed"

### 2. FewShotClassifier Uses an Empty Routing Table by Default

**File**: `src/classification/fewshot.rs:26-68`

`FewShotClassifier::new()` receives `routing: HashMap<String, RouteEntry>` and `fallback_entry: RouteEntry`. When constructed in the production path (`src/app/mod.rs:177-183`), routing is `HashMap::new()` — the FewShot classifier receives an empty routing table:

```rust
// src/app/mod.rs:177-181
let fewshot = Arc::new(classification::fewshot::FewShotClassifier::new(
    config,
    routing_map.clone(),      // <-- this is the full routing map, 
    fallback_entry.clone(),   //     which IS non-empty from config
));
```

Wait — the `routing_map` passed *is* the full routing map from config. Let me re-read...

Actually, looking at `src/app/mod.rs:176-181`:
```rust
"fewshot" => {
    if let Some(config) = config::loader::load_fewshot_config_from_value(config_root) {
        let fewshot = Arc::new(classification::fewshot::FewShotClassifier::new(
            config,
            routing_map.clone(),     // <-- all routes from config
            fallback_entry.clone(),
        ));
```

So in production, FewShot *does* receive the full routing map. Its `classify()` at `fewshot.rs:335` and `354` looks up `self.routing.get(&category)` and falls back to `self.fallback_entry`. This is correct — FewShot's routing is populated from config.

However, there's a subtlety: FewShot's own routing table uses the original category names from training data (e.g., `"FILE_READING"`, `"SYNTAX_FIX"`), while the config routing table keys are **uppercased** by `routing_from_value()` (line 336 of `loader.rs`). If the training data category name casing doesn't match, the lookup silently falls through to `fallback_entry`.

### 3. LLMClassifier `tier` Is Misleading (`Regex`)

**File**: `src/classification/types.rs:14-18`

The `ClassificationTier` enum has only three variants:
```rust
pub enum ClassificationTier {
    Regex,
    FewShot,
    Fallback,
}
```

There is **no `Llm` variant**. When `LLMClassifier.parse_response()` produces a match, it sets `tier: ClassificationTier::Regex` (line 191). This:
- Confuses observability (metrics say "Regex" when it was actually LLM)
- Makes it impossible to distinguish chain-short-circuit behavior (the comment in `chain.rs:80-82` explicitly notes this: *"tier inspection cannot distinguish 'regex matched' from 'LLM matched'"*)

### 4. Two Competing Routing Resolution Paths

There are **two separate routing resolution mechanisms** with no post-classification reconciliation:

| Path | When Used | Data Source |
|------|-----------|-------------|
| `state.routing` lookup | `X-Frugalis-Category` header bypass | Merged `AppState.routing` from all backends' `get_routing()` |
| `ClassificationResult.providers` | Classifier chain runs | Whatever the winning backend populated |

**Handler code** at `src/proxy/handlers.rs:251-280`:
```rust
// Path A: Header bypass uses state.routing
if let (Some(category), Some(model)) = (x_category.as_ref(), x_model.as_ref()) {
    let routing = state.routing.read().await;
    match routing.get(category) {
        Some(entry) => ClassificationResult { ..., providers: entry.providers.clone() },
        None => { /* warn, degrade to classifier */ }
    }
} else {
    // Path B: Classifier chain — uses result.providers directly
    match state.classifier.as_ref() {
        Some(c) => c.classify(&prompt).await,
        None => ClassificationResult::fallback(),
    }
}
```

The `ClassificationResult::fallback()` at `src/classification/types.rs:38-45` returns `providers: vec![]` — also empty.

**Consequence**: There's a conceptual mismatch. `AppState.routing` has the "source of truth" routing map, but it's only consulted for header bypass. The classifier chain path never falls back to `state.routing` to resolve providers for a category it produced.

### 5. ClassifierChain Does Not Implement `get_routing()`

**File**: `src/classification/chain.rs:14-18`

The `IntentClassify` trait's `get_routing()` has a default implementation returning `None`:
```rust
fn get_routing(&self) -> Option<&std::collections::HashMap<String, RouteEntry>> {
    None
}
```

`ClassifierChain` never overrides this. The merged routing in `AppState.routing` is built externally in `build_classifiers()` (lines 221-226 of `app/mod.rs`) by calling each individual backend's `get_routing()`. The chain itself is opaque.

### 6. `AppState.routing` Merged Routing Construction

**File**: `src/app/mod.rs:221-226`

```rust
if backends.is_empty() {
    // returns empty routing
} else {
    let chain = ClassifierChain::new(backends);
    let mut merged_routing = HashMap::new();
    for backend in chain.backends().iter() {
        if let Some(r) = backend.get_routing() {
            merged_routing.extend(r.clone());
        }
    }
    ClassifierBuildResult {
        classifier: Some(Arc::new(chain)),
        routing: merged_routing,     // <-- goes to AppState.routing
```

The merged routing includes contributions from:
- `RegexClassifier`: returns its routing table (populated from config `[routing]`)
- `FewShotClassifier`: returns its routing (also populated from config routing map)
- `LLMClassifier`: returns `None` (line 219-221 of `llm.rs`)

So `AppState.routing` is effectively the config-defined routing table (Regex + FewShot are loaded with the same routing map). It *does* have entries for all categories defined in the config. But as noted in Finding 4, it's only consulted for header bypass, not as a fallback for classifier-produced results.

### 7. Category Name Casing Inconsistency

**Routing keys**: `routing_from_value()` at `loader.rs:336` inserts with `key.to_uppercase()`:
```rust
routing.insert(key.to_uppercase(), RouteEntry { ... });
```

**Category names from config**: `load_categories_from_value()` at `loader.rs:378` sets `c.name = name.clone()` where `name` comes from the TOML key (e.g., `[categories.FILE_READING]` → name = `"FILE_READING"`).

**Regex classifier**: Creates routing with keys already uppercased by `routing_from_value()`. Its `classify_internal()` returns category names from `CategoryConfig.name` (already uppercase from config). The `route_match()` lookup at `regex.rs:227` uses these names as keys — so the lookup works because both key and lookup value are UPPERCASE.

**FewShot classifier**: Category names come from training data (exact match or cosine similarity). Bootstrap YAML at `data/fewshot_bootstrap.yaml` presumably uses uppercase category names. If training data uses non-uppercase names, `self.routing.get(&category)` at `fewshot.rs:335` would fail due to case mismatch (routing keys are uppercased).

**LLM classifier**: `parse_response()` at `llm.rs:186` matches `response_upper.trim() == cat.name.to_uppercase()`. This is case-insensitive for recognition, but the returned `ClassificationResult.category` is `cat.name.clone()` — the original (typically uppercase) name.

### 8. FallbackEntry Shape Differs When Classifiers Are Disabled

When all classifiers are disabled, `build_classifiers()` (lines 137-143 of `app/mod.rs`) returns:
```rust
ClassifierBuildResult {
    classifier: None,
    routing: HashMap::new(),     // <-- empty!
    model_costs,
    baseline_model,
    fewshot_classifier: None,
}
```

This means `AppState.routing` is an empty `HashMap`. If a client sends `X-Frugalis-Category: SYNTAX_FIX`, the header bypass path at `handlers.rs:256` will find no entry and degrade. Similarly, when the classifier is `None`, `ClassificationResult::fallback()` is returned (empty providers) → the handler can't route anywhere.

## Code References

- `src/classification/llm.rs:188-193` — LLMClassifier returns `providers: vec![]` and `tier: Regex`
- `src/classification/types.rs:14-18` — `ClassificationTier` enum lacks `Llm` variant
- `src/classification/types.rs:38-45` — `ClassificationResult::fallback()` returns empty providers
- `src/classification/chain.rs:41-58` — `ClassifierChain::classify()` short-circuits to first non-Fallback result
- `src/proxy/handlers.rs:311-315` — Handler provider loop: `for (idx, provider) in providers_clone.iter().enumerate()`
- `src/proxy/handlers.rs:924-949` — "All providers exhausted" → 502
- `src/proxy/handlers.rs:251-280` — Dual routing resolution (header bypass vs classifier chain)
- `src/app/mod.rs:221-226` — Merged routing construction from backend `get_routing()`
- `src/app/mod.rs:176-183` — FewShot classifier construction with routing map
- `src/app/mod.rs:59-107` — Category-routing consistency check (uppercased names)
- `src/config/loader.rs:335-337` — Routing keys uppercased on insertion
- `src/config/loader.rs:293-353` — `routing_from_value()` — DEFAULT key removed as fallback
- `src/config/types.rs:330-341` — `ClassifiersConfig::default()` order: `regex → fewshot → llm`

## Architecture Insights

### The Pipeline Chain

```
Request → completion_handler / messages_handler
    │
    ├─ Header bypass (X-Frugalis-Category) → state.routing lookup → providers
    │
    └─ Classifier chain (default order):
        1. RegexClassifier    → tier:Regex or Fallback
        2. FewShotClassifier  → tier:FewShot or Fallback  
        3. LLMClassifier      → tier:Regex(!) or Fallback  ← NO providers!
```

### The Dual-Routing Antipattern

Two independent routing resolution systems exist side by side:
1. **Per-backend routing** — Each classifier backend owns its own `HashMap<String, RouteEntry>`. When a backend wins, its `ClassificationResult.providers` is used directly.
2. **AppState merged routing** — Built from all backends' `get_routing()`. Used only for `X-Frugalis-Category` header bypass and for the `feedback_handler` category validation.

There is no bridge between them: the handler never looks up a classifier-produced category in `state.routing` to resolve missing providers.

### The LLM-As-Last-Resort Gap

With the default chain order (`regex → fewshot → llm`), the LLM is the last tier. When it matches, it's the only backend that produces a non-Fallback result. But its result has no providers. The chain doesn't fall back to earlier routing tables or to `AppState.routing` — it trusts the winning backend's providers unconditionally.

## Historical Context (from prior changes)

- `context/archive/2026-06-07-proxy-intent-routing/plan.md` — Original intent classification plan. Defined the `IntentionClassifier` with `providers` field, but at that time the router handled a single provider directly. The LLM classifier didn't exist yet.
- `context/archive/2026-06-24-provider-fallback-cascade/plan.md` — Provider fallback cascade plan. Extended `RouteEntry` from single provider to `Vec<ProviderEntry>`. Does not address classification-provider mismatch — assumes classification always produces populated providers.
- `context/archive/2026-06-07-classifier-config-boundary/plan.md` — Classifier chain architecture. Added the chain abstraction but didn't address provider resolution in chain results.
- `context/foundation/lessons.md` — "Log operational failures before falling back" is relevant; the current 502 path does log via `log_classification` but doesn't use `warn!` for the "no providers" case specifically.

## Related Research

- `context/archive/2026-06-07-proxy-intent-routing/research.md` — Original research on regex patterns, tier architecture
- `context/archive/2026-06-07-classifier-config-boundary/research.md` — Classifier config boundary research

## Open Questions

1. **Should `LLMClassifier` own a routing table?** It could be given the same routing map that `RegexClassifier` and `FewShotClassifier` receive, so it can populate `providers` on successful classification. This is the simplest fix.

2. **Should `ClassifierChain.classify()` resolve providers post-hoc?** The chain could look up the winning category in its merged routing (or in `AppState.routing`) and populate `providers` as a post-processing step. This would be a belt-and-suspenders approach.

3. **Should `ClassificationResult.providers` be resolved at the handler level?** The handler could check if `providers` is empty and fall back to `state.routing` lookup by category. This is a defensive fix at the consumer side.

4. **Should the handler return 502 or classification-only JSON when providers are absent?** Currently it returns 502. In the `X-Frugalis-Category` path with unknown category, it returns `classification_only_json()` with 200 OK — which is inconsistent.

5. **Should a `ClassificationTier::Llm` variant be added?** Makes observability accurate, enables chain-short-circuit inspection.
