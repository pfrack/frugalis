# Critical Logging — Plan Brief

> Full plan: `context/changes/critical-logging/plan.md`

## What & Why

Replace 19 ad-hoc `eprintln!`/`println!` calls with the `tracing` crate for structured, level-filtered observability, and add `tower-http::TraceLayer` for automatic per-request HTTP tracing. The current raw-print approach has no level filtering, no structured output, and no request-level spans — making production debugging unnecessarily noisy and opaque.

## Starting Point

Today, all observability goes through raw `eprintln!`/`println!` with manual `WARN:`/`ERROR` prefixes across `main.rs` (10 calls), `persistence.rs` (7 calls), and `intent_classificator.rs` (2 calls). `tower-http` is already a dependency but the `trace` feature is not enabled. The router is assembled in `build_app()` at `src/main.rs:553`.

## Desired End State

All production log messages use `tracing::error!`/`warn!`/`info!`/`debug!` macros. Runtime level is controlled via standard `RUST_LOG` env var (default `info`). Output defaults to human-readable compact text; `LOG_FORMAT=json` switches to structured JSON. Every HTTP request gets a tracing span with method, URI, status, and latency via `tower-http::TraceLayer`. Sensitive headers are not logged. Test output stays clean.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Logging crate | `tracing` + `tracing-subscriber` | Axum/Tokio native, env-filter for level control, zero-boilerplate structured fields. | Plan |
| Output format | Text default, JSON via `LOG_FORMAT=json` | Human-readable for local dev, machine-parseable for Render log aggregators. | Plan |
| Level control | `RUST_LOG` env var | Universal convention; default `info` when unset. | Plan |
| HTTP tracing | Add `tower-http::TraceLayer` | Free request-level spans (method, URI, status, latency) from an already-present dependency. | Plan |
| Header handling | Default behavior (no headers logged) | `DefaultMakeSpan`/`DefaultOnResponse` don't include headers — sensitive data is excluded by construction. | Plan |
| Message migration | Keep text, drop `WARN:`/`ERROR` prefixes | Preserves familiar wording; macros already convey the level. | Plan |
| Test output | Suppress tracing with `with_test_writer().try_init()` | Keeps `cargo test` clean; captured output visible on failure. | Plan |
| Panic hook | Keep `eprintln!` | Tracing subscriber may be unreliable during unwind; raw stderr is safest. | Plan |

## Scope

**In scope:**
- Add `tracing` 0.1 and `tracing-subscriber` 0.3 (with `env-filter` and `json` features) to `Cargo.toml`
- Enable `tower-http`'s `trace` feature
- Initialize subscriber at top of `main()` with `RUST_LOG` + `LOG_FORMAT` support
- Add `TraceLayer` to router in `build_app()`
- Migrate 19 production print calls to tracing macros
- Suppress tracing in test helpers

**Out of scope:**
- Structured span fields (request_id, category, model) — deferred
- Migrating test SKIP eprintlns (15 calls)
- Dashboard handler instrumentation beyond TraceLayer
- Log message rewrites

## Architecture / Approach

```
main() → tracing_subscriber::init() → PersistenceConfig → IntentClassifier → build_app()
                                                                                    │
                                                                    Router ─ TraceLayer (outermost)
                                                                      │
                                                                      ├── /health
                                                                      ├── /v1/* (proxy routes)
                                                                      ├── /dashboard/* (basic auth)
                                                                      └── /static (ServeDir)
```

All production code (`persistence.rs`, `intent_classificator.rs`, handler functions) calls `tracing::info!`/`warn!`/`error!`/`debug!` instead of `println!`/`eprintln!`. The subscriber reads `RUST_LOG` at startup for level filtering and `LOG_FORMAT` for output style. TraceLayer sits as the outermost router layer, creating a span per request.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Dependencies & subscriber init | New crates compile, subscriber boots in main() and tests, RUST_LOG/LOG_FORMAT work | Test double-init panics if `try_init()` placement is wrong |
| 2. TraceLayer integration | Every HTTP request gets a method/URI/status/latency span | Breaking existing test assertions (status codes, headers) |
| 3. Migrate log calls | 19 print calls become tracing macros with appropriate levels | Missed a call site or wrong level assignment |

**Prerequisites:** None — this change is self-contained, no DB migrations, no API contract changes.
**Estimated effort:** ~1 session across 3 phases.

## Open Risks & Assumptions

- `RUST_LOG` is a global Rust ecosystem convention — if Render or another tool also sets it, there may be unexpected interactions. In practice, Render does not set `RUST_LOG`, so this is low risk.
- `tracing-subscriber` 0.3's `json` feature requires `serde_json` — already a dependency.
- The panic hook stays as `eprintln!` because tracing's global collector may be in an inconsistent state during unwind. This is a deliberate design choice, not a gap.

## Success Criteria (Summary)

- `cargo build` and `cargo test` pass at every phase boundary
- `RUST_LOG=info` shows startup messages; `RUST_LOG=warn` suppresses them
- `LOG_FORMAT=json` emits structured JSON
- TraceLayer spans appear for every HTTP request
- No `eprintln!` remains in production paths (excluding panic hook and tests)
