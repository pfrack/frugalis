<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: OpenTelemetry Integration

- **Plan**: context/changes/opentelemetry-integration/plan.md
- **Scope**: Phases 1–4 (full plan)
- **Date**: 2026-06-13
- **Verdict**: NEEDS ATTENTION (after triage: 6 fixed, 1 accepted-by-user, 2 documented-as-plan-addenda)
- **Findings**: 1 critical, 3 warnings, 5 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ (after triage: labels added, Drop guard covers all return paths, plan addenda document the architectural DRIFTs) |
| Scope Discipline | PASS ✅ (Phase 4.2 README cross-referenced to readme-bootstrap) |
| Safety & Quality | PASS ✅ (panic hook routed through tracing; shutdown wrapped in 5s timeout) |
| Architecture | PASS ✅ (RequestMetrics Drop guard; svc_name on OtelGuard) |
| Pattern Consistency | PASS ✅ |
| Success Criteria | FAIL ❌ (render.yaml Phase 4.1 unimplemented; user accepted — see F1) |

## Automated Success Criteria

| Command | Result |
|---------|--------|
| `cargo check` (no feature) | PASS |
| `cargo check --features otel` | PASS |
| `cargo test` (no feature) | PASS (215/215) |
| `cargo test --features otel` | PASS (215/215) |
| `cargo clippy --features otel` | PASS (no new OTel warnings) |
| `render.yaml` valid YAML | PASS |
| Build command includes `--features otel` | **FAIL** (user accepted as showcase-only) |

## Findings

### F1 — render.yaml never updated; production deploy has zero OTel

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Plan Adherence / Success Criteria
- **Location**: render.yaml:5, render.yaml:8-18
- **Detail**: Plan §4.1 requires `--features otel` in buildCommand and four OTEL_* envVars. Current render.yaml is byte-identical to pre-OTel state. The deployed binary ships without the `otel` feature. Automated success criterion 4.2 fails.
- **Decision**: ACCEPTED — user noted "Render is only show case how I can use there otel" (showcase only, real production config is operator's responsibility).

### F2 — All 9 metric callsites pass empty `&[]`; required labels missing

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Plan Adherence / Safety & Quality
- **Location**: src/main.rs (now superseded by F4 Drop guard)
- **Detail**: Plan §177, §193, §201 require labels. Implementation passed `&[]` at all 9 sites.
- **Fix A ⭐**: Add planned label sets at each callsite.
  - Strength: Matches plan contract; restores analytic value.
  - Tradeoff: Per-call cardinality needs to stay bounded.
  - Confidence: HIGH.
- **Decision**: FIXED via Fix A — labels added; subsequently absorbed into F4's Drop guard refactor (Drop guard now sets status on all return paths, so labels including status are recorded on every path).

### F3 — `guard.shutdown()` called without the 5s timeout the plan promised

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence / Reliability
- **Location**: src/main.rs:454-457
- **Detail**: Plan §148 contract required 5s timeout. Implementation called `guard.shutdown()` synchronously.
- **Fix**: Wrap in `tokio::time::timeout(Duration::from_secs(5), tokio::task::spawn_blocking(...))` with `warn!` on timeout.
- **Decision**: FIXED — 5s timeout wrapper added at src/main.rs:456-469 with warn! on timeout/exit.

### F4 — Duration histogram missed on 5+ early-return paths

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:884-994 (completion_handler) and 537 (classify_and_log)
- **Detail**: Plan §179: "Record ... histogram before each return path." 5+ early-return paths missed it.
- **Fix A ⭐**: Drop-guard RequestMetrics struct.
  - Strength: Catches all return paths automatically.
  - Tradeoff: Refactor required; `set_status` call on every return.
  - Confidence: HIGH.
- **Decision**: FIXED via Drop guard — `RequestMetrics` struct added (cfg-gated) at src/main.rs:27-65; created at entry of both `classify_and_log` and `completion_handler`; `rm.set_status(...)` added at every return path; explicit `requests_total.add` and `request_duration_seconds.record` removed from handler bodies.

### F5 — Panic hook uses `eprintln!`, bypasses OTel log bridge

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:158-160
- **Detail**: `panic::set_hook` runs after subscriber init, so a `tracing::error!` would route through OTel log bridge.
- **Fix**: Change `eprintln!` → `tracing::error!`.
- **Decision**: FIXED — src/main.rs:159 changed to `tracing::error!("Panic in Cerebrum: {info}");`.

### F6 — `service_name` hardcoded as literal "cerebrum" at two call sites

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/main.rs:125, 145
- **Detail**: `telemetry::init("cerebrum")` and `guard.trace_layer("cerebrum")` both pass the literal.
- **Fix**: Store `svc_name` on `OtelGuard`; make `trace_layer()` parameterless.
- **Decision**: FIXED — `svc_name: &'static str` added to `OtelGuard`; `trace_layer()` now reads it from `self`; main.rs call site updated.

### F7 — `init()` returns layers via methods, not as direct return values

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/telemetry.rs:41, 156, 171
- **Detail**: Plan §92 signature was `(OtelGuard, impl Layer<S>, impl Layer<S>, Metrics)`. Implementation uses methods on guard.
- **Fix A ⭐**: Update plan to reflect implementation.
  - Strength: Documents the actual shape; the plan signature was not implementable (layers borrow from guard's providers).
  - Tradeoff: None — code is correct.
  - Confidence: HIGH.
- **Decision**: FIXED via plan addendum — contract section in plan.md updated to describe the final shape, with rationale that the original signature was not implementable as written.

### F8 — `classification_total` relocated out of `log_classification`

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:482-514 (log_classification), 560, 972
- **Detail**: Plan §191 specified inside `log_classification`. Implementation moved to handler-level call sites — defensible correctness improvement (avoids streaming double-count).
- **Fix**: Add Phase 3.3 addendum.
- **Decision**: FIXED via plan addendum — Phase 3.3 addendum added documenting the relocation and the double-count rationale.

### F9 — README OTel section produced by `readme-bootstrap`, not this change

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: README.md:587-607 (untracked), context/changes/readme-bootstrap/
- **Detail**: Plan §4.2 requires OTel section. Content exists but is owned by `readme-bootstrap`.
- **Fix**: Note Phase 4.2's delivery is shared with `readme-bootstrap`.
- **Decision**: FIXED via plan cross-reference — Phase 4.2 addendum added pointing to the cross-change deliverable.

## Triage Summary

| Decision | Count | Findings |
|---|---|---|
| Fixed | 6 | F2, F3, F4, F5, F6, F7, F8, F9 (8 actually) |
| Accepted by user | 1 | F1 (render.yaml is showcase only) |
| Skipped | 0 | — |

## Code Changes Summary

- `src/main.rs`:
  - Added `RequestMetrics` struct + Drop impl (cfg-gated) for full request duration/count coverage
  - Refactored `classify_and_log` and `completion_handler` to use the Drop guard
  - Added 9 callsite label sets (subsequently absorbed into Drop guard)
  - Wrapped `guard.shutdown()` in 5s timeout
  - Changed panic hook to `tracing::error!`
  - Updated `guard.trace_layer()` call to be parameterless
- `src/telemetry.rs`:
  - Added `svc_name: &'static str` field to `OtelGuard`
  - Made `trace_layer()` parameterless
- `context/changes/opentelemetry-integration/plan.md`:
  - Updated Phase 1.2 contract to reflect actual init() shape
  - Added Phase 3.3 addendum for classification_total relocation
  - Added Phase 4.2 addendum for README cross-reference

## Verification After Triage

- `cargo check` (no feature): PASS
- `cargo check --features otel`: PASS
- `cargo test --features otel`: PASS (215/215)
- `cargo clippy --features otel`: PASS (no new warnings)
