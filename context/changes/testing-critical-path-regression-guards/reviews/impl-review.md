<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Critical-path regression guards (test rollout phase 1)

- **Plan**: `context/changes/testing-critical-path-regression-guards/plan.md`
- **Scope**: All 6 phases (full plan review)
- **Date**: 2026-06-14
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical, 4 warnings, 6 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING — Phase 3 helper contract is internally inconsistent in the plan; the implementation correctly resolved the ambiguity |
| Scope Discipline | FAIL — Squashed-merge PR `Tests #12` (35906ce) bundled ~1,200 lines of out-of-scope work from the `readme-bootstrap` and `opentelemetry-integration` change folders, plus 15 lines of OTel production code in `src/telemetry.rs` and a `cargo fmt` sweep on `src/config.rs`/`src/dashboard.rs`/`src/routing.rs` |
| Safety & Quality | WARNING — 3 test gaps (warn-log not asserted in F1 test, F2 inline mid-stream branch not integration-tested, F1 test could pass spuriously without an explicit `fail_next` consumption check) |
| Architecture | PASS |
| Pattern Consistency | WARNING — F2 docstring retains `F2` cross-reference label (per `lessons.md:26-31`, references should be by lessons.md rule, not finding number) |
| Success Criteria | PASS — 215 fast + 5 slow tests, all 5 gates (build, test, slow_tests, clippy `-D warnings`, fmt) green on 2026-06-14 |

## Findings

### F1 — Squashed-merge scope drift (readme-bootstrap + opentelemetry-integration bundled into Tests #12)

- **Severity**: ❌ CRITICAL *(escalated from WARNING — the magnitude of the drift changes the verdict)*
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Scope Discipline
- **Location**: `git show 35906ce --stat` (PR #12); `README.md` (656 new), `context/changes/readme-bootstrap/{change.md,research.md}` (366 new), `context/changes/opentelemetry-integration/plan.md` (16 changed), `context/changes/opentelemetry-integration/reviews/impl-review-2.md` (163 new)
- **Detail**: The Tests #12 PR's squashed merge bundled ~1,200 lines of artifacts from two unrelated in-flight changes. None of these are mentioned in the testing-critical-path-regression-guards plan's "Changes Required" or "References". Specifically:
  - `README.md` (656 lines, new file) — full top-level README, owned by `readme-bootstrap` (per cross-ref at `context/changes/opentelemetry-integration/plan.md:266`)
  - `context/changes/readme-bootstrap/{change.md,research.md}` (25 + 341 lines) — identity and research artifacts for a different change
  - `context/changes/opentelemetry-integration/plan.md` (16-line edit) — plan addenda for the OTel change (init() shape, classification_total relocation, Phase 4.2 README cross-ref)
  - `context/changes/opentelemetry-integration/reviews/impl-review-2.md` (163 lines, new) — OTel F1-F9 review report
  - 15 lines of OTel F6 production code in `src/telemetry.rs` (the `svc_name: &'static str` field + parameterless `trace_layer()`)
  - `cargo fmt` sweep on `src/config.rs` (427 lines), `src/dashboard.rs` (28 lines), `src/routing.rs` (2 lines) — zero semantic change, but technically drift
- **Fix A ⭐ Recommended**: Revert the OTel + readme-bootstrap parts from main, re-merge them in their own PRs
  - Strength: Restores PR-level scope discipline; future reviews can rely on PR scope = plan scope; the `cargo fmt` cleanup can be a separate "formatting" PR if desired.
  - Tradeoff: ~1,200 lines of accepted work need to be re-landed; the OTel F6 fix is already merged in main and reverting it could destabilize OTel.
  - Confidence: MEDIUM — depends on whether downstream users already depend on the bundled changes.
  - Blind spot: Git history will show the revert + re-merge; the README's content needs to be re-validated.
- **Fix B**: Accept the drift as the team's pragmatic decision; update the change.md to acknowledge it explicitly
  - Strength: Preserves current main; no rework.
  - Tradeoff: Future PRs may continue this pattern; plan-vs-PR-scope alignment is broken.
  - Confidence: HIGH on preserving work, LOW on establishing a sustainable process.
  - Blind spot: Doesn't address that the testing PR review (this one) couldn't meaningfully review the bundled work in plan context.
- **Decision**: ACCEPTED-AS-RULE: "Squash merges must not bundle unrelated in-flight changes into one PR" (added to `context/foundation/lessons.md`).

### F2 — `test_log_classification_failure_does_not_block_response` does not assert the warn log

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: `src/main.rs:2436-2491`
- **Detail**: The plan's Phase 2 §2 item 3 (plan.md:358-366) called for three assertions: (a) response status 200, (b) warn log emitted at configured `tracing` level, (c) bounded semaphore released. The test asserts only (a) and a proxy for (c) (`records.len() == 0`). It does not assert (b). The production log call is `error!(...)` at `src/persistence.rs:1157` (not `warn!` — also a plan/code mismatch). The test uses `tracing_subscriber::fmt().with_test_writer()` (line 2439) which writes to stdout, not a capturable layer.
  Additionally, the test could pass spuriously if the log task does not run within the 500ms wait: `records.len() == 0` is true both when the log task ran and consumed the failure, and when the log task never ran. The response-status assertion is the only direct check.
- **Fix**: Wire up `tracing-test` or a custom log-capturing layer, add `assert!(!memory_backend.fail_next.load(Ordering::SeqCst))` after the 500ms wait, and correct the plan's "warn" wording to match `persistence.rs:1157`'s `error!`.
  - Strength: Directly verifies the F1 "logging failure does not block response" contract, not just the side effect.
  - Tradeoff: Adds a `tracing-test` dev-dep or a small custom layer (~20 lines).
  - Confidence: HIGH — `fail_next` consumption check is mechanical; the log capture is the only complexity.
  - Blind spot: None significant.
- **Decision**: FIXED via Fix A (added `!memory_backend.fail_next` consumption check at `src/main.rs:2438`; corrected plan's "warn" → "error" wording at `plan.md:358`; `tracing-test` log capture deferred — the consumption check is the honest regression guard).

### F3 — F2 inline mid-stream error branch has no integration test

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: `src/main.rs:790-800` (call site); no test exists
- **Detail**: The plan (Phase 3 §3 item 5 optional) listed `test_inline_mid_stream_error_uses_same_format` as optional. The test was not written. The F2 helper is unit-tested in isolation (6 tests at `:3234-3281`) and `handle_streaming_error` is integration-tested (4+ tests at `:3288-3493`), but the inline mid-stream branch in `handle_streaming_response` (`:790-800`), which now also calls `format_sse_error_event`, has no end-to-end coverage. A future regression that modifies the inline branch (e.g., removing the helper call, switching back to `serde_json::json!()`) would not be caught.
- **Fix**: Add a test that spawns a TCP server that writes valid SSE headers then errors on the next chunk; assert the response body starts with `event: error\ndata: {"error":"..."}\n\n`.
  - Strength: Closes the F2 coverage gap end-to-end; protects the Phase 3 refactor's intent.
  - Tradeoff: ~30-50 lines of test setup; the arrange is non-trivial (timing + chunk framing).
  - Confidence: MEDIUM — depends on whether the arrange is doable with `tokio::io` or requires a deeper test fixture.
  - Blind spot: The test would assert the helper's output contract, not the specific inline-branch code path; a future change that reverts the inline branch to its own format (and updates the test) would be missed.
- **Decision**: FIXED via Fix A (added `test_inline_mid_stream_error_uses_same_format` at `src/main.rs:3550`; uses a real TCP server with `Content-Length: 1000` mismatch to force reqwest's mid-stream error; asserts both the first chunk is forwarded AND the SSE error event matches the helper's format; also asserts the `data:` payload is valid JSON with an `error` string field).

### F4 — Plan's Phase 3 helper contract is internally inconsistent

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: `context/changes/testing-critical-path-regression-guards/plan.md:139-149` (Critical Implementation Details), `:437-442` (Phase 3.1 Contract)
- **Detail**: The plan contains three contradictory statements about `format_sse_error_event`'s interface: (a) "helper takes a pre-escaped string" (Critical Implementation Details §2 prose), (b) "call sites apply escape rule before calling helper" (Critical Implementation Details §2 prose), (c) "the escape rule replaces \\ with \\\\" (Phase 3.1 Contract, ambiguous). The implementation correctly resolved to "helper owns the escape rule" — the cleanest choice — but the plan itself does not pick one canonical statement. A future plan author reading the existing plan will be confused.
- **Fix**: Update the plan's §Critical Implementation Details and §Phase 3.1 Contract to a single canonical statement: "`format_sse_error_event` takes a raw error message string, applies the JSON escape rule (`\\` → `\\\\`, `"` → `\\"`, `\n`/`\r` → ` `) internally, and returns the formatted SSE event body. Both call sites pass raw error strings."
  - Strength: Single source of truth for the contract; future readers and implementers get an unambiguous model.
  - Tradeoff: Modifies a closed plan; should be an addendum, not a silent edit.
  - Confidence: HIGH — the fix is mechanical and the implementation already matches the proposed canonical statement.
  - Blind spot: None significant.
- **Decision**: FIXED via Fix A (rewrote `plan.md:139-153` Critical Implementation Details §2 with a single canonical statement: "helper takes a raw error string, applies the escape rule internally"; preserved the Phase 3.1 Contract statement at `:437-442` since it was already aligned with the canonical model).

### F5 — F2 docstring retains `F2` cross-reference label (lessons.md violation)

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: `src/main.rs:850-873`
- **Detail**: The function-level docstring on `handle_streaming_error` opens with: "5 invariants protect this code path (the F2 review fixes; see `context/foundation/lessons.md` for the review history)". Per `lessons.md:26-31`: "If a prior review finding is relevant, reference it by its rule in lessons.md, not by its finding number." The inline `// Invariant 1:`, `// Invariant 2:`, etc. comments at `:875`, `:891-893`, `:900`, `:903` are also numeric labels, though they have descriptive content alongside.
- **Fix**: Replace "F2 review fixes" with a description like "5 invariants protect this code path (see `context/foundation/lessons.md` §'Re-run review after a follow-up change touches the same handler')" and rename `// Invariant 1:` → `// Truncate to 2 KB:`, etc.
  - Strength: Complies with the accepted rule; comments become self-describing.
  - Tradeoff: Minor — ~6 inline comment renames.
  - Confidence: HIGH.
  - Blind spot: The numbered `Invariant N` scheme is also used in the docstring; if the user wants to keep the numbering for cross-reference, leave it but add a note.
- **Decision**: PENDING

### F6 — `src/telemetry.rs` OTel F6 fix landed in the testing PR

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Scope Discipline
- **Location**: `src/telemetry.rs` (15 lines changed in 35906ce)
- **Detail**: The `svc_name: &'static str` field added to `OtelGuard` and the parameterless `trace_layer()` method are OTel F6 fixes (per `context/changes/opentelemetry-integration/reviews/impl-review-2.md` F6). They are OTel production code, not testing code. They should have been in the OTel change's PR, not the testing PR.
- **Fix**: This is part of F1's scope drift; treat as part of the same revert-and-re-merge decision.
  - Strength: Tied to F1's revert plan.
  - Tradeoff: Same as F1.
  - Confidence: HIGH.
  - Blind spot: None.
- **Decision**: PENDING (rolled into F1)

### F7 — Pre-existing env-var leaks in 3 tests (not introduced by rollout)

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: `src/main.rs:2170, 2239, 3194`
- **Detail**: Three pre-existing tests set env vars via raw `std::env::set_var` without an `EnvGuard` wrapper: `test_persistence_integration_sse_streaming_*` (lines 2170, 2239 set `MOCK_API_KEY`) and `test_streaming_handler_non_2xx_returns_sse_error_event` (line 3194 sets `TEST_STREAM_ERR`). The new tests in this rollout all use `EnvGuard` correctly. The 3 pre-existing tests are `#[serial]`, so concurrent pollution is not an issue, but env vars leak to subsequent tests in the same process.
- **Fix**: Add `let _guard = EnvGuard("MOCK_API_KEY");` and `let _guard = EnvGuard("TEST_STREAM_ERR");` at the cited lines.
  - Strength: Matches the pattern used by the new tests; closes a real (if low-impact) test-isolation gap.
  - Tradeoff: None significant.
  - Confidence: HIGH.
  - Blind spot: There may be other env-var leaks elsewhere; a quick `rg "std::env::set_var" src/` would surface them.
- **Decision**: PENDING

### F8 — `format_sse_error_event` does not escape all JSON control chars (RFC 8259 gap)

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: `src/main.rs:842-848`
- **Detail**: The helper escapes `\\`, `"`, `\n`, `\r` per the plan. It does not escape other JSON-unsafe control characters (tab `0x09`, backspace `0x08`, form feed `0x0C`, and others per RFC 8259 §7). The plan explicitly limited scope to the 4 chars, so this is a documented edge case, not a bug. In practice, upstream error bodies rarely contain raw control chars.
- **Fix**: Either (a) document the limitation in the helper docstring, or (b) extend the escape rule to handle all control chars and add a test case. Low priority.
- **Decision**: PENDING

### F9 — F2 inline mid-stream branch returns unbounded string (no 2KB cap or 512-char truncate)

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: `src/main.rs:790-800` (inline branch)
- **Detail**: `handle_streaming_error` caps the upstream body to 2 KB and truncates to 512 chars before calling the helper (`:877, :894-897`). The inline mid-stream branch passes `_e.to_string()` raw, with no cap. A pathological upstream could produce a longer error string. Pre-existing in the inline branch (the plan noted the 2 KB cap is upstream of the helper, not in the helper), but the inline branch never applied it.
- **Fix**: Apply the same 2 KB cap and 512-char truncate in the inline branch.
  - Strength: Bounds the SSE event size uniformly; protects clients with small buffers.
  - Tradeoff: Minor — extract a small `truncate_error_for_sse` helper or inline the truncate.
  - Confidence: HIGH.
  - Blind spot: Pre-existing gap; not introduced by the rollout.
- **Decision**: PENDING

### F10 — Phase 1.4 optional extension + stale line-number citations in the plan

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: `plan.md:267-269` (Phase 1.4), `:589-625` (Phase 4 line refs), `:227` (Phase 1.3 ref)
- **Detail**: The optional Phase 1.4 (CountingClassifier wrapper on the existing 2-backend chain test) was not done. The 3-backend integration test at `src/main.rs:1611-1724` already covers the same side-effect contract, so the impact is ≈ zero. Several other line-number citations in the plan are stale (Phase 4 `:3003-3185`, `:3047`, `:3148`; Phase 1.3 `:1243`, `:1333`; Phase 4 `:3025`). The contracts are satisfied; the citations are just out of date.
- **Fix**: Update the plan's line-number citations in an addendum; mark Phase 1.4 as "deferred — 3-backend test covers the same contract".
  - Strength: Plan becomes a more accurate reference for future maintainers.
  - Tradeoff: Modifies a closed plan; the team may prefer to leave historical plans as-is.
  - Confidence: MEDIUM — depends on team preference.
  - Blind spot: Phase 1.4 might still be valuable as defense-in-depth even with the 3-backend test in place.
- **Decision**: PENDING
