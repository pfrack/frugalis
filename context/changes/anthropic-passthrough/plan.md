# Anthropic Pass-Through Proxy Implementation Plan

## Overview

Add `POST /v1/messages` endpoint that accepts Anthropic Messages API requests, classifies intent via the existing classifier, routes to an Anthropic-compatible upstream, and forwards request/response verbatim. This is the foundation for multi-protocol support — proving the route works end-to-end before adding protocol translation.

## Current State Analysis

- `POST /v1/chat/completions` exists as the sole proxy endpoint, handling OpenAI-format traffic
- Intent classification extracts prompts from OpenAI message format (`extract_last_user_message`)
- `auth_headers_for` supports pluggable auth via `AuthProviderConfig` but has no Anthropic-specific config today
- Streaming is byte-forwarding with keepalive injection — reusable for Anthropic pass-through
- `RouteEntry.provider_type` distinguishes upstream protocols (currently `"openai_compatible"`, `"nvidia"`, etc.)

### Key Discoveries:

- `extract_last_user_message` (src/persistence.rs:1084) assumes OpenAI format (`content` is always a string). Anthropic has `content` as string OR array of content blocks — need a new extractor.
- `build_upstream_request` (src/main.rs:840) always sets `Authorization: Bearer` or uses `auth_headers_for`. Anthropic upstreams need `x-api-key` + `anthropic-version` headers.
- `handle_streaming_response` (src/main.rs:947) is protocol-agnostic (byte pipe) — works for Anthropic SSE pass-through without modification.
- Tests use `test_app_with_http_client()` returning `(Router, MockServer)` — same pattern applies for the new endpoint.

## Desired End State

`POST /v1/messages` accepts any valid Anthropic Messages API request body, classifies intent, and proxies to an Anthropic upstream with correct auth headers. Streaming and non-streaming both work. OTel metrics and persistence logging have full parity with the existing endpoint. The proxy's own errors are returned in Anthropic error format.

**Verification**: Send a valid Anthropic Messages request to `/v1/messages` with a mock upstream configured → receive the upstream's Anthropic-format response unchanged (except `model` field overridden).

## What We're NOT Doing

- No protocol translation (Anthropic↔OpenAI) — that's the next two changes
- No new config schema for routes — we reuse `RouteEntry` with `provider_type: "anthropic"`
- No changes to the existing `/v1/chat/completions` handler
- No new dependencies in Cargo.toml
- No Anthropic-specific rate limiting

## Implementation Approach

Mirror the existing `completion_handler` structure but for Anthropic protocol: validate content-type → extract prompt → classify → resolve API key → build upstream request (with Anthropic auth) → forward → return response. Proxy errors use Anthropic error format.

## Phase 1: Anthropic Prompt Extractor + Auth Support

### Overview

Add `extract_last_user_message_anthropic()` for classification, and configure `auth_headers_for` to handle `provider_type: "anthropic"`.

### Changes Required:

#### 1. Anthropic prompt extractor

**File**: `src/persistence.rs`

**Intent**: Add `extract_last_user_message_anthropic(body: &str) -> String` that finds the last `role: "user"` message and extracts text content from both string and array-of-blocks formats. Same DoS cap (1,000 messages, 10,000 chars) as existing extractor.

**Contract**: `pub fn extract_last_user_message_anthropic(body: &str) -> String` — returns empty string on parse failure.

#### 2. Anthropic auth header support

**File**: `src/intent_classifier.rs`

**Intent**: When `provider_type == "anthropic"`, `auth_headers_for` should return `x-api-key` header. Add a hard-coded `anthropic-version: 2023-06-01` header alongside.

**Contract**: `auth_headers_for(providers, "anthropic", key)` returns `[("x-api-key", key), ("anthropic-version", "2023-06-01")]`. This works if an `[[auth_providers]]` entry with `type = "anthropic"` exists in config OR via hard-coded fallback for the `"anthropic"` type.

#### 3. Unit tests

**File**: `src/persistence.rs` (test module)

**Intent**: Test `extract_last_user_message_anthropic` with: simple string content, array of text blocks, mixed text+image blocks (images ignored), empty messages array, malformed JSON.

**Contract**: 5 unit tests covering the cases above.

**File**: `src/intent_classifier.rs` (test module)

**Intent**: Test `auth_headers_for` with `provider_type = "anthropic"` returns correct headers.

**Contract**: 1 unit test.

### Success Criteria:

#### Automated Verification:

- All existing tests still pass: `cargo test`
- New unit tests pass: `cargo test extract_last_user_message_anthropic`
- New auth test passes: `cargo test auth_headers_for`
- Clippy clean: `cargo clippy -- -D warnings`

#### Manual Verification:

- None for this phase — pure functions tested automatically.

---

## Phase 2: Messages Handler + Route Wiring

### Overview

Add the `messages_handler` function and wire it into the router at `/v1/messages`. The handler mirrors `completion_handler` but uses Anthropic prompt extraction and returns errors in Anthropic format.

### Changes Required:

#### 1. Messages handler function

**File**: `src/main.rs`

**Intent**: Add `async fn messages_handler(State, HeaderMap, Bytes) -> Response` that: validates content-type, extracts prompt via `extract_last_user_message_anthropic`, classifies intent, resolves API key, builds upstream request (with model override), forwards to upstream (streaming or buffered), and logs classification. Proxy errors use Anthropic error format (`{"type": "error", "error": {"type": "...", "message": "..."}}`).

**Contract**: Same signature as `completion_handler`. Internal flow:
1. Validate `content-type: application/json`
2. Parse body as UTF-8
3. Check `x-cerebrum-category` / `x-cerebrum-model` headers (same override mechanism)
4. Extract prompt → classify
5. Resolve `api_key_env`
6. Call `build_upstream_request_anthropic(client, classification, body, api_key, auth_providers)` — same as `build_upstream_request` but uses `auth_headers_for` which now handles anthropic
7. Send upstream, handle response (streaming or buffered)
8. Return response

The `build_upstream_request_anthropic` is a near-clone of `build_upstream_request` with one difference: it adds `anthropic-version` header when provider_type is anthropic.

#### 2. Anthropic error helper

**File**: `src/main.rs`

**Intent**: Add `anthropic_error_json(error_type: &str, message: &str) -> String` for the proxy's own errors (auth failure, bad request, no endpoint).

**Contract**: Returns `{"type":"error","error":{"type":"<error_type>","message":"<message>"}}`.

#### 3. Route registration

**File**: `src/main.rs` (in `build_app`)

**Intent**: Register `/messages` route in the `proxy_routes` router alongside `/chat/completions`.

**Contract**: `.route("/messages", post(messages_handler))` added to `proxy_routes` (which is nested under `/v1`).

#### 4. OTel instrumentation

**File**: `src/main.rs`

**Intent**: Add `RequestMetrics` tracking with `route: "/v1/messages"` and `classification_total` metric emission, mirroring `completion_handler`'s pattern.

**Contract**: `#[cfg(feature = "otel")]` blocks identical to `completion_handler` but with route label `"/v1/messages"`.

### Success Criteria:

#### Automated Verification:

- Compiles clean: `cargo build`
- Clippy clean: `cargo clippy -- -D warnings`
- Existing tests pass: `cargo test`

#### Manual Verification:

- Endpoint responds to POST at `/v1/messages` (even if it returns classification-only JSON when no upstream is configured)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Integration Tests

### Overview

End-to-end tests using httpmock for the new endpoint: non-streaming, streaming, error responses, and auth rejection.

### Changes Required:

#### 1. Integration tests

**File**: `src/main.rs` (test module)

**Intent**: Add integration tests mirroring existing patterns from `test_app_with_http_client`. Tests cover:
1. Auth rejection (no bearer token → 401)
2. Non-streaming success: mock upstream returns Anthropic JSON → proxy forwards it
3. Streaming success: mock upstream returns Anthropic SSE → proxy byte-forwards with keepalive
4. Upstream error: mock returns 429 → proxy forwards error body
5. Classification-only: no http_client configured → returns classification JSON
6. Model override: verify the forwarded request has the classifier-selected model, not the client's

**Contract**: 6 integration tests using `httpmock::MockServer` + `tower::ServiceExt::oneshot`.

### Success Criteria:

#### Automated Verification:

- All new integration tests pass: `cargo test messages_handler`
- Full suite green: `cargo test`

#### Manual Verification:

- None — integration tests cover the HTTP contract.

---

## Phase 4: OpenAPI Spec Update

### Overview

Add `/v1/messages` to the OpenAPI spec following the lessons.md rule.

### Changes Required:

#### 1. OpenAPI spec

**File**: `openapi/completions.yaml`

**Intent**: Add `POST /v1/messages` path with Anthropic Messages request/response schema. Document streaming (`text/event-stream`) and error responses in Anthropic format.

**Contract**: New path entry under `paths:` with request body schema (model, max_tokens, messages required), 200 response (message object or SSE stream), 400/401/502 errors in Anthropic format.

### Success Criteria:

#### Automated Verification:

- YAML is valid: `python3 -c "import yaml; yaml.safe_load(open('openapi/completions.yaml'))"`
- Existing tests still pass: `cargo test`

#### Manual Verification:

- Spec is consistent with actual endpoint behavior.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests:

- `extract_last_user_message_anthropic`: string content, array blocks, mixed blocks, empty, malformed
- `auth_headers_for` with `provider_type: "anthropic"`

### Integration Tests:

- Auth gate (401 without token)
- Non-streaming round-trip (mock Anthropic upstream)
- Streaming round-trip (mock Anthropic SSE upstream)
- Upstream error forwarding
- Classification-only fallback
- Model override verification

### Manual Testing Steps:

1. Configure a route with `provider_type = "anthropic"` pointing to a real Anthropic-compatible endpoint
2. Send a request via `curl -X POST http://localhost:10000/v1/messages -H "Authorization: Bearer TOKEN" -H "Content-Type: application/json" -d '{"model":"claude-sonnet-4-20250514","max_tokens":100,"messages":[{"role":"user","content":"Say hello"}]}'`
3. Verify response is valid Anthropic Messages format
4. Repeat with `"stream": true` and verify SSE events arrive

## Performance Considerations

- No additional overhead vs existing pass-through — same byte-forwarding pattern
- `extract_last_user_message_anthropic` does one JSON parse (same as existing)
- No buffering of streaming responses (lesson: "Handle upstream error bodies without full buffering")

## References

- Research: `context/changes/anthropic-passthrough/research.md`
- Existing handler: `src/main.rs:1130` (`completion_handler`)
- Existing streaming: `src/main.rs:947` (`handle_streaming_response`)
- Auth helpers: `src/intent_classifier.rs:427` (`auth_headers_for`)
- Prompt extraction: `src/persistence.rs:1084` (`extract_last_user_message`)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Anthropic Prompt Extractor + Auth Support

#### Automated

- [x] 1.1 All existing tests still pass: `cargo test` — d819486
- [x] 1.2 New unit tests pass: `cargo test extract_last_user_message_anthropic` — d819486
- [x] 1.3 New auth test passes: `cargo test auth_headers_for` — d819486
- [x] 1.4 Clippy clean: `cargo clippy -- -D warnings` — d819486

### Phase 2: Messages Handler + Route Wiring

#### Automated

- [x] 2.1 Compiles clean: `cargo build` — d819486
- [x] 2.2 Clippy clean: `cargo clippy -- -D warnings` — d819486
- [x] 2.3 Existing tests pass: `cargo test` — d819486

#### Manual

- [ ] 2.4 Endpoint responds to POST at `/v1/messages`

### Phase 3: Integration Tests

#### Automated

- [x] 3.1 All new integration tests pass: `cargo test messages_handler`
- [x] 3.2 Full suite green: `cargo test`

### Phase 4: OpenAPI Spec Update

#### Automated

- [ ] 4.1 YAML is valid
- [ ] 4.2 Existing tests still pass: `cargo test`

#### Manual

- [ ] 4.3 Spec is consistent with actual endpoint behavior
