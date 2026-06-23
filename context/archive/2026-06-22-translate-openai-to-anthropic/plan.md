# OpenAI → Anthropic Protocol Translation Implementation Plan

## Overview

Enhance the existing `POST /v1/chat/completions` endpoint to detect when the routed upstream speaks Anthropic protocol (via `provider_type == "anthropic"`), translate the OpenAI Chat Completions request to Anthropic Messages format, forward it, and translate the response (including SSE streaming) back to OpenAI format. This enables OpenAI-speaking clients to use Anthropic-compatible providers (Claude API, DeepSeek /anthropic, Kimi, Z.ai, Fireworks) through cerebrum without client changes.

## Current State Analysis

- `completion_handler` (`src/main.rs:1166`) handles OpenAI traffic, forwards to upstream with model override and auth headers
- `messages_handler` (`src/main.rs:1399`) is a pass-through for Anthropic traffic (no translation)
- `build_upstream_request` (`src/main.rs:864`) builds the upstream reqwest with model override and auth
- `auth_headers_for` (`src/intent_classifier.rs:430`) already emits `x-api-key` + `anthropic-version: 2023-06-01` for `provider_type == "anthropic"`
- `RouteEntry` (`src/routing.rs:7`) has `provider_type` field used for auth header resolution
- `handle_streaming_response` (`src/main.rs:985`) handles SSE streaming with keepalive
- `handle_buffered_response` (`src/main.rs:910`) handles non-streaming responses
- No protocol translation exists today — the two handlers are independent pass-through paths

## Desired End State

When a request arrives at `POST /v1/chat/completions` and the classifier routes it to an upstream with `provider_type == "anthropic"`:

1. The request body is translated from OpenAI Chat Completions format to Anthropic Messages format
2. Auth headers use `x-api-key` + `anthropic-version` (already handled by `auth_headers_for`)
3. The upstream receives a valid Anthropic Messages request
4. The response (non-streaming) is translated from Anthropic Messages format back to OpenAI Chat Completions format
5. The response (streaming) is translated from Anthropic SSE events to OpenAI SSE chunks
6. Non-2xx upstream errors are translated from Anthropic error shape to OpenAI error envelope

Verification: Unit tests cover all translation edge cases; httpmock e2e tests verify the full request→translate→forward→translate→respond pipeline.

## What We're NOT Doing

- Not translating Anthropic → OpenAI for the `/v1/messages` endpoint (pass-through stays)
- Not supporting OpenAI `n > 1` (dropped — Anthropic doesn't support it)
- Not supporting `response_format` (dropped — Anthropic equivalent is different)
- Not translating `logprobs`, `logit_bias`, `seed`, `frequency_penalty`, `presence_penalty` (dropped)
- Not adding a new `/v1/anthropic/chat/completions` endpoint — translation happens inside existing handler
- Not changing the routing config format — `provider_type` already exists

## Implementation Approach

Create a new `src/protocol_translation.rs` module with pure translation functions. Wire it into `completion_handler` with a `provider_type == "anthropic"` check. The translation layer sits between the request body parsing and the upstream request building.

## Critical Implementation Details

- **`max_tokens` is required by Anthropic** — default to 4096 if absent in the OpenAI request
- **Message alternation** — consecutive `role: "tool"` messages must be merged into a single `role: "user"` message with multiple `tool_result` blocks
- **`arguments` parsing** — OpenAI `tool_calls[].function.arguments` is a JSON string; Anthropic `tool_use[].input` is a parsed object. Must `serde_json::from_str`; on malformed JSON, pass as `{"raw": "..."}`

---

## Phase 1: Translation Module

### Overview

Create `src/protocol_translation.rs` with all translation functions. This phase produces pure functions with no side effects — easy to unit test.

### Changes Required:

#### 1. New file: `src/protocol_translation.rs`

**Intent**: Create a new module containing all OpenAI ↔ Anthropic translation functions.

**Contract**:

- `translate_request(body: &serde_json::Value) -> Result<serde_json::Value, String>` — takes parsed OpenAI Chat Completions request body, returns Anthropic Messages request body. Handles: system message extraction, message format conversion (user/assistant/tool), tool definitions, tool_choice, max_tokens default.
- `translate_response(body: &serde_json::Value) -> Result<serde_json::Value, String>` — takes parsed Anthropic Messages response body, returns OpenAI Chat Completions response body. Handles: content blocks → message fields, stop_reason → finish_reason, usage mapping, tool_use → tool_calls.
- `translate_error(body: &str, status: u16) -> String` — takes Anthropic error body and status, returns OpenAI error envelope JSON string.
- `translate_stream_event(event_type: &str, data: &str, state: &mut StreamTranslateState) -> Option<String>` — takes an Anthropic SSE event (type + data), maintains streaming state, returns OpenAI SSE chunk(s) or None. `StreamTranslateState` tracks `chunk_id`, `model`, `tool_index`.
- `parse_sse_events(bytes: &[u8]) -> Vec<(String, String)>` — parses raw SSE bytes into (event_type, data) pairs.

Message conversion rules (from research doc §1.2):
- System messages → extracted to top-level `system` field, joined with `"\n\n"`
- User messages: string content → array of content blocks; image_url → image source
- Assistant messages: string → text block; tool_calls → tool_use blocks; reasoning_content → thinking block
- Tool results: consecutive `role: "tool"` messages merged into single `role: "user"` with multiple `tool_result` blocks
- Anthropic strict alternation enforced: no consecutive same-role messages

#### 2. Module registration: `src/main.rs`

**Intent**: Add `mod protocol_translation;` to the module declarations.

**Contract**: Add `mod protocol_translation;` alongside existing module declarations (line 68-75).

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- `cargo test protocol_translation` — all unit tests pass
- Unit tests cover: basic text request, system message extraction, tool definitions, tool_choice mapping, assistant with tool_calls, reasoning_content, consecutive tool messages merged, image content, max_tokens default, non-streaming response, response with tool_use, response with thinking, stop_reason mapping, error translation, streaming event translation

#### Manual Verification:

- Review translation functions against research doc §1-§4 for completeness

---

## Phase 2: Handler Integration

### Overview

Wire the translation layer into `completion_handler`. When `provider_type == "anthropic"`, translate the request before forwarding and translate the response before returning.

### Changes Required:

#### 1. `src/main.rs` — `completion_handler` (line ~1297)

**Intent**: After classification and before `build_upstream_request`, check if `classification.provider_type == "anthropic"`. If so, translate the request body from OpenAI to Anthropic format, then build the upstream request with the translated body.

**Contract**:
- Check `classification.provider_type == "anthropic"` after line ~1297
- If true: call `protocol_translation::translate_request()` on the parsed body, serialize translated body to bytes, pass to `build_upstream_request` (or a new variant that accepts pre-translated bytes)
- The existing `build_upstream_request` function currently re-parses the body and overrides `model` — for Anthropic translation, the translated body already has the correct model set, so we need to either (a) skip the model override in `build_upstream_request` for translated bodies, or (b) build the reqwest request directly without going through `build_upstream_request`

#### 2. `src/main.rs` — Response handling in `completion_handler` (line ~1349)

**Intent**: For Anthropic upstreams, translate the response back to OpenAI format before returning to the client.

**Contract**:
- For non-streaming: after `handle_buffered_response`, if `provider_type == "anthropic"`, parse the response body as Anthropic Messages response, translate to OpenAI format, re-serialize
- For streaming: the `handle_streaming_response` byte stream needs to be intercepted — Anthropic SSE events must be translated to OpenAI SSE chunks before sending to the client. This requires a new streaming handler variant or wrapping the byte stream in a translation layer
- For non-2xx errors: if `provider_type == "anthropic"`, translate the error body from Anthropic shape to OpenAI error envelope

#### 3. `src/main.rs` — Error handling

**Intent**: Translate Anthropic-format error responses to OpenAI error envelope for consistency.

**Contract**: In the non-2xx paths (upstream error, streaming error), if `provider_type == "anthropic"`, call `protocol_translation::translate_error()` instead of `upstream_error_json()`.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- `cargo test` — all existing tests still pass (no regressions)
- `cargo test translation` — new integration tests pass

#### Manual Verification:

- Send an OpenAI-format request to `/v1/chat/completions` with a route configured for `provider_type: "anthropic"`, verify the response is in OpenAI format

---

## Phase 3: Testing

### Overview

Comprehensive test coverage: unit tests for translation edge cases + httpmock e2e tests for the full pipeline.

### Changes Required:

#### 1. Unit tests in `src/protocol_translation.rs`

**Intent**: Cover all translation edge cases from the research doc.

**Contract**:

Request translation tests:
- Basic text message → system extraction + user message conversion
- Multiple system messages joined with `"\n\n"`
- User message with image_url → image content block
- Assistant with tool_calls → tool_use blocks (arguments JSON string → input object)
- Assistant with reasoning_content → thinking + text blocks
- Consecutive tool messages merged into single user message
- Tool definitions: `function.name/description/parameters` → `name/description/input_schema`
- Tool choice mapping: auto/none/required/specific function
- max_tokens default to 4096 when absent
- Fields dropped: n, frequency_penalty, presence_penalty, logprobs, logit_bias, seed, response_format, stream_options

Response translation tests:
- Text content blocks → concatenated message.content
- Thinking content → reasoning_content
- Tool use blocks → tool_calls array
- Stop reason mapping: end_turn→stop, max_tokens→length, tool_use→tool_calls, stop_sequence→stop
- Usage mapping: input_tokens→prompt_tokens, output_tokens→completion_tokens, total_tokens computed
- Redacted thinking blocks omitted

Streaming translation tests:
- message_start → role chunk
- content_block_start (tool_use) → tool_calls index chunk
- content_block_delta (text_delta) → content delta
- content_block_delta (thinking_delta) → reasoning_content delta
- content_block_delta (input_json_delta) → tool_calls arguments delta
- message_delta (stop_reason) → finish_reason chunk
- message_delta (usage) → usage chunk
- message_stop → `[DONE]`

Error translation tests:
- Anthropic error body → OpenAI error envelope

#### 2. E2E tests with httpmock in `src/main.rs`

**Intent**: Test the full request→translate→forward→translate→respond pipeline through the Axum handler.

**Contract**:
- Mock Anthropic upstream that receives Anthropic-format request and returns Anthropic-format response
- Send OpenAI-format request to `completion_handler`
- Assert response is valid OpenAI Chat Completions format
- Test both streaming and non-streaming paths
- Test error path: mock returns Anthropic error, client receives OpenAI error envelope

### Success Criteria:

#### Automated Verification:

- `cargo test protocol_translation` — all unit tests pass
- `cargo test test_completion_handler_anthropic_translation` — e2e tests pass
- `cargo test test_completion_handler_anthropic_streaming` — streaming e2e tests pass
- `cargo test test_completion_handler_anthropic_error` — error e2e tests pass

#### Manual Verification:

- Review test coverage against research doc edge cases (§4)

---

## Testing Strategy

### Unit Tests:

- All translation functions in `protocol_translation.rs` with hand-crafted JSON
- Edge cases: empty messages, malformed tool_call arguments, missing max_tokens, consecutive tool messages, image content

### Integration Tests:

- httpmock simulating Anthropic upstream
- Full handler pipeline: Axum request → classification → translation → mock upstream → translation → response

### Manual Testing Steps:

1. Configure a route with `provider_type: "anthropic"` pointing to a real Anthropic API
2. Send OpenAI-format request via curl to `/v1/chat/completions`
3. Verify response is valid OpenAI format
4. Test streaming with `"stream": true`
5. Test tool use scenario

## References

- Related research: `context/changes/translate-openai-to-anthropic/research.md`
- Auth header logic: `src/intent_classifier.rs:430` (`auth_headers_for`)
- Completion handler: `src/main.rs:1166`
- Messages handler: `src/main.rs:1399` (Anthropic pass-through pattern)
- Route entry type: `src/routing.rs:7`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Translation Module

#### Automated

- [x] 1.1 `cargo build` compiles without errors — 16b3527
- [x] 1.2 `cargo test protocol_translation` — all unit tests pass — 16b3527

#### Manual

- [x] 1.3 Review translation functions against research doc §1-§4 for completeness — 16b3527

### Phase 2: Handler Integration

#### Automated

- [x] 2.1 `cargo build` compiles without errors — 28879b3
- [x] 2.2 `cargo test` — all existing tests still pass (no regressions) — 28879b3

#### Manual

- [x] 2.3 Send OpenAI-format request to `/v1/chat/completions` with Anthropic route, verify response is OpenAI format — 28879b3

### Phase 3: Testing

#### Automated

- [x] 3.1 `cargo test protocol_translation` — all unit tests pass — b8bbf08
- [x] 3.2 `cargo test test_completion_handler_anthropic_translation` — e2e tests pass — b8bbf08
- [x] 3.3 `cargo test test_completion_handler_anthropic_streaming` — streaming e2e tests pass — b8bbf08
- [x] 3.4 `cargo test test_completion_handler_anthropic_error` — error e2e tests pass — b8bbf08

#### Manual

- [x] 3.5 Review test coverage against research doc edge cases (§4) — b8bbf08
