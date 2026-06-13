# OpenTelemetry Integration Implementation Plan

## Overview

Integrate OpenTelemetry into cerebrum behind a Cargo feature flag (`otel`), exporting application-level traces, metrics, and logs via OTLP HTTP to any compatible backend. Add graceful shutdown to flush telemetry on process exit. The existing `tracing` crate infrastructure means traces require zero business-logic changes; metrics require explicit instruments at 4 handler callsites; logs bridge automatically via `opentelemetry-appender-tracing`.

## Current State Analysis

Cerebrum uses `tracing` + `tracing-subscriber` with a layered registry pattern (`src/main.rs:115-123`). HTTP request spans are already created by `TraceLayer::new_for_http()` at line 975. Classification outcomes are logged to PostgreSQL via `log_classification()` but no real-time metrics exist. There is no graceful shutdown — `axum::serve` blocks until the process is killed.

### Key Discoveries:

- `src/main.rs:123` — `tracing_subscriber::registry().with(fmt_layer).init()` is the insertion point for OTel layers
- `src/main.rs:389-394` — Server startup with no graceful shutdown; needs `with_graceful_shutdown()` wrapper
- `src/main.rs:400-430` — `log_classification()` helper computes `start.elapsed()` for duration; natural metrics point
- `src/main.rs:717-870` — `completion_handler` has `let start = Instant::now()` at line 722; upstream request at line 832 has no independent timing
- `src/main.rs:875-890` — `classify_handler` with its own `start` at line 880
- `Cargo.toml` — Already depends on `reqwest` with `rustls-tls`, so `opentelemetry-otlp` HTTP transport adds no new TLS stack
- `render.yaml` — Uses env vars for configuration; OTLP endpoint/headers fit this model

## Desired End State

When built with `--features otel` and `OTEL_ENABLED=true` is set at runtime:
- All `tracing` spans (including HTTP request spans from tower-http) are exported as OTel traces via OTLP HTTP
- 4 application metrics are recorded and exported: request count, request duration, classification count, upstream duration
- All `tracing` events at `info`+ level are exported as OTel logs with automatic trace/span ID correlation
- On SIGTERM/SIGINT, pending telemetry is flushed before exit (bounded to 5s timeout)

When built without the feature, or when `OTEL_ENABLED` is unset: zero overhead, no OTel code runs.

Verification: deploy to Render with Grafana Cloud OTLP endpoint configured, confirm traces/metrics/logs appear in Grafana within 60s of a request.

## What We're NOT Doing

- Replacing the PostgreSQL-based dashboard — OTel complements it, both coexist
- Adding a `/metrics` Prometheus scrape endpoint — OTLP push is sufficient
- Custom span instrumentation on individual functions — the existing `tracing` spans and `TraceLayer` are enough
- Head-based sampling — at current traffic (<1000 req/s), 100% sampling is fine
- W3C `traceparent` header propagation to upstream LLM providers — they don't support it

## Implementation Approach

Add a `src/telemetry.rs` module behind `#[cfg(feature = "otel")]` that owns all OTel provider initialization, shutdown, and metric instrument creation. The main tracing subscriber init conditionally adds OTel layers. Metrics are recorded via global meter instruments accessed from handler code (also feature-gated). Graceful shutdown uses `tokio::signal` to catch SIGTERM/SIGINT and flush providers before exit.

## Phase 1: Feature-Gated Dependencies & Telemetry Module

### Overview

Add OTel crates behind a Cargo feature flag and create the `src/telemetry.rs` module that encapsulates provider initialization, metric instruments, and shutdown.

### Changes Required:

#### 1. Cargo.toml — Add feature flag and conditional dependencies

**File**: `Cargo.toml`

**Intent**: Add an `otel` feature that pulls in the 5 OTel crates with HTTP transport. Keep default features empty so dev builds are unaffected.

**Contract**: New `[features]` section with `otel` feature; 5 new conditional dependencies under `[dependencies]` using `optional = true` or `dep:` syntax.

```toml
[features]
default = []
otel = [
    "dep:opentelemetry",
    "dep:opentelemetry_sdk",
    "dep:opentelemetry-otlp",
    "dep:tracing-opentelemetry",
    "dep:opentelemetry-appender-tracing",
]

# Under [dependencies]:
opentelemetry = { version = "0.28", features = ["trace", "metrics", "logs"], optional = true }
opentelemetry_sdk = { version = "0.28", features = ["rt-tokio", "trace", "metrics", "logs"], optional = true }
opentelemetry-otlp = { version = "0.28", default-features = false, features = ["http-proto", "trace", "metrics", "logs", "reqwest-client", "reqwest-rustls"], optional = true }
tracing-opentelemetry = { version = "0.29", optional = true }
opentelemetry-appender-tracing = { version = "0.28", optional = true }
```

Note: Pin to the 0.28 compatible set (tracing-opentelemetry version is always otel+1). If 0.32 is confirmed available and compatible at implementation time, use that instead — the API patterns are the same.

#### 2. src/telemetry.rs — OTel module

**File**: `src/telemetry.rs`

**Intent**: Encapsulate all OTel initialization in one module. Exports an `init()` function that returns provider handles and OTel tracing/log layers, plus a `shutdown()` function, plus a `Metrics` struct holding the 4 instruments.

**Contract**:

- `pub struct OtelGuard` — holds `SdkTracerProvider`, `SdkMeterProvider`, `SdkLoggerProvider`
- `pub struct Metrics` — holds `Counter<u64>` for requests, `Histogram<f64>` for duration, `Counter<u64>` for classifications, `Histogram<f64>` for upstream duration
- `pub fn init(service_name: &str) -> Option<(OtelGuard, impl Layer<S>, impl Layer<S>, Metrics)>` — reads `OTEL_ENABLED` env var; returns `None` if disabled. Builds OTLP HTTP exporters, creates providers, returns layers + metrics instruments
- `pub fn shutdown(guard: OtelGuard)` — calls `.shutdown()` on all three providers (traces → metrics → logs order)
- Reads standard env vars: `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`

#### 3. src/main.rs — Declare module

**File**: `src/main.rs`

**Intent**: Add the conditional module declaration.

**Contract**: `#[cfg(feature = "otel")] mod telemetry;`

### Success Criteria:

#### Automated Verification:

- `cargo check` passes (no feature): confirms no breakage to default build
- `cargo check --features otel` passes: confirms OTel deps resolve and telemetry module compiles
- `cargo test` passes (no feature): existing tests unaffected

#### Manual Verification:

- Review that `cargo tree --features otel` shows expected OTel dependency tree without duplicate TLS stacks

---

## Phase 2: Tracing Integration & Graceful Shutdown

### Overview

Wire the OTel tracing and log layers into the subscriber registry. Add a signal handler so the server shuts down gracefully and flushes telemetry.

### Changes Required:

#### 1. src/main.rs — Conditional OTel layer registration

**File**: `src/main.rs` (tracing init block, around line 115-123)

**Intent**: When `otel` feature is enabled and `telemetry::init()` returns `Some`, add the OTel tracing layer and log bridge layer to the registry alongside the existing fmt layer. Store the `OtelGuard` and `Metrics` for later use.

**Contract**: The registry init changes from `.with(fmt_layer).init()` to `.with(fmt_layer).with(otel_trace_layer).with(otel_log_layer).init()` (conditionally). The `OtelGuard` is held in a variable that lives until shutdown. `Metrics` is stored in `AppState` or passed alongside it.

#### 2. src/main.rs — Add Metrics to AppState

**File**: `src/main.rs` (AppState struct)

**Intent**: Store OTel metric instruments in AppState so handlers can record metrics.

**Contract**: Add field `#[cfg(feature = "otel")] pub metrics: Option<telemetry::Metrics>` to `AppState`. Populated from `telemetry::init()` result during startup.

#### 3. src/main.rs — Graceful shutdown with OTel flush

**File**: `src/main.rs` (server startup, around line 389-394)

**Intent**: Replace bare `axum::serve(listener, app).await` with `.with_graceful_shutdown(signal)` and flush OTel providers after the server stops accepting connections.

**Contract**: Add `async fn shutdown_signal()` that awaits SIGTERM or SIGINT via `tokio::signal`. After `axum::serve` returns, call `telemetry::shutdown(guard)` with a 5s timeout. Log the shutdown event before flushing.

### Success Criteria:

#### Automated Verification:

- `cargo check --features otel` passes
- `cargo test` passes (existing tests unaffected)
- `cargo test --features otel` passes (OTel init is skipped in tests when `OTEL_ENABLED` is unset)

#### Manual Verification:

- Run locally with `OTEL_ENABLED=true` and an OTLP endpoint (e.g., local collector or Grafana Cloud); confirm traces appear after making a request
- Send SIGTERM to the process; confirm "Shutdown signal received" log line and clean exit within 5s

---

## Phase 3: Metrics Instrumentation

### Overview

Add the 4 core metrics at handler callsites: request count, request duration, classification count, and upstream duration.

### Changes Required:

#### 1. src/main.rs — Record request count and duration in completion_handler

**File**: `src/main.rs` (`completion_handler`, starting at line 717)

**Intent**: Increment `cerebrum.requests.total` counter at handler entry. Record `cerebrum.request.duration_seconds` histogram before each return path. Labels: method, route, status.

**Contract**: Feature-gated block that accesses `state.metrics` and calls `.add(1, &attributes)` on the counter and `.record(duration, &attributes)` on the histogram. Duration measured from existing `start` variable.

#### 2. src/main.rs — Record request count and duration in classify_handler

**File**: `src/main.rs` (`classify_handler`, starting at line 875)

**Intent**: Same pattern as completion_handler — increment request counter, record duration.

**Contract**: Same instrument access pattern, different route label value.

#### 3. src/main.rs — Record classification count in log_classification

**File**: `src/main.rs` (`log_classification`, line 400)

**Intent**: Increment `cerebrum.classification.total` counter each time a classification is logged. Labels: category, tier.

**Contract**: Feature-gated block after the existing DB logging logic. Accesses metrics from `state.metrics`.

#### 4. src/main.rs — Record upstream duration in completion_handler

**File**: `src/main.rs` (around the upstream request at line 832)

**Intent**: Add an `Instant::now()` before `upstream_req.send().await` and record `cerebrum.upstream.duration_seconds` after the response arrives. Labels: provider, model, status.

**Contract**: New `let upstream_start = Instant::now();` before the send, and a histogram `.record()` after the response is received (or error). This timing is independent of the overall handler duration.

### Success Criteria:

#### Automated Verification:

- `cargo check --features otel` passes
- `cargo test --features otel` passes — metrics recording with `None` metrics is a no-op
- `cargo clippy --features otel` passes

#### Manual Verification:

- Run locally with OTLP exporter pointed at a collector; make several requests of different types
- Verify in Grafana that `cerebrum.requests.total`, `cerebrum.request.duration_seconds`, `cerebrum.classification.total`, and `cerebrum.upstream.duration_seconds` appear with correct labels
- Confirm zero overhead when `metrics` is `None` (feature disabled or OTEL_ENABLED unset)

---

## Phase 4: Deployment Configuration

### Overview

Update Render deployment config and document the required environment variables for production OTLP export.

### Changes Required:

#### 1. render.yaml — Add OTel env vars and update build command

**File**: `render.yaml`

**Intent**: Add OTLP configuration env vars (endpoint, headers, service name, enabled flag) and update the build command to include `--features otel`.

**Contract**:

```yaml
buildCommand: cargo build --release --features otel
envVars:
  # ... existing vars ...
  - key: OTEL_ENABLED
    value: "true"
  - key: OTEL_SERVICE_NAME
    value: cerebrum
  - key: OTEL_EXPORTER_OTLP_ENDPOINT
    sync: false
  - key: OTEL_EXPORTER_OTLP_HEADERS
    sync: false
```

#### 2. Documentation — Add OTel section to README or config docs

**File**: `README.md` or equivalent

**Intent**: Document the OTel integration: what env vars to set, how to disable, which backend to use.

**Contract**: Section titled "Observability / OpenTelemetry" explaining: feature flag, env vars, recommended backend (Grafana Cloud free tier), and how to verify it's working.

### Success Criteria:

#### Automated Verification:

- `render.yaml` is valid YAML (parseable)
- Build command includes `--features otel`

#### Manual Verification:

- Deploy to Render staging with Grafana Cloud OTLP credentials configured
- Confirm traces, metrics, and logs appear in Grafana within 60s of first request
- Confirm no errors in application logs related to OTLP export
- Disable `OTEL_ENABLED` and redeploy; confirm zero OTel overhead (no export attempts in logs)

---

## Testing Strategy

### Unit Tests:

- `telemetry::init()` returns `None` when `OTEL_ENABLED` is unset
- Metrics struct fields are accessible and instruments can be called without panicking
- `shutdown()` completes without error when called with valid providers

### Integration Tests:

- Existing test suite passes with and without `--features otel`
- Handler tests with `metrics: None` in AppState confirm no panics on the metrics recording paths

### Manual Testing Steps:

1. Build with `cargo build --features otel`, run with `OTEL_ENABLED=true` and a local OTLP collector
2. Send requests to `/v1/chat/completions` and `/v1/classify`
3. Verify traces show HTTP span with nested handler spans
4. Verify metrics counters increment and histograms record
5. Verify logs appear with trace_id and span_id fields
6. Send SIGTERM; verify flush completes and process exits cleanly

## Performance Considerations

- Batch export with default 5s interval — no per-request network calls
- Metric instruments are pre-allocated; recording is a cheap atomic operation
- Log bridge filters to `info`+ for OTel export (matching existing env filter)
- `OTEL_ENABLED=false` / feature disabled = zero overhead
- Memory: default queue size (2048 spans, 2048 log records) is bounded

## Migration Notes

- No data migration needed
- Existing PostgreSQL dashboard continues to work unchanged
- Feature can be enabled/disabled per deploy without data loss
- If OTel backend is unreachable, export silently fails (fire-and-forget) per lesson "Log operational failures before falling back"

## References

- Research: `context/changes/opentelemetry-integration/research.md`
- Roadmap item: S-11 in `context/foundation/roadmap.md`
- Lesson: "Log operational failures before falling back" — applies to OTel export failures
- Crate docs: `opentelemetry-otlp` 0.28+ with `http-proto` + `reqwest-client` features

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Feature-Gated Dependencies & Telemetry Module

#### Automated

- [x] 1.1 `cargo check` passes (no feature)
- [x] 1.2 `cargo check --features otel` passes
- [x] 1.3 `cargo test` passes (no feature)

#### Manual

- [ ] 1.4 `cargo tree --features otel` shows expected OTel deps without duplicate TLS

### Phase 2: Tracing Integration & Graceful Shutdown

#### Automated

- [ ] 2.1 `cargo check --features otel` passes
- [ ] 2.2 `cargo test` passes
- [ ] 2.3 `cargo test --features otel` passes

#### Manual

- [ ] 2.4 Traces appear in OTLP backend after local request
- [ ] 2.5 SIGTERM produces clean shutdown with flush

### Phase 3: Metrics Instrumentation

#### Automated

- [ ] 3.1 `cargo check --features otel` passes
- [ ] 3.2 `cargo test --features otel` passes
- [ ] 3.3 `cargo clippy --features otel` passes

#### Manual

- [ ] 3.4 All 4 metrics appear in Grafana with correct labels
- [ ] 3.5 Zero overhead confirmed when metrics is None

### Phase 4: Deployment Configuration

#### Automated

- [ ] 4.1 `render.yaml` is valid YAML
- [ ] 4.2 Build command includes `--features otel`

#### Manual

- [ ] 4.3 Traces/metrics/logs appear in Grafana Cloud within 60s of Render deploy
- [ ] 4.4 OTEL_ENABLED=false redeploy shows zero export attempts
