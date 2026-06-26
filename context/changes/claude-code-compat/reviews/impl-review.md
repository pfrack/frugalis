<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Claude Code Compatibility

- **Plan**: context/changes/claude-code-compat/plan.md
- **Scope**: All 4 phases (automated items)
- **Date**: 2026-06-26
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical, 5 warnings, 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING ⚠️ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | PASS ✅ |
| Architecture | WARNING ⚠️ |
| Pattern Consistency | PASS ✅ |
| Success Criteria | WARNING ⚠️ (manual items pending) |

## Findings

### F1 — log_classification signature divergence from plan

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:1001-1053
- **Detail**: Plan specified modifying `log_classification` signature to accept `usage: Option<UsageBreakdown>` and `session_id: Option<&str>`. Instead, the old 8-param `log_classification` was preserved and a new 10-param `log_classification_with_usage` was added, sharing `enqueue_inference_record`. Functionally complete — no data gap — but the signature contract in the plan was not followed.
- **Fix**: Document the deviation as a plan addendum in the Progress section, noting the decomposition was intentional for backward compatibility with streaming open/close call sites.
- **Decision**: PENDING

### F2 — Usage capture missing on 2 of 3 streaming paths

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence, Observability
- **Location**: src/main.rs:1402-1489, 1903-2029
- **Detail**: Of the three streaming paths, only `handle_anthropic_streaming_response` (Anthropic upstream → OAI client) accumulates usage via `StreamTranslateState.collected_usage()` and calls `log_classification_with_usage` on close. The other two paths — `handle_streaming_response` (OpenAI passthrough) and `handle_translating_anthropic_stream` (OAI→Anth translated) — call `log_classification` at both open and close with no usage data. The plan's Phase 4 explicitly required token field finalization for all streaming paths: "restructure so token fields are finalized when the stream closes."
- **Fix A ⭐ Recommended**: Add usage accumulation to `handle_streaming_response` and `handle_translating_anthropic_stream`. For the OpenAI passthrough path, parse `usage` from the terminal SSE chunk. For the translated path, add usage fields to `AnthropicStreamState` and populate from `message_delta` usage.
  - Strength: Closes the observability gap fully per plan contract.
  - Tradeoff: Requires understanding SSE event parsing for OpenAI passthrough (the path that byte-passthroughs the upstream response).
  - Confidence: HIGH — the same pattern is already implemented in `handle_anthropic_streaming_response`.
  - Blind spot: No test pushes data through `handle_streaming_response` to verify the usage capture integration.
- **Fix B**: Document as known gap and defer to follow-up
  - Strength: Zero risk of regression; defers complex SSE parsing.
  - Tradeoff: Streaming paths (which are the primary Claude Code traffic pattern) lose token and session attribution in the inference log.
  - Confidence: MEDIUM — depends on whether observability is needed for streaming traffic.
  - Blind spot: The plan explicitly called for this to be done.
- **Decision**: PENDING

### F3 — Handler duplication in completion_handler and messages_handler

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Architecture
- **Location**: src/main.rs:2036-2439, 2454-2792
- **Detail**: `completion_handler` (403 lines) and `messages_handler` (339 lines) share ~250 lines of nearly identical provider-cascade logic (API key resolution, empty-endpoint handling, retry, error logging, provider iteration). Any future bugfix to the cascade loop must be applied twice. This is a pre-existing architectural issue, not introduced by this change.
- **Fix**: Extract the shared provider-cascade loop into a parameterized helper function. Out of scope for this change — flag as tech debt.
- **Decision**: PENDING

### F4 — Dead code suppression with #[allow(dead_code)]

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classifier.rs:91-98, src/persistence.rs:1068-1070
- **Detail**: `ClassificationResult` carries `endpoint`, `provider_type`, `api_key_env` with `#[allow(dead_code)]` — these fields are never read from the struct. `InferenceLog` carries `provider_attempts` and `final_provider` the same way. Per the project's "Delete dead code rather than suppressing warnings" lesson, these should be removed.
- **Fix**: Remove the dead fields from both structs. Pre-existing issue — not introduced by this change.
- **Decision**: PENDING

### F5 — Hardcoded model list / conditional shape omitted

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:872-908
- **Detail**: `models_handler` returns a hardcoded static list of 3 claude IDs with Anthropic shape. The plan's optional provision — "when the inbound request carries `anthropic-version`, return the Anthropic shape; otherwise the OpenAI shape" — was not implemented. The plan described this as optional, so this is a minor adherence note, not a defect.
- **Fix**: Either (A) implement the conditional shape, or (B) document in the plan why the optional feature was deferred (the endpoint always returns Anthropic shape, which works for both clients since Claude Code doesn't need the OpenAI shape).
- **Decision**: PENDING

### F6 — cache_control detection uses substring matching

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:2648
- **Detail**: `body_str.contains("\"cache_control\"")" on raw JSON string can false-positive if a string value contains the text. Only affects a debug log, not functional behavior.
- **Fix**: Use `serde_json::Value` structural check instead of substring match.
- **Decision**: PENDING

### F7 — Manual verification items pending

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Success Criteria
- **Location**: plan.md Progress section
- **Detail**: All 8 manual checklist items across 4 phases are unchecked. Automated items are all complete. Per the plan's "Implementation Note" after each phase, manual confirmation was expected before proceeding. However, the automated work was completed consistently across all phases and all automated tests pass.
- **Fix**: Complete manual verification or document intent to defer.
- **Decision**: PENDING
