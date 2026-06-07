---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 7940421e3d801a63974e0f060b8ad4f39f322853
branch: main
repository: cerebrum
topic: "Completing intent-aware proxy: upstream routing and SSE streaming"
tags: [research, upstream-routing, sse-streaming, reqwest, provider-agnostic, proxy, s-01]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
last_updated_note: "Added follow-up research for provider-agnostic routing design; added follow-up for route split and multi-change decomposition"
---

# Research: Upstream Routing and SSE Streaming for Intent-Aware Proxy

**Date**: 2026-06-01T00:00:00+02:00
**Researcher**: pfrack
**Git Commit**: 7940421e3d801a63974e0f060b8ad4f39f322853
**Branch**: main
**Repository**: cerebrum

## Research Question

The intent-aware proxy is not doing upstream routing and SSE streaming. The `completion_handler` classifies intents and returns JSON metadata, but never forwards requests to upstream LLM APIs. What exactly is missing, and what needs to be built to complete S-01?

## Summary

**The proxy classifies but doesn't route.** The `completion_handler` at `src/main.rs:119-175` performs intent classification, builds an `InferenceRecord` for the dashboard, and returns `{"status":"classified","category":"SYNTAX_FIX","model":"gpt-4o-mini","tier":"Regex"}` — but never makes an outbound HTTP call. The `endpoint` field from `ClassificationResult` is parsed from TOML and carried through the data structures but reaches a dead end in the handler. No SSE streaming code exists.

**Three things are missing:**
1. An HTTP client (`reqwest`) for upstream API calls — not in `Cargo.toml`
2. Request forwarding logic in `completion_handler` — the handler returns a synthetic JSON, not a proxied response
3. SSE streaming response — the handler returns `(StatusCode, String)`, which is non-streaming

**The gap was intentional.** The proxy-intent-routing plan (`plan.md:39`) explicitly scoped upstream proxying out: "No upstream model proxying (no `reqwest`, no SSE streaming, no OpenRouter API calls)." Classification was delivered as a checkpoint; upstream routing is the next phase.

## Detailed Findings

### 1. Current Handler: What It Does vs. What It Should Do

**File**: `src/main.rs:119-175`

**Current behavior** (step by step):
1. Validates `Content-Type: application/json` → returns 415 if mismatched
2. Records `start` timestamp for duration measurement
3. Converts body bytes to string, calls `persistence::extract_last_user_message()` to parse the OpenAI-compatible JSON and extract the last user message
4. Classifies the prompt via `state.classifier.as_ref().map(|c| c.classify(&prompt))` or falls back to `ClassificationResult::fallback()` (category=`"CASUAL"`, model=`"gpt-4o-mini"`, endpoint=`""`)
5. Constructs a synthetic JSON response:
   ```json
   {"status":"classified","category":"COMPLEX_REASONING","model":"claude-3.5-sonnet","tier":"Regex"}
   ```
6. Returns `(StatusCode::OK, response_body)` — a fully buffered, non-streaming response
7. Enqueues a fire-and-forget `log_inference()` task for dashboard persistence

**What's NOT happening:**
- The `classification.endpoint` field is never read (only `.category`, `.model`, `.tier` are used in the JSON response at lines 141-146)
- No HTTP request is made to any upstream API
- No response streaming occurs
- No upstream API authentication (API keys) is configured or used

### 2. The `endpoint` Field: Parsed but Dead-Ended

**`RouteEntry` struct** (`src/intent_classificator.rs:10-14`):
```rust
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,           // defined but never consumed by handler
    pub cost_per_1m_input_tokens: Option<f64>,
}
```

**`ClassificationResult` also carries `endpoint`** (`src/intent_classificator.rs:56-61`):
```rust
pub struct ClassificationResult {
    pub category: String,
    pub model: String,
    pub endpoint: String,           // propagated from RouteEntry, never read
    pub tier: ClassificationTier,
}
```

**Where `endpoint` gets populated:**

| Source | File:Line | Value |
|--------|-----------|-------|
| `hardcoded_routing()` — all 5 entries | `intent_classificator.rs:219-255` | `String::new()` (empty) |
| `load_routing_from_file()` — from TOML | `intent_classificator.rs:342-345` | Parsed from `value.get("endpoint")` |
| `ClassificationResult::fallback()` | `intent_classificator.rs:382-388` | `String::new()` (empty) |
| `route_match()` | `intent_classificator.rs:516-524` | `route.endpoint.clone()` |
| `route_fallback()` | `intent_classificator.rs:526-533` | `self.fallback_entry.endpoint.clone()` |

**The TOML parser works** — `load_routing_from_file()` correctly extracts `endpoint` from `routing.toml`. However:
- `routing.toml` does not exist in the repo; only `routing.toml.example` exists
- The example file (`routing.toml.example:7,11,15,19,23`) has all endpoints pointing to `https://openrouter.ai/api/v1/chat/completions`
- Without a real `routing.toml`, `load_routing()` falls back to `hardcoded_routing()` where all endpoints are `String::new()`
- In the handler at line 141-146, `classification.endpoint` is **never read** — the field exists but the handler ignores it

### 3. Dependencies: What's Missing in Cargo.toml

**Current dependencies** (`Cargo.toml:1-19`): `axum`, `tokio`, `base64`, `subtle`, `tower`, `serde_json`, `sqlx`, `uuid`, `chrono`, `askama`, `askama_web`, `regex`, `toml`

**Not present:**
| Crate | Needed For | Features Required |
|-------|-----------|-------------------|
| `reqwest` (0.12) | Upstream HTTP calls to OpenRouter/LLM APIs | `json`, `stream`, `rustls-tls` |
| `tokio-stream` (0.1) | Stream combinators (`StreamExt`) for SSE line parsing | (default) |
| `futures` (0.3) | `Stream` trait, `stream::iter` | (default) |

Note: `tokio-stream` 0.1.18 and `futures-util` 0.3.32 are already transitive dependencies (via `sqlx-core` and `axum` respectively), but not direct deps. `reqwest` is entirely absent from the dependency tree.

### 4. SSE Streaming: What Axum 0.8 Provides

**Axum 0.8.9** (already in Cargo.toml) provides these SSE types via `axum::response::sse`:

| Type | Purpose |
|------|---------|
| `Sse<S>` | Wraps a `Stream<Item = Result<Event, E>>` into an SSE response. Sets `Content-Type: text/event-stream`, `Cache-Control: no-cache`. Implements `IntoResponse`. |
| `Event` | Builder: `Event::default().data("text").event("name").id("1")`. Implements `From<String>` and `From<&str>`. |
| `KeepAlive` | Periodic comment pings (`: keepalive\n\n`): `KeepAlive::new().interval(Duration::from_secs(15))` |

**No SSE code exists anywhere in the codebase.** Zero matches for `sse`, `text/event-stream`, `Event::default()`, `axum::response::sse`, or `KeepAlive`.

### 5. Handler Return Type: Must Change for Streaming

The handler currently returns `(StatusCode, String)`. For SSE streaming, the return type must be:
```rust
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
```
Or, with error handling:
```rust
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, StatusCode>
```

The auth middleware (`require_proxy_bearer`) calls `next.run(request).await` and expects a `Response<Body>`. Since `Sse<S>` implements `IntoResponse`, the middleware chain works without changes.

### 6. Architecture: No Conflict Between Buffered Body and Streaming Response

The request body must be buffered to extract the user message for classification, but this does not conflict with streaming the response:

```
1. POST /v1/chat/completions → Axum buffers body as Bytes     [ONE-SHOT]
2. Extract user message + classify intent                    [SYNCHRONOUS]
3. Forward buffered body to upstream API via reqwest         [STREAMING REQUEST]
4. Upstream returns SSE stream → reqwest bytes_stream()       [STREAMING RESPONSE]
5. Parse SSE lines, wrap in axum::response::sse::Event       [STREAMING TRANSFORM]
6. Return Sse<impl Stream> to Axum                           [STREAMING RESPONSE]
7. Fire-and-forget DB log continues in background             [ASYNC, DETACHED]
```

Steps 1-2 are synchronous/buffered. Steps 3-6 are streaming. The buffered `Bytes` body is forwarded directly to `reqwest` — no deserialization/re-serialization needed.

### 7. Upstream SSE Proxying Pattern

The canonical pattern for upstream SSE proxying in Rust/Axum:

```rust
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use tokio_stream::StreamExt;

async fn completion_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    // 1. Classification (existing logic)
    let classification = classify(&body, &state);

    // 2. Get upstream URL from classification
    let endpoint = if classification.endpoint.is_empty() {
        "https://openrouter.ai/api/v1/chat/completions".to_string()
    } else {
        classification.endpoint.clone()
    };

    // 3. Forward request to upstream
    let upstream_response = state.http_client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", state.upstream_api_key))
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")))?;

    // 4. Convert upstream SSE byte stream to Axum Event stream
    let byte_stream = upstream_response.bytes_stream();
    let event_stream = byte_stream
        .map(|chunk| {
            let bytes = chunk.unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            Ok(Event::default().data(text.into_owned()))
        });

    // 5. Log inference (fire-and-forget, same as now)
    // ...

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()))
}
```

**Key decisions:**
- Forward raw `body.to_vec()` rather than re-serializing — preserves unknown fields
- Share a single `reqwest::Client` via `AppState` for connection pooling
- Use `KeepAlive::new().interval(Duration::from_secs(15))` to prevent Render's 60s proxy timeout
- Set a read timeout: `reqwest::Client::builder().timeout(Duration::from_secs(300))`
- Detect `data: [DONE]` from upstream to close the stream cleanly

### 8. API Key Management

**Existing pattern**: No API keys are managed. The codebase uses env vars for secrets (`AuthConfig::from_env()` reads `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`).

**Proposed**: An `UPSTREAM_API_KEY` env var (e.g., OpenRouter API key: `sk-or-v1-...`). The key is used in the `Authorization: Bearer <key>` header for upstream requests. It should follow the existing `required_env()` pattern from `auth.rs:97-103`.

Alternatively, API keys could be per-endpoint in `routing.toml`, but this adds complexity. A single `UPSTREAM_API_KEY` env var is simpler and follows established patterns.

### 9. Logging: Existing Fire-and-Forget Pattern Still Works

The current `log_inference()` call (`src/main.rs:167-172`) spawns a detached `tokio::spawn` task that persists the `InferenceRecord` after the response is assembled. In the streaming scenario:

- Classification + logging setup happens **before** the stream starts
- The `duration_ms` metric captures only classification time (as today), not total upstream streaming time
- Tracking actual upstream latency would require adding a second timestamp before the upstream HTTP call

The `JoinHandle` is intentionally detached (`src/main.rs:167-172`) — panics in the logging task are isolated from the response path. This is documented in `impl-review.md:F3` and the decision was to keep this pattern.

### 10. Historical Context: Why Routing Was Deferred

**Plan.md:39** (`context/changes/proxy-intent-routing/plan.md`):
> "No upstream model proxying (no `reqwest`, no SSE streaming, no OpenRouter API calls)"

**Plan-brief.md:45-46**:
> "**Out of scope:** Upstream model proxying (reqwest, SSE streaming, OpenRouter API calls)"

**Plan-brief.md:8**:
> "This is the core of S-01 — the north-star slice that validates intent-aware triage. Upstream model proxying is deferred to a future change."

The reasoning was incremental delivery: validate classification end-to-end (regex patterns, TOML routing config, handler integration, dashboard visibility), then add proxying as a separate change. This follows the research recommendation at `research.md:246`:
> "Regex Tier 1 + ONNX Tier 2 is a composable pipeline that can be implemented incrementally"

**Roadmap.md:24** (S-01 north star):
> "Smallest end-to-end proof: proxy accepts a request, classifies intent (regex first, cheap-model fallback for ambiguous), routes to an appropriate upstream model, and streams response back via SSE."

The north star is NOT yet met — classification works, routing does not.

**PRD FR-003** (must-have): "Gateway can route each request using an intent-to-path mapping policy."
**PRD FR-004** (must-have): "Gateway can return incremental response chunks to clients during long-running completions."

Both FR-003 and FR-004 remain unimplemented.

### 11. Integration Points: Where to Make Changes

| File | Line(s) | Change |
|------|---------|--------|
| `Cargo.toml` | After line 18 | Add `reqwest`, `tokio-stream`, `futures` direct dependencies |
| `src/main.rs:56-61` | `AppState` struct | Add `http_client: reqwest::Client` and `upstream_api_key: String` |
| `src/main.rs:89-93` | `main()` initialization | Build reqwest client, add to AppState |
| `src/main.rs:119-175` | `completion_handler` | Full rewrite: classification + upstream HTTP call + SSE streaming |
| `src/main.rs:141-146` | Response assembly | Replace synthetic JSON with SSE stream |
| `src/main.rs:387-421` | `test_app()` and `test_app_with_classifier()` | Add reqwest client and API key to test state |
| `src/auth.rs:97-103` | `required_env()` pattern | Reuse for `UPSTREAM_API_KEY` validation |

**No changes needed**: `src/intent_classificator.rs` (classification pipeline is complete), `src/persistence.rs` (logging is unchanged), `src/auth.rs` (middleware is unchanged), `templates/` (dashboard already renders category/model).

### 12. Test Implications

The handler currently has tests that expect a 200 JSON response with `"category":"SYNTAX_FIX"`. With SSE streaming:
- Unit tests need a mock upstream or a test-only code path
- The `test_app()` helper must include a reqwest client (or mock it)
- Integration tests need an upstream mock or test double

Options:
1. **Test-only code path**: Add a config flag or env var that keeps the old JSON response for testing
2. **Mock upstream**: Use `wiremock` or `httpmock` to simulate the upstream API
3. **Feature gate**: Use a Cargo feature `test-mode` that returns classification JSON instead of streaming

## Architecture Insights

1. **The buffered Bytes → streaming response pattern is natural.** Classification requires full text (buffered body), but the response is a separate streaming phase. No architectural conflict.

2. **The `endpoint` field is already wired through the pipeline.** `ClassificationResult.endpoint` exists, is populated from TOML, and just needs to be consumed. The data flow is: `routing.toml → RouteEntry.endpoint → ClassificationResult.endpoint → handler reads endpoint → upstream HTTP call`. Only the last step is missing.

3. **Fire-and-forget logging is compatible with streaming.** The logging task spawns before the stream starts and runs independently. No changes needed to `persistence.rs`.

4. **The `Option<Arc<T>>` pattern works for the reqwest client.** But since an HTTP client is always needed (even without classification, the fallback path still routes), it could be non-optional. Alternatively, keep it `Option` for testability — test_app sets it to `None` and the handler degrades to the current JSON-only response.

5. **SSE keepalive is critical for Render.** Render's load balancer has a 60-second proxy timeout (`infrastructure.md:51`). Without keepalive pings, long completions (30+ seconds) may get disconnected. `KeepAlive::new().interval(Duration::from_secs(15))` is the mitigation.

## Historical Context (from prior changes)

- `context/changes/proxy-intent-routing/plan.md:39` — Explicit decision: upstream proxying out of scope
- `context/changes/proxy-intent-routing/research.md:207-213` — reqwest + tokio-stream listed as future S-01 dependencies
- `context/changes/proxy-intent-routing/research.md:266` — SSE streaming mechanics flagged as "not yet researched"
- `context/foundation/roadmap.md:24` — S-01 north star requiring routing + streaming
- `context/foundation/prd.md:70-75` — FR-003 (routing) and FR-004 (streaming) as must-have requirements
- `context/foundation/shape-notes.md:55` — Original seed: "classify intent via gpt-4o-mini via OpenRouter"
- `context/changes/proxy-intent-routing/impl-review.md:90-96` — F6 (Content-Type validation) and F7 (sanitize return type) observations

## Related Research

- `context/changes/proxy-intent-routing/research.md` — Original classification research; sections 8-17 define the regex-only classifier that's now implemented
- `context/foundation/infrastructure.md` — Render deployment details including the 60s proxy timeout requiring keepalive
- `context/changes/data-persistence-async-logging/plan.md` — `InferenceRecord` schema with `category` and `upstream_model` fields
- `context/changes/inference-log-inspection/plan.md` — Dashboard queries rendering classification results

## Open Questions

1. **API key config**: Single `UPSTREAM_API_KEY` env var, or per-endpoint keys in `routing.toml`? Single key follows existing patterns; multi-key requires routing.toml changes.

2. **Error handling strategy**: If the upstream API returns 4xx/5xx, should the handler return an error to the client, or a synthetic SSE event with the error?

3. **Streaming vs. non-streaming**: Should the handler support both streaming (`stream: true` in the forwarded body) and non-streaming (collect full upstream response, return as single JSON)? The PRD FR-004 requires streaming but non-streaming may be needed for some upstream APIs.

4. **Model name prefixing**: OpenRouter expects model names like `openai/gpt-4o-mini` or `anthropic/claude-3.5-sonnet`. The current routing uses bare names like `gpt-4o-mini` and `claude-3.5-sonnet`. Where should the provider prefix be added?

5. **Test strategy**: How to test the streaming handler without a real upstream API? Mock via `wiremock`, or add a test-only non-streaming code path?

## Follow-up Research: Provider-Agnostic Routing Design (2026-06-01T12:00:00+02:00)

This follow-up addresses the constraint that upstream routing should work with **any provider** — not just OpenRouter. The original research assumed OpenRouter throughout. This section redesigns the routing config and data structures to be provider-agnostic.

### 13. Provider Auth Matrix

Two fundamental patterns cover ~90% of LLM providers:

| Provider | Auth Header | Base URL | OpenAI-Compat? | Extra Headers |
|---|---|---|---|---|
| OpenAI | `Authorization: Bearer <key>` | `https://api.openai.com/v1` | N/A (standard) | None |
| OpenRouter | `Authorization: Bearer <key>` | `https://openrouter.ai/api/v1` | Yes | `HTTP-Referer`, `X-Title` (optional) |
| Groq | `Authorization: Bearer <key>` | `https://api.groq.com/openai/v1` | Yes | None |
| DeepSeek | `Authorization: Bearer <key>` | `https://api.deepseek.com/v1` | Yes | None |
| Together AI | `Authorization: Bearer <key>` | `https://api.together.xyz/v1` | Yes | None |
| Mistral | `Authorization: Bearer <key>` | `https://api.mistral.ai/v1` | Yes | None |
| Fireworks | `Authorization: Bearer <key>` | `https://api.fireworks.ai/inference/v1` | Yes | None |
| xAI (Grok) | `Authorization: Bearer <key>` | `https://api.x.ai/v1` | Yes | None |
| **Anthropic** | **`x-api-key: <key>`** | `https://api.anthropic.com` | **No** — different body schema | `anthropic-version: 2023-06-01` |
| Azure OpenAI | `api-key: <key>` | Custom per-resource | Yes (with different URL) | None |
| **Ollama** | **None** | `http://localhost:11434/v1` | Yes | None |
| vLLM / TGI | None (configurable) | Variable | Yes | Varies |

**Two provider adapters cover the field:**
1. **`openai_compatible`** — `Authorization: Bearer <key>`, forwards body as-is, works with ~90% of providers
2. **`anthropic`** — `x-api-key: <key>`, translates OpenAI body to Anthropic Messages format (system prompt as top-level field)

Ollama is `openai_compatible` with no API key. vLLM/TGI are `openai_compatible` with an optional auth header.

### 14. Current Codebase: Provider-Agnostic at Source Level

**The Rust source is already provider-agnostic.** Zero references to OpenRouter, Anthropic, or any specific provider exist in `src/`. The only provider assumption is in `routing.toml.example` (all endpoints point to `https://openrouter.ai/api/v1/chat/completions`).

| File | OpenRouter assumption? |
|---|---|
| `src/intent_classificator.rs:96` (`DEFAULT_ENDPOINT = ""`) | No — defaults to empty |
| `hardcoded_routing()` at `intent_classificator.rs:217-257` | No — all endpoints are `String::new()` |
| `src/main.rs` (all code) | No — no reference to any provider |
| `routing.toml.example:5-23` | **Yes** — every endpoint is OpenRouter |
| `context/changes/upstream-proxy-routing/research.md` (original) | **Yes** — assumes OpenRouter |
| `context/foundation/shape-notes.md:55` | Mentions OpenRouter once as tentative |
| `context/foundation/roadmap.md:115` | Treats provider choice as an open question |
| `context/foundation/prd.md` | Provider-agnostic — says "appropriate upstream model" |

### 15. What Needs to Change: RouteEntry and routing.toml

#### RouteEntry must gain fields for provider configuration

**Current** (`src/intent_classificator.rs:10-14`):
```rust
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,
    pub cost_per_1m_input_tokens: Option<f64>,
}
```

**Required additions:**

| Field | Type | Purpose |
|---|---|---|
| `provider_type` | `String` | `"openai_compatible"`, `"anthropic"`, `"ollama"` — determines auth header + body translation |
| `api_key_env` | `Option<String>` | Name of env var holding the API key (e.g., `"OPENAI_API_KEY"`). `None` for no-auth providers like Ollama |
| `extra_headers` | `HashMap<String, String>` | Provider-specific headers: `HTTP-Referer`/`X-Title` for OpenRouter, `anthropic-version` for Anthropic |

**Optional additions (could defer):**
- `timeout_secs: Option<u64>` — per-model timeout
- `max_retries: Option<u32>` — per-model retry count

#### ClassificationResult must carry auth info downstream

`ClassificationResult` (`src/intent_classificator.rs:56-61`) currently carries `category`, `model`, `endpoint`, `tier`. It must also carry `provider_type`, `api_key_env`, `extra_headers` so the handler can construct the correct upstream request.

#### routing.toml needs two-level structure: providers + routing

**Current format** (flat, one-size-fits-all):
```toml
[COMPLEX_REASONING]
model = "claude-3.5-sonnet"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
```

**Provider-agnostic format** (providers defined once, referenced by name):
```toml
# ── Provider definitions ──
[providers.openai]
type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"
api_version = "2023-06-01"

[providers.groq]
type = "openai_compatible"
api_key_env = "GROQ_API_KEY"
base_url = "https://api.groq.com/openai/v1"

[providers.ollama]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
# no api_key_env — local, no auth

[providers.openrouter]
type = "openai_compatible"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
extra_headers = { "HTTP-Referer" = "https://cerebrum.example.com", "X-Title" = "Cerebrum" }

# ── Intent → provider+model routing ──
[COMPLEX_REASONING]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
cost_per_1m_input_tokens = 3.00

[FILE_READING]
provider = "groq"
model = "llama-3.3-70b-versatile"

[SYNTAX_FIX]
provider = "openai"
model = "gpt-4o-mini"
cost_per_1m_input_tokens = 0.15

[CASUAL]
provider = "groq"
model = "llama-3.3-70b-versatile"

[fallback]
provider = "openrouter"
model = "openai/gpt-4o-mini"
```

**Alternative (simpler, single-level for MVP):**

```toml
[COMPLEX_REASONING]
model = "claude-sonnet-4-20250514"
endpoint = "https://api.anthropic.com/v1/messages"
provider_type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[CASUAL]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"

[LOCAL]
model = "llama3.2"
endpoint = "http://localhost:11434/v1/chat/completions"
provider_type = "openai_compatible"
# no api_key_env — Ollama has no auth
```

The two-level design (providers + routing) is **cleaner** because it avoids repeating the same provider config across multiple categories. Four categories all routing to OpenRouter would repeat `provider_type = "openai_compatible"` and `api_key_env = "OPENROUTER_API_KEY"` four times. However, the single-level design is **simpler to implement in MVP** — it requires only adding fields to the existing flat TOML parser.

### 16. Auth Header Construction: Lookup Table

A small in-code lookup table maps `provider_type` strings to auth behavior:

```rust
fn auth_headers_for(
    provider_type: &str,
    api_key: &str,
) -> (String, String, Vec<(String, String)>) {
    // Returns (header_name, header_value, extra_headers)
    match provider_type {
        "openai_compatible" | "openrouter" | "" =>
            ("authorization".into(), format!("Bearer {api_key}"), vec![]),
        "anthropic" =>
            ("x-api-key".into(), api_key.to_string(), vec![
                ("anthropic-version".into(), "2023-06-01".into())
            ]),
        "ollama" | "local" =>
            ("".into(), "".into(), vec![]),  // no auth
        _ =>
            ("authorization".into(), format!("Bearer {api_key}"), vec![]),
    }
}
```

### 17. TOML Parsing Changes Required

**File**: `src/intent_classificator.rs:326-354` (`load_routing_from_file`)

The parser currently reads `model`, `endpoint`, `cost_per_1m_input_tokens`. For provider-agnostic support, it must additionally read:
- `provider_type` from `value.get("provider_type")` — defaults to `""` (empty = openai_compatible)
- `api_key_env` from `value.get("api_key_env")` — optional, defaults to `None`

For a two-level design (providers section + routing), a separate `[providers]` table parse step loads provider definitions first, then routing entries reference them by name.

### 18. Secret Management Pattern

The existing pattern (`src/auth.rs:97-103`) validates secrets at startup via env vars:

| Secret Env Var | Provider |
|---|---|
| `OPENAI_API_KEY` | OpenAI |
| `ANTHROPIC_API_KEY` | Anthropic |
| `OPENROUTER_API_KEY` | OpenRouter |
| `DEEPSEEK_API_KEY` | DeepSeek |
| `GROQ_API_KEY` | Groq |

Rather than loading ALL possible keys at startup, the proxy should **lazily read the key** when a route entry references it via `api_key_env`. This avoids requiring env vars for providers that aren't used. The first time a route uses `api_key_env = "ANTHROPIC_API_KEY"`, the handler reads that env var. If missing, return a 502 with a clear error.

Lazy loading also aligns with the existing graceful-degradation pattern: if the classifier can't initialize, the gateway still starts and classifies with CASUAL fallback. Similarly, if an upstream API key is missing, that specific intent can fall back to another provider.

### 19. What "Any Provider" Means in Practice

| Scenario | Support |
|---|---|
| Direct OpenAI API | `provider_type = "openai_compatible"`, `api_key_env = "OPENAI_API_KEY"` |
| Direct Anthropic API | `provider_type = "anthropic"` — needs body translation adapter |
| OpenRouter (multiprovider) | `provider_type = "openai_compatible"`, uses OpenRouter as a relay |
| Groq / DeepSeek / Together / Mistral | `provider_type = "openai_compatible"` with their API key |
| Local Ollama | `provider_type = "openai_compatible"`, no api_key_env |
| Self-hosted vLLM / TGI | `provider_type = "openai_compatible"`, optional api_key_env |
| Any OpenAI-compatible proxy (LiteLLM, etc.) | `provider_type = "openai_compatible"` |

**Body translation** is only needed for Anthropic's Messages API (different schema). All other providers accept the standard OpenAI chat completions body. For MVP, the Anthropic adapter can be deferred — it's the one outlier.

### 20. Revised Integration Points

Updated from the original finding (Section 11), reflecting provider-agnostic changes:

| File | Line(s) | Change |
|------|---------|--------|
| `Cargo.toml` | After line 18 | Add `reqwest`, `tokio-stream`, `futures` direct dependencies |
| `src/intent_classificator.rs:10-14` | `RouteEntry` struct | Add `provider_type: String`, `api_key_env: Option<String>`, `extra_headers: HashMap<String, String>` |
| `src/intent_classificator.rs:56-61` | `ClassificationResult` | Add `provider_type: String`, `api_key_env: Option<String>` |
| `src/intent_classificator.rs:326-354` | `load_routing_from_file()` | Parse new TOML fields: `provider_type`, `api_key_env`, `extra_headers` |
| `src/intent_classificator.rs:217-257` | `hardcoded_routing()` | Add `provider_type: String::new()` to all RouteEntry defaults |
| `src/main.rs:56-61` | `AppState` struct | Add `http_client: reqwest::Client` |
| `src/main.rs:89-93` | `main()` initialization | Build reqwest client, add to AppState |
| `src/main.rs:119-175` | `completion_handler` | Rewrite: read auth headers from classification, make upstream call, stream SSE |
| `routing.toml.example` | Entire file | Replace with provider-agnostic example using `provider_type` and `api_key_env` |
| `render.yaml` | `envVars` section | Add optional `sync: false` entries for provider API keys used in deployment |

**No changes needed**: `src/persistence.rs` (unchanged), `src/auth.rs` (unchanged), `templates/` (dashboard displays unchanged).

### 21. Revised Open Questions

1. **Single-level vs two-level routing.toml**: Flat (one `[CATEGORY]` block per intent with all provider fields inline) is simpler to implement. Two-level (`[providers.X]` + `[CATEGORY]` referencing a provider by name) is cleaner for multiple categories sharing providers. Start with flat for MVP, refactor to two-level when needed.

2. **Anthropic body translation**: Should it be an in-code adapter (translate before forwarding) or should Anthropic be deferred entirely? Deferring avoids ~50 lines of body translation; Anthropic is the only major outlier.

3. **API key validation**: Lazy (read when first used, as described in Section 18) or eager (validate all referenced keys at startup)? Lazy is more flexible but errors surface mid-request.

4. **Error handling**: If an upstream API returns 4xx/5xx, should the handler return the raw error as SSE, or a structured Cerebrum error event?

5. **Streaming vs non-streaming**: Should the handler support both modes? Most OpenAI-compatible APIs accept `stream: true` in the body. The gateway could force `stream: true` or respect whatever the client sends.

## Follow-up Research: Route Split and Multi-Change Decomposition (2026-06-01T12:00:00+02:00)

This follow-up addresses the request to: (1) move classification to a separate API route, and (2) divide the upstream proxy work into more, smaller changes.

### 22. Feasibility of Separate Classify Endpoint

**Yes, this is a trivial change.** Adding `POST /v1/classify` is a ~30-line net addition with zero breaking changes.

**Current router** (`src/main.rs:336-360`):
```
proxy_routes (/v1/*, bearer auth):
  POST /chat/completions  →  completion_handler
```

**After adding classify route:**
```
proxy_routes (/v1/*, bearer auth):
  POST /chat/completions  →  completion_handler  (unchanged)
  POST /classify          →  classify_handler    (NEW)
```

**Why it's safe:**

- **Auth is inherited** — the `.layer(require_proxy_bearer)` on `proxy_routes` (`src/main.rs:339-342`) applies to ALL routes in the sub-router. New route gets auth for free.
- **No existing test breaks** — all tests hit `/v1/chat/completions`, `/health`, or `/dashboard/*`. Adding a route has no side effects on existing route behavior.
- **One test needs relocation** — `test_completion_handler_returns_classification_json` (`src/main.rs:424-453`) currently hits `/v1/chat/completions` to test classification. After the split, it should hit `/v1/classify` instead. When `completion_handler` later becomes a routing proxy, that test would break anyway.

### 23. Separability of Classification Logic

**`classify()` is a pure, stateless method** (`src/intent_classificator.rs:456-534`). It takes `&self` (the pre-compiled `RegexSet` + routing table) and a `&str` prompt, returns a `ClassificationResult`. No async, no I/O, no side effects. Any handler with `AppState` can call it.

**`extract_last_user_message` is already a shared utility** (`src/persistence.rs:417-447`). Its own doc comment at line 415-416 describes it as "shared utility used by both snippet extraction and the intent classifier." No changes needed.

**The classify handler** is literally the first half of `completion_handler` (`src/main.rs:119-147`), minus the logging block:

```rust
async fn classify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    // Content-type guard (identical to completion_handler:124-130)
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/json".to_string());
    }

    let body_str = std::str::from_utf8(&body).unwrap_or("");
    let prompt = persistence::extract_last_user_message(body_str);

    let classification = state.classifier.as_ref()
        .map(|c| c.classify(&prompt))
        .unwrap_or_else(intent_classificator::ClassificationResult::fallback);

    let response_body = serde_json::json!({
        "status": "classified",
        "category": classification.category,
        "model": classification.model,
        "tier": format!("{:?}", classification.tier),
    }).to_string();

    (StatusCode::OK, response_body)
}
```

**Notable:** The classify handler does NOT log to persistence. Classification alone is a read-only operation — no inference record to log. If logging classify-only requests is desired, it can be added later.

### 24. Coupling Points Between Handler and Classifier

`completion_handler` currently couples classification and response assembly:

| Lines | Concern | Extractable? |
|---|---|---|
| 124-130 | Content-Type validation | Shared (both handlers need it) |
| 132 | Timer start | Routing-specific (measures upstream latency) |
| 134-135 | Body parse + prompt extraction | Shared (both handlers need prompt text) |
| 137-139 | Classify call | Classification-specific (moves to classify_handler) |
| 141-146 | Build classification JSON | Classification-specific (moves to classify_handler) |
| 150-172 | Fire-and-forget logging | Routing-specific (logs inference events with upstream model) |

**When `completion_handler` becomes a routing proxy**, the classify call (lines 137-139) and JSON response (lines 141-146) are replaced with: (a) an optional header-based pre-classification skip, (b) an upstream HTTP call, (c) response assembly.

### 25. Four-Change Decomposition

The upstream proxy work divides into four independently-shippable changes:

---

#### Change 1: `classify-endpoint` — Separate classification API route

**Delivers:** A dedicated `POST /v1/classify` endpoint returning classification JSON. Classification is decoupled from the proxy handler, available as a standalone service.

**Files touched:**
| File | Change |
|------|--------|
| `src/main.rs` | Add `classify_handler` function (~25 lines after line 175). Add `.route("/classify", post(classify_handler))` in `build_app` (at line 338). |
| `openapi/completions.yaml` | Add `POST /v1/classify` path alongside existing `/v1/chat/completions`. |

**Tests:**
- `test_classify_handler_returns_classification_json` — new, hits `/v1/classify`
- All 17 existing tests pass unchanged
- `test_completion_handler_returns_classification_json` still passes (not yet moved)

**Explicitly NOT doing:**
- No changes to `completion_handler` (still returns classification JSON)
- No changes to `Cargo.toml`, `src/intent_classificator.rs`, `src/auth.rs`, `src/persistence.rs`
- No logging from classify endpoint

---

#### Change 2: `reqwest-upstream-routing` — Add upstream HTTP routing (non-streaming)

**Delivers:** `POST /v1/chat/completions` forwards requests to an upstream LLM API and returns the collected response body. FR-003 (routing) is delivered. A single `UPSTREAM_API_KEY` env var secures all upstream calls.

**Files touched:**
| File | Change |
|------|--------|
| `Cargo.toml` | Add `reqwest = { version = "0.12", features = ["json", "stream", "rustls-tls"] }` |
| `src/main.rs` | `AppState`: add `http_client: Option<reqwest::Client>`. `main()`: build client. `completion_handler`: rewrite to forward body to upstream via classification endpoint, collect response. `test_app()` / `test_app_with_classifier()`: set `http_client: None`. |
| `openapi/completions.yaml` | Update response schema for proxied upstream response. |

**Handler behavior matrix:**
| `UPSTREAM_API_KEY` set? | `classification.endpoint` | Result |
|---|---|---|
| No | Any | Degrade: return classification JSON (current behavior) |
| Yes | Empty | 502: `{"error":"no endpoint configured"}` |
| Yes | Non-empty | Forward body to upstream, collect `.text().await`, return as JSON |

**Tests:**
- `test_upstream_returns_response` — mock HTTP server, verify handler returns upstream body
- `test_upstream_unreachable_returns_502` — dead endpoint, verify 502
- `test_upstream_request_includes_auth_header` — verify `Authorization: Bearer <key>` header
- All existing tests pass (http_client=None → degraded path returns classification JSON)
- Existing `test_completion_handler_returns_classification_json` still passes (no http_client → degraded)

**Explicitly NOT doing:**
- No SSE streaming — response is fully buffered
- No provider-agnostic config — single `UPSTREAM_API_KEY` with `Authorization: Bearer` for all
- No changes to `RouteEntry`, `ClassificationResult`, `routing.toml.example`
- No keepalive pings

---

#### Change 3: `provider-agnostic-config` — Generalize routing configuration

**Delivers:** Each intent category can route to a different provider with its own API key and auth scheme. `routing.toml` gains `provider_type` and `api_key_env` fields.

**Files touched:**
| File | Change |
|------|--------|
| `src/intent_classificator.rs` | `RouteEntry`: add `provider_type: String`, `api_key_env: Option<String>`. `ClassificationResult`: propagate new fields. `load_routing_from_file()`: parse new TOML fields. `hardcoded_routing()`: add defaults. Add `auth_headers_for()` lookup function. All tests updated for new fields. |
| `src/main.rs` | `completion_handler`: read `provider_type` + `api_key_env` from classification, resolve key lazily from env var, construct auth header via lookup table. Remove global `UPSTREAM_API_KEY` from AppState. |
| `routing.toml.example` | Replace with provider-agnostic format. |

**Auth lookup table (`auth_headers_for`):**
| `provider_type` | Header | Extra headers |
|---|---|---|
| `"openai_compatible"` | `Authorization: Bearer <key>` | — |
| `"anthropic"` | `x-api-key: <key>` | `anthropic-version: 2023-06-01` |
| `"ollama"` | (no auth) | — |
| `""` (default) | `Authorization: Bearer <key>` | — |

**Provider-agnostic routing.toml:**
```toml
[COMPLEX_REASONING]
model = "claude-sonnet-4-20250514"
endpoint = "https://api.anthropic.com/v1/messages"
provider_type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[CASUAL]
model = "llama-3.3-70b-versatile"
endpoint = "https://api.groq.com/openai/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "GROQ_API_KEY"

[LOCAL]
model = "llama3.2"
endpoint = "http://localhost:11434/v1/chat/completions"
provider_type = "ollama"
```

**Tests:**
- TOML parsing tests for new fields
- `auth_headers_for()` dispatch tests per provider type
- Lazy key loading: missing env var → 502
- Ollama no-auth header test
- Existing tests updated for new RouteEntry fields (no behavior change)

**Explicitly NOT doing:**
- No SSE streaming
- No two-level `[providers.X]` TOML format (flat only)
- No Anthropic body translation adapter
- No per-model timeout/retry config

---

#### Change 4: `sse-streaming-proxy` — SSE streaming responses

**Delivers:** Upstream responses are streamed incrementally as SSE events with keepalive pings. FR-004 (streaming) is delivered. S-01 is complete.

**Files touched:**
| File | Change |
|------|--------|
| `Cargo.toml` | Add `tokio-stream = "0.1"`, `futures = "0.3"` as direct dependencies |
| `src/main.rs` | `completion_handler`: return type → `Response` (supports both `Sse<S>` and JSON degradation). Replace `response.text().await` with `response.bytes_stream()` → `StreamExt::map` → `Event::default().data()`. Add `KeepAlive::new().interval(Duration::from_secs(15))`. Handle `data: [DONE]` marker. `test_completion_handler_returns_classification_json` → moved to `/v1/classify` route. |
| `openapi/completions.yaml` | Update response to `text/event-stream` with SSE event schema. |

**Handler return type evolution:**
```
Change 1–3: async fn completion_handler(...) -> (StatusCode, String)
Change 4:   async fn completion_handler(...) -> Response
```

Using `Response` (via `IntoResponse`) allows both paths:
- **Degradation** (no http_client): `(StatusCode::OK, json_body).into_response()` — sets `Content-Type: application/json`
- **Upstream success**: `Sse::new(stream).keep_alive(...)` → `into_response()` — sets `Content-Type: text/event-stream`
- **Upstream error**: `(StatusCode::BAD_GATEWAY, error_body).into_response()`

**Tests:**
- `test_streaming_handler_returns_sse_content_type` — verify `Content-Type: text/event-stream`
- `test_streaming_handler_emits_sse_events` — mock multi-chunk upstream SSE, verify forwarded events
- `test_streaming_handler_includes_data_done` — verify `[DONE]` termination
- `test_streaming_handler_upstream_error_returns_502` — non-streaming error path
- `test_completion_handler_returns_classification_json` updated to hit `/v1/classify`
- All other 16 tests unchanged

**Explicitly NOT doing:**
- No dual streaming/non-streaming mode — always streams when client is configured
- No SSE event type annotations (`.event("completion")`)
- No upstream response body transformation
- No streaming duration tracking (`duration_ms` still captures classification time only)

---

### 26. Dependency Graph

```
Change 1: classify-endpoint
   │
   │  (classification is decoupled from proxy handler)
   │
   ▼
Change 2: reqwest-upstream-routing     ← adds HTTP client, upstream calls
   │
   │  (routing works with single key)
   │
   ▼
Change 3: provider-agnostic-config     ← generalizes auth, extends RouteEntry
   │
   │  (routing config is final)
   │
   ▼
Change 4: sse-streaming-proxy          ← adds streaming response mode
   │
   ▼
S-01 COMPLETE: classify + route + stream
```

**Sequencing rationale:**

1. **Change 1 first** — decouples classification from the proxy handler. This is a ~30-line change that establishes the API boundary and makes subsequent changes clearer. It also means the classify endpoint exists and can be tested independently before any routing work begins.

2. **Change 2 second** — the most impactful step: actual routing. Delivers FR-003. The single-key limitation is a deliberate stepping stone to validate reqwest integration before adding provider dispatch complexity.

3. **Change 3 third** — generalizes the routing config. Touches the classifier module (which was untouched in Changes 1-2). The auth lookup table and TOML parsing changes are self-contained.

4. **Change 4 last** — streaming is a response-mode change, orthogonal to routing logic. Delivers FR-004 and completes S-01.

**Why not combine Changes 1+2:** Change 1 is ~30 lines and validates the classify/proxy separation. If the separation needs rethinking, it surfaces before any routing code is written. The cost of an extra change is minimal; the downside of combining is a mixed-concern change (new route + HTTP client + handler rewrite).

**Why not combine Changes 2+3:** Change 2 touches `Cargo.toml`, `main.rs`, and OpenAPI. Change 3 additionally touches `intent_classificator.rs` (struct fields, TOML parser, constructor, auth_headers_for) and `routing.toml.example`. Together they'd be a large change touching 5+ files across two modules with ~150 lines of net-new code.

### 27. Revised Integration Points (All 4 Changes)

| Change | Files | Net-new lines (approx) |
|---|---|---|
| 1. `classify-endpoint` | `src/main.rs`, `openapi/completions.yaml` | ~40 |
| 2. `reqwest-upstream-routing` | `Cargo.toml`, `src/main.rs`, `openapi/completions.yaml` | ~80 |
| 3. `provider-agnostic-config` | `src/intent_classificator.rs`, `src/main.rs`, `routing.toml.example` | ~120 |
| 4. `sse-streaming-proxy` | `Cargo.toml`, `src/main.rs`, `openapi/completions.yaml` | ~60 |

**Total across all 4 changes:** ~300 lines net-new code, ~3-4 files per change (max), each change independently testable and shippable.

### 28. Test Migration Plan

| Test | Current route | After Change 1 | After Change 4 |
|---|---|---|---|
| `test_completion_handler_returns_classification_json` | `/v1/chat/completions` | `/v1/chat/completions` (unchanged) | `/v1/classify` (rewritten) |
| `test_classify_handler_returns_classification_json` (new) | — | `/v1/classify` (added) | `/v1/classify` (unchanged) |
| `routes_auth_proxy_requires_valid_bearer_token` | `/v1/chat/completions` | unchanged | unchanged |
| All 14 dashboard/DB tests | Various dashboard routes | unchanged | unchanged |

Only 1 existing test migrates routes, and only in Change 4 when `completion_handler` stops returning classification JSON.

### 29. Revisited Open Questions

1. **Should the classify endpoint log?** No — classification alone is a read-only operation. No inference occurred. Logging can be added later if classification query analytics are desired.

2. **Should `completion_handler` still call `classify()` internally?** In Change 2, yes — the handler classifies + routes in one call. Alternatively, the client can call `/v1/classify` first, then pass the result via `X-Cerebrum-Category` / `X-Cerebrum-Model` headers to `/v1/chat/completions`, skipping re-classification. This header-skip pattern can be added in Change 2 or deferred.

3. **What happens when `completion_handler` no longer returns classification JSON?** `test_completion_handler_returns_classification_json` migrates to `/v1/classify` in Change 4. Until then (Changes 1-3), the test stays on `/v1/chat/completions` because the handler still returns JSON in degraded mode.

4. **Keep the classify endpoint behind the same bearer auth?** Yes — it lives under `/v1/` inside `proxy_routes`, sharing the same `require_proxy_bearer` middleware. No additional auth changes needed.
