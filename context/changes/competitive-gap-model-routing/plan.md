# Competitive Gap: Model Routing Compatibility — Implementation Plan

## Overview

Close 4 practical gaps identified in competitive analysis against free-claude-code, ccm, and freedius. Each gap is a small, independent feature that improves Claude Code compatibility and operational robustness without changing cerebrum's intent-routing architecture.

## Current State Analysis

- Proxy routes exist at `/v1/chat/completions`, `/v1/messages`, `/v1/classify`, `/v1/feedback`
- Routes are nested under `/v1` with auth middleware
- `AppState.routing` holds a `HashMap<String, RouteEntry>` with per-category routing (model name, endpoint, provider_type, api_key_env)
- Provider types: `openai_compatible`, `anthropic`, `nvidia_nim`, `ollama`
- Existing `SHORT_PROMPT_LEN` config (default 30) already detects short prompts in the regex classifier

### Key Discoveries:

- `src/main.rs:2436-2439` — route wiring in `build_app()`
- `src/main.rs:83-101` — `AppState` struct with `routing` HashMap available
- `src/main.rs:714` — `health()` handler pattern (simple static response)
- `src/main.rs:1728` — provider_type branching for protocol translation
- `src/intent_classifier.rs:424-470` — `auth_headers_for()` already distinguishes `nvidia_nim`

## Desired End State

1. `GET /v1/models` returns a valid Anthropic-style model list (static, derived from routing config)
2. Requests routed to `nvidia_nim` have unsupported fields stripped before forwarding
3. `POST /v1/messages/count_tokens` returns a local token approximation
4. Known trivial Claude Code probes are short-circuited with canned responses (no upstream call)

Verification: Claude Code connects through cerebrum with `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` without errors; NIM-routed requests don't fail on unsupported fields; latency on trivial probes is <5ms.

## What We're NOT Doing

- Per-tier model routing (Opus/Sonnet/Haiku → different providers) — conflicts with intent-routing paradigm
- Dynamic model discovery from upstream providers — overkill for a static stub
- Full tiktoken/BPE token counting — chars/4 heuristic is sufficient
- OpenAI Responses API (`/v1/responses`) — Codex support out of scope
- Rate limiting — separate concern

## Implementation Approach

Four independent phases, each delivering one gap closure. No dependencies between phases — can be implemented in any order or in parallel.

## Phase 1: `/v1/models` Static Endpoint

### Overview

Add a `GET /v1/models` handler that returns a hardcoded list of Claude model names. This satisfies Claude Code's gateway model discovery without requiring dynamic upstream queries.

### Changes Required:

#### 1. Models handler

**File**: `src/main.rs`

**Intent**: Add a `models_handler` function that returns a static JSON response with model entries. Place it near the `health()` handler for consistency.

**Contract**: `async fn models_handler() -> impl IntoResponse` returning JSON matching:
```json
{
  "data": [
    {"id": "claude-sonnet-4-6-20250514", "object": "model", "created": 1700000000, "owned_by": "anthropic"},
    {"id": "claude-haiku-4-5-20250514", "object": "model", "created": 1700000000, "owned_by": "anthropic"},
    {"id": "claude-opus-4-20250514", "object": "model", "created": 1700000000, "owned_by": "anthropic"}
  ],
  "object": "list",
  "has_more": false
}
```

#### 2. Route wiring

**File**: `src/main.rs`

**Intent**: Wire the models endpoint into the proxy router, **outside** the auth layer — model discovery should be unauthenticated (matches Claude Code's expectation that it can probe before authenticating).

**Contract**: Add `.route("/v1/models", get(models_handler))` to the top-level router (alongside `/health`), not inside the `proxy_routes` nest.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes with new unit test for models_handler
- `cargo clippy` clean

#### Manual Verification:

- `curl http://localhost:10000/v1/models` returns valid JSON with model list
- Claude Code with `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` shows model picker

---

## Phase 2: NIM Field Sanitization

### Overview

Strip fields that NVIDIA NIM rejects before forwarding translated requests. Applies only when `provider_type == "nvidia_nim"`.

### Changes Required:

#### 1. Sanitization function

**File**: `src/main.rs` (near the translation call site)

**Intent**: Add a function that removes known-unsupported fields from the request body when provider is NIM. Called after protocol translation but before forwarding.

**Contract**: `fn sanitize_for_nim(body: &mut serde_json::Value)` — removes keys: `top_k`, `metadata`, `thinking`, `system` (if model doesn't support it — but safer to keep system and only strip the others). Strip list: `["top_k", "metadata", "thinking"]` from the top level of the request object.

#### 2. Call site integration

**File**: `src/main.rs`

**Intent**: Insert sanitization call in both the OpenAI pass-through path and the Anthropic→OpenAI translation path, after body preparation but before `client.post()`.

**Contract**: Guard with `if classification.provider_type == "nvidia_nim" { sanitize_for_nim(&mut body); }` at both translation branches.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes with unit test verifying fields are stripped
- `cargo clippy` clean

#### Manual Verification:

- Request with `top_k` field routed to NIM succeeds (field stripped, no 400 error)

---

## Phase 3: `/v1/messages/count_tokens` Stub

### Overview

Return a local token count approximation so Claude Code's context window management works without hitting upstream.

### Changes Required:

#### 1. Count tokens handler

**File**: `src/main.rs`

**Intent**: Add a handler that extracts the messages array from the request body, concatenates text content, and returns `chars / 4` as the token estimate.

**Contract**: `async fn count_tokens_handler(body: Bytes) -> impl IntoResponse` — parses body as JSON, extracts text from `messages[*].content`, sums character lengths, divides by 4. Returns:
```json
{"input_tokens": <estimated_count>}
```

#### 2. Route wiring

**File**: `src/main.rs`

**Intent**: Wire under the auth-protected proxy routes alongside `/messages`.

**Contract**: `.route("/messages/count_tokens", post(count_tokens_handler))` inside `proxy_routes`.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes with unit test verifying token estimation
- `cargo clippy` clean

#### Manual Verification:

- `curl -X POST http://localhost:10000/v1/messages/count_tokens -H 'Authorization: Bearer ...' -d '{"messages":[{"role":"user","content":"hello world"}]}'` returns `{"input_tokens": 2}`

---

## Phase 4: Request Optimizations (Trivial Probe Short-Circuit)

### Overview

Detect known trivial Claude Code probes and return canned responses without hitting upstream. Saves latency and quota.

### Changes Required:

#### 1. Optimization check function

**File**: `src/main.rs`

**Intent**: Add a function that inspects the request body and returns an optional canned response if the request matches a known trivial pattern. Called early in the handler before classification.

**Contract**: `fn try_optimize_request(body: &[u8]) -> Option<axum::response::Response>` — checks:
- Empty messages array → return empty assistant response
- Single message with content matching known probe patterns (e.g., exact match `"hello"`, `"hi"`, `"test"` when body is tiny) → skip classification, return a minimal response

Returns `None` if the request should proceed normally.

#### 2. Handler integration

**File**: `src/main.rs`

**Intent**: Call the optimization check at the top of both `completion_handler` and `messages_handler`, before classification and upstream routing.

**Contract**: Early return if `try_optimize_request` returns `Some(response)`. Log the optimization hit at `debug!` level.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes with unit test for optimization patterns
- `cargo clippy` clean

#### Manual Verification:

- Request with empty messages array returns instantly without upstream call
- Normal requests still route through classification as before

---

## Testing Strategy

### Unit Tests:

- `models_handler` returns valid JSON with expected structure
- `sanitize_for_nim` removes target fields, preserves others
- `count_tokens_handler` returns correct approximation for various message shapes
- `try_optimize_request` matches known patterns, returns None for normal requests

### Integration Tests:

- Full request flow through proxy with NIM provider doesn't fail on `top_k`
- Models endpoint accessible without auth

### Manual Testing Steps:

1. Start cerebrum with NIM routing config
2. Send request with `top_k` field — verify no 400 from NIM
3. Verify `/v1/models` returns model list without auth
4. Verify `/v1/messages/count_tokens` returns reasonable token estimate
5. Connect Claude Code with gateway discovery enabled

## Performance Considerations

- `/v1/models` is a static response — zero allocation beyond JSON serialization (could be `&'static str`)
- NIM sanitization is O(n) field removal on a small object — negligible
- `count_tokens` avoids tokenizer dependencies — chars/4 is ~1μs
- Request optimizations save full upstream round-trips on trivial probes

## References

- Related research: `context/changes/competitive-gap-model-routing/research.md`
- Existing route structure: `src/main.rs:2432-2465`
- Provider auth handling: `src/intent_classifier.rs:424-470`
- FCC model listing: `providers/model_listing.py` in free-claude-code repo
- FCC request optimizations: `api/` module in free-claude-code repo

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: /v1/models Static Endpoint

#### Automated

- [x] 1.1 cargo test passes with models_handler unit test — 53c6b33
- [x] 1.2 cargo clippy clean — 53c6b33

#### Manual

- [ ] 1.3 curl /v1/models returns valid JSON model list
- [ ] 1.4 Claude Code model picker works with gateway discovery

### Phase 2: NIM Field Sanitization

#### Automated

- [x] 2.1 cargo test passes with sanitize_for_nim unit test — b6ec082
- [x] 2.2 cargo clippy clean — b6ec082

#### Manual

- [ ] 2.3 Request with top_k to NIM provider succeeds

### Phase 3: /v1/messages/count_tokens Stub

#### Automated

- [x] 3.1 cargo test passes with count_tokens unit test — 5c2f3d9
- [x] 3.2 cargo clippy clean — 5c2f3d9

#### Manual

- [ ] 3.3 curl count_tokens returns reasonable estimate

### Phase 4: Request Optimizations

#### Automated

- [x] 4.1 cargo test passes with try_optimize_request unit test
- [x] 4.2 cargo clippy clean

#### Manual

- [ ] 4.3 Empty messages array returns instantly without upstream call
- [ ] 4.4 Normal requests still route through classification
