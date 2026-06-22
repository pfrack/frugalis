<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: OpenTelemetry Integration

- **Plan**: context/changes/opentelemetry-integration/plan.md
- **Scope**: Phases 1–4 (full plan)
- **Date**: 2026-06-15
- **Verdict**: NEEDS ATTENTION → after triage: 3 fixed
- **Findings**: 0 critical, 1 warning, 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ (F2 fixed — Drop guard coverage restored) |
| Scope Discipline | PASS ✅ |
| Safety & Quality | PASS ✅ (F1 fixed — model label removed, label set is now bounded) |
| Architecture | PASS ✅ |
| Pattern Consistency | PASS ✅ (F3 fixed — eprintln! replaced with tracing::warn!) |
| Success Criteria | PASS ✅ (Phase 4.2 intentionally unmet per impl-review-2 F1) |

## Automated Success Criteria

| Command | Result |
|---------|--------|
| `cargo check` (no feature) | PASS |
| `cargo check --features otel` | PASS |
| `cargo test` (no feature) | PASS (221/221) |
| `cargo test --features otel` | PASS (221/221) |
| `cargo clippy --features otel --all-targets -- -D warnings` | PASS |
| `cargo tree --features otel` (no duplicate TLS) | PASS |
| `render.yaml` valid YAML | PASS |
| Build command includes `--features otel` | **intentionally unmet** (per impl-review-2 F1 user-accepted) |

### 8/8 impl-review-2 fixes verified in place

| Fix | Contract | Status |
|---|---|---|
| F2/F4 (labels on Drop guard) | method/route/status | ✅ in place at src/main.rs:56-60 |
| F3 (5s timeout wrapper) | `timeout(5s, spawn_blocking(...))` + `warn!` | ✅ in place at src/main.rs:498-509 |
| F4 (Drop guard coverage) | `rm.set_status()` on all return paths | ✅ restored in this review (F2) — see below |
| F5 (panic hook via tracing) | `tracing::error!` | ✅ in place at src/main.rs:204 |
| F6 (svc_name on OtelGuard) | field + parameterless `trace_layer()` | ✅ in place at src/telemetry.rs:21, 158-164 |
| F7 (plan addendum — init() shape) | addendum in plan | ✅ in place at plan.md:98 |
| F8 (plan addendum — classification_total relocation) | addendum in plan | ✅ in place at plan.md:201 |
| F9 (plan addendum — README cross-reference) | addendum in plan | ✅ in place at plan.md:265 |

## Findings

### F1 — Unbounded OTel label cardinality via X-Cerebrum-Model

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:1100, src/main.rs:1120-1121 (source: src/main.rs:1001)
- **Detail**: `completion_handler` accepts `X-Cerebrum-Model` as an override header and propagates its raw value into `ClassificationResult.model` at src/main.rs:1001. `category` is validated against the routing table, but `model` is taken verbatim. That same value is used as an OTel metric label at src/main.rs:1100 (error path) and 1120-1121 (success path). Any authenticated client can send a unique `X-Cerebrum-Model` per request and explode OTel label cardinality, causing memory blowup in the collector/backend. All other labels (`category`, `tier`, `provider`, `method`, `route`, `status`) are bounded.
- **Fix A ⭐ Recommended**: Drop the `model` label (provider_type already in the same set).
  - Strength: One-line at 2 sites; no schema change; provider_type is already bounded and informative.
  - Tradeoff: Loses model-level analytics (small — model is captured in traces via `svc_name`/spans).
  - Confidence: HIGH.
  - Blind spot: Dashboards that filter by `model` will break.
- **Fix B**: Add a `model` field to `RouteEntry`, validate header against it.
  - Strength: Preserves model-level analytics; bounded by config.
  - Tradeoff: Multi-file change (config.rs, intent_classifier.rs).
  - Confidence: HIGH.
  - Blind spot: Migration of existing routing entries.
- **Fix C**: Hash or truncate the header value before labeling.
  - Strength: Smallest code change; preserves some signal.
  - Tradeoff: Hash is opaque; truncation loses precision.
  - Confidence: MEDIUM.
  - Blind spot: Hash collisions across legitimate models.
- **Decision**: FIXED via Fix A — `KeyValue::new("model", classification.model.clone())` removed from both call sites. Label set is now `[provider, status]` (both bounded).

### F2 — RequestMetrics::set_status missing at buffered-response final return

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:1148-1157
- **Detail**: F4 from impl-review-2 (Drop guard covers all return paths) was correctly applied in the original OTel PR, but the Tests PR (commit 35906ce, +1543/-369 in src/main.rs) rewrote completion_handler and the `set_status` call was not re-applied at the buffered-response final return. The handler computes the real upstream status via `handle_buffered_response` and branches `log_status` on it, but did not call `rm.set_status(status)` before returning. Result: for non-200 upstream responses on the buffered path, the `cerebrum.requests.total` and `cerebrum.request.duration_seconds` metrics were labeled with `status="200"` instead of the actual status. This is the exact pattern the lesson "Re-run review after a follow-up change touches the same handler" was written to catch. All 7 other return paths in completion_handler (lines 963, 975, 1066, 1083, 1107, 1132) correctly set_status; this was the only gap.
- **Fix**: Add `rm.set_status(status);` at the buffered-response final return. The local `status` is already in scope from `handle_buffered_response`.
- **Decision**: FIXED — `#[cfg(feature = "otel")] rm.set_status(status);` added at src/main.rs:1156 (just before the final `json_response(status, body)`).

### F3 — eprintln! in telemetry::init() instead of tracing::warn!

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/telemetry.rs:67, 84, 101
- **Detail**: All other operational-failure logs in the codebase use `tracing::warn!` / `tracing::error!`. `telemetry::init()` used `eprintln!` for the three OTLP exporter build failures, breaking the codebase pattern. Note: `telemetry::init()` runs at src/main.rs:168, before the global tracing subscriber is set at src/main.rs:197 — so `tracing::warn!` is a no-op at that point. The user chose to accept this trade-off in favor of codebase consistency.
- **Fix**: Replace the three `eprintln!` calls with `tracing::warn!`.
- **Decision**: FIXED — `eprintln!("OTLP {Exporter} failed to build: {e}")` → `tracing::warn!("OTLP {Exporter} failed to build: {e}")` at all three sites.

## Triage Summary

| Decision | Count | Findings |
|---|---|---|
| Fixed | 3 | F1 (Fix A), F2, F3 |

## Code Changes Summary

- `src/main.rs`:
  - Removed `KeyValue::new("model", classification.model.clone())` from the upstream_duration_seconds recording at both call sites (error path and success path) — `src/main.rs:1100, 1121`
  - Added `#[cfg(feature = "otel")] rm.set_status(status);` at the buffered-response final return — `src/main.rs:1156`
- `src/telemetry.rs`:
  - Replaced `eprintln!` with `tracing::warn!` at all three OTLP exporter build-failure sites — `src/telemetry.rs:67, 84, 101`

## Verification After Triage

- `cargo check` (no feature): PASS
- `cargo check --features otel`: PASS
- `cargo test --features otel`: PASS (221/221)
- `cargo test` (no feature): PASS (221/221)
- `cargo clippy --features otel --all-targets -- -D warnings`: PASS
