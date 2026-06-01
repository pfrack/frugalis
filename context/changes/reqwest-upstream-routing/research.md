---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 7940421e3d801a63974e0f060b8ad4f39f322853
branch: main
repository: cerebrum
topic: "Upstream HTTP routing with single API key"
tags: [research, upstream-routing, reqwest, http-client, proxy]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
---

# Research: Upstream HTTP Routing (reqwest)

Extracted from the master research doc at `context/changes/upstream-proxy-routing/research.md`.

## Current Handler: What It Does vs. What It Should Do

**File**: `src/main.rs:119-175`

**Current behavior**:
1. Validates `Content-Type: application/json` → returns 415 if mismatched
2. Records `start` timestamp for duration measurement
3. Converts body bytes to string, calls `persistence::extract_last_user_message()` to parse the OpenAI-compatible JSON
4. Classifies the prompt via `state.classifier.as_ref().map(|c| c.classify(&prompt))` or falls back to `ClassificationResult::fallback()`
5. Constructs a synthetic JSON response
6. Returns `(StatusCode::OK, response_body)` — a fully buffered, non-streaming response
7. Enqueues a fire-and-forget `log_inference()` task for dashboard persistence

**What's NOT happening** (what this change delivers):
- The `classification.endpoint` field is never read (only `.category`, `.model`, `.tier` are used)
- No HTTP request is made to any upstream API
- No upstream API authentication is configured or used

## The `endpoint` Field: Parsed but Dead-Ended

**`RouteEntry` struct** (`src/intent_classificator.rs:10-14`):
```rust
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,           // defined but never consumed by handler
    pub cost_per_1m_input_tokens: Option<f64>,
}
```

The `endpoint` field is populated from TOML by `load_routing_from_file()` at `intent_classificator.rs:342-345`, propagated through `ClassificationResult` via `route_match()` and `route_fallback()`, but never read by `completion_handler`.

## Dependencies: What's Missing

| Crate | Needed For | Features Required |
|---|---|---|
| `reqwest` (0.12) | Upstream HTTP calls to LLM APIs | `json`, `stream`, `rustls-tls` |

`reqwest` is entirely absent from the dependency tree.

## Architecture: No Conflict Between Buffered Body and Streaming Response

The request body must be buffered to extract the user message for classification, but this does not conflict with streaming the response:

```
1. POST /v1/chat/completions → Axum buffers body as Bytes     [ONE-SHOT]
2. Extract user message + classify intent                    [SYNCHRONOUS]
3. Forward buffered body to upstream API via reqwest         [STREAMING REQUEST]
4. Upstream returns response → reqwest response              [BUFFERED FOR THIS CHANGE]
5. Return collected response body                             [BUFFERED — SSE streaming is Change 4]
6. Fire-and-forget DB log continues in background             [ASYNC, DETACHED]
```

## API Key Management

**Current pattern**: The codebase uses env vars for secrets (`AuthConfig::from_env()` reads `PROXY_API_BEARER_TOKEN`, etc.).

**For this change**: A single `UPSTREAM_API_KEY` env var (e.g., OpenRouter API key: `sk-or-v1-...`). The key is used in the `Authorization: Bearer <key>` header for upstream requests. Follows the existing `required_env()` pattern from `auth.rs:97-103`.

## Logging: Existing Fire-and-Forget Pattern Still Works

The current `log_inference()` call spawns a detached `tokio::spawn` task. For this change:
- Classification + logging setup happens before the upstream HTTP call
- The `duration_ms` metric captures only classification time (as today), not total upstream latency

## Integration Points

| File | Line(s) | Change |
|---|---|---|
| `Cargo.toml` | After line 18 | Add `reqwest = { version = "0.12", features = ["json", "stream", "rustls-tls"] }` |
| `src/main.rs:56-61` | `AppState` struct | Add `http_client: Option<reqwest::Client>` and remove `upstream_api_key` (handled as env var inline) |
| `src/main.rs:89-93` | `main()` initialization | Build reqwest client, add to AppState |
| `src/main.rs:119-175` | `completion_handler` | Rewrite: classify, read endpoint from classification, make upstream HTTP call, collect buffered response, return |
| `src/main.rs:362-421` | `test_app()` / `test_app_with_classifier()` | Set `http_client: None` (degraded path returns classification JSON) |
| `openapi/completions.yaml` | Response schema | Update for proxied upstream response |

**No changes needed**: `src/intent_classificator.rs`, `src/persistence.rs`, `src/auth.rs`

## Handler Behavior Matrix

| `UPSTREAM_API_KEY` set? | `classification.endpoint` | Result |
|---|---|---|
| No | Any | Degrade: return classification JSON (current behavior) |
| Yes | Empty | 502: `{"error":"upstream_error","status":502,"message":"no endpoint configured"}` |
| Yes | Non-empty | Forward body to upstream, collect `.text().await`, return as JSON |

## Consider: Accept X-Cerebrum-Category / X-Cerebrum-Model Headers

(Resolved in planning: Accept these headers to skip re-classification.)

Clients that have already called `/v1/classify` can pass the result as headers to skip re-classification:
- `X-Cerebrum-Category: SYNTAX_FIX`
- `X-Cerebrum-Model: gpt-4o-mini`

If both headers are present and valid, the handler bypasses `classify()` entirely and uses the provided category/model for routing and logging.

## Test Implications

- Unit tests need a mock upstream or a test-only code path
- `test_app()` includes `http_client: None` → degraded path returns classification JSON
- Existing `test_completion_handler_returns_classification_json` still passes (no http_client → degraded)
- New tests needed:
  - `test_upstream_returns_response` — mock HTTP server, verify handler returns upstream body
  - `test_upstream_unreachable_returns_502` — dead endpoint, verify 502
  - `test_upstream_request_includes_auth_header` — verify `Authorization: Bearer <key>` header
  - `test_upstream_skip_classify_via_headers` — verify X-Cerebrum headers skip classification

## Open Questions (Resolved in Planning)

1. **Pre-classify header skip**: Accept X-Cerebrum-Category/X-Cerebrum-Model headers
2. **Error handling**: Structured Cerebrum JSON error `{"error":"upstream_error","status":502,"message":"..."}`
3. **Streaming**: No — this change returns buffered responses. SSE streaming is Change 4.
4. **Single API key**: Yes — `UPSTREAM_API_KEY` env var for all upstream calls. Multi-provider keys are Change 3.
