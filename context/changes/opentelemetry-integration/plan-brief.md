# OpenTelemetry Integration — Plan Brief

> Full plan: `context/changes/opentelemetry-integration/plan.md`
> Research: `context/changes/opentelemetry-integration/research.md`

## What & Why

Integrate OpenTelemetry into cerebrum to export application-level traces, metrics, and structured logs via OTLP HTTP to any compatible observability backend. Cerebrum currently has no real-time operational telemetry — classification outcomes are only visible in PostgreSQL, and there's no distributed tracing or metrics export. This adds the missing observability layer for production operations.

## Starting Point

Cerebrum already uses the `tracing` crate with a layered subscriber registry (`tracing-subscriber::registry().with(fmt_layer).init()`). HTTP request spans exist via `tower-http::TraceLayer`. Render.com provides infrastructure-level OTel metrics natively. What's missing is application-level telemetry export — the bridge from `tracing` to OTel backends, custom business metrics, and graceful shutdown for flush guarantees.

## Desired End State

When deployed with the `otel` feature and `OTEL_ENABLED=true`, every request produces a trace visible in Grafana (or any OTLP backend), key business metrics (request rate, latency, classification distribution, upstream duration) are charted in real-time, and all application logs carry trace/span IDs for correlation. On process exit (deploy/restart), pending telemetry is flushed within 5 seconds.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Signals in scope | All three (traces, metrics, logs) | Complete observability in one change; log layer is minimal effort given existing tracing bridge | Plan |
| Feature gating | Cargo feature flag + runtime kill-switch | Zero build cost in dev, zero runtime cost when disabled — best of both worlds | Research + Plan |
| Metrics selection | Core 4 (requests, duration, classification, upstream) | Covers primary operational questions without over-instrumenting | Plan |
| Graceful shutdown | Signal handler with provider flush | Render sends SIGTERM on deploy; flushing prevents data loss | Plan |
| Backend coupling | Generic OTLP (standard env vars) | Zero vendor lock-in; works with Grafana, Axiom, SigNoz out of the box | Plan |
| Log export scope | All tracing events at info+ | Complete log picture with automatic trace ID correlation | Plan |
| Transport protocol | OTLP HTTP (not gRPC) | Simpler for Render (no HTTP/2 requirement); reuses existing reqwest | Research |

## Scope

**In scope:**
- OTel tracer/meter/logger provider initialization behind feature flag
- Tracing-opentelemetry layer for span export
- Log bridge via opentelemetry-appender-tracing
- 4 application metrics with labels
- Graceful shutdown with SIGTERM handling and flush
- Render deployment config with OTLP env vars

**Out of scope:**
- Replacing PostgreSQL dashboard (coexists)
- Prometheus `/metrics` scrape endpoint
- Custom span instrumentation on business functions
- Trace context propagation to upstream LLM APIs
- Head-based sampling (100% at current traffic)
- W3C traceparent header forwarding

## Architecture / Approach

New `src/telemetry.rs` module (feature-gated) owns all OTel initialization. It creates three providers (tracer, meter, logger), returns subscriber layers for the registry and a `Metrics` struct for handlers. The existing `tracing_subscriber::registry()` gains two conditional layers (OTel traces + OTel logs). Handlers access metric instruments via `AppState.metrics`. On shutdown, providers flush in order (traces → metrics → logs) with a 5s timeout.

```
Request → TraceLayer (existing) → Handler → log_classification()
                |                      |              |
                ↓                      ↓              ↓
         OTel trace layer     Metrics.record()   Metrics.add()
                |                      |              |
                ↓                      ↓              ↓
         OTLP HTTP batch export → Backend (Grafana Cloud)
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Feature-gated deps & telemetry module | Compiles with OTel crates; `src/telemetry.rs` exists | Version compatibility between OTel crates |
| 2. Tracing integration & graceful shutdown | Traces + logs exported; clean exit on SIGTERM | OTel layer type complexity with boxed layers |
| 3. Metrics instrumentation | 4 core metrics recorded at handler callsites | Correct label attribution across all return paths |
| 4. Deployment configuration | Render builds with feature; OTLP endpoint configured | Credential management for OTLP headers |

**Prerequisites:** Grafana Cloud account (free tier) with OTLP endpoint and API key generated.
**Estimated effort:** ~2-3 sessions across 4 phases (each phase is independently deployable).

## Open Risks & Assumptions

- OTel Rust crate versions move fast; pinned versions may need updating at implementation time (API patterns are stable)
- Build time increase (~10-15s) when feature is enabled — acceptable for CI/production builds
- Assumes Render's 10s SIGTERM grace period is sufficient for 5s flush timeout + connection drain

## Success Criteria (Summary)

- Traces, metrics, and logs visible in Grafana Cloud within 60s of a production request
- Zero performance/build impact when feature is disabled (existing dev workflow unchanged)
- Clean shutdown with no telemetry data loss on Render redeploy
