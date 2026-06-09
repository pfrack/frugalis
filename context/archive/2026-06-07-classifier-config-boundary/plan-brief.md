# Classifier Config Boundary (S-09a) — Plan Brief

> Full plan: `context/changes/classifier-config-boundary/plan.md`
> Research: `context/changes/classifier-config-boundary/research.md`

## What & Why

Formalize the generic/specific config boundary for classifier backends. The current code has per-backend enable/disable but lacks a global master switch, configurable ordering, and a clean construction pattern. After S-09 added LLMClassifier, the main.rs classifier construction is ~100 lines of nested if/else with duplicated routing merge logic.

## Starting Point

- Both RegexClassifier and LLMClassifier are operational (S-07a, S-07b, S-09 complete)
- Per-backend enable/disable exists in TOML (`[regex_classifier] enabled`, `[llm_classifier] enabled`)
- Order is hardcoded: regex first, then LLM
- RegexClassifierConfig only has `enabled` field; LLMClassifierConfig is rich
- Config is TOML-based (not env vars)

## Desired End State

- `[classifiers]` section in config.toml with `enabled` (global switch) and `order` (backend priority)
- RegexClassifierConfig extended with `timeout_secs` field
- main.rs refactored to ~25-line loop with single routing merge
- Backward compatible: defaults preserve current behavior

## Key Decisions Made

| Decision | Choice | Why | Source |
|---|---|---|---|
| Config mechanism | TOML only | Established pattern in codebase; env vars only for secrets | Research |
| Global switch | `[classifiers] enabled` | Single point to disable all classification | Research |
| Ordering | `[classifiers] order` array | Simple position-based; matches chain semantics | Research |
| Regex timeout | timeout_secs = 5 | Keep simple; match LLM pattern | User (Q2) |

## Scope

**In scope:**
- Add [classifiers] TOML section with enabled + order
- Extend RegexClassifierConfig with timeout_secs
- Refactor main.rs to loop pattern

**Out of scope:**
- Env var overrides for config
- Retry logic for RegexClassifier
- Changes to ClassifierChain, IntentClassify trait, ClassificationResult

## Architecture / Approach

Three phases:
1. Add ClassifiersConfig struct and loader in src/config.rs
2. Extend RegexClassifierConfig with timeout_secs
3. Refactor main.rs lines 87-192 to loop pattern with single routing merge

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. ClassifiersConfig | New [classifiers] config section | Low — follows existing pattern |
| 2. Regex extension | timeout_secs field for regex classifier | Low — simple field addition |
| 3. main.rs refactor | Loop pattern, single routing merge | Medium — large code change |

**Prerequisites:** S-07a, S-07b, S-09 all archived
**Estimated effort:** ~2-3 sessions across 3 phases

## Open Risks & Assumptions

- Refactor touches ~100 lines of main.rs — risk of regression if not tested
- Default behavior must be preserved (regex first, LLM if enabled)

## Success Criteria Summary

- Default config behavior unchanged
- Can disable all classifiers with `[classifiers] enabled = false`
- Can reorder backends with `[classifiers] order = ["llm", "regex"]`
- All existing tests pass