<!-- PLAN-REVIEW-REPORT -->
# Plan Review: Extract Generic Classifier Config (S-07a)

- **Plan**: context/changes/extract-generic-classifier-config/plan.md
- **Mode**: Deep
- **Date**: 2026-06-07
- **Verdict**: SOUND
- **Findings**: 2 critical 1 warning 0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| End-State Alignment | PASS |
| Lean Execution | PASS |
| Architectural Fitness | PASS |
| Blind Spots | PASS |
| Plan Completeness | PASS |

## Grounding
Grounding: 5/5 paths ✓, 3/3 symbols ✓, brief↔plan ✓

## Findings

### F1 — Circular module dependency between config.rs and intent_classifier.rs

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Architectural Fitness
- **Location**: Phase 1 — Create config.rs Module
- **Detail**:
  Moving routing functions (load_routing_from_file, hardcoded_routing, load_routing) to config.rs requires config to import RouteEntry and ModelCosts from intent_classifier. Meanwhile, ClassificationResult::fallback() will import env_or_default from config. This creates a cycle: config -> intent_classifier -> config. Rust forbids cyclic module dependencies; compilation will fail.
- **Fix A ⭐ Recommended**: Extract a new `routing.rs` module containing RouteEntry, ModelCosts, and DEFAULT_MODEL* constants. Re-export these from intent_classifier to preserve external API. Then config.rs imports from routing, breaking the cycle.
  - Strength: Clean separation; shared types in a low-level module with no deps. Preserves intent_classifier::RouteEntry via re-export.
  - Tradeoff: New module and import updates (~4-6 files). Slight project size increase.
  - Confidence: HIGH — standard cycle-breaking pattern.
  - Blind spot: Need to verify no hidden deps on moved constants.
- **Fix B**: Reverse direction: Do NOT move env_or_default to config. Keep it in intent_classifier. Then config only depends on intent_classifier (for types) and intent_classifier does not depend on config → no cycle.
  - Strength: Minimal changes; most of the plan as written still works.
  - Tradeoff: Generic config helpers remain in intent_classifier, weakening abstraction. Future LLMClassifier duplication.
  - Confidence: MEDIUM — simpler but less clean.
  - Blind spot: LLMClassifier still needs env var reading.
- **Decision**: FIXED — Applied Fix A in plan (routing.rs extraction)

### F2 — Missing visibility for DEFAULT_MODEL* constants

- **Severity**: ❌ CRITICAL
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Completeness
- **Location**: Phase 1 — New File: src/config.rs (hardcoded_routing)
- **Detail**:
  After moving hardcoded_routing() to config.rs, it uses DEFAULT_MODEL*, DEFAULT_MODEL_COMPLEX, and DEFAULT_MODEL_READING from intent_classifier.rs. These constants are private, preventing config.rs from accessing them.
- **Fix**: Add `pub` to the three constants in intent_classifier.rs (lines 161-163).
- **Decision**: FIXED — Handled by F1 (constants now pub(crate) in routing.rs)

### F3 — No unit tests for new config module functions

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Completeness
- **Location**: Phase 1 — Create config.rs Module
- **Detail**:
  The plan moves several functions to config.rs (env_or_default, load_routing_from_file, hardcoded_routing, load_routing, build_model_costs) but does not include any unit tests for these functions. Reliance solely on existing integration tests risks missing edge cases, especially for build_model_costs override logic and error handling in routing loaders.
- **Fix**: Add a Phase 1.4 to write unit tests for config.rs covering:
  - env_or_default behavior
  - load_routing_from_file success and failure
  - hardcoded_routing defaults
  - load_routing fallback behavior
  - build_model_costs cost combination and overrides
  - Strength: Improves confidence in new module; catches regressions early.
  - Tradeoff: Additional implementation and maintenance; may delay Phase 2.
  - Confidence: MEDIUM — tests add time but reduce risk of subtle bugs in cost overrides.
  - Blind spot: Unknown whether integration tests already sufficiently exercise these paths.
- **Decision**: FIXED — Added Phase 1.4 with unit test requirements to plan
