# reqwest-upstream-routing Implementation Plan

## Overview

Add upstream HTTP routing to `completion_handler` (`POST /v1/chat/completions`). After classifying the prompt, the handler reads the endpoint and API key env name from the classification result, resolves the key from the environment, builds the appropriate auth headers via `auth_headers_for()`, overrides the `model` field in the request body to match the classified model, forwards the body to the upstream API, and returns the buffered response. When no key or endpoint is configured, the handler degrades to returning classification JSON (current behavior). Part 2 of 4 in the upstream proxy routing sequence.

## Current State Analysis

`completion_handler` at `src/main.rs:197-228` classifies the prompt then returns a synthetic JSON response (`{"status":"classified",...}`). It never reads `classification.endpoint`, `classification.api_key_env`, or `classification.provider_type` — all three fields are populated by the classifier but dead-ended in the handler.

`RouteEntry` (`src/intent_classificator.rs:10-16`) already carries `endpoint: String`, `provider_type: String`, and `api_key_env: Option<String>`. `auth_headers_for()` (`intent_classificator.rs:278-289`) maps provider type to the correct HTTP auth header format but has no call site in the handler.

`reqwest` is absent from the dependency tree (`Cargo.toml` line 19). No HTTP client exists anywhere in the codebase.

`AppState` (`src/main.rs:56-61`) has no HTTP client field. `test_app()` and `test_app_with_classifier()` construct `AppState` without one.

`openapi/completions.yaml` documents the v1/chat/completions response schema as the current synthetic classification JSON — it has not been updated for proxied upstream responses.

### Key Discoveries:

- **`RouteEntry` already carries all routing fields**: `endpoint`, `provider_type`, `api_key_env` at `intent_classificator.rs:10-16`. The handler just needs to consume them.
- **`auth_headers_for()` exists and works**: `intent_classificator.rs:278-289` — supports openai_compatible (Bearer), anthropic (x-api-key), ollama/local (no auth), with Bearer fallback for unknown types.
- **`required_env()` pattern exists**: `auth.rs:97-103` — the handler can use `std::env::var()` inline for key lookup, consistent with existing secret handling conventions.
- **`test_app_with_enriched_classifier()` already tests with `api_key_env`**: `main.rs:566-613` — proves the classifier propagates `api_key_env`, `provider_type`, and `endpoint` into `ClassificationResult`.
- **Existing tests expect `test_app()` to not need a client**: `test_app()` at `main.rs:439-452` creates `AppState` with no classifier and no persistence. Adding `http_client: None` preserves all existing test behavior.

## Desired End State

- `completion_handler` classifies the prompt, resolves the API key from the env var named by `classification.api_key_env`, builds auth headers via `auth_headers_for()`, overrides the request body's `model` field to `classification.model`, forwards the body to `classification.endpoint` via `reqwest`, collects the buffered response, and returns it with `Content-Type: application/json`
- When `api_key_env` is `None` or the env var is missing/empty, the handler degrades to returning classification JSON (identical to current behavior)
- When `endpoint` is empty but a key is configured, the handler returns `502` with `{"error":"upstream_error","status":502,"message":"no endpoint configured"}`
- When upstream returns a non-2xx response, the handler wraps it as `{"error":"upstream_error","status":<status>,"message":"<body>"}` with the same HTTP status code
- When `X-Cerebrum-Category` and `X-Cerebrum-Model` headers are both present and valid, the handler skips classification entirely and uses the provided values for routing and logging
- `AppState` carries `http_client: Option<reqwest::Client>`; when `None`, the handler degrades (no upstream calls attempted)
- `main()` builds a `reqwest::Client` with a 300s timeout when `UPSTREAM_API_KEY` is not the gate — the client is always built if the crate is linked; degradation is per-request based on `api_key_env` availability
- Existing tests pass unchanged; new `httpmock`-based tests verify upstream forwarding, auth header attachment, error handling, and skip-classify behavior
- `openapi/completions.yaml` reflects the proxy response schema

## What We're NOT Doing

- No SSE streaming (Change 4)
- No multi-provider configuration tooling or UI (Change 3 adds full multi-provider dispatch)
- No response transformations beyond model field override
- No retry logic, circuit breaking, or request hedging (future concerns)
- No modifications to `classify_handler` or `classify_and_log` — only `completion_handler` changes
- No modifications to `persistence.rs`, `auth.rs`, or `intent_classificator.rs`
- No JSON Schema validation of upstream responses

## Implementation Approach

Build the `reqwest::Client` once at startup in `main()` with a 300s connect + read timeout, store it in `AppState` as `Option<reqwest::Client>`. In `completion_handler`, after classification (or after reading `X-Cerebrum-*` headers), resolve the API key from the env var named by `classification.api_key_env` using `std::env::var()`. If the key is absent, degrade to classification JSON. Otherwise, deserialize the request body, override the `model` field, re-serialize, build auth headers via `auth_headers_for()`, and `POST` to `classification.endpoint`. Collect the response body, log the inference, and return.

Use `httpmock` for tests that need upstream roundtrips. The `httpmock` server runs on `0.0.0.0:0` in each test, providing a real HTTP endpoint that `reqwest` can call. Existing tests continue to pass because `test_app()` sets `http_client: None`.

## Phase 1: Dependencies & State Wiring

### Overview

Add `reqwest` to `Cargo.toml`, `httpmock` as a dev-dependency, extend `AppState` with an HTTP client field, and build the client in `main()`.

### Changes Required:

#### 1. Add reqwest dependency

**File**: `Cargo.toml`

**Intent**: Add `reqwest` with the features needed for JSON serialization and TLS, plus `httpmock` for tests.

**Contract**: After line 18 (`regex = "1"`), add `reqwest = { version = "0.12", features = ["json", "rustls-tls"] }`. After line 19 (`toml = "0.8"` or the new reqwest line), add `[dev-dependencies]` section with `httpmock = "0.7"`.

#### 2. Extend AppState

**File**: `src/main.rs:56-61`

**Intent**: Add `http_client: Option<reqwest::Client>` so the handler can make upstream HTTP calls. The `Option` wrapper allows test configurations to omit the client.

**Contract**: Add field `http_client: Option<reqwest::Client>` to the `AppState` struct.

#### 3. Build reqwest client in main()

**File**: `src/main.rs:89-98`

**Intent**: Construct a `reqwest::Client` with a 300-second timeout so long-running upstream model calls don't get cut off. The client is always built (not gated on any env var) since it's just a handle; actual upstream calls are gated per-request by `api_key_env` availability.

**Contract**: Before `let app_state = Arc::new(AppState { ... })`, build the client:
```rust
let http_client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(300))
    .build()
    .expect("reqwest client should build");
```
Add `http_client: Some(http_client)` to the `AppState` struct literal.

#### 4. Update test helpers

**File**: `src/main.rs:439-498`

**Intent**: `test_app()` and `test_app_with_classifier()` must include `http_client: None` in their `AppState` structs so existing tests continue to pass (handler degrades to classification JSON when client is `None`).

**Contract**: Add `http_client: None,` to the `AppState` struct literals in `test_app()` (after `classify_db_log: false,`), `test_app_with_classifier()` (after `classify_db_log: false,`), and `test_app_with_enriched_classifier()` (after `classify_db_log: false,`).

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds (reqwest compiles and links)
- `cargo test` — all existing tests pass with `http_client: None` in test helpers
- `cargo test auth` — auth tests unaffected
- `cargo test routes_auth` — route auth tests unaffected

---

## Phase 2: Handler Rewrite

### Overview

Rewrite `completion_handler` to perform the full upstream routing flow: classify → resolve API key → build auth headers → override model → forward to upstream → collect response → return. Add `X-Cerebrum-Category`/`X-Cerebrum-Model` header support for skip-classify.

### Changes Required:

#### 1. Rewrite completion_handler

**File**: `src/main.rs:197-228`

**Intent**: Replace the current synthetic-JSON-returning handler with one that calls the upstream API. The handler must: (a) optionally skip classification via X-Cerebrum headers, (b) resolve the API key from the env var named by `classification.api_key_env`, (c) degrade to classification JSON when no key/endpoint is available, (d) override the `model` field in the request body, (e) forward to upstream with the correct auth headers, (f) handle upstream errors with a structured Cerebrum envelope, and (g) return `Content-Type: application/json`.

**Contract**: The handler signature stays `async fn completion_handler(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> (StatusCode, String)`. 

The body (flow):
1. If `X-Cerebrum-Category` and `X-Cerebrum-Model` headers are both present and non-empty, skip classification — build a synthetic `ClassificationResult` with the header values, `tier: Fallback`, and `endpoint`/`api_key_env`/`provider_type` from the routing table lookup (or empty if the category isn't in routing)
2. Otherwise, run the existing classify + log_classification flow (extract prompt, call `state.classifier.as_ref().map(|c| c.classify(&prompt)).unwrap_or_else(...)`)
3. Check `state.http_client.is_some()` — if `None`, degrade to classification JSON (return `(StatusCode::OK, json)` identical to current behavior)
4. Check `classification.api_key_env` — if `None`, degrade to classification JSON
5. Resolve the API key via `std::env::var(api_key_env_name)` — if missing or empty, degrade to classification JSON
6. Check `classification.endpoint` — if empty, return 502 `{"error":"upstream_error","status":502,"message":"no endpoint configured"}`
7. Deserialize body bytes into `serde_json::Value`, set `["model"] = serde_json::Value::String(classification.model.clone())`, re-serialize to bytes
8. Build upstream request: `state.http_client.as_ref().unwrap().post(&classification.endpoint).headers(auth_headers).body(modified_body)`
9. Attach auth headers via `auth_headers_for(&classification.provider_type, &api_key)`
10. `.send().await` — on connection/network error, return 502 with `{"error":"upstream_error","status":502,"message":"<error description>"}`
11. Read upstream response status — if non-2xx, read body text, return `{"error":"upstream_error","status":<status_code>,"message":"<body>"}` with the upstream's status code
12. Read response body as text
13. Log classification (fire-and-forget, same as today)
14. Return `(StatusCode::OK, response_text)` 

Note: the handler now returns `impl IntoResponse` or uses `axum::response::Response` to set `Content-Type: application/json` on the response. The current signature `(StatusCode, String)` does not set Content-Type.

**Contract (signature change)**: Change signature to `-> impl IntoResponse` to allow setting the Content-Type header. Use `axum::response::Json` or construct a `Response` with the header.

#### 2. Add X-Cerebrum header support

**File**: `src/main.rs:197-228` (within the handler)

**Intent**: Clients that have already called `/v1/classify` can pass the result as `X-Cerebrum-Category` and `X-Cerebrum-Model` headers to skip re-classification. This reduces latency for pre-classified requests.

**Contract**: Extract `x_cerebrum_category` and `x_cerebrum_model` from headers at the top of the handler. If both are `Some` and non-empty, look up the category in `state.classifier.as_ref().map(|c| c.routing.get(&category))` to get the `RouteEntry`. If found, build a `ClassificationResult` with the header values and the route entry's endpoint/api_key_env/provider_type. If the category is not in routing, degrade to classification JSON. If only one header is present (or both missing), fall through to normal classification.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds (handler compiles with new reqwest calls)
- `cargo test` — all existing tests pass (degradation path returns same classification JSON)
- New tests pass (see Phase 3)

#### Manual Verification:

- Start app with `UPSTREAM_API_KEY` set to a valid OpenRouter key and `routing.toml` with non-empty endpoints
- Send `POST /v1/chat/completions` with a classification-matching prompt (e.g., "fix this bug" → SYNTAX_FIX → gpt-4o-mini)
- Verify the response is actual model output (not the old `{"status":"classified",...}` JSON)
- Verify `Content-Type: application/json` is present on the response

---

## Phase 3: Tests

### Overview

Add `httpmock`-based tests for upstream routing behavior and ensure existing tests are compatible with the new `AppState` field.

### Changes Required:

#### 1. Add upstream routing tests

**File**: `src/main.rs` (within the `#[cfg(test)] mod tests` block, after existing tests)

**Intent**: Verify that `completion_handler` correctly forwards requests to upstream when configured, handles errors, attaches auth headers, and supports skip-classify via X-Cerebrum headers.

**Contract**: Four new `#[tokio::test]` async tests:

- **`test_upstream_returns_response`**: Creates a `test_app_with_http_client()` helper that builds a real `reqwest::Client` and an `httpmock::MockServer`. Configures the mock server to respond `200 OK` with `{"choices":[{"message":{"content":"hello"}}]}`. Sends a POST to `/v1/chat/completions` with a SYNTAX_FIX-prompt body. Asserts the response body is `{"choices":[...]}` (the mocked upstream response). The test helper creates classifier routing with `endpoint: mock_server.url("/v1/chat/completions")` and `api_key_env: Some("TEST_UPSTREAM_KEY")`, setting `TEST_UPSTREAM_KEY=sk-test` in the env.

- **`test_upstream_request_includes_auth_header`**: Similar setup, but the mock server's expectation verifies the incoming request has `Authorization: Bearer sk-test`. Asserts 200.

- **`test_upstream_unreachable_returns_502`**: Configures an endpoint pointing to a dead server (e.g., `http://127.0.0.1:1`). Asserts the response is 502 and body contains `"upstream_error"`.

- **`test_upstream_skip_classify_via_headers`**: Sends a POST with `X-Cerebrum-Category: SYNTAX_FIX` and `X-Cerebrum-Model: gpt-4o-mini` headers and a body that would normally classify as CASUAL (e.g., `"hello"`). The mock server is configured for the SYNTAX_FIX endpoint. Asserts the response is the mocked upstream body (not CASUAL classification JSON). Verifies classification was skipped.

#### 2. Adapt test_app for Option<reqwest::Client>

**File**: `src/main.rs:439-498`

**Intent**: All `AppState` struct literals in test helpers need `http_client: None` to match the new struct field. This was started in Phase 1; verify coverage.

**Contract**: Ensure `test_app()`, `test_app_with_classifier()`, and `test_app_with_enriched_classifier()` all have `http_client: None` in their `AppState` literals.

### Success Criteria:

#### Automated Verification:

- `cargo test` — all tests pass, including 4 new upstream routing tests and all existing tests
- `cargo test routes_auth` — route auth tests unaffected

#### Manual Verification:

- Run `cargo test -- --nocapture` and observe httpmock server start/stop in test output (no port conflicts)
- Verify test names are descriptive in test output

---

## Phase 4: OpenAPI Spec Update

### Overview

Update `openapi/completions.yaml` to document the proxied upstream response schema and error envelope.

### Changes Required:

#### 1. Update v1/chat/completions response schema

**File**: `openapi/completions.yaml`

**Intent**: The current 200 response schema documents the old synthetic classification JSON. After this change, the 200 response is the upstream model's completion response (e.g., OpenAI-compatible chat completion). The error responses need the Cerebrum error envelope.

**Contract**:
- **200 response**: Change description to "Upstream model completion response". Remove the `status`/`category`/`model`/`tier` properties. Replace with a description that the response body is the upstream model's raw JSON output (OpenAI-compatible chat completion format). Add a `Content-Type: application/json` note.
- **502 response**: Add new error response:
  ```yaml
  "502":
    description: Upstream API unreachable or no endpoint configured
    content:
      application/json:
        schema:
          type: object
          properties:
            error:
              type: string
              enum: [upstream_error]
            status:
              type: integer
              example: 502
            message:
              type: string
          required: [error, status, message]
  ```
- **4xx/5xx proxied**: Add a note that upstream 4xx/5xx responses are wrapped in the same `{"error":"upstream_error","status":<n>,"message":"..."}` envelope with the upstream's status code.

### Success Criteria:

#### Manual Verification:

- OpenAPI spec is valid YAML (visually inspect or run a YAML linter)
- Spec accurately describes the new behavior

---

## Testing Strategy

### Unit Tests (automated, run with `cargo test`):

- `test_completion_handler_returns_classification_json` — unchanged, verifies degradation when `http_client: None`
- `test_upstream_returns_response` — mocked upstream returns body, handler passes it through
- `test_upstream_request_includes_auth_header` — verifies `Authorization: Bearer sk-test` sent upstream
- `test_upstream_unreachable_returns_502` — dead endpoint produces Cerebrum error envelope
- `test_upstream_skip_classify_via_headers` — X-Cerebrum headers bypass classification
- `test_completion_does_not_include_enriched_fields` — unchanged, verifies provider_type/api_key/endpoint don't leak into classification JSON
- `test_classify_no_enriched_fields` — unchanged

### Integration Tests (requires `UPSTREAM_API_KEY` and a real upstream):

- Not in scope for this change. End-to-end integration testing with a real upstream API is deferred.

### Manual Testing Steps:

1. Set `UPSTREAM_API_KEY` to a valid OpenRouter key
2. Set up `routing.toml` with endpoint URLs for each category (e.g., `https://openrouter.ai/api/v1/chat/completions`)
3. Send `POST /v1/chat/completions` with `{"model":"...","messages":[{"role":"user","content":"fix this bug"}]}` and verify the response is actual model output
4. Send the same request with `X-Cerebrum-Category: SYNTAX_FIX` and `X-Cerebrum-Model: gpt-4o-mini` and verify skip-classify works
5. Unset `UPSTREAM_API_KEY` (or use a category without `api_key_env`) and verify degradation to classification JSON
6. Remove the `endpoint` from a routing entry and verify 502 when that category is classified

## Performance Considerations

- The reqwest client is a single shared instance with connection pooling (built-in in reqwest) — no per-request client creation overhead
- The 300s timeout is generous for model inference but prevents infinite hangs
- Body deserialization + model override + re-serialization adds ~0.1-0.5ms for typical chat completion bodies (<10KB)
- No streaming overhead — the response is fully buffered, same as current behavior

## Migration Notes

- Existing deployments without `api_key_env` in `routing.toml` will see no change in behavior — the handler degrades to classification JSON
- To enable upstream routing, add `endpoint`, `api_key_env`, and `provider_type` to `routing.toml` entries and set the corresponding env vars
- The `openapi/completions.yaml` change is documentation-only and backwards-compatible

## References

- Research: `context/changes/reqwest-upstream-routing/research.md`
- Master research: `context/changes/upstream-proxy-routing/research.md`
- `auth_headers_for()`: `src/intent_classificator.rs:278-289`
- `RouteEntry` struct: `src/intent_classificator.rs:10-16`
- `required_env()` pattern: `src/auth.rs:97-103`
- `completion_handler` (current): `src/main.rs:197-228`
- `test_app_with_enriched_classifier()`: `src/main.rs:566-613`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Dependencies & State Wiring

#### Automated

- [x] 1.1 `cargo build` succeeds (reqwest compiles and links)
- [x] 1.2 `cargo test` — all existing tests pass
- [x] 1.3 `cargo test auth` — auth tests unaffected
- [x] 1.4 `cargo test routes_auth` — route auth tests unaffected

### Phase 2: Handler Rewrite

#### Automated

- [x] 2.1 `cargo build` succeeds (handler compiles with reqwest calls)
- [x] 2.2 `cargo test` — all existing tests pass (degradation path unchanged)

#### Manual

- [ ] 2.3 Manual test: upstream call returns real model output (requires valid upstream key and routing config)

### Phase 3: Tests

#### Automated

- [x] 3.1 `cargo test` — all tests pass including 4 new upstream routing tests
- [x] 3.2 `cargo test routes_auth` — route auth tests unaffected

### Phase 4: OpenAPI Spec Update

#### Manual

- [ ] 4.1 OpenAPI spec is valid YAML and describes the new behavior

### Impl-Review Notes (2026-06-02)

- **F1 (FIXED)**: `log_classification` moved from before to after each exit point with status derived from upstream result (`"ok"`, `"upstream_error"`, `"bad_request"`).
- **F2 (FIXED)**: Content-Type check moved above `from_utf8`; invalid UTF-8 body returns 400 instead of silently producing empty prompt.
- **F3 (FIXED)**: Upstream error body truncated to 512 chars in client response; full body logged server-side via `eprintln!`.
- **F4 (FIXED)**: Upstream response body read via chunked streaming with 10 MB cap; oversized responses return 502.
- **F5 (FIXED)**: All 9 `persistence::tests::test_fetch_*` integration tests adapted to work on a shared DB: unique category/model prefixes per test prevent stale data collisions; `.expect()` replaces `.ok()` for INSERT/DELETE calls; assertions use `>=` or per-test filtering instead of assuming an empty DB.
