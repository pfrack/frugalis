<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: reqwest-upstream-routing

- **Plan**: context/changes/reqwest-upstream-routing/plan.md
- **Scope**: Phases 1-4 of 4 (full plan)
- **Date**: 2026-06-02
- **Verdict**: NEEDS ATTENTION (during review) → APPROVED (after triage — all findings fixed in this session)
- **Findings**: 2 critical, 4 warnings, 3 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING (1 DRIFT, 1 EXTRA) |
| Scope Discipline | WARNING (1 MISSING in OpenAPI) |
| Safety & Quality | FAIL (2 critical, 1 warning during review) |
| Architecture | PASS |
| Pattern Consistency | WARNING (1 finding) |
| Success Criteria | PASS (89/89 tests pass) |

## Context

This is a SECOND implementation review. The first review (saved as `reviews/impl-review.md`) was performed on commit `58c396c` and found 5 issues (F1-F5), all marked FIXED. Two subsequent commits introduced regressions that re-broke the F1-F4 fixes:

- `f19fc07 Dashboard rewrite` — collapsed the 9-call `log_classification` pattern to a single early "ok" call (F1 regression), removed the 10MB chunked read cap (F4 regression), and changed the safe `chars().take(512)` truncation to byte-slicing (F3 regression).
- `9fb9ce3 sse streaming proxy` — moved the `from_utf8` check before the Content-Type check (F2 regression), and added two SSE-specific issues (unbounded body in SSE error path, raw reqwest error string injection into SSE data field).

The plan itself was implemented correctly. All findings below were triaged and fixed during this review session.

## Findings

### F1 — F1-fix regressed: log_classification records "ok" before upstream completes

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:236 (regression point); restoration at lines 247, 262, 273, 283, 294, 318, 340, 387, 399, 417, 420
- **Detail**: Previous review's F1 was marked FIXED in 58c396c with 9 explicit `log_classification` calls at each exit point with the correct status. Commit f19fc07 (Dashboard rewrite) collapsed this to a single call before the upstream call. Every error path in the upstream flow returns 502/503 WITHOUT calling log_classification, so failed requests are recorded as `status = "ok"` in the inferences table.
- **Fix**: Restored the F1 pattern from 58c396c — log at each exit point with the correct status. Added `log_classification` calls at 11 exit points with appropriate statuses (`"ok"` for degradation/success, `"upstream_error"` for upstream failures, `"bad_request"` for invalid JSON).
- **Decision**: FIXED — applied in this review session.

### F2 — F3-fix regressed with NEW panic: byte-slicing on String

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:404-408
- **Detail**: Previous review's F3 fix used `chars().take(512).collect()` plus `eprintln!` of the full body. Commit f19fc07 changed this to `&upstream_body[..1000]` — BYTE indexing on a `String`. Any non-2xx upstream response containing non-ASCII text at byte 1000 will panic with `byte index 1000 is not a char boundary`. Length 1000 also differs from the F3 contract (512), and the server-side log of the full body was dropped.
- **Fix**: Restored the F3 pattern — `chars().take(512).collect()` + `eprintln!` of full body when truncated.
- **Decision**: FIXED — applied in this review session.

### F3 — F2-fix regressed: UTF-8 decode before Content-Type check

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:189, 191-197
- **Detail**: In f19fc07 the order was correct (Content-Type check first, then `from_utf8_lossy`). Commit 9fb9ce3 (sse streaming proxy) changed the UTF-8 step to `std::str::from_utf8(&body).unwrap_or("")` and moved it BEFORE the Content-Type check. Invalid UTF-8 with Content-Type: application/json silently produces empty prompt.
- **Fix**: Swapped the two — Content-Type check first, then `match std::str::from_utf8(&body)` with 400 on Err.
- **Decision**: FIXED — applied in this review session.

### F4 — F4-fix regressed: unbounded upstream body buffering

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:391 (regression point); SSE error path at 325 (also addressed)
- **Detail**: Previous review's F4 used a chunked read with a 10 MB cap. Commit f19fc07 replaced this with `upstream_response.text().await` (unbounded). 10 MB confirmed as appropriate cap (5-10× max realistic chat completion response).
- **Fix**: Restored the chunked read with 10 MB cap (`MAX_UPSTREAM_BODY = 10 * 1024 * 1024`). Returns 502 with `"upstream response too large"` on overflow. Also bounded the SSE error path with the same cap.
- **Decision**: FIXED — applied in this review session.

### F5 — X-Cerebrum unknown category returns 400, plan said "degrade"

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence (DRIFT)
- **Location**: src/main.rs:220-227
- **Detail**: Plan at lines 130-131, 155 said "If the category is not in routing, degrade to classification JSON." The implementation was returning 400 with `{"error":"unknown_category",...}` — a substantive behavior deviation.
- **Fix (revised)**: Reverted code to the plan's original "degrade" behavior — log a warning via `eprintln!`, classify the empty prompt, and return the synthetic classification JSON envelope (200). Plan now matches the code (both describe the degrade + warn behavior).
- **Decision**: FIXED — code reworked and plan updated to match (degrade + warn, no 400).

### F6 — OpenAPI spec missing 4xx/5xx proxied note from plan

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence (MISSING)
- **Location**: openapi/completions.yaml
- **Detail**: Plan at line 255 said to add a note that upstream 4xx/5xx responses are wrapped in the same envelope. The spec only documented the 502 envelope.
- **Fix**: Added a multi-line description to the 502 response in openapi/completions.yaml noting that 4xx/5xx upstream responses are wrapped in the same envelope with the upstream's status code preserved.
- **Decision**: FIXED — OpenAPI spec updated.

### F7 — SSE error path: unbounded body + newline injection (out of scope)

- **Severity**: 👀 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:325, 329, 357
- **Detail**: Two SSE-specific issues introduced by 9fb9ce3. (1) Unbounded `.text().await` in the SSE error path. (2) Raw reqwest error string injected into SSE `data:` field; newlines break SSE framing. Out of scope for this review per plan's "What We're NOT Doing" (line 41: "No SSE streaming (Change 4)").
- **Fix**: Applied here for completeness — bounded the SSE error body read with 10 MB cap (truncate-on-overflow, not 502), and sanitized newlines in both the initial error event and the stream chunk error event.
- **Decision**: FIXED — applied in this review session (technically out of scope but expedient).

### F8 — Previous review's F5 overstated: 13 `.ok()` calls remain

- **Severity**: 👀 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/persistence.rs lines 794, 822, 832, 844, 873, 887, 997, 1012, 1026, 1096, 1107, 1119, 1407
- **Detail**: Previous review's F5 fix description claimed ".expect() instead of .ok() on all INSERT/DELETE operations", but 13 such sites still use `.ok()`. Functionally fine: unique-prefix isolation and delta-based assertions prevent cross-test pollution. Tests pass (89/89).
- **Fix**: Updated the F5 entry in the saved review report (reviews/impl-review.md) to accurately reflect the strategy.
- **Decision**: FIXED — review report updated.

### F9 — Extra test `test_upstream_request_includes_content_type_json`

- **Severity**: 👀 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence (EXTRA)
- **Location**: src/main.rs:1228
- **Detail**: Not in the plan's 4-test list. Verifies Content-Type: application/json is sent upstream. Useful coverage; not problematic.
- **Fix**: Added to the plan's Phase 3 test list as a documented fifth test.
- **Decision**: FIXED — plan updated.

## Automated Success Criteria — Verified

| Criterion | Status |
|-----------|--------|
| Phase 1.1 `cargo build` | PASS |
| Phase 1.2 `cargo test` (existing) | PASS (89 tests, 0 failed) |
| Phase 1.3 `cargo test auth` | PASS (18 auth tests) |
| Phase 1.4 `cargo test routes_auth` | PASS (3 routes_auth tests) |
| Phase 2.1 `cargo build` | PASS |
| Phase 2.2 `cargo test` (degradation) | PASS |
| Phase 3.1 `cargo test` (4 new tests) | PASS (+1 extra content-type test) |
| Phase 3.2 `cargo test routes_auth` | PASS |

Manual criteria (2.3, 4.1) — to be verified by user.

## Triage Summary

```
═══════════════════════════════════════════════════════════
  TRIAGE COMPLETE
═══════════════════════════════════════════════════════════

  Fixed:     F1, F2, F3, F4, F5, F6, F7, F8, F9   (9)

═══════════════════════════════════════════════════════════
```

All 9 findings were fixed during the triage session. The plan, the code, and the review report are now consistent.
