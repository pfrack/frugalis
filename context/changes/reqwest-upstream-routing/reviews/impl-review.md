<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: reqwest-upstream-routing

- **Plan**: context/changes/reqwest-upstream-routing/plan.md
- **Scope**: Phases 1-4 of 4
- **Date**: 2026-06-02
- **Verdict**: APPROVED
- **Findings**: 0 critical 0 warnings 0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | PASS ✅ |
| Architecture | PASS ✅ |
| Pattern Consistency | PASS ✅ |
| Success Criteria | PASS ✅ |

## Findings

### F1 — Log status "ok" written before upstream call completes

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:274
- **Detail**: `log_classification` is called with status `"ok"` at line 274, _before_ the upstream HTTP roundtrip (lines 341-376). If the upstream returns a non-2xx status or a connection error, the inference record committed to the database shows `status = "ok"` — misleading for operational metrics, cost-savings estimates, and downstream consumers querying the inferences table for successful completions.
- **Fix**: Move `log_classification` to after the upstream response (after line 378), using a status derived from the upstream result (`"ok"` for 2xx, `"upstream_error"` for errors). Alternatively, log a classification-only entry first and update/replace it after the upstream roundtrip.
  - Strength: Aligns the persisted status with the actual outcome; critical for accurate cost-savings dashboards that filter by status.
  - Tradeoff: The duration_ms measured by `start.elapsed()` would include the upstream roundtrip time, which changes the latency metric (this is arguably more correct — total wall-clock time from request to response — but differs from what the classify_handler logs).
  - Confidence: MEDIUM — the intent classification and the upstream roundtrip are separate concerns; the plan at line 143 says "Log classification (fire-and-forget, same as today)" but doesn't specify whether it's before or after the upstream call. Worth discussing whether a second log entry for the upstream result is better.
  - Blind spot: Haven't verified how the savings/latency dashboards consume the `status` field — filtering by `status = 'ok'` would currently count failed upstream calls as successes.

- **Decision**: FIXED — log_classification moved to after upstream response at each exit point with correct status (`"ok"`, `"upstream_error"`, `"bad_request"`).

### F2 — Silent UTF-8 failure producing empty prompt

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:222
- **Detail**: `std::str::from_utf8(&body).unwrap_or("")` at line 222 silently converts non-UTF-8 body bytes to an empty string. The Content-Type check at line 228 executes _after_ this, so a binary-body request with `Content-Type: application/json` reaches classification with an empty prompt (`""`). The classifier returns CASUAL/Fallback silently instead of rejecting the malformed request with a 400.
- **Fix**: Move the Content-Type check above `from_utf8`, and return a 400 `{"error":"bad_request","message":"invalid UTF-8 body"}` when `from_utf8` fails. This pattern is already partially present (line 319-328 handles JSON deserialization failures with a 400).
  - Strength: Prevents silent misclassification; matches the defensive JSON-error pattern already in the handler.
  - Tradeoff: None — the change is ~5 lines shifted earlier in the function.
  - Confidence: HIGH.
  - Blind spot: None significant.

- **Decision**: FIXED — Content-Type check moved before `from_utf8`; 400 returned on invalid UTF-8.

### F3 — Upstream error body echoed verbatim to client

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:366-376
- **Detail**: When the upstream API returns a non-2xx response, the full upstream response body is embedded verbatim into the `"message"` field of the Cerebrum error envelope returned to the client. Upstream provider error pages may contain internal IPs, stack traces, rate-limit headers, or API key fragments.
- **Fix A ⭐ Recommended**: Truncate the upstream body to the first 512 characters in the error envelope, and log the full body server-side.
  - Strength: Prevents information leakage while preserving enough context for debugging; low implementation cost.
  - Tradeoff: Some upstream error detail is lost to API consumers (though full error is still server-logged).
  - Confidence: HIGH — this is a standard reverse-proxy sanitization pattern.
  - Blind spot: Haven't checked whether any production upstream providers actually leak sensitive data in error bodies (unlikely but worth scanning logs).

- **Fix B**: Return a generic "upstream request failed" message without including any upstream body.
  - Strength: Maximum safety — zero leak potential.
  - Tradeoff: Debugging becomes much harder; every upstream error requires server-log inspection.
  - Confidence: HIGH.
  - Blind spot: None.

- **Decision**: FIXED via Fix A — upstream error body truncated to 512 chars in client response; full body logged server-side.

### F4 — Unbounded upstream response body buffering

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:354
- **Detail**: `upstream_response.text().await` buffers the _entire_ upstream response body into a `String` in memory with no maximum size bound. A misbehaving or compromised upstream (or one returning a large streaming response by mistake) could exhaust server memory. The 300s timeout bounds wall-clock time but not byte count.
- **Fix**: Replace `.text().await` with a bounded read — e.g., collect body bytes up to a limit and convert to string only up to that limit. For responses exceeding the limit, return a 502 with a message indicating the response was too large. A 10 MB cap is reasonable for chat completion responses.
  - Strength: Eliminates the unbounded-memory risk; the cap is generous enough for real model outputs (typical chat completions are <50KB).
  - Tradeoff: If a model returns an abnormally large response (>10MB), it will be truncated instead of delivered. But such responses would already be problematic for the original client.
  - Confidence: HIGH — this is a known reverse-proxy hardening practice.
  - Blind spot: Exact memory cap should be configurable or aligned with what the upstream API's max_tokens would produce.

- **Decision**: FIXED — upstream body read via chunked streaming with 10 MB cap; oversized responses return 502.

### F5 — 9 pre-existing persistence integration tests fail

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Success Criteria
- **Location**: src/persistence.rs (tests)
- **Detail**: Running `cargo test` reveals 9 failures in `persistence::tests::test_fetch_*` — latency summary, savings estimate, and related tests (`test_fetch_latency_summary_empty`, `test_fetch_latency_summary_with_data`, `test_fetch_latency_summary_time_filter`, `test_fetch_latency_summary_unclassified_count`, `test_fetch_savings_estimate_empty`, `test_fetch_savings_estimate_with_data`, `test_fetch_savings_estimate_filters_null_category`, `test_fetch_savings_estimate_unknown_cost_model`, `test_fetch_savings_estimate_historical_fallback`). These tests appear to run against a shared database with leftover data from prior test runs (assertions like `left: 50, right: 0` suggest stale records). **These failures are pre-existing and unrelated to this change** — all reqwest-upstream-routing tests pass and the handler changes do not touch persistence code.
  - Fix A ⭐ Recommended: Document this as known tech debt and address in a separate change focused on DB test isolation (e.g., per-test UUID filters or a test-specific schema).
    - Strength: Acknowledges the issue without blocking this change; the test failures predate the reqwest work.
    - Tradeoff: The plan's Success Criteria say "all existing tests pass" (Phase 1.2, 2.2, 3.1) — this criterion is technically not met, though the failures are in an unrelated module.
    - Confidence: HIGH — git blame would confirm these tests haven't changed in this commit.
    - Blind spot: Haven't verified whether these tests previously passed in CI (they may require a specific DB setup).

  - Fix B: Skip integration tests with `cargo test --lib` (only unit tests) and run integration tests separately with `cargo test persistence_integration`.
    - Strength: Clean separation of concerns; the reqwest change can claim "all unit tests pass."
    - Tradeoff: Hides existing integration test failures rather than surfacing them.
    - Confidence: MEDIUM — depends on whether this workflow is practical for the project.
    - Blind spot: CI pipeline may not support this split.

- **Decision**: FIXED — all 9 persistence integration tests now pass. Unique category/model prefixes isolate each test from stale data; delta-based assertions where needed; `.expect()` instead of `.ok()` on all INSERT/DELETE operations.

