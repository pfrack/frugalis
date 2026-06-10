---
date: 2026-06-09T17:00:52+02:00
researcher: Claude
git_commit: 83c2703
branch: code-review-cleanup
repository: cerebrum
topic: "OpenTelemetry integration possibilities for cerebrum"
tags: [research, codebase, opentelemetry, observability, tracing, metrics]
status: complete
last_updated: 2026-06-09
last_updated_by: Claude
---

# Research: OpenTelemetry Integration Possibilities for Cerebrum

**Date**: 2026-06-09T17:00:52+02:00
**Researcher**: Claude
**Git Commit**: 83c2703
**Branch**: code-review-cleanup
**Repository**: cerebrum

## Research Question

What are the possibilities for integrating OpenTelemetry into cerebrum, considering the existing tracing infrastructure, Render.com deployment, and the goal of full observability (traces, metrics, logs)?

## Summary

Cerebrum is well-positioned for OpenTelemetry (OTel) integration because it already uses the `tracing` crate ecosystem, which is the standard bridge to OTel in Rust. The integration path is:

1. **Traces**: Add `tracing-opentelemetry` layer → zero code changes to existing `tracing::info!()` / `warn!()` calls
2. **Metrics**: Add OTel metrics instruments for request counts, latencies, classification outcomes
3. **Logs**: Bridge existing `tracing` events to OTel logs via `opentelemetry-appender-tracing`
4. **Export**: OTLP exporter to any backend (Grafana Cloud free tier, SigNoz Cloud, Axiom)

**Key finding**: Render.com launched native OpenTelemetry metrics streaming in March 2025 — infrastructure metrics (CPU, memory, network) are already available. This change adds **application-level** telemetry.

## Detailed Findings

### Current Observability State

| Signal | Current State | OTel Path |
|--------|--------------|-----------|
| **Traces** | `TraceLayer::new_for_http()` from tower-http — logs request spans but doesn't export them | Add `tracing-opentelemetry` layer to registry |
| **Metrics** | None — classification outcomes only visible in PostgreSQL `inferences` table | Add OTel Counter/Histogram instruments |
| **Logs** | `tracing` + `tracing-subscriber` with `env-filter` + optional JSON format | Add `opentelemetry-appender-tracing` layer |
| **Infra metrics** | Render native OTel streaming (CPU, mem, disk, network) | Already available |

### Architecture Fit

The existing tracing setup in `src/main.rs:46-54`:
```rust
let log_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
let fmt_layer = match std::env::var("LOG_FORMAT").as_deref() {
    Ok("json") => fmt::layer().json().with_filter(log_filter).boxed(),
    _ => fmt::layer().compact().with_filter(log_filter).boxed(),
};
tracing_subscriber::registry().with(fmt_layer).init();
```

This is a **layered registry** — adding OTel is just adding more layers:
```rust
tracing_subscriber::registry()
    .with(fmt_layer)                    // existing console/JSON output
    .with(otel_tracing_layer)          // NEW: export spans to OTel backend
    .with(otel_log_layer)              // NEW: bridge tracing events to OTel logs
    .init();
```

### Required Crates

```toml
[dependencies]
# Core OTel SDK
opentelemetry = "0.31"
opentelemetry_sdk = { version = "0.31", features = ["rt-tokio"] }

# OTLP exporter (HTTP preferred for Render — no need for gRPC/tonic)
opentelemetry-otlp = { version = "0.31", features = ["http-proto", "trace", "metrics", "logs"] }

# Bridge: tracing → OTel traces
tracing-opentelemetry = "0.30"

# Bridge: tracing events → OTel logs
opentelemetry-appender-tracing = "0.31"
```

**Dependency impact**: ~15 new transitive crates. `opentelemetry-otlp` with `http-proto` uses `reqwest` (already a dependency) for HTTP transport — no new TLS stack.

### Metrics to Instrument

| Metric | Type | Labels | Source |
|--------|------|--------|--------|
| `cerebrum.requests.total` | Counter | method, route, status | completion_handler, classify_handler |
| `cerebrum.request.duration_seconds` | Histogram | method, route, status | completion_handler |
| `cerebrum.classification.total` | Counter | category, tier (regex/fallback) | classify_and_log |
| `cerebrum.upstream.duration_seconds` | Histogram | provider, model, status | upstream request path |
| `cerebrum.streaming.active` | UpDownCounter | — | handle_streaming_response |
| `cerebrum.classification.savings_usd` | Counter | category, model | log_classification |

### Backend Options (Free Tier)

| Backend | Free Tier | Signals | OTLP Native | Notes |
|---------|-----------|---------|-------------|-------|
| **Grafana Cloud** | 50GB logs, 10K metrics series, 50GB traces/mo | All | Yes | Best ecosystem, Tempo+Loki+Mimir |
| **SigNoz Cloud** | 30-day trial → self-host free | All | Yes (native) | OTel-native, ClickHouse storage |
| **Axiom** | 500GB ingest/mo | All | Yes | Simple, generous free tier |
| **Render native** | Included | Infra metrics only | Yes (push) | Already available for host metrics |

**Recommendation**: Grafana Cloud free tier — covers all three signals, excellent dashboarding, and the free tier is generous for a single-service proxy.

### Deployment Considerations (Render.com)

1. **No sidecar collector needed** — OTLP HTTP export directly from the app to the backend
2. **Environment variables** for configuration (fits Render's env var model):
   ```
   OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp-gateway-prod-us-east-0.grafana.net/otlp
   OTEL_EXPORTER_OTLP_HEADERS=Authorization=Basic <base64>
   OTEL_SERVICE_NAME=cerebrum
   OTEL_ENABLED=true
   ```
3. **Graceful shutdown** required — flush pending spans/metrics before process exit
4. **Conditional enablement** — when `OTEL_ENABLED` is unset, skip OTel layers entirely (zero overhead in dev)

### Implementation Phases (Suggested)

**Phase 1: Traces** (lowest effort, highest value)
- Add `tracing-opentelemetry` layer
- All existing `tracing::info!()` spans automatically exported
- `TraceLayer` HTTP spans get trace IDs, parent context propagation
- W3C `traceparent` header propagation for distributed tracing

**Phase 2: Metrics** (medium effort, high value for dashboard replacement)
- Add OTel meters for request count, latency histogram, classification counters
- Could eventually replace the PostgreSQL-based dashboard metrics with live OTel queries
- Prometheus-compatible exposition via `/metrics` endpoint (optional, for local scraping)

**Phase 3: Logs** (low effort, medium value)
- Bridge `tracing` events to OTel logs
- Correlate logs with trace IDs automatically
- Structured log export to Grafana Loki

### Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Increased latency from span export | Low — batch export is async | Use `with_batch_exporter()`, 5s export interval |
| Memory pressure from buffered spans | Low | Set max queue size (2048 default is fine) |
| OTel backend unavailable | None — fire-and-forget | Export failures logged at debug level, don't affect requests |
| Build time increase | Medium (~10-15s) | OTel crates are compile-heavy; feature-gate behind `otel` feature flag |
| `opentelemetry-prometheus` crate deprecated | None | Use OTLP export instead; Prometheus scraping via collector if needed |

### Feature-Gating Strategy

```toml
[features]
default = []
otel = ["opentelemetry", "opentelemetry_sdk", "opentelemetry-otlp", "tracing-opentelemetry", "opentelemetry-appender-tracing"]
```

This keeps the binary lean for development and only enables OTel in production builds. CI tests without `--features otel` to avoid OTel SDK initialization in tests.

## Code References

- `src/main.rs:46-54` — Current tracing subscriber initialization (insertion point for OTel layers)
- `src/main.rs:775` — `TraceLayer::new_for_http()` (already creates HTTP spans)
- `src/main.rs:170-185` — `log_classification()` helper (metrics instrumentation point)
- `src/main.rs:454-521` — `handle_streaming_response()` (streaming metrics point)
- `render.yaml` — Deployment config (env vars for OTLP endpoint)
- `Cargo.toml` — Dependency management

## Architecture Insights

1. **Zero-code-change traces**: Because cerebrum uses `tracing` throughout, adding the OTel layer exports all existing spans without modifying any business logic.

2. **Complement, don't replace, the dashboard**: The PostgreSQL-based dashboard (S-02/S-03/S-04) serves a different purpose (operator-facing historical view). OTel provides real-time operational observability. Both coexist.

3. **The `tracing` → OTel bridge is the standard Rust pattern**: The `tracing-opentelemetry` crate is maintained by the Tokio team and is the canonical integration path. No exotic patterns needed.

4. **Render.com is OTel-ready**: Their March 2025 launch of OTel metrics streaming confirms the platform supports the protocol natively. Application-level OTLP export over HTTPS works without any special configuration.

## Historical Context

- `context/foundation/roadmap.md` — S-10 phase 12 already mentions "Prometheus metrics + health enhancements" as a planned item
- `context/foundation/lessons.md` — "Log operational failures before falling back" rule applies to OTel export failures (must not crash on export failure)

## Related Research

- No prior OTel research exists in the change archive

## Open Questions

1. **Backend choice**: Grafana Cloud vs Axiom vs self-hosted SigNoz? (Recommendation: Grafana Cloud free tier for MVP)
2. **Metrics vs PostgreSQL dashboard**: Should OTel metrics eventually replace the DB-backed dashboard queries, or remain complementary?
3. **Compile-time feature gate**: Should OTel be always-on or behind a Cargo feature flag? (Recommendation: feature flag for now, always-on once stable)
4. **Sampling**: At current traffic (<1000 req/s), sample 100% of traces. At scale, implement head-based sampling.
