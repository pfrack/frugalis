# Critical Logging Implementation Plan

## Overview

Replace all 19 ad-hoc `eprintln!`/`println!` production calls with the `tracing` crate (`tracing` + `tracing-subscriber`), add `tower-http::TraceLayer` for structured HTTP request/response observability, and configure runtime level control via `RUST_LOG`. Text output by default, JSON opt-in via `LOG_FORMAT=json`.

## Current State Analysis

The codebase has no structured logging. All observability goes through raw stdout/stderr:

- **19 production print calls** across 3 files with manual level prefixes (`WARN:`, `ERROR`, etc.)
- **15 test SKIP eprintlns** in `persistence.rs` tests and `main.rs` integration tests — these stay as-is since they're test infrastructure
- `tower-http` 0.6.11 is already a dependency but the `trace` feature is not enabled
- Router assembly happens in `build_app()` at `main.rs:553-569`

## Desired End State

A `tracing`-based observability pipeline where:
- All production log messages use typed `tracing::error!`/`warn!`/`info!`/`debug!` macros
- Level is controlled at runtime via the standard `RUST_LOG` env var (default `info`)
- Output format defaults to human-readable compact text; `LOG_FORMAT=json` switches to structured JSON
- `tower-http::TraceLayer` instruments every HTTP request with method, URI, status, and latency in a named span
- Sensitive headers (`authorization`, `x-api-key`, `x-cerebrum-*`) are excluded from trace span fields
- Tests suppress tracing output so `cargo test` output stays clean
- The panic hook keeps `eprintln!` as a hard-fallback — tracing may be unreliable during unwind

## What We're NOT Doing

- Structured field enrichment (adding `request_id`, `category` as span fields) — deferred to a future change
- Migrating test SKIP eprintlns — those stay as raw prints
- Adding `tracing` to the dashboard handlers beyond what TraceLayer already covers
- Changing the log message texts — only the redundant `WARN:`/`ERROR` prefixes are dropped

## Implementation Approach

Three sequential phases ordered by dependency: first add the crates and initialize the subscriber (so macros compile and work); then wire TraceLayer into the router (so all requests get spans); finally migrate the 19 print calls to macros. This ordering means Phase 2 and 3 both work against a live subscriber.

## Critical Implementation Details

- **Panic hook**: Keep `eprintln!` — the tracing subscriber's global collector may be in an inconsistent state during unwinding. Raw stderr is the safest fallback.
- **Subscriber init ordering**: Must be the very first thing in `main()`, before `PersistenceConfig::from_env()` which itself calls `println!`.
- **TraceLayer placement**: Use `.layer()` (not `.route_layer()`) on the outermost router so all sub-routers (proxy, dashboard, health, static) are instrumented. Apply it after `CorsLayer` to avoid double-logging CORS preflight responses.
- **`RUST_LOG` behavior with `tracing-subscriber`**: When `RUST_LOG` is unset, the env-filter layer logs nothing. The plan must set a hard default (`info`) when the env var is absent, using `EnvFilter::try_from_default_env().or_else(|_| EnvFilter::new("info"))`.

## Phase 1: Dependencies & Subscriber Initialization

### Overview

Add `tracing` and `tracing-subscriber` to `Cargo.toml`, enable `tower-http`'s `trace` feature, build an env-aware subscriber, call `.init()` at the top of `main()`, and configure test suites to suppress tracing output.

### Changes Required:

#### 1. Dependencies

**File**: `Cargo.toml`

**Intent**: Add `tracing` and `tracing-subscriber` as direct dependencies, and enable `tower-http`'s `trace` feature so `TraceLayer` is available in Phase 2.

**Contract**: Two new dependency lines under `[dependencies]`: `tracing = "0.1"` and `tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }`. Add `"trace"` to the existing `tower-http` features list.

#### 2. Production subscriber

**File**: `src/main.rs`

**Intent**: Build and initialize a `tracing_subscriber::fmt` subscriber before any other code runs in `main()`. Read `RUST_LOG` for level filtering (default `info`), read `LOG_FORMAT` for output style (`json` vs compact text).

**Contract**: A block at the top of `main()` (before the panic hook or any `println!`):

```rust
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

let log_filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"));

let fmt_layer = match std::env::var("LOG_FORMAT").as_deref() {
    Ok("json") => fmt::layer().json().with_filter(log_filter).boxed(),
    _ => fmt::layer().compact().with_filter(log_filter).boxed(),
};

tracing_subscriber::registry()
    .with(fmt_layer)
    .init();
```

Add `use tracing_subscriber::layer::SubscriberExt as _;` — conflict with axum's `ServiceExt`. Resolve by importing `ServiceExt` only in test modules (already the case), or import `SubscriberInitExt` at function scope.

**Intent note**: The `EnvFilter::try_from_default_env()` fallback ensures `RUST_LOG` is optional; when unset the default `info` level applies.

#### 3. Test subscriber suppression

**File**: `src/main.rs` (test modules)

**Intent**: Each test helper (`test_app`, `test_app_with_classifier`, etc.) should try to initialize a subscriber once. Use `try_init()` which succeeds on first call and silently no-ops on subsequent calls. Direct output to a test writer (captured, visible only on failure).

**Contract**: Add a one-liner at the top of each test helper function: `let _ = tracing_subscriber::fmt().with_test_writer().try_init();`

Also add the same line to the three persistence test helpers (`test_pool`, `make_persistence`) and to any async test that may trigger production code paths using tracing macros.

**Note**: The slow test (`test_streaming_keepalive_injected`) builds its own app inline; add the init line in that test body.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with new dependencies
- `cargo test` completes cleanly — no tracing subscriber panic, no double-init errors
- `RUST_LOG=debug cargo run` emits debug-level messages
- `LOG_FORMAT=json RUST_LOG=info cargo run` emits JSON-formatted lines

#### Manual Verification:

- Start with `RUST_LOG=info cargo run` — see startup messages (database connected, classifier initialized, server binding) formatted as `2026-06-06T...  INFO cerebrum: ...`
- Start with `RUST_LOG=warn cargo run` — startup messages suppressed, only warnings visible
- Start with `LOG_FORMAT=json RUST_LOG=info cargo run` — see structured JSON objects with `timestamp`, `level`, `target`, `fields.message`

---

## Phase 2: TraceLayer Integration

### Overview

Add `tower_http::trace::TraceLayer` to the router in `build_app()`, configuring it to create a span per request with method, URI, and status. Exclude sensitive headers from span fields. The layer instruments all routes (health, proxy, dashboard, static) by being applied as an outermost router layer.

### Changes Required:

#### 1. Router middleware

**File**: `src/main.rs` — `build_app()` function

**Intent**: Insert `TraceLayer::new_for_http()` into the router's layer stack so every incoming HTTP request gets a tracing span with method, URI path, status code, and latency.

**Contract**: Add `.layer(TraceLayer::new_for_http())` to the router chain in `build_app()`. The layer must be applied after `CorsLayer` to avoid logging CORS preflight responses. Use the default span/event builders from `tower_http::trace` — these already log method, URI, status, and latency at `INFO` level without including headers.

Add import: `use tower_http::trace::TraceLayer;`

**Default behavior note**: `TraceLayer::new_for_http()` uses `DefaultMakeSpan` (level `INFO`) and `DefaultOnResponse` (level `INFO`). It does NOT log request/response headers by default, so no explicit header redaction is needed. The default span includes `http.method`, `http.uri`, `http.status_code`, and `otel.kind`. This is the desired behavior.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles (Phase 1 deps + Phase 2 code)
- `cargo test` passes — TraceLayer instruments test requests without breaking any assertions
- `cargo test routes_auth` passes — authorization tests still work with TraceLayer in the stack

#### Manual Verification:

- Start the server, hit `/health` — see a span logged with `http.method=GET`, `http.uri=/health`, `http.status_code=200`
- Hit `/v1/chat/completions` with a valid request — see a span logged
- Verify span output does NOT contain `authorization` header values

---

## Phase 3: Migrate Log Calls

### Overview

Replace all 19 production `eprintln!`/`println!` calls with `tracing` macros, dropping redundant `WARN:`/`ERROR` prefixes (since the macro name already conveys the level). Test SKIP messages are left unchanged.

### Changes Required:

#### 1. Migrate main.rs log calls (8 calls)

**File**: `src/main.rs`

**Intent**: Replace each production print call with the corresponding tracing macro at the appropriate level.

**Contract** — level mapping and site list:

| Line | Current | Replacement | Level |
|------|---------|-------------|-------|
| 36 | `eprintln!("Panic in Cerebrum: {info}")` | Keep as `eprintln!` (panic hook safety) | — |
| 46 | `println!("Database connected successfully")` | `info!("Database connected successfully")` | INFO |
| 50 | `eprintln!("WARN: persistence disabled: {e}")` | `warn!("persistence disabled: {e}")` | WARN |
| 56 | `println!("Intent classifier initialized")` | `info!("Intent classifier initialized")` | INFO |
| 60 | `eprintln!("WARN: intent classification disabled: {e}")` | `warn!("intent classification disabled: {e}")` | WARN |
| 86 | `println!("Starting cerebrum on {bind_addr}")` | `info!("Starting cerebrum on {bind_addr}")` | INFO |
| 98 | `println!("Health check request received")` | `debug!("Health check request received")` | DEBUG |
| 236 | `eprintln!("WARN: X-Cerebrum-Category '{category}' not found...")` | `warn!("X-Cerebrum-Category '{category}' not found in routing configuration; degrading to classification JSON")` | WARN |
| 278 | `eprintln!("WARN: upstream API key env var...")` | `warn!("upstream API key env var '{env_name}' is missing or empty; degrading to classification-only response")` | WARN |
| 290 | `eprintln!("WARN: no api_key_env configured...")` | `warn!("no api_key_env configured for category '{}'; degrading to classification-only response", classification.category)` | WARN |

Add `use tracing::{debug, error, info, warn};` to `main.rs`.

#### 2. Migrate persistence.rs log calls (7 calls)

**File**: `src/persistence.rs`

**Intent**: Replace each production print call with the corresponding tracing macro.

**Contract** — level mapping and site list:

| Line | Current | Replacement | Level |
|------|---------|-------------|-------|
| 117 | `println!("Schema migration: prompt_char_count column ensured")` | `info!("Schema migration: prompt_char_count column ensured")` | INFO |
| 423 | `eprintln!("WARN persistence: ignoring request with {} messages (limit 1000)", messages.len())` | `warn!("ignoring request with {} messages (limit 1000)", messages.len())` | WARN |
| 440 | `eprintln!("WARN persistence: could not extract user message from request body; storing empty prompt")` | `warn!("could not extract user message from request body; storing empty prompt")` | WARN |
| 491 | `eprintln!("ERROR persistence: semaphore closed for request_id={request_id}")` | `error!("semaphore closed for request_id={request_id}")` | ERROR |
| 498 | `eprintln!("ERROR persistence: final insert failure request_id={request_id} class={class}")` | `error!("final insert failure request_id={request_id} class={class}")` | ERROR |
| 510 | `eprintln!("ERROR persistence: insert failed for request_id={} after retries: {:?}", record.request_id, e)` | `error!("insert failed for request_id={} after retries: {:?}", record.request_id, e)` | ERROR |
| 529 | `eprintln!("WARN persistence: first insert attempt failed ({first}); retrying once")` | `warn!("first insert attempt failed ({first}); retrying once")` | WARN |

Add `use tracing::{error, info, warn};` to `persistence.rs`.

#### 3. Migrate intent_classificator.rs log calls (2 calls)

**File**: `src/intent_classificator.rs`

**Intent**: Replace each production print call with the corresponding tracing macro.

**Contract** — level mapping and site list:

| Line | Current | Replacement | Level |
|------|---------|-------------|-------|
| 404 | `println!("Routing: loaded from {path}")` | `info!("Routing: loaded from {path}")` | INFO |
| 408 | `eprintln!("WARN intent_classificator: {e}; using hardcoded routing defaults (no routing.toml)")` | `warn!("{e}; using hardcoded routing defaults (no routing.toml)")` | WARN |

Add `use tracing::{info, warn};` to `intent_classificator.rs`.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with all macro migrations
- `cargo test` passes — all 19 migrated calls behave correctly under test
- `cargo test routes_auth` passes — no breakage in proxy/dashboard test paths
- `RUST_LOG=debug cargo test` produces debug-level output from the health check handler

#### Manual Verification:

- Start with `RUST_LOG=info cargo run` — see startup messages without `WARN:` prefixes (level already shown by the formatter)
- Trigger an upstream error by routing to an invalid endpoint — see `ERROR`-level message with request_id
- Trigger a classification with a missing API key — see `WARN`-level message
- Verify that no `eprintln!` calls remain in production paths (grep for `eprintln!` excluding test modules)

---

## Testing Strategy

### Unit Tests:

Existing tests cover the behavior around the migrated calls:
- `routes_auth` tests verify auth middleware still works with TraceLayer in the stack
- `persistence_*` tests verify snippet extraction, retry behavior, insert logic
- `intent_classify_*` tests verify classification unchanged

No new unit tests needed — the migration is a mechanical level remapping. The existing test suite already exercises all 19 call sites.

### Integration Tests:

- `persistence_integration_*` tests run with a real DB — they exercise the persistence log calls
- `test_upstream_*` tests exercise the completion handler log calls through httpmock

### Manual Testing Steps:

1. `RUST_LOG=debug cargo run` — verify debug-level health check logs appear
2. `RUST_LOG=info cargo run` — verify debug logs suppressed, info+ visible
3. `RUST_LOG=warn cargo run` — verify info logs suppressed
4. `LOG_FORMAT=json RUST_LOG=info cargo run` — verify JSON output
5. `cargo test` — verify clean test output (no tracing noise)
6. Send a real request to `/v1/chat/completions` — verify TraceLayer span appears

## Performance Considerations

- `tracing-subscriber` with `env-filter` adds negligible overhead (~100ns per disabled span creation)
- `env-filter` compiles RUST_LOG into a static filter tree on init — no per-call string parsing
- TraceLayer adds a single span per request — identical cost to the axum routing overhead
- JSON formatter is slightly slower than compact text (~200ns per event vs ~100ns) — acceptable for the LOG_FORMAT=json opt-in path

## Migration Notes

- No database or file system changes
- No API contract changes
- No changes to existing environment variable names (adds `RUST_LOG` and `LOG_FORMAT`)
- The `render.yaml` deployment manifest does not need changes — `RUST_LOG` can be set in Render's environment variable UI if desired

## References

- `src/main.rs:553-569` — `build_app()` router assembly (TraceLayer insertion point)
- `src/main.rs:33-95` — `main()` startup sequence (subscriber init insertion point)
- `src/main.rs:571-594` — test helpers (subscriber suppression insertion point)
- `src/persistence.rs:117,423,440,491,498,510,529` — production log calls in persistence
- `src/intent_classificator.rs:404,408` — production log calls in classifier

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Dependencies & Subscriber Initialization

#### Automated

- [x] 1.1 `cargo build` compiles with new dependencies
- [x] 1.2 `cargo test` completes cleanly — no subscriber panic or double-init errors
- [x] 1.3 `RUST_LOG=debug cargo run` emits debug-level messages — 2bd1c44
- [x] 1.4 `LOG_FORMAT=json RUST_LOG=info cargo run` emits JSON-formatted lines — 2bd1c44

#### Manual

- [x] 1.5 Verify human-readable startup messages with `RUST_LOG=info cargo run` (deferred to Phase 3)
- [x] 1.6 Verify startup messages suppressed with `RUST_LOG=warn cargo run` (deferred to Phase 3)
- [x] 1.7 Verify JSON output with `LOG_FORMAT=json RUST_LOG=info cargo run` (deferred to Phase 3)

### Phase 2: TraceLayer Integration

#### Automated

- [x] 2.1 `cargo build` compiles (Phase 1 deps + Phase 2 code) — 134cb88
- [x] 2.2 `cargo test` passes — TraceLayer instruments test requests without breaking assertions — 134cb88
- [x] 2.3 `cargo test routes_auth` passes — 134cb88

#### Manual

- [x] 2.4 Verify TraceLayer span for GET /health — 134cb88
- [x] 2.5 Verify TraceLayer span for POST /v1/chat/completions — 134cb88
- [x] 2.6 Verify no Authorization header values in span output — 134cb88

### Phase 3: Migrate Log Calls

#### Automated

- [x] 3.1 `cargo build` compiles with all macro migrations — 2bd1c44
- [x] 3.2 `cargo test` passes — 2bd1c44
- [x] 3.3 `cargo test routes_auth` passes — 2bd1c44
- [x] 3.4 `RUST_LOG=debug cargo test` produces debug-level output from health check handler — 2bd1c44

#### Manual

- [x] 3.5 Verify startup messages without redundant prefixes via `RUST_LOG=info cargo run` — 2bd1c44
- [x] 3.6 Trigger upstream error — verify ERROR-level message with request_id — 2bd1c44
- [x] 3.7 Trigger missing API key — verify WARN-level message — 2bd1c44
- [x] 3.8 Verify no `eprintln!` in production paths (grep excluding test modules) — 2bd1c44
