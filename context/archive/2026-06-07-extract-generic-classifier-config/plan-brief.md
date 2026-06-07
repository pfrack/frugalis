# Extract Generic Classifier Config (S-07a) — Plan Brief

> Full plan: `context/changes/extract-generic-classifier-config/plan.md`
> Research: `context/changes/extract-generic-classifier-config/research.md`

## What & Why

Move 7 generic configuration items (routing loading, model costs, baseline model, default endpoints, short prompt length) from `RegexClassifier::from_env()` to a new `src/config.rs` module and `main()`. `RegexClassifier` becomes a pure classification constructor — patterns, weights, and thresholds only. The same config becomes available to the future `LLMClassifier` (S-09) without duplication, following the "config bundled at construction time" principle.

## Starting Point

Today, `RegexClassifier::from_env()` reads env vars, parses `routing.toml`, builds a `ModelCosts` table, and assembles routing — then `main()` clones `model_costs` and `baseline_model` back out of the classifier to populate `AppState`. The classifier owns generic config it shouldn't, and `LLMClassifier` (S-09) would have to duplicate all this logic or receive it from a separate source.

## Desired End State

`main()` loads routing via `config::load_routing()`, builds costs via `config::build_model_costs()`, and reads `BASELINE_MODEL` directly. These values are passed to both `RegexClassifier::from_env(routing, fallback_entry, short_prompt_len)` and `AppState` — no cloning, no duplication. Adding `LLMClassifier` later means calling the same config functions and passing the same values to its constructor.

## Key Decisions Made

| Decision | Choice | Why | Source |
|---|---|---|---|
| Module placement | New `src/config.rs` | Keeps main.rs lean, clear separation | Plan |
| SHORT_PROMPT_LEN delivery | Constructor parameter | Classifiers stay pure — no AppState coupling | Plan |
| Error path (classifier fails) | Empty defaults for costs/baseline | Current behavior, simplest Err branch | Plan |
| env_or_default helper | Move to config.rs | Single location for all env-reading helpers | Plan |
| DEFAULT_MODEL* constants | Stay in intent_classifier.rs | Model knowledge stays co-located | Research |
| NVIDIA_ENDPOINT | Move to config.rs | Infra/routing default, not classifier concern | Plan |
| from_costs constructor | Make `pub(crate)` | config.rs needs to construct ModelCosts | Plan |
| Test builder pattern | Add explicit params to `make_test_app_state` | Tests explicitly construct what they need, mirrors main() | Plan |

## Scope

**In scope:**
- New `src/config.rs` module with `load_routing()`, `hardcoded_routing()`, `build_model_costs()`, `env_or_default()`
- Change `RegexClassifier::from_env()` signature to accept injected config
- Remove `model_costs` and `baseline_model` from `RegexClassifier` struct
- Add `short_prompt_len` field to `RegexClassifier` struct
- Update `main()` to call config functions and assemble AppState directly
- Update all 11+ test call sites (7 `from_values()` + 4 `make_test_app_state()` + 1 slow test)

**Out of scope:**
- Creating `CategoryConfig` (S-07b)
- Building `LLMClassifier` (S-09)
- Changing `IntentClassify` trait or `ClassifierChain`
- Moving pattern arrays, weights, or classification thresholds

## Architecture / Approach

```
Before:                          After:
┌─────────────────────┐          ┌──────────────┐
│ RegexClassifier     │          │  config.rs   │
│  from_env():        │          │  load_routing│
│   read env vars     │          │  build_costs │
│   load routing.toml │          │  env_or_def  │
│   build model_costs │          └──────┬───────┘
│   read baseline     │                 │
│   build patterns    │          main() calls config,
│   compile regex     │          passes results to:
└─────────┬───────────┘          ┌──────────────┐
          │                      │ RegexCls     │
   main() clones                │  (patterns,  │
   model_costs,                  │   weights,   │
   baseline_model                │   thresholds)│
   from classifier               └──────┬───────┘
                                 ┌──────┴───────┐
                                 │   AppState   │
                                 │  (costs,     │
                                 │   baseline,  │
                                 │   routing)   │
                                 └──────────────┘
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Create config.rs Module | All generic functions moved to new module, compiles independently | Import path errors for `env_or_default` in `ClassificationResult::fallback()` |
| 2. Slim RegexClassifier + main() + tests | Constructor simplified, main() owns config, all tests updated | Missed a `from_values()` call site (compile-time catch) or an `#[cfg(test)]` import of removed fields |

**Prerequisites:** S-07 (IntentClassify trait) already implemented, routing.toml format is stable
**Estimated effort:** ~2 sessions across 2 phases

## Open Risks & Assumptions

- **Persistence tests** in `src/persistence.rs` use `super::super::intent_classifier::ModelCosts::from_costs(...)` — these continue to work since `from_costs` becomes `pub(crate)` instead of `#[cfg(test)]`
- **DEFAULT_MODEL_COMPLEX** is used by config.rs for the default BASELINE_MODEL — requires a cross-module import from config to intent_classifier, which is acceptable (config wires the app together)
- **Removing classifier model_costs tests**: 4 tests in `intent_classifier.rs` directly read `c.model_costs.get(...)` and `c.baseline_model`. These become compilation errors after field removal. Equivalent coverage exists in persistence tests (`test_fetch_savings_estimate_with_data`, etc.)

## Success Criteria (Summary)

- All existing tests pass (`cargo test`, `cargo test auth`, `cargo test routes_auth`, `cargo test slow_tests`)
- Service starts with `RUST_LOG=info cargo run` — routing loads, classifier initializes
- Classification endpoints return correct results (category, model, tier)
- Dashboard savings page shows cost data (with DATABASE_URL set)
- Graceful degradation: missing routing.toml or DATABASE_URL does not crash
