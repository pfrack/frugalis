# Classifier Chain Routing Integrity — Plan Brief

> Full plan: `context/changes/classifier-chain-routing-integrity/plan.md`
> Research: `context/changes/classifier-chain-routing-integrity/research.md`

## What & Why

`LLMClassifier` returns `providers: vec![]` on successful classification, causing the handler to fall through to **502 "all providers failed"** — even though intent classification was correct. This happens when the chain escalates past regex and fewshot to the LLM tier. Fixing this requires giving the LLM backend its own routing table and adding defensive guards in the handler.

## Starting Point

- Three classifier backends (`regex → fewshot → llm`) run as a chain in `ClassifierChain`
- `RegexClassifier` and `FewShotClassifier` each own a routing table and populate `ClassificationResult.providers`
- `LLMClassifier` has no routing table — it only calls an upstream LLM API to get a category name
- The handler iterates `classification.providers` unconditionally; empty providers means no upstream attempt, which falls through to 502

## Desired End State

Every classification path produces a valid response:
- LLM match → populated `providers` → actual upstream routing
- Classifier fallback or empty providers → 200 with classification JSON (shows intent, no proxy error)
- Classifiers disabled → routing still available for header bypass; fallback returns classification JSON
- LLM tier distinguishable from regex tier in metrics (`ClassificationTier::Llm`)

## Key Decisions Made

| Decision | Choice | Why | Source |
|---|---|---|---|
| Where to fix empty providers | Give LLMClassifier a routing table | Matches existing Regex/FewShot pattern; fix at source | Plan |
| Empty providers handler behavior | 200 with classification JSON | Consistent with X-Frugalis-Category unknown-category path; better UX than 502 | Plan |
| Handler defensive guard | Add guard alongside backend fix | Defense-in-depth against future regressions | Plan |
| ClassificationTier::Llm | Add now, folded into LLMClassifier phase | Needed for test assertions to distinguish LLM from regex in chain tests | Plan |
| Headless (no-classifiers) mode | Preserve routing from config | Header bypass should work even when classification is disabled | Plan |
| FewShot casing | Uppercase before routing lookup | Defensive normalisation; routing keys are always uppercase | Plan |

## Scope

**In scope:**
- Add routing table to `LLMClassifier` + implement `get_routing()`
- Add `ClassificationTier::Llm` variant
- Defensive empty-providers guard in `completion_handler` and `messages_handler`
- Preserve routing map when classifiers disabled
- Normalize FewShot category-name casing on lookup
- Integration test: 3-backend chain escalation through full handler pipeline

**Out of scope:**
- `get_routing()` on `ClassifierChain` itself (backends contribute individually)
- `ClassificationResult::fallback()` providers population (handler guard catches this)
- Regex classifier casing fix (categories/config/routing are all uppercase in practice)
- OTel metric label format changes (`{:?}` debug output handles new variant automatically)

## Architecture / Approach

Three-phased fix:

```
Phase 1: LLMClassifier ← routing table, Llm tier
    ├── types.rs: add ClassificationTier::Llm
    ├── llm.rs: +routing + fallback_entry fields, populate providers, get_routing()
    ├── app/mod.rs: pass routing_map + fallback to LLMClassifier::new()
    └── chain.rs test: update assertion (Regex → Llm), pass routing

Phase 2: Handler guards + headless + integration test
    ├── handlers.rs: empty-providers → 200 classification JSON
    ├── app/mod.rs: preserve routing_map when classifiers disabled
    └── handlers.rs test: 3-backend chain escalation integration test

Phase 3: FewShot casing
    └── fewshot.rs: .to_uppercase() on routing lookup
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. LLMClassifier routing + Llm tier | LLM matches produce valid providers; tier is distinguishable | Enum addition touches chain tests; low risk, small surface |
| 2. Handler guard + headless + test | No path to 502 from empty providers; headless works; proven end-to-end | Guard placement (before vs after OTel metrics) affects observability |
| 3. FewShot casing | Lowercase training-data categories route correctly | None — pure defensive fix |

**Prerequisites:** Rust toolchain, repo cloned at current HEAD
**Estimated effort:** ~1 session across 3 phases

## Open Risks & Assumptions

- The `LLMClassifier::new()` signature grows from 4 to 6 params — all call sites (production + 5 test call sites) must be updated
- The chain escalation test already has a mock LLM endpoint; the new integration test must replicate this setup inside the handler test module
- FewShot bootstrap YAML at `data/fewshot_bootstrap.yaml` is assumed to use uppercase category names matching config; the casing fix is defensive

## Success Criteria (Summary)

- LLM-classified requests route to the correct upstream provider (not 502)
- Empty providers after any classification path returns 200 with classification JSON
- `cargo test` passes with no regressions across all test suites
- Classification tier for LLM results reports `Llm` in metrics
