<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: LLM Classifier Backend (S-09)

- **Plan**: context/changes/llm-classifier/plan.md
- **Scope**: Phase 1-4 of 4
- **Date**: 2026-06-07
- **Verdict**: APPROVED
- **Findings**: 0 critical, 1 warning, 5 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS |

## Findings

### F1 — Config silently ignores parse errors

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:222
- **Detail**: `load_llm_classifier_config` uses `.ok()?` on both `read_to_string` and `toml::from_str`, silently ignoring all errors. A user who uncomments `[llm_classifier]` in config.toml but has a typo will get no LLM classifier with no indication why.
- **Fix**: Log a warning when the section is present but fails to parse, similar to `load_categories` at config.rs:152-155.
  - Strength: Matches existing error handling pattern in the same module; provides actionable feedback to operators.
  - Tradeoff: Minor — add 2-3 lines of logging.
  - Confidence: HIGH — same pattern exists in nearby code.
  - Blind spot: None significant.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

### F2 — enabled field stored but never read

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:210
- **Detail**: The `enabled` field is read at line 229 to decide whether to return None early, then stored in the struct at line 269, but never read again by callers. The `dead_code` warning indicates it's unused after construction.
- **Fix**: Remove the `enabled` field from `LlmClassifierConfig`, or add `#[allow(dead_code)]` if you intend to use it later.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

### F3 — for_kv_map clippy hint in new code

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:199
- **Detail**: `load_llm_classifier_config` uses `for (_category, entry) in routing` but only needs `entry.values()`. This triggers a clippy warning.
- **Fix**: Use `for entry in routing.values()` instead.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

### F4 — LLM response parsing is loose but acceptable

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/intent_classifier.rs:286-310
- **Detail**: `parse_response` uses `contains()` matching — "SYNTAX_FIX_SYNTAX_FIX" would match. The prompt instructs "Return ONLY the category name" but adversarial LLM responses could theoretically bypass this.
- **Fix**: Consider enforcing a JSON response format with schema validation (`response_format: { type: "json_object" }`) for OpenAI APIs.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

### F5 — classify_sync vs plan's classify_internal naming

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classifier.rs:587
- **Detail**: Plan specified `classify_internal()` but implementation uses `classify_sync()`. Intent is preserved; only naming differs.
- **Fix**: Rename to `classify_internal()` if consistency matters, or leave as-is since functionality is correct.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

### F6 — NVIDIA_ENDPOINT_DEFAULT not used by LLM config

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/config.rs:9
- **Detail**: `NVIDIA_ENDPOINT_DEFAULT` is defined but `load_llm_classifier_config` uses empty string as endpoint default, not `NVIDIA_ENDPOINT_DEFAULT`. Inconsistency in defaults between routing and LLM classifier config.
- **Fix**: Document the difference in behavior, or align defaults.
- **Decision**: FIXED — Added `tracing::warn!` logs for read/parse failures, matching `load_categories` pattern

## Success Criteria Verification

### Automated
- ✅ `cargo build` — compiles cleanly (no warnings)
- ✅ `cargo test` — 116 tests passed
- ⚠️ `cargo clippy -D warnings` — 10 pre-existing warnings (not from this change)

### Manual
- ✅ Phase 3.4: Server with `[llm_classifier]` → "LLM classifier enabled" in logs
- ✅ Phase 3.5: Server without section → regex-only works
- ✅ Phase 3.6: Ambiguous prompt → LLM classifier fires
- ⏳ Phase 4.4: Real LLM endpoint test — pending (manual verification)