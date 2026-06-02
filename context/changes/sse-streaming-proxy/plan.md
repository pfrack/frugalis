# SSE Streaming Proxy Implementation Plan

## Overview

Add SSE streaming responses to `completion_handler` (`POST /v1/chat/completions`). When the client sends `stream: true` in the request body, forward the upstream response body as `text/event-stream` with manual keepalive pings injected every 15 seconds. Uses bare passthrough (no Axum `Event` wrapping) — raw upstream SSE bytes are forwarded directly. Non-2xx upstream responses on a streaming request are delivered as SSE `event: error` events. Mid-stream upstream connection drops inject a final error event before closing. When `stream: false` or absent, the handler buffers and returns JSON as today. Part 4 of 4 in the upstream proxy routing sequence.

## Current State Analysis

`completion_handler` at `src/main.rs:176` buffers the entire upstream response via `response.text().await` (line 318) and always returns `Content-Type: application/json`. There is no SSE code anywhere in the codebase. Axum 0.8 provides `axum::response::sse::{Sse, Event, KeepAlive}` but we bypass `Sse`/`Event` for bare passthrough — the upstream is already an SSE producer.

The handler currently returns `impl IntoResponse` (various `(StatusCode, String)` tuples). This must change to a concrete `Response` type because the two paths (buffered JSON vs streaming raw bytes) produce different response shapes that cannot unify under a single `impl IntoResponse` return.

`reqwest` 0.12 is already a direct dependency (added in Change 2). `tokio-stream` and `futures` are transitive dependencies (via `sqlx-core` and `axum`) but need to be added as direct dependencies in `Cargo.toml`.

The `http_client: Option<reqwest::Client>` degradation path (classification-only JSON when no client is configured) must be preserved — tests rely on it.

### Key Discoveries:

- **Handler buffers entire upstream response**: `src/main.rs:318` — `upstream_response.text().await` collects the full body before returning. No streaming awareness.
- **Body is already parsed for model override**: `src/main.rs:284-296` — `serde_json::from_str(body_str)` extracts the model field. The `stream` field can be extracted in the same parse.
- **No SSE code exists**: Zero references to `axum::response::sse`, `Sse`, `Event`, or `KeepAlive` anywhere in the codebase.
- **Content-Type gate exists**: `src/main.rs:189-195` — handler returns 415 for non-JSON content types. This must be relaxed or bypassed for streaming responses since the response Content-Type is `text/event-stream`, not the request Content-Type.
- **`test_app()` has `http_client: None`**: `src/main.rs:392-406` — all existing tests use classification-only degradation. No existing test exercises upstream forwarding, so the streaming path is net-new test surface.
- **Render's 60s proxy timeout**: Per `context/changes/sse-streaming-proxy/research.md:117`, keepalive pings every 15s prevent Render's load balancer from killing long completions.

## Desired End State

- `completion_handler` returns `Response` (not `impl IntoResponse`)
- When `stream: true` in the client request body, the handler forwards the upstream response as `text/event-stream` with 15s keepalive comment pings (`: keepalive\n\n`) injected when the upstream is quiet
- Raw upstream bytes are forwarded without Axum `Event` wrapping — the upstream is the SSE producer
- When `stream: true` and upstream returns a non-2xx status, read the error body and deliver it as an SSE `event: error` event, then close the stream
- When `stream: true` and the upstream connection drops mid-stream, inject a final `event: error` event with the error details, then close the stream
- When `stream: false` or absent, buffered JSON behavior is unchanged
- When `http_client` is `None`, the classification-only JSON degradation path is unchanged
- Existing tests pass without modification
- New httpmock-based tests verify: SSE content type, keepalive comments, error event injection, and `stream: false` backward compatibility

## What We're NOT Doing

- No Axum `Sse`/`Event` wrapping — bare passthrough forwards raw upstream SSE bytes
- No SSE line parsing or `[DONE]` marker detection — upstream is responsible for SSE framing
- No modifications to `classify_handler`, `classify_and_log`, `persistence.rs`, `auth.rs`, or `intent_classificator.rs`
- No changes to the classification or API key resolution flow
- No retry logic, circuit breaking, or request hedging for streaming
- No `reqwest` streaming-specific timeout (the existing 300s timeout applies to both paths)
- No modifications to the `routing.toml.example` or routing configuration

## Implementation Approach

Add `stream` field extraction alongside the existing model override at `src/main.rs:284-296` — one JSON parse serves both purposes. Branch after sending the upstream request: if `stream: true`, enter the streaming path; otherwise, continue with the existing buffered path.

The streaming path uses a `tokio::sync::mpsc` channel to bridge the upstream byte stream with a keepalive ticker. A spawned task runs a `tokio::select!` loop: forward upstream chunks as they arrive, inject `: keepalive\n\n` every 15s when quiet, and inject a final `event: error` if the upstream stream errors. The receiving end becomes the response body via `axum::body::Body::from_stream()`.

For non-2xx upstream errors on streaming requests, read the error body as text, wrap it in a short SSE `event: error` message, and return it as a non-streaming `text/event-stream` body.

## Critical Implementation Details

- **Channel-based keepalive**: The `tokio::select!` loop in a spawned task merges upstream chunks with periodic keepalive ticks. This approach avoids the complexity of a custom `Stream` implementation with pin-projection. The channel capacity (32) prevents backpressure from a slow client from blocking the upstream read.
- **Infallible error type for `Body::from_stream`**: Since the channel carries `Bytes` (not `Result`), the stream passed to `Body::from_stream` must map to `Result<Bytes, E>` where `E: Into<BoxError>`. Use `std::convert::Infallible` as the error type — it implements `std::error::Error` and the channel never produces errors (errors are handled in the spawned task by injecting error events).
- **Interval first tick**: `tokio::time::interval` fires immediately on first tick. Skip the first tick with `interval.tick().await` after creation to avoid injecting a keepalive before any upstream data arrives.

## Phase 1: Dependencies & Handler Refactoring

### Overview

Add `tokio-stream` and `futures` as direct dependencies. Change `completion_handler`'s return type from `impl IntoResponse` to `Response`. Add `stream` field extraction alongside the existing model override. All existing tests pass.

### Changes Required:

#### 1. Add streaming dependencies

**File**: `Cargo.toml`

**Intent**: Add `tokio-stream` and `futures` as direct dependencies for stream combinators and the `Stream` trait. Both are already transitive deps but need explicit entries.

**Contract**: Add `tokio-stream = "0.1"` and `futures = "0.3"` to `[dependencies]`.

#### 2. Change handler return type

**File**: `src/main.rs:176-185`

**Intent**: Change the handler signature to return `Response` instead of `impl IntoResponse`. This is necessary because the streaming path returns a `Response` built via `Response::builder()` while the buffered path uses `.into_response()` — the two concrete types differ.

**Contract**: Change `-> impl IntoResponse` to `-> Response`. All existing return points that use `(StatusCode, String)` tuples must become explicit `Response` values. Use the pattern `(StatusCode::OK, [("Content-Type", "application/json")], body).into_response()` for JSON responses.

#### 3. Extract stream field from request body

**File**: `src/main.rs:284-296`

**Intent**: In the existing JSON body parse that overrides the `model` field, also extract `stream` as a boolean. This avoids a second deserialization.

**Contract**: After `body_json["model"] = ...`, add `let client_wants_stream = body_json.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);`. Pass `client_wants_stream` down to the response path decision point.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles cleanly with new dependencies
- `cargo test` — all existing tests pass unchanged
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass

#### Manual Verification:

- Verify handler builds successfully with `Response` return type
- Verify no behavior change for `stream: false` or absent requests

---

## Phase 2: Streaming Implementation

### Overview

Implement the streaming response path triggered by `stream: true`. Use channel-based keepalive injection, SSE error events for upstream failures, and raw byte forwarding.

### Changes Required:

#### 1. Add streaming path in completion_handler

**File**: `src/main.rs:317` (after `upstream_response` is received)

**Intent**: Branch on `client_wants_stream`. If `true`, check upstream status: non-2xx returns an SSE error event; 2xx starts a stream with keepalive injection. If `false`, continue with existing buffered logic.

**Contract**:

The streaming path for a 2xx upstream response:
1. Get `upstream_response.bytes_stream()` from reqwest
2. Create a `tokio::sync::mpsc::channel::<Bytes>(32)`
3. Spawn a task with `tokio::spawn` that runs a `tokio::select!` loop:
   - On upstream chunk: forward the raw `Bytes` through the channel
   - On upstream stream error: inject `event: error\ndata: {"error":"<message>"}\n\n` and break
   - On keepalive tick (every 15s, after skipping the first tick): inject `b": keepalive\n\n"` through the channel
4. Wrap the channel receiver in `Body::from_stream()` with `std::convert::Infallible` error mapping
5. Return a `Response` with `Content-Type: text/event-stream`, `Cache-Control: no-cache`, status 200

The streaming path for a non-2xx upstream response:
1. Read the error body as text via `upstream_response.text().await`
2. Build an SSE error event: `event: error\ndata: <error_body>\n\n`
3. Return as `Body::from(sse_error_string)` with `Content-Type: text/event-stream` and the upstream status code

The channel-based keepalive snippet (non-obvious pattern):

```rust
let byte_stream = upstream_response.bytes_stream();
let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(32);

tokio::spawn(async move {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
    interval.tick().await; // skip immediate first tick
    let mut stream = byte_stream;
    loop {
        tokio::select! {
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => { if tx.send(bytes).await.is_err() { break; } }
                    Some(Err(e)) => {
                        let _ = tx.send(Bytes::from(
                            format!("event: error\ndata: {{\"error\":\"{}\"}}\n\n", e)
                        )).await;
                        break;
                    }
                    None => break,
                }
            }
            _ = interval.tick() => {
                if tx.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                    break;
                }
            }
        }
    }
});

let body = Body::from_stream(
    tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|bytes| Ok::<_, std::convert::Infallible>(bytes))
);
```

#### 2. Add imports

**File**: `src/main.rs` (import section)

**Intent**: Add necessary imports for streaming types.

**Contract**: Add `use axum::body::Body;` (if not already imported), `use tokio_stream::StreamExt;` (for `.next()` on byte stream), and `use std::convert::Infallible;`.

### Success Criteria:

#### Automated Verification:

- `cargo test` — all existing + new streaming tests pass
- `cargo build --release` compiles cleanly
- `cargo test routes_auth` — route auth tests pass (middleware unchanged)

#### Manual Verification:

- Send `curl` request with `"stream": true` to a real upstream; verify SSE events arrive incrementally
- Verify keepalive comments appear during a long completion (>15s silence from upstream)
- Verify non-streaming behavior unchanged with `"stream": false`
- Verify degradation path unchanged (no http_client → classification JSON)

---

## Phase 3: OpenAPI Spec

### Overview

Update `openapi/completions.yaml` to document the `text/event-stream` response type for streaming requests and the corresponding SSE error event format.

### Changes Required:

#### 1. Add text/event-stream response type

**File**: `openapi/completions.yaml`

**Intent**: Document that `POST /v1/chat/completions` can return `text/event-stream` when `stream: true` is in the request body. Add the SSE error event schema.

**Contract**: Under the `200` response for `/v1/chat/completions`, add a `text/event-stream` content type alongside the existing `application/json` content. The event-stream schema should describe the raw SSE byte stream. Add the request body `stream` field as an optional boolean (default: `false`).

#### 2. Add stream field to request body

**File**: `openapi/completions.yaml`

**Intent**: Document the `stream` field in the request body schema for `/v1/chat/completions`.

**Contract**: Add `stream: type: boolean, default: false` to the request body properties.

### Success Criteria:

#### Automated Verification:

- OpenAPI spec parses without errors (validate via any YAML/Swagger parser)
- No schema regressions for existing response types

#### Manual Verification:

- Review the updated spec for accuracy
- Verify `stream` field is documented in the request body

---

## Testing Strategy

### Unit Tests (in `src/main.rs` `#[cfg(test)]`):

- `test_streaming_handler_returns_sse_content_type` — httpmock returns SSE data; verify response `Content-Type: text/event-stream` and `Cache-Control: no-cache`
- `test_streaming_handler_forwards_upstream_bytes` — httpmock returns multi-chunk SSE; verify downstream body contains the raw upstream data
- `test_streaming_handler_non_2xx_returns_sse_error_event` — httpmock returns 503 on streaming request; verify response body starts with `event: error`
- `test_streaming_respects_stream_field` — httpmock returns SSE; verify `stream: false` returns buffered JSON (existing behavior), `stream: true` returns SSE
- `test_streaming_keepalive_injected` — httpmock with a delayed response that yields no data for >15s; verify keepalive comment appears in the stream body (requires collecting full stream)
- `test_streaming_degradation_no_client` — `test_app()` (http_client: None) with `stream: true` returns classification-only JSON

### Integration Tests:

- Verify the complete flow: bearer auth → classify → forward → stream back with keepalive

### Manual Testing Steps:

1. Start a local server, configure routing with a real upstream endpoint and API key
2. Send `curl -X POST /v1/chat/completions -d '{"messages":[{"role":"user","content":"Hello"}],"stream":true}'` and verify SSE events stream back
3. Send without `stream: true` and verify buffered JSON response
4. Send `stream: true` to a dead upstream and verify `event: error` in the response
5. Monitor a long-running completion (>30s) and verify keepalive comments appear

## Performance Considerations

- The channel-based keepalive spawns one `tokio` task per streaming request. Memory overhead is minimal (~1KB per task). Tasks are cleaned up when the channel sender or receiver is dropped.
- `Body::from_stream` uses chunked transfer encoding, which keeps memory usage proportional to the largest single chunk (typically a few KB), not the total response size.
- The existing 300s `reqwest` timeout applies to both streaming and buffered paths.

## Migration Notes

No data migration needed. This change is purely additive — existing non-streaming behavior is unchanged.

## References

- Research: `context/changes/sse-streaming-proxy/research.md`
- Master research: `context/changes/upstream-proxy-routing/research.md`
- Prior change plan: `context/changes/reqwest-upstream-routing/plan.md`
- Handler: `src/main.rs:176-348`
- AppState: `src/main.rs:20-27`
- Cargo.toml: `Cargo.toml:7-21`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Dependencies & Handler Refactoring

#### Automated

- [x] 1.1 `cargo build` compiles cleanly with new dependencies — 752520d
- [x] 1.2 `cargo test` — all existing tests pass unchanged — 752520d
- [x] 1.3 `cargo test auth` — auth tests pass — 752520d
- [x] 1.4 `cargo test routes_auth` — route auth tests pass — 752520d

#### Manual

- [ ] 1.5 Verify no behavior change for `stream: false` or absent requests

### Phase 2: Streaming Implementation

#### Automated

- [x] 2.1 `cargo test` — all existing + new streaming tests pass — dfe624d
- [x] 2.2 `cargo build --release` compiles cleanly — dfe624d
- [x] 2.3 New tests: SSE content type, upstream byte forwarding, error event, stream field respect, keepalive injection, degradation path — dfe624d

#### Manual

- [ ] 2.4 Verify SSE events stream back incrementally with a real upstream
- [ ] 2.5 Verify keepalive comments appear during a >15s silent completion
- [ ] 2.6 Verify `stream: false` behavior unchanged
- [ ] 2.7 Verify degradation path unchanged (no http_client → classification JSON)

### Phase 3: OpenAPI Spec

#### Automated

- [x] 3.1 OpenAPI spec parses without errors — 687fe0a

#### Manual

- [ ] 3.2 Review updated spec for accuracy
- [ ] 3.3 Verify `stream` field is documented in request body
