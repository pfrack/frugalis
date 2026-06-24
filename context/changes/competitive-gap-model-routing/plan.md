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

```bash
# 1. Model discovery — no auth required
curl -s http://localhost:10000/v1/models | jq .

# Expected: JSON with "data" array containing claude-sonnet-4, claude-haiku-4-5, claude-opus-4
# Each entry has "id", "object": "model", "owned_by": "anthropic"

# 2. Verify has_more is false
curl -s http://localhost:10000/v1/models | jq '.has_more'
# Expected: false

# 3. Verify model count
curl -s http://localhost:10000/v1/models | jq '.data | length'
# Expected: 3

# 4. Claude Code integration test
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 claude --model http://localhost:10000
# Expected: model picker appears with the 3 models listed
```

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

```bash
# 1. Send request with top_k field to an NIM-routed category
# (replace CATEGORY with a category that routes to nvidia_nim provider)
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "CATEGORY",
    "messages": [{"role": "user", "content": "hello"}],
    "top_k": 50,
    "metadata": {"key": "value"},
    "thinking": {"type": "enabled"}
  }'

# Expected: 200 OK (not 400). Fields are silently stripped before forwarding.

# 2. Verify the request succeeds without top_k (baseline sanity check)
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "CATEGORY",
    "messages": [{"role": "user", "content": "hello"}]
  }'

# Expected: 200 OK — same as above, confirming no regression
```

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

```bash
# 1. Simple text — expect ~3 tokens (12 chars / 4)
curl -s -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"hello world!"}]}'

# Expected: {"input_tokens":3}

# 2. Longer text — expect ~31 tokens (~124 chars)
curl -s -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Write a function that calculates the fibonacci sequence up to n terms using dynamic programming in Rust."}]}'

# Expected: {"input_tokens":26} (104 chars / 4)

# 3. Multiple messages — both concatenated
curl -s -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"system","content":"You are a helpful assistant."},{"role":"user","content":"hello world!"}]}'

# Expected: {"input_tokens":10} (40 chars total / 4)

# 4. Content blocks format (Anthropic-style array)
curl -s -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":[{"type":"text","text":"hello world!"}]}]}'

# Expected: {"input_tokens":3}

# 5. Missing auth — expect 401
curl -s -w "\nHTTP %{http_code}" -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"hello"}]}'

# Expected: 401 Unauthorized
```

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

```bash
# 1. Empty messages — expect instant empty assistant response (no upstream call)
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[]}'

# Expected: instant 200 with choices[0].message.content="" and usage all zeros

# 2. Known probe "hello" — expect instant canned response
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"user","content":"hello"}]}'

# Expected: instant 200 with "Hi! How can I help you today?"

# 3. Known probe "hi" — same pattern
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"user","content":"hi"}]}'

# Expected: instant 200 with "Hi! How can I help you today?"

# 4. Known probe "test" — same pattern
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"user","content":"test"}]}'

# Expected: instant 200 with "Hi! How can I help you today?"

# 5. Anthropic-format probe via /v1/messages
curl -s -X POST http://localhost:10000/v1/messages \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -d '{"model":"test","messages":[{"role":"user","content":"hello"}],"max_tokens":1024}'

# Expected: instant 200 with Anthropic-format response (content[0].text = "Hi! How can I help you today?")

# 6. Normal request — verify it still routes through classification (NOT optimized)
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"user","content":"Explain quantum computing in detail"}]}'

# Expected: goes to upstream (not instant, not canned) — verify by checking latency and real response

# 7. Multi-message request — NOT a probe, should route normally
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"hello"}]}'

# Expected: routes through classification (multi-message doesn't match probe pattern)
```

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

```bash
# Start cerebrum with your config
cargo run --release

# Set your auth token
export PROXY_API_BEARER_TOKEN="your-token-here"

# 1. Model discovery (Phase 1)
curl -s http://localhost:10000/v1/models | jq .
# Expected: 3 models listed

# 2. NIM field stripping (Phase 2)
# Requires a category configured with provider_type: nvidia_nim
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"NIM_CATEGORY","messages":[{"role":"user","content":"hello"}],"top_k":50,"metadata":{}}'
# Expected: 200 OK, not 400

# 3. Token counting (Phase 3)
curl -s -X POST http://localhost:10000/v1/messages/count_tokens \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"hello world!"}]}'
# Expected: {"input_tokens":3}

# 4. Probe optimization (Phase 4)
curl -s -X POST http://localhost:10000/v1/chat/completions \
  -H "Authorization: Bearer ${PROXY_API_BEARER_TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"model":"test","messages":[{"role":"user","content":"hello"}]}'
# Expected: instant "Hi! How can I help you today?"

# 5. Claude Code integration (Phase 1 + all)
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 claude --model http://localhost:10000
# Expected: model picker, no errors, normal conversation works
```

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

- [x] 1.3 curl /v1/models returns valid JSON model list — 53c6b33
- [x] 1.4 Claude Code model picker works with gateway discovery — 53c6b33

### Phase 2: NIM Field Sanitization

#### Automated

- [x] 2.1 cargo test passes with sanitize_for_nim unit test — b6ec082
- [x] 2.2 cargo clippy clean — b6ec082

#### Manual

- [x] 2.3 Request with top_k to NIM provider succeeds — b6ec082

### Phase 3: /v1/messages/count_tokens Stub

#### Automated

- [x] 3.1 cargo test passes with count_tokens unit test — 5c2f3d9
- [x] 3.2 cargo clippy clean — 5c2f3d9

#### Manual

- [x] 3.3 curl count_tokens returns reasonable estimate — 5c2f3d9

### Phase 4: Request Optimizations

#### Automated

- [x] 4.1 cargo test passes with try_optimize_request unit test — ecf26b2
- [x] 4.2 cargo clippy clean — ecf26b2

#### Manual

- [x] 4.3 Empty messages array returns instantly without upstream call — ecf26b2
- [x] 4.4 Normal requests still route through classification — ecf26b2
