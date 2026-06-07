# Intent Classifier Trait — Plan Brief

> Full plan: `context/changes/intent-classifier-trait/plan.md`
> Roadmap: `context/foundation/roadmap.md` (S-07)

## What & Why

Extract `IntentClassifier` into an `IntentClassify` trait with a single `classify()` method, rename the concrete struct to `RegexClassifier`, and add `ClassifierChain` for ordered fallback across multiple backends. This enables pluggable classifiers — the immediate motivation is roadmap S-09 (LLM-based classifier), which requires a trait to swap in as a new backend. Zero behavioral change.

## Starting Point

`IntentClassifier` is a concrete struct in `src/intent_classificator.rs:74` with 7 public fields and a synchronous `classify()` method. `AppState` holds an `Option<Arc<IntentClassifier>>`. Handlers call `.classify()` directly on the concrete type. The `completion_handler` also reads `.routing` on the classifier for the X-Cerebrum-Category header path. Dashboard handlers clone `model_costs()` and `baseline_model` from the classifier for savings estimates. A `CostProvider` trait already exists in `persistence.rs`, proving the narrow-trait-boundary pattern.

## Desired End State

An `IntentClassify` trait with one method: `fn classify(&self, prompt: &str) -> ClassificationResult`. `RegexClassifier` (renamed from `IntentClassifier`) implements it. `ClassifierChain` wraps `Vec<Arc<dyn IntentClassify + Send + Sync>>` and iterates backends — if one returns `Fallback`, the chain tries the next. `AppState` holds `Option<Arc<ClassifierChain>>` plus separate fields for `routing`, `model_costs`, and `baseline_model` extracted from `RegexClassifier` at startup. All existing tests pass unchanged; new tests cover chain behavior and the trait boundary.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Trait scope | Single `classify()` method only | Narrow trait — minimal contract for future backends; costs and config stay on the concrete type. | Plan |
| Chain logic | Try next on Fallback tier | Uses existing `ClassificationTier` as the confidence signal; no new fields on ClassificationResult. | Plan |
| Naming | `IntentClassify` / `RegexClassifier` / `ClassifierChain` | Roadmap S-07 names; `IntentClassifier` → `RegexClassifier` frees the name for the trait role. | Roadmap |
| AppState shape | `Option<Arc<ClassifierChain>>` + separate fields for routing/costs/baseline | Chain is opaque to handlers; config is read directly from AppState fields — eliminates duplicated dashboard code. | Plan |
| Construction | `from_env`/`from_values` on `RegexClassifier` only | No async trait needed; construction code already knows the concrete type. | Plan |
| Chain scope | Included in this change | Roadmap S-07 describes chain as part of the trait work; deferring would cause two AppState shape changes. | Plan |
| Cost/baseline access | Separate AppState fields (`model_costs`, `baseline_model`) | Dashboard handlers currently clone from classifier — direct AppState reads are simpler and remove duplication. | Plan |
| Field visibility | Keep `pub` on RegexClassifier | Tests and future code inspect classifier config; no breaking API changes needed. | Plan |
| Test coverage | Full suite: update fixtures + chain tests + stub impl | Ensures chain logic is verified and trait boundary compiles correctly for future backends. | Plan |

## Scope

**In scope:**
- Define `IntentClassify` trait with `classify()` method
- Rename `IntentClassifier` → `RegexClassifier`
- New `ClassifierChain` struct implementing `IntentClassify`
- Update `AppState` with chain + extracted config fields
- Update all production handlers and dashboard code
- Update all 7 test helpers (6 in main.rs, 1 in intent_classificator.rs)
- Add chain unit tests + stub trait implementation test

**Out of scope:**
- `LLMClassifier` backend (roadmap S-09)
- `provider_path` URL derivation (roadmap S-08)
- Classification algorithm changes
- Routing configuration format changes
- Async trait methods

## Architecture / Approach

```
IntentClassify (trait)
├── RegexClassifier      — existing regex-based classification
│   (pub fields: set, metadata, routing, fallback_entry, model_costs, baseline_model)
│   inherent: from_env(), from_values(), model_costs()
└── ClassifierChain      — iterates Vec<Arc<dyn IntentClassify + Send + Sync>>
    impl classify(): for each backend, call classify();
                    if tier != Fallback → return it;
                    else → continue;
                    return last result (or fallback() if empty)

AppState {
    classifier: Option<Arc<ClassifierChain>>,
    routing: Arc<HashMap<String, RouteEntry>>,  // extracted from RegexClassifier
    model_costs: ModelCosts,                     // extracted from RegexClassifier
    baseline_model: String,                      // extracted from RegexClassifier
}
```

At startup, `RegexClassifier::from_env()` is called once. Its routing, model_costs, and baseline_model are copied into AppState. The RegexClassifier is wrapped in `Arc::new(ClassifierChain::new(vec![regex_arc]))`. Handlers call `classify()` through the chain; the completion_handler reads `state.routing` directly for header-based overrides; dashboard reads `state.model_costs` and `state.baseline_model` directly.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Define trait + rename struct | `IntentClassify` trait + `RegexClassifier` in `intent_classificator.rs` | Rename could miss a reference — grep-and-compile catches all. |
| 2. Add ClassifierChain | Chain struct with fallback-on-Fallback logic | Edge case: empty chain must not panic. |
| 3. Update AppState + consumers | All handlers and test helpers work with new types | 7 test helpers to update — easy to miss a field in one. |
| 4. New tests | Chain logic tests + stub trait impl | Stub must implement `Send + Sync` for `Arc<dyn>` compatibility. |

**Prerequisites:** None — all prerequisite changes (classify-endpoint, provider-agnostic-config) are already implemented.
**Estimated effort:** ~3-4 sessions across 4 phases. Phase 3 is the largest (touching 7 test helpers).

## Open Risks & Assumptions

- **Stub test `Send + Sync`**: The stub classifier must satisfy `Send + Sync` bounds for `Arc<dyn IntentClassify + Send + Sync>`. If `ClassificationResult` gains non-Send/Sync fields later, this test will catch it at compile time.
- **HashMap clone in tests**: Each test helper clones the routing HashMap once. This is fine for small test routing tables (2-4 entries). If routing tables grow, consider using `Arc` internally in `RegexClassifier`.
- **Dashboard `classifier_active` flag**: Still uses `state.classifier.is_some()` — this tells whether ANY classifier chain is active, which remains semantically correct.

## Success Criteria (Summary)

- All existing tests pass without assertion changes
- Chain correctly falls through on Fallback tier and returns first Regex match
- X-Cerebrum-Category header override works through the new `state.routing` field
- Dashboard savings estimate renders with the correct baseline model
- Server gracefully degrades when routing.toml is absent (hardcoded routing)
