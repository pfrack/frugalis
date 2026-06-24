# Anthropic → OpenAI Protocol Translation Implementation Plan

## Overview

Enhance the existing `POST /v1/messages` endpoint to detect when the routed upstream speaks OpenAI protocol (via `provider_type != "anthropic"`), translate the Anthropic Messages request to OpenAI Chat Completions format, forward it, and translate the response (including SSE streaming) back to Anthropic format. This enables Anthropic-speaking clients (Claude Code, etc.) to use OpenAI-compatible providers (NVIDIA NIM, OpenRouter, Groq, Cerebras, Ollama) through cerebrum without client changes.

## Current State Analysis

- `messages_handler` (`src/main.rs:1399`) handles Anthropic traffic as a pass-through — no translation exists today
- `completion_handler` (`src/main.rs:1166`) handles OpenAI traffic, forwards to upstream with model override and auth headers
- `build_upstream_request` (`src/main.rs:864`) builds the upstream reqwest with model override and auth
- `auth_headers_for` (`src/intent_classifier.rs:430`) already emits `Authorization: Bearer` for `provider_type != "anthropic"` (OpenAI-compatible)
- `RouteEntry` (`src/routing.rs:7`) has `provider_type` field used for auth header resolution
- `handle_streaming_response` (`src/main.rs:985`) handles SSE streaming with keepalive
- `handle_buffered_response` (`src/main.rs:910`) handles non-streaming responses
- No protocol translation exists today — the two handlers are independent pass-through paths

## Desired End State

When a request arrives at `POST /v1/messages` and the classifier routes it to an upstream with `provider_type != "anthropic"` (i.e., OpenAI-compatible):

1. The request body is translated from Anthropic Messages format to OpenAI Chat Completions format
2. Auth headers use `Authorization: Bearer` (already handled by `auth_headers_for`)
3. The upstream receives a valid OpenAI Chat Completions request
4. The response (non-streaming) is translated from OpenAI Chat Completions format back to Anthropic Messages format
5. The response (streaming) is translated from OpenAI SSE chunks to Anthropic SSE events (stateful emitter)
6. Non-2xx upstream errors are translated from OpenAI error shape to Anthropic error envelope

Verification: Unit tests cover all translation edge cases; httpmock e2e tests verify the full request→translate→forward→translate→respond pipeline.

## What We're NOT Doing

- Not translating OpenAI → Anthropic for the `/v1/chat/completions` endpoint (pass-through stays)
- Not supporting Anthropic `top_k` (dropped — no OpenAI equivalent)
- Not supporting `metadata` (dropped)
- Not handling `thinking` request param (dropped — no standard OpenAI field; provider-specific)
- Not adding a new `/v1/openai/messages` endpoint — translation happens inside existing handler
- Not changing the routing config format — `provider_type` already exists
- Not implementing NIM field sanitization (can be added later if needed)

## Implementation Approach

Extend the existing `src/protocol_translation.rs` module (created by sibling plan translate-openai-to-anthropic) with Anthropic → OpenAI translation functions. Wire them into `messages_handler` with a `provider_type != "anthropic"` check. The translation layer sits between the request body parsing and the upstream request building.

## Critical Implementation Details

- **`stream_options.include_usage = true`** — must be set on the OpenAI request when streaming, so usage tokens arrive in the last chunk
- **Post-pass reasoning fix** — if ANY message has `reasoning_content`, ALL assistant messages with `tool_calls` but no reasoning need `reasoning_content: " "` (space) for DeepSeek/Kimi compatibility
- **Streaming state machine** — OpenAI SSE → Anthropic SSE requires a stateful emitter tracking `block_index`, `open_block`, `message_started`, and `tool_state` to properly open/close content blocks
- **`input` → `arguments` conversion** — Anthropic `tool_use.input` (object) must become OpenAI `tool_calls[].function.arguments` (JSON string via `serde_json::to_string`)

---

## Phase 1: Translation Module

### Overview

Add Anthropic → OpenAI translation functions to `src/protocol_translation.rs`. This phase produces pure functions with no side effects — easy to unit test.

### Changes Required:

#### 1. `src/protocol_translation.rs` — New translation functions

**Intent**: Add functions for Anthropic → OpenAI translation, complementing the existing OpenAI → Anthropic functions from the sibling plan.

**Contract**:

- `anthropic_to_openai_request(body: &serde_json::Value) -> Result<serde_json::Value, String>` — takes parsed Anthropic Messages request body, returns OpenAI Chat Completions request body. Handles: system field → system message, message format conversion (user/assistant/tool_result), tool definitions, tool_choice, `stream_options.include_usage = true` when streaming, post-pass reasoning fix.
- `anthropic_to_openai_response(body: &serde_json::Value) -> Result<serde_json::Value, String>` — takes parsed OpenAI Chat Completions response body, returns Anthropic Messages response body. Handles: message → content blocks (thinking → text → tool_use order), finish_reason → stop_reason, usage mapping, `arguments` string → `input` object.
- `anthropic_to_openai_error(body: &str, status: u16) -> String` — takes OpenAI error body and status, returns Anthropic error envelope JSON string.
- `anthropic_to_openai_stream_event(event_type: &str, data: &str, state: &mut AnthropicStreamState) -> Option<String>` — takes an OpenAI SSE event (type + data), maintains streaming state, returns Anthropic SSE event(s) or None. `AnthropicStreamState` tracks `block_index`, `open_block`, `message_started`, `tool_state`.

Message conversion rules (from research doc §1.2):
- System field (string or block array) → prepended `role: "system"` message, block texts joined with `"\n\n"`
- User messages: string content passes through; text blocks joined; image source → image_url; tool_result blocks → separate `role: "tool"` messages
- Assistant messages: text blocks → content string; tool_use blocks → tool_calls array; thinking blocks → reasoning_content; `input` object → `arguments` JSON string
- `redacted_thinking` blocks: always drop

#### 2. Module registration (if not already done by sibling plan)

**Intent**: Ensure `mod protocol_translation;` is declared in `src/main.rs`.

**Contract**: Add `mod protocol_translation;` alongside existing module declarations (line 68-75) if not already present.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- `cargo test protocol_translation` — all unit tests pass
- Unit tests cover: system field (string), system field (block array), user text content, user text blocks joined, user image source, user tool_result → tool messages, assistant text, assistant tool_use, assistant thinking + text, redacted_thinking dropped, tool definitions, tool_choice mapping, post-pass reasoning fix, non-streaming response, response with tool_use, response with thinking, finish_reason mapping, usage mapping, streaming event translation (message_start, content_block_start/stop/delta for text/thinking/tool_use, message_delta, message_stop), error translation

#### Manual Verification:

- Review translation functions against research doc §1-§4 for completeness

---

## Phase 2: Handler Integration

### Overview

Wire the translation layer into `messages_handler`. When `provider_type != "anthropic"`, translate the request before forwarding and translate the response before returning.

### Changes Required:

#### 1. `src/main.rs` — `messages_handler` (line ~1448)

**Intent**: After classification and before `build_upstream_request`, check if `classification.provider_type != "anthropic"`. If so, translate the request body from Anthropic to OpenAI format, then build the upstream request with the translated body.

**Contract**:
- Check `classification.provider_type != "anthropic"` after line ~1448
- If true: call `protocol_translation::anthropic_to_openai_request()` on the parsed body, serialize translated body to bytes, pass to `build_upstream_request` (or a new variant that accepts pre-translated bytes)
- The existing `build_upstream_request` function re-parses the body and overrides `model` — for translated bodies, the translated body already has the correct model set, so we need to either (a) skip the model override in `build_upstream_request` for translated bodies, or (b) build the reqwest request directly without going through `build_upstream_request`

#### 2. `src/main.rs` — Response handling in `messages_handler` (line ~1587)

**Intent**: For OpenAI upstreams, translate the response back to Anthropic format before returning to the client.

**Contract**:
- For non-streaming: after `handle_buffered_response`, if `provider_type != "anthropic"`, parse the response body as OpenAI Chat Completions response, translate to Anthropic format, re-serialize
- For streaming: the `handle_streaming_response` byte stream needs to be intercepted — OpenAI SSE chunks must be translated to Anthropic SSE events before sending to the client. This requires a new streaming handler variant or wrapping the byte stream in a translation layer. The stateful emitter must track block_index, open_block, message_started, tool_state.
- For non-2xx errors: if `provider_type != "anthropic"`, translate the error body from OpenAI shape to Anthropic error envelope

#### 3. `src/main.rs` — Error handling

**Intent**: Translate OpenAI-format error responses to Anthropic error envelope for consistency.

**Contract**: In the non-2xx paths (upstream error, streaming error), if `provider_type != "anthropic"`, call `protocol_translation::anthropic_to_openai_error()` instead of passing through.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- `cargo test` — all existing tests still pass (no regressions)
- `cargo test anthropic_to_openai` — new integration tests pass

#### Manual Verification:

- Send an Anthropic-format request to `/v1/messages` with a route configured for `provider_type: "openai_compatible"`, verify the response is in Anthropic format

---

## Phase 3: Testing

### Overview

Comprehensive test coverage: unit tests for translation edge cases + httpmock e2e tests for the full pipeline.

### Changes Required:

#### 1. Unit tests in `src/protocol_translation.rs`

**Intent**: Cover all translation edge cases from the research doc.

**Contract**:

Request translation tests:
- System field as string → prepended system message
- System field as block array → joined with `"\n\n"`
- User text content passes through
- User text blocks joined
- User image source → image_url format
- User tool_result blocks → separate tool messages
- Assistant text → content string
- Assistant tool_use → tool_calls (input object → arguments JSON string)
- Assistant thinking → reasoning_content
- redacted_thinking dropped
- Tool definitions: `name/description/input_schema` → `function.name/description/parameters`
- Tool choice mapping: auto/any/none/specific tool
- Post-pass reasoning fix: assistant with tool_calls but no reasoning gets `reasoning_content: " "`
- Fields dropped: top_k, metadata, thinking
- stream_options.include_usage set when streaming

Response translation tests:
- Content string → text block
- reasoning_content → thinking block (prepend)
- tool_calls → tool_use blocks (arguments string → input object)
- Finish reason mapping: stop→end_turn, length→max_tokens, tool_calls→tool_use, function_call→tool_use, content_filter→end_turn
- Usage mapping: prompt_tokens→input_tokens, completion_tokens→output_tokens, total_tokens omitted

Streaming translation tests:
- First chunk with delta.role → message_start event
- delta.reasoning_content → thinking content_block_start + content_block_delta
- delta.content → text content_block_start + content_block_delta
- delta.tool_calls[i] (new) → tool_use content_block_start + input_json_delta
- delta.tool_calls[i] (existing) → input_json_delta
- finish_reason → message_delta with stop_reason
- [DONE] → message_stop
- Block transitions: close previous block before opening new one

Error translation tests:
- OpenAI error body → Anthropic error envelope

#### 2. E2E tests with httpmock in `src/main.rs`

**Intent**: Test the full request→translate→forward→translate→respond pipeline through the Axum handler.

**Contract**:
- Mock OpenAI upstream that receives OpenAI-format request and returns OpenAI-format response
- Send Anthropic-format request to `messages_handler`
- Assert response is valid Anthropic Messages format
- Test both streaming and non-streaming paths
- Test error path: mock returns OpenAI error, client receives Anthropic error envelope

### Success Criteria:

#### Automated Verification:

- `cargo test protocol_translation` — all unit tests pass
- `cargo test test_messages_handler_openai_translation` — e2e tests pass
- `cargo test test_messages_handler_openai_streaming` — streaming e2e tests pass
- `cargo test test_messages_handler_openai_error` — error e2e tests pass

#### Manual Verification:

- Review test coverage against research doc edge cases (§4)

---

## Testing Strategy

### Unit Tests:

- All translation functions in `protocol_translation.rs` with hand-crafted JSON
- Edge cases: empty messages, empty tool arguments, missing content blocks, redacted_thinking, post-pass reasoning fix, block transitions in streaming

### Integration Tests:

- httpmock simulating OpenAI upstream
- Full handler pipeline: Axum request → classification → translation → mock upstream → translation → response

### Manual Testing Steps:

1. Configure a route with `provider_type: "openai_compatible"` pointing to a real OpenAI API
2. Send Anthropic-format request via curl to `/v1/messages`
3. Verify response is valid Anthropic format
4. Test streaming with `"stream": true`
5. Test tool use scenario

## References

- Related research: `context/changes/translate-anthropic-to-openai/research.md`
- Sibling plan: `context/changes/translate-openai-to-anthropic/plan.md`
- Auth header logic: `src/intent_classifier.rs:430` (`auth_headers_for`)
- Messages handler: `src/main.rs:1399`
- Completion handler: `src/main.rs:1166` (OpenAI pass-through pattern)
- Route entry type: `src/routing.rs:7`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Translation Module

#### Automated

- [x] 1.1 `cargo build` compiles without errors
- [x] 1.2 `cargo test protocol_translation` — all unit tests pass

#### Manual

- [ ] 1.3 Review translation functions against research doc §1-§4 for completeness

### Phase 2: Handler Integration

#### Automated

- [x] 2.1 `cargo build` compiles without errors
- [x] 2.2 `cargo test` — all existing tests still pass (no regressions)

#### Manual

- [ ] 2.3 Send Anthropic-format request to `/v1/messages` with OpenAI route, verify response is Anthropic format

### Phase 3: Testing

#### Automated

- [x] 3.1 `cargo test protocol_translation` — all unit tests pass
- [x] 3.2 `cargo test test_messages_handler_openai_translation` — e2e tests pass
- [x] 3.3 `cargo test test_messages_handler_openai_streaming` — streaming e2e tests pass
- [x] 3.4 `cargo test test_messages_handler_openai_error` — error e2e tests pass

#### Manual

- [ ] 3.5 Review test coverage against research doc edge cases (§4)
