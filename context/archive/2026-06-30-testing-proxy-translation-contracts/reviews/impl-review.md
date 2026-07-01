<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Proxy Translation Contract Tests

- **Plan**: `context/changes/testing-proxy-translation-contracts/plan.md`
- **Scope**: Phase 1 + Phase 2 of 3 (re-review after post-triage refactor `3673f81`)
- **Date**: 2026-07-01
- **Verdict**: APPROVED (all findings resolved via triage)
- **Findings**: 0 critical  1 warning  2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS (after F1 fix) |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS (after F3 fix) |
| Success Criteria | PASS — `cargo test`: 439 passed |

## Findings

### F1 — Test name drift: 2.5 request-body capture test

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: `src/proxy/handlers.rs:3343` (pre-fix)
- **Detail**: Plan 2.5 contract specified test name `test_messages_handler_anthropic_request_translation_body_shape`. Implementation used `test_messages_handler_openai_translation_request_body`. The test also uses httpmock `.body_contains()` substring assertions rather than JSON-parsing the captured request body, but all functional checks (messages, system, max_tokens, no cache_control) are verified.
- **Fix**: Renamed test to `test_messages_handler_anthropic_request_translation_body_shape` to match the plan contract.
- **Decision**: FIXED — renamed (same pattern as F2 fix in prior review, commit `7c0b362`)

### F2 — Bare `.unwrap()` chains in buffered test

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: `src/proxy/handlers.rs:3312-3331` (pre-fix)
- **Detail**: `test_messages_handler_openai_translation_buffered` used bare `.get("type").unwrap().as_str().unwrap()` chains. These panic with unhelpful messages on mismatch. Sibling tests use `assert_eq!(..., Some(...))` or `.expect("...")`.
- **Fix**: Replaced bare `.unwrap()` with `.and_then(|v| v.as_str())` + `Some(...)` assertions and `.expect("...")` with descriptive messages.
- **Decision**: FIXED

### F3 — Inline `test_app_with_openai_translation` duplicates centralized harness

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: `src/proxy/handlers.rs:3181-3236` (pre-fix, deleted)
- **Detail**: The post-triage refactor (`3673f81`) centralized all 5 harnesses in `src/app/test_helpers.rs` into `test_app_with_provider`. `test_app_with_openai_translation` was left as an inline duplicate — a near-copy of `test_app_with_http_client` differing only in model name (`"gpt-4o"` vs `"sf-model"`) and registering only SYNTAX_FIX instead of SYNTAX_FIX+CASUAL.
- **Fix**: Replaced all 5 call sites with `test_app_with_http_client(env, 10_485_760)`, deleted the inline builder. Model name difference is immaterial (no test assertion depends on the routing model name). The extra CASUAL route entry is harmless (all callers use SYNTAX_FIX).
- **Decision**: FIXED

## Verification Evidence

- `cargo test --no-fail-fast` → **439 passed (1 suite, 10.80s)** — final
- `cargo test handlers --no-fail-fast` → **67 passed (0.77s)** — handler module
- All Phase 1 harnesses verified present: `test_app_with_nim_http_client`, `test_app_with_ollama_http_client` via `test_app_with_provider`
- All Phase 2 tests verified: 8/8 plan items MATCH, 1/9 DRIFT (F1, now fixed)

## Triage Outcome

- **Fixed**: F1 (rename test), F2 (replace bare .unwrap()), F3 (delete inline builder, use centralized harness)
- **Skipped**: none
- **Dismissed**: none

## Final Verdict

**APPROVED** — all findings resolved. Phase 1 + Phase 2 work is sound. Phase 3 remains deferred (reverted in `894681a`, all items pending in plan Progress).

## Notes

This is a re-review. The prior review (2026-06-30, findings F1-F7) was fully resolved via commits `894681a`, `24b6f43`, `7c0b362`, `3673f81`, `7bca895`. This review found 3 new findings introduced by the post-triage refactor (`3673f81`) and original implementation in `d3a347e`. All 3 are now fixed.
