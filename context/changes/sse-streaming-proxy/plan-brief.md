# SSE Streaming Proxy — Plan Brief

> Full plan: `context/changes/sse-streaming-proxy/plan.md`
> Research: `context/changes/sse-streaming-proxy/research.md`

## What & Why

Add SSE streaming to `POST /v1/chat/completions`. When the client sends `stream: true`, forward upstream response bytes as `text/event-stream` with 15s keepalive pings. This prevents Render's 60s proxy timeout from killing long completions and eliminates the memory pressure of buffering full responses. Completes FR-004 (streaming) — the final piece of the upstream proxy routing sequence.

## Starting Point

`completion_handler` at `src/main.rs:318` buffers the entire upstream response via `upstream_response.text().await` and always returns `Content-Type: application/json`. No SSE code exists anywhere in the codebase, though Axum 0.8 provides the necessary types. `reqwest` 0.12 is already a dependency (added in Change 2). The handler's return type is `impl IntoResponse` — this must become a concrete `Response` to support the two different response shapes (buffered JSON vs streaming raw bytes).

## Desired End State

`stream: true` requests produce a `text/event-stream` response with raw upstream SSE bytes forwarded directly (bare passthrough — no Axum `Event` wrapping). Keepalive comments (`: keepalive\n\n`) are injected every 15 seconds when the upstream is quiet. Non-2xx upstream responses are delivered as `event: error` SSE events. Mid-stream upstream disconnections inject a final error event before closing. `stream: false` or absent requests continue with buffered JSON exactly as today. The `http_client: None` degradation path (classification-only JSON) is unchanged.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| SSE forwarding strategy | Bare passthrough (no Axum Event wrapping) | Upstream is already an SSE producer — wrapping would double-frame. | Plan |
| Keepalive mechanism | Manual with mpsc channel + select! loop | Bare passthrough bypasses Axum's `Sse` type which owns `KeepAlive`; channel-based merging avoids custom Stream impl with pin-projection. | Plan |
| Stream detection | Parse body once, extract model + stream together | Already parsing JSON for model override — zero additional allocations. | Plan |
| Upstream non-2xx on streaming | SSE `event: error` with error body | Client gets errors in their expected streaming format rather than a JSON/SSE inconsistency. | Plan |
| Mid-stream upstream failure | Inject final error event, then close | Client gets explicit notification, not a silent truncation. | Plan |
| Test strategy | httpmock with chunked body responses | Uses existing dev-dependency; verifies Content-Type, body content, and error event formatting. | Plan |
| Keepalive interval | 15 seconds | Prevents Render's 60s proxy timeout from killing connections — 4 pings per minute is well within safety margin. | Research |

## Scope

**In scope:**
- `tokio-stream` + `futures` direct dependencies in Cargo.toml
- Handler return type: `impl IntoResponse` → `Response`
- `stream` field extraction alongside model override
- Channel-based keepalive injection (mpsc + tokio::select!)
- SSE error events for non-2xx upstream and mid-stream failures
- 6 new streaming tests (Content-Type, byte forwarding, error events, stream field respect, keepalive injection, degradation)
- OpenAPI spec update with `text/event-stream` response type and `stream` request field

**Out of scope:**
- Axum `Sse`/`Event` wrapping (bare passthrough only)
- SSE line parsing or `[DONE]` marker detection
- Changes to `classify_handler`, `classify_and_log`, `persistence.rs`, `auth.rs`, `intent_classificator.rs`
- Retry, circuit breaking, or request hedging
- Changes to routing configuration

## Architecture / Approach

```
Client → POST /v1/chat/completions {"messages":[...], "stream": true}
  ├─ Classify prompt (unchanged)
  ├─ Resolve API key, build auth headers (unchanged)
  ├─ Override body.model, extract body.stream (one JSON parse)
  ├─ POST to upstream endpoint
  ├─ stream: true, upstream 2xx?
  │   ├─ bytes_stream() → mpsc channel
  │   │   ┌─ tokio::spawn: select! { chunk → forward | tick → ": keepalive\n\n" | error → "event: error..." }
  │   │   └─ Body::from_stream(receiver) → Response { Content-Type: text/event-stream }
  │   └─ upstream non-2xx → SSE "event: error\ndata: {body}\n\n" → close
  └─ stream: false → buffer and return JSON (unchanged)
```

Keepalive is injected via a spawned task running a `tokio::select!` loop: forward upstream bytes eagerly, inject `: keepalive\n\n` after 15s of silence (skipping the immediate first tick), and inject an `event: error` on upstream stream failure. The channel receiver feeds `Body::from_stream()`.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Dependencies & Handler Refactoring | Cargo.toml updates, Response return type, stream field extraction | Handler signature change breaks existing return points if not all converted to Response |
| 2. Streaming Implementation | Channel-based keepalive, error events, 6 new httpmock tests | Channel backpressure from slow clients; spawned task lifetime management |
| 3. OpenAPI Spec | Updated completions.yaml with event-stream response type | None — documentation-only |

**Prerequisites:** Changes 1-3 (classify-endpoint, reqwest-upstream-routing, provider-agnostic-config) must be complete — `reqwest::Client` in AppState, `auth_headers_for()`, and per-category endpoint config are all live on main.
**Estimated effort:** ~1 session across 3 phases

## Open Risks & Assumptions

- **Channel backpressure**: A slow downstream client could fill the 32-element mpsc channel, causing the spawned task to block on `tx.send()`. In practice, `Body::from_stream` reads eagerly, so backpressure is unlikely under normal conditions.
- **Spawned task cleanup**: The spawned keepalive task exits when either the channel receiver is dropped (client disconnects) or the upstream stream ends. No explicit cancellation is needed — `tokio::select!` naturally exits the loop.
- **`Infallible` as error type**: `std::convert::Infallible` was stabilized as implementing `std::error::Error` in Rust 1.34. The project's MSRV (via Render) supports this.
- **Upstream SSE format assumption**: Bare passthrough assumes the upstream produces valid SSE. If an upstream sends non-SSE data on a streaming endpoint, the client receives raw bytes — this is the client's responsibility to handle.
- **`stream` field in the body**: Assumes OpenAI-compatible body format where `stream` is a top-level boolean. Non-conforming clients that use headers or query params for streaming control are not supported.

## Success Criteria (Summary)

- `stream: true` requests produce `Content-Type: text/event-stream` with raw upstream bytes forwarded and keepalive comments every 15s
- `stream: false` behavior is unchanged (buffered JSON with `Content-Type: application/json`)
- Non-2xx upstream on streaming returns an SSE `event: error` event
- Mid-stream upstream failures inject a final error event before closing
- `cargo test` passes all existing + 6 new streaming tests
