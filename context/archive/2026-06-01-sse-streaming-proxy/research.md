---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 7940421e3d801a63974e0f060b8ad4f39f322853
branch: main
repository: cerebrum
topic: "SSE streaming for upstream proxy responses"
tags: [research, sse-streaming, keepalive, axum, streaming-proxy]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
---

# Research: SSE Streaming Proxy

Extracted from the master research doc at `context/changes/upstream-proxy-routing/research.md`.

## Dependencies: What's Missing

| Crate | Needed For | Features Required |
|---|---|---|
| `tokio-stream` (0.1) | Stream combinators (`StreamExt`) for SSE line parsing | (default) |
| `futures` (0.3) | `Stream` trait, `stream::iter` | (default) |

`tokio-stream` 0.1.18 and `futures-util` 0.3.32 are already transitive dependencies (via `sqlx-core` and `axum` respectively), but not direct deps. `reqwest` was added in Change 2.

## SSE Streaming: What Axum 0.8 Provides

**Axum 0.8.9** provides these SSE types via `axum::response::sse`:

| Type | Purpose |
|---|---|
| `Sse<S>` | Wraps a `Stream<Item = Result<Event, E>>` into an SSE response. Sets `Content-Type: text/event-stream`, `Cache-Control: no-cache`. Implements `IntoResponse`. |
| `Event` | Builder: `Event::default().data("text").event("name").id("1")`. Implements `From<String>` and `From<&str>`. |
| `KeepAlive` | Periodic comment pings (`: keepalive\n\n`): `KeepAlive::new().interval(Duration::from_secs(15))` |

**No SSE code exists anywhere in the codebase.**

## Handler Return Type: Must Change for Streaming

The handler currently returns `(StatusCode, String)`. For SSE streaming, the return type must change to support both paths. Using `Response` (via `IntoResponse`) allows:
- **Degradation** (no http_client): `(StatusCode::OK, json_body).into_response()` — sets `Content-Type: application/json`
- **Upstream success**: `Sse::new(stream).keep_alive(...)` → `into_response()` — sets `Content-Type: text/event-stream`
- **Upstream error**: `(StatusCode::BAD_GATEWAY, error_body).into_response()`

```rust
async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // ...
}
```

## Upstream SSE Proxying Pattern

The canonical pattern for upstream SSE proxying in Rust/Axum:

```rust
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use tokio_stream::StreamExt;

async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1. Classification (existing logic)
    let classification = classify(...);

    // 2. Forward request to upstream
    let upstream_response = state.http_client
        .post(&endpoint)
        .header("Authorization", ...)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| ...)?;

    // 3. Handle streaming vs non-streaming
    if client_requested_streaming(&body) {
        let byte_stream = upstream_response.bytes_stream();
        let event_stream = byte_stream
            .map(|chunk| {
                let bytes = chunk.unwrap_or_default();
                let text = String::from_utf8_lossy(&bytes);
                Ok(Event::default().data(text.into_owned()))
            });
        Ok(Sse::new(event_stream)
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
    } else {
        // Collect buffered response
        let text = upstream_response.text().await...;
        Ok((StatusCode::OK, text).into_response())
    }
}
```

**Key decisions:**
- Forward raw `body.to_vec()` rather than re-serializing — preserves unknown fields
- Share a single `reqwest::Client` via `AppState` for connection pooling
- Use `KeepAlive::new().interval(Duration::from_secs(15))` to prevent Render's 60s proxy timeout
- Set a read timeout: `reqwest::Client::builder().timeout(Duration::from_secs(300))`
- Respect client's `stream` field in the body — when `stream: true`, use SSE; otherwise collect and return as single JSON

## Logging in Streaming Context

The current `log_inference()` call spawns a detached `tokio::spawn` task. In the streaming scenario:
- Classification + logging setup happens **before** the stream starts
- The `duration_ms` metric captures only classification time (as today), not total upstream streaming time

The `JoinHandle` is intentionally detached — panics in the logging task are isolated from the response path.

## SSE Keepalive Is Critical for Render

Render's load balancer has a 60-second proxy timeout (`infrastructure.md:51`). Without keepalive pings, long completions (30+ seconds) may get disconnected. `KeepAlive::new().interval(Duration::from_secs(15))` is the mitigation.

## Integration Points

| File | Line(s) | Change |
|---|---|---|
| `Cargo.toml` | After line 18 | Add `tokio-stream = "0.1"`, `futures = "0.3"` direct dependencies |
| `src/main.rs` | `completion_handler` | Return type → `Response`. Replace `response.text().await` with `response.bytes_stream()` → SSE stream or buffered path based on `stream` field. Add KeepAlive. |
| `src/main.rs` | `test_app()` tests | Update `test_completion_handler_returns_classification_json` to hit `/v1/classify` (it moves here). |
| `openapi/completions.yaml` | Response schema | Add `text/event-stream` response content type with SSE event schema. |

**No changes needed**: `src/intent_classificator.rs`, `src/persistence.rs`, `src/auth.rs`, `routing.toml.example`

## Test Implications

- `test_streaming_handler_returns_sse_content_type` — verify `Content-Type: text/event-stream`
- `test_streaming_handler_emits_sse_events` — mock multi-chunk upstream SSE, verify forwarded events
- `test_streaming_handler_includes_data_done` — verify `[DONE]` termination
- `test_streaming_handler_upstream_error_returns_502` — non-streaming error path
- `test_streaming_respects_stream_field` — verify `stream: false` returns buffered JSON
- `test_completion_handler_returns_classification_json` updated to hit `/v1/classify`

## Open Questions (Resolved in Planning)

1. **Streaming toggle**: Respect client's `stream` field — if `stream: true` in the forwarded body, stream SSE; otherwise collect and return JSON.
2. **Keepalive interval**: 15 seconds — follows research recommendation for Render's 60s proxy timeout.
3. **`[DONE]` marker**: Detect and forward `data: [DONE]` from upstream to close the stream cleanly.
