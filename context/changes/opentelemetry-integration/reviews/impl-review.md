<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: OpenTelemetry Integration

- **Plan**: context/changes/opentelemetry-integration/plan.md
- **Scope**: Phases 1–4 (full plan)
- **Date**: 2026-06-13
- **Verdict**: APPROVED
- **Findings**: 0 critical, 1 warning, 6 observations (all resolved)

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | PASS ✅ |
| Architecture | PASS ✅ |
| Pattern Consistency | PASS ✅ |
| Success Criteria | PASS ✅ |

## Automated Success Criteria

| Command | Result |
|---------|--------|
| `cargo check` (no feature) | PASS |
| `cargo check --features otel` | PASS |
| `cargo test` (no feature) | PASS (188/188) |
| `cargo test --features otel` | PASS (188/188) |
| `cargo clippy --features otel` | PASS (no new warnings) |
| `render.yaml` valid YAML | PASS |

### Manual Success Criteria

All manual items in the plan's Progress section remain unchecked (no OTel endpoint configured for verification). This is expected — manual verification requires a live OTLP collector.

## Findings

### F1 — `completion_handler` missing request/classification/duration metrics

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:775 (completion_handler)
- **Detail**: The plan specifies recording `requests_total`, `classification_total`, and `request_duration_seconds` in both `completion_handler` (Phase 3.1) and `classify_handler` (Phase 3.2). The implementation only adds these metrics to `classify_and_log` (called from `classify_handler`). `completion_handler` handles classification inline and never calls `classify_and_log`, so all `/v1/chat/completions` traffic bypasses these three metrics entirely. Only `upstream_duration_seconds` is recorded in the completion path.
- **Fix A ⭐ Recommended**: Add metric recording blocks in `completion_handler` matching the pattern in `classify_and_log`.
  - Strength: Completes the planned metric coverage; same 3-line `#[cfg]` blocks used elsewhere.
  - Tradeoff: Minor code duplication across the two handler paths.
  - Confidence: HIGH — identical pattern already proven in `classify_and_log`.
  - Blind spot: None significant.
- **Decision**: FIXED (added `requests_total`, `classification_total`, `request_duration_seconds` in `completion_handler`)

### F2 — `.expect()` on OTLP exporter builders

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/telemetry.rs:61-65, 72-76, 83-87
- **Detail**: Three `.build().expect(...)` calls on OTLP exporter builders. If the OTLP endpoint URL is malformed or TLS setup fails, the server panics at startup. The exporter builder creates configuration only (no network connection at build time), so this is a low-probability event. Follows the same pattern as `expect("Failed to bind TCP listener")` elsewhere, but converting to `Result` would match codebase conventions for fallible init.
- **Fix**: Propagate errors instead of `.expect()`. Change all three builders to return `Result` and let `init()` return `None` on failure.
- **Decision**: FIXED (replaced `.expect()` with `match` + `eprintln!` + `return None`)

### F3 — `Option` return type vs codebase `Result` pattern

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/telemetry.rs:44
- **Detail**: `telemetry::init()` returns `Option<(OtelGuard, Metrics)>`. Existing fallible init patterns (`AuthConfig::from_env()`, `PersistenceConfig` backends) use `Result<Self, String>`. The `Option` return swallows diagnostic info when `OTEL_ENABLED` is unset (intentional), but the hidden `.expect()` inside panics on build failures — an inconsistent mix of recovery strategies.
- **Fix**: Convert `init()` to return `Result<(OtelGuard, Metrics), String>` (or `Box<dyn Error>`), with `OTEL_ENABLED=false` being `Err` that the caller maps to `None`.
- **Decision**: FIXED (combined with F2 — `.expect()` replaced with `match` + `eprintln!` + `return None`; kept `Option` return with explicit error logging on failure)

### F4 — `String::leak()` for `OTEL_SERVICE_NAME`

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/telemetry.rs:53-55
- **Detail**: `std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(...).leak()` is used to get a `&'static str` for the OTel resource builder. This memory leak is intentional and bounded (one string per process lifetime, typically <100 bytes). However, it introduces a `leak()` pattern that could be copied elsewhere without the same bounded justification.
- **Fix**: Accept as-is — single process-lifetime allocation is acceptable. Document why `leak()` is used with a brief comment to discourage casual adoption.
- **Decision**: FIXED (added comment explaining why `leak()` is used and discouraging casual adoption)

### F5 — Metric export failures silently dropped

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/telemetry.rs:89-92
- **Detail**: `SdkMeterProvider::with_periodic_exporter()` uses a background task that swallows export errors by default. If the OTLP endpoint becomes unreachable after startup, metric data loss occurs silently. This is acceptable per the plan's "fire-and-forget" design intent but means operators have no direct signal when metrics are not reaching the backend.
- **Fix**: Accept as fire-and-forget per plan. OTel SDK's `PeriodicReader` worker already logs export errors internally via `otel_error!` (maps to `tracing::error!`).
- **Decision**: ACCEPTED (SDK already logs export errors internally; no change needed)

### F6 — `upstream_start` `#[cfg]` fragility

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:905-925
- **Detail**: `upstream_start` is declared under `#[cfg(feature = "otel")]` and used in two subsequent `#[cfg]` blocks. This is correct but fragile — a refactor that moves any `.record()` call outside its cfg guard would fail to compile. No immediate action needed.
- **Fix**: Accept as-is. Alternative: always create `upstream_start` (unconditionally) — measurable overhead is ~nanoseconds per request.
- **Decision**: FIXED (made `upstream_start` unconditional with `#[cfg_attr(not(feature = "otel"), allow(unused_variables))]`)

### F7 — Provider shutdown before process exit

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:430-433
- **Detail**: `guard.shutdown()` is called after `axum::serve` returns, but the global tracing subscriber still holds layers that reference the tracer/logger providers. After shutdown, further trace/log emission is silently dropped. In practice the process exits immediately after, so this window is negligible.
- **Fix**: Accept as-is — process exit immediately follows shutdown call.
- **Decision**: SKIPPED (process exits immediately after; window is negligible)
