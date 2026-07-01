# Proxy Translation Contract Tests — Implementation Plan

## Overview

Add handler-level integration tests that verify protocol translation correctness by asserting structural invariants on translated request/response bodies, auth headers, and SSE event sequences. Tests exercise the full handler pipeline through `test_app()` + `httpmock` with known-good reference outputs.

## Current State Analysis

**Existing protocol unit tests**: 118 tests across `src/protocol/` cover individual translation functions adequately.

**Existing handler integration tests**: 13 tests exercise the Anthropic provider type through `test_app_with_anthropic_http_client` (`src/app/test_helpers.rs:187`). These assert HTTP status, content-type, substring presence — verifying "the pipeline didn't crash" rather than "the pipeline produced the correct shape."

**Test harness inventory**:
| Harness | Provider Type | httpmock Path | Covers |
|---|---|---|---|
| `test_app_with_http_client` (`test_helpers.rs:114`) | openai_compatible | `/v1/chat/completions` | OpenAI Chat proxy |
| `test_app_with_anthropic_http_client` (`test_helpers.rs:187`) | anthropic | `/v1/messages` | Anthropic passthrough + translation |
| `test_app_with_cache` (`test_helpers.rs:355`) | openai_compatible | `/v1/chat/completions` | Cache |
| `test_app_with_openai_responses_http_client` (`test_helpers.rs:457`) | openai_responses | `/v1/responses` | R5 passthrough |
| _missing_ | nvidia_nim | — | — |
| _missing_ | ollama | — | — |
| _missing_ | local | — | — |

**Key untested surfaces** (from `research.md`):
- `completion_handler` Anthropic branch (`handlers.rs:407`): 5 existing tests, none assert translated body shape
- `messages_handler` translation path (`handlers.rs:1218`, `needs_translation = true`): 1 existing test, substring-only
- `handle_responses_anthropic_streaming_response` (`responses_streaming.rs:164`): zero tests at any level
- Streaming state-machine gaps: no full tool_use stream test, no multi-block (thinking→tool→text) test, no usage-only chunk test

### Key Discoveries

- `test_app_with_anthropic_http_client` (`src/app/test_helpers.rs:187`) is a complete template for adding new provider-type harnesses — copy, change endpoint path and provider_type string
- `auth_headers_for` in `src/classification/llm.rs:251` is the single emission point for all auth headers; its provider-type dispatch is the key contract surface
- Anthropic streaming response (`handle_anthropic_streaming_response` at `streaming.rs:207`) already has an integration test (`handlers.rs:2938`) that can be extended with structural assertions
- `sanitize_for_nim` in `src/proxy/util.rs:29` strips `top_k`, `metadata`, `thinking` from request bodies — the only body modification for non-Anthropic provider types

## Desired End State

Every handler-level translation path has at least one integration test that asserts structural invariants on the translated output:
- Field presence (required fields exist)
- Field types (values are correct JSON types)
- Field absence (provider-type-specific fields don't leak into wrong-protocol output)
- Mapping correctness (stop_reason → finish_reason, tool_use → tool_calls, etc.)
- Auth header correctness (correct header names and values per provider type)

The `handle_responses_anthropic_streaming_response` two-stage pipeline has a dedicated integration test feeding known Anthropic SSE sequences and asserting the final Responses SSE event structure.

All 5 provider types have at least one round-trip test through their respective handler path.

## What We're NOT Doing

- NOT adding unit tests on protocol translation functions — 118 already exist
- NOT byte-for-byte reference fixtures — asserts structural invariants, not exact body bytes
- NOT testing cache interaction with translation (no Anthropic+cache harness)
- NOT testing classifier chain routing — that is test plan Phase 2
- NOT testing persistence or snippet extraction — that is test plan Phase 3
- NOT adding CI wiring or cookbook updates — that is test plan Phase 4

## Implementation Approach

Three phases ordered by risk priority:

1. **New harnesses** — create missing provider-type harnesses first so all subsequent tests have the scaffolding
2. **Anthropic + openai_compatible body-contract tests** — highest-risk types with most complex translation; exercise all three handlers
3. **Remaining provider types + Responses two-stage streaming** — nvidia_nim, ollama, local passthrough contracts + dedicated Responses streaming sub-phase

Tests live inline in `src/proxy/handlers.rs`, `src/proxy/streaming.rs`, and `src/proxy/responses_handler.rs` per the AGENTS.md convention. Test harness variants that are reusable across multiple test files go in `src/app/test_helpers.rs`.

## Phase 1: Provider-Type Test Harnesses

### Overview

Create missing harness functions for `nvidia_nim` and `ollama` provider types. (No harness needed for `local` — it has no endpoint and no auth headers; covered by `test_app_with_classifier` which returns classification-only JSON.)

### Changes Required

#### 1. New harness: `test_app_with_nim_http_client`

**File**: `src/app/test_helpers.rs`

**Intent**: Create a harness matching the pattern of `test_app_with_anthropic_http_client` (lines 187-258) but with `provider_type = "nvidia_nim"` and mock on `/v1/chat/completions`. NIM strips `top_k`, `metadata`, `thinking` from request bodies via `sanitize_for_nim` — the mock server URL must be `/v1/chat/completions` (NIM speaks OpenAI protocol).

**Contract**: `pub fn test_app_with_nim_http_client(env_var_name: &str, max_upstream_body_bytes: usize) -> (Router, httpmock::MockServer)`. Returns axum Router + httpmock MockServer. Routing entries: `SYNTAX_FIX` and `CASUAL` → `provider_type = "nvidia_nim"`.

#### 2. New harness: `test_app_with_ollama_http_client`

**File**: `src/app/test_helpers.rs`

**Intent**: Create a harness matching the pattern of `test_app_with_anthropic_http_client` but with `provider_type = "ollama"`. Ollama emits no auth headers (`auth_headers_for` returns empty vec for `local`/`ollama` types). Mock on `/v1/chat/completions`.

**Contract**: `pub fn test_app_with_ollama_http_client(env_var_name: &str, max_upstream_body_bytes: usize) -> (Router, httpmock::MockServer)`. Routing entries: `SYNTAX_FIX` and `CASUAL` → `provider_type = "ollama"`.

### Success Criteria

#### Automated Verification

- `test_app_with_nim_http_client` compiles and returns a functional Router
- `test_app_with_ollama_http_client` compiles and returns a functional Router
- Existing tests using other harnesses continue to pass: `cargo test`

#### Manual Verification

- New harnesses follow the exact field layout of `test_app_with_anthropic_http_client` (same AppState construction, same auth config)
- Both new harnesses produce routing entries with the correct provider_type string

---

## Phase 2: Anthropic + OpenAI-Compatible Body-Contract Tests

### Overview

Add structural-invariant assertions to handler integration tests for the two highest-risk provider types. Extend existing tests where possible; add new tests where gaps exist. Tests assert: field presence, field types, field absence (protocol-specific fields don't leak), and mapping correctness (stop_reason → finish_reason, tool_use → tool_calls).

### Changes Required

#### 1. Extend `completion_handler` Anthropic non-streaming test

**File**: `src/proxy/handlers.rs`

**Intent**: Extend `test_completion_handler_anthropic_translation` (line 2830) to assert the full translated response body shape — not just status + substring. The test already sends an OpenAI Chat request through the Anthropic translation path and receives a mock Anthropic response translated back to OpenAI.

**Contract**: Add assertions on the response JSON body after parsing with `parse_json_body`:
- `json["object"] == "chat.completion"`
- `json["choices"][0]["message"]["content"]` is a string, non-empty
- `json["choices"][0]["message"]["role"] == "assistant"`
- `json["usage"]["prompt_tokens"]` is a number
- `json["usage"]["completion_tokens"]` is a number
- `json["usage"]["total_tokens"]` is a number
- `json["model"]` is a string, non-empty
- `json["id"]` is a string, starts with `"chatcmpl-"` or is non-empty
- No Anthropic-only fields: `json["type"]` absent, `json["stop_reason"]` absent

#### 2. Extend `completion_handler` Anthropic streaming test

**File**: `src/proxy/handlers.rs`

**Intent**: Extend `test_completion_handler_anthropic_streaming` (line 2938) to assert SSE event structure across the full sequence. The existing test sends a text-only stream; add a variant with tool_use blocks and thinking blocks.

**Contract**: Add one new test `test_completion_handler_anthropic_streaming_full_sequence` that feeds a full Anthropic SSE sequence through httpmock: `message_start` → `content_block_start(text)` → `content_block_delta(text)`×2 → `content_block_stop` → `content_block_start(thinking)` → `content_block_delta(thinking)` → `content_block_stop` → `content_block_start(tool_use + id/name)` → `content_block_delta(input_json)` → `content_block_stop` → `message_delta(stop_reason="end_turn" + usage)` → `message_stop`. Assert on the collected output body:
- Contains role chunk: `"delta":{"role":"assistant"}`
- Contains text content: `"delta":{"content":"`
- Contains reasoning: `"delta":{"reasoning_content":"`
- Contains tool call: `"delta":{"tool_calls":[` with `"function":{"name":"`, `"arguments":"`
- Contains `"finish_reason":"stop"` (mapped from `end_turn`)
- Contains usage: `"usage":{"prompt_tokens":` , `"completion_tokens":` , `"total_tokens":`
- Ends with `data: [DONE]`
- No Anthropic event types leak (no `event: message_start`, no `event: content_block_delta`)

#### 3. New test: `completion_handler` Anthropic buffered tool_use response

**File**: `src/proxy/handlers.rs`

**Intent**: Verify that an Anthropic response containing a `tool_use` content block translates correctly to the OpenAI `tool_calls` shape.

**Contract**: New test using `test_app_with_anthropic_http_client`. Mock upstream returns Anthropic response with a `tool_use` block (`type: "tool_use"`, `id`, `name`, `input`). Assert response:
- `json["choices"][0]["message"]["tool_calls"][0]["id"]` present
- `json["choices"][0]["message"]["tool_calls"][0]["function"]["name"]` present
- `json["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]` present (JSON string)
- `json["choices"][0]["finish_reason"] == "tool_calls"` (mapped from `stop_reason: "tool_use"`)

#### 4. Extend `messages_handler` translation test

**File**: `src/proxy/handlers.rs`

**Intent**: Extend `test_messages_handler_openai_translation_streaming` (line 3151) with structural-invariant assertions. Add a new buffered (non-streaming) test for the same translation path.

**Contract** — extend existing streaming test assertions:
- Output starts with `event: message_start` with `data: {"type":"message_start","message":{`
- Output contains `event: content_block_start` with `"content_block":{"type":"text"`
- Output contains `event: content_block_delta` with `"delta":{"type":"text_delta","text":"`
- Output contains `event: message_delta` with `"delta":{"stop_reason":"end_turn"`
- Output ends with `event: message_stop`

**Contract** — new buffered test `test_messages_handler_openai_translation_buffered`:
- Parse response body as JSON
- `json["type"] == "message"`
- `json["role"] == "assistant"`
- `json["content"]` is array, non-empty
- `json["content"][0]["type"] == "text"`
- `json["usage"]["input_tokens"]` is number
- `json["usage"]["output_tokens"]` is number
- `json["stop_reason"]` present, non-empty
- No OpenAI-only fields: `json["object"]` absent, `json["choices"]` absent

#### 5. New test: `messages_handler` Anthropic request translation asserts body shape

**File**: `src/proxy/handlers.rs`

**Intent**: Verify that when `needs_translation = true`, the Anthropic Messages request body is correctly translated to OpenAI Chat format before forwarding.

**Contract**: New test using a variant of `test_app_with_openai_translation`. Use httpmock to capture the forwarded body via a mock that checks the request. Assert on the captured request body:
- `captured["messages"]` is an array
- Each message has `"role"` and `"content"` fields
- `captured["system"]` present when the Anthropic input had a top-level `system` array (stringified)
- `captured["max_tokens"]` present
- Anthropic cache_control NOT present in `captured` body (was stripped)

#### 6. New test: `completion_handler` openai_compatible body contract

**File**: `src/proxy/handlers.rs`

**Intent**: The existing openai_compatible tests verify streaming passthrough but not buffered response shape. Add a test verifying the response body shape for the buffered (non-streaming) OpenAI passthrough path.

**Contract**: New test `test_completion_handler_openai_buffered_response_shape` using `test_app_with_http_client`. Mock returns standard OpenAI Chat response. Assert:
- `json["object"] == "chat.completion"`
- `json["choices"]` is array, non-empty
- `json["choices"][0]["message"]["role"] == "assistant"`
- `json["usage"]["prompt_tokens"]`, `completion_tokens`, `total_tokens` all numbers
- `json["model"]` present

### Success Criteria

#### Automated Verification

- All new and extended tests pass: `cargo test`
- Tests for `completion_handler` Anthropic: buffered response shape, streaming full sequence, tool_use→tool_calls mapping
- Tests for `messages_handler` translation: buffered response shape, request body translation
- Test for `completion_handler` openai_compatible: buffered response shape

#### Manual Verification

- Structural assertions do not break when upstream model names or IDs change (no hardcoded model IDs in assertions)
- Streaming assertions work with httpmock delivering the full body in a single chunk (per historical impl-review F1 finding — multi-chunk is a known gap, not blocking this phase)

---

## Phase 3: Remaining Provider Types + Full Responses Coverage

### Overview

Add passthrough contract tests for `nvidia_nim` and `ollama` provider types. Add full `/v1/responses` coverage: structural assertions on existing R1/R2 tests, the 5 missing F4 tests from `codex-responses-api/follow-ups/review-fixes.md`, and the two-stage streaming + tool_use tests for the Responses Anthropic path.

### Changes Required

#### 1. Structural assertions on existing responses_handler tests

**File**: `src/proxy/responses_handler.rs`

**Intent**: The 4 existing responses_handler tests (R1 non-streaming line 764, R1 streaming line 808, R2 non-streaming line 858, R2 streaming line 901) verify HTTP status and substring presence but not structural invariants. Add body-shape assertions to each.

**Contract** — R1 non-streaming (`test_responses_handler_openai_non_streaming`):
- `json["object"] == "response"`
- `json["id"]` starts with `"resp_"`
- `json["status"]` is `"completed"` or `"incomplete"`
- `json["output"]` is array, non-empty
- `json["output"][0]["type"] == "message"`
- `json["output"][0]["role"] == "assistant"`
- `json["output"][0]["content"]` is array
- `json["output"][0]["content"][0]["type"] == "output_text"`
- `json["usage"]["input_tokens"]` is number
- `json["usage"]["output_tokens"]` is number

**Contract** — R1 streaming (`test_responses_handler_openai_streaming`):
- Contains `response.created` event with `sequence_number` field
- Contains `response.output_item.added` with `"type":"message"`
- Contains `response.content_part.added` with `"type":"output_text"`
- Contains `response.output_text.delta` with `"delta"` containing text
- Final event is `response.completed` with `"status":"completed"` and `"usage"`

**Contract** — R2 non-streaming (`test_responses_handler_anthropic_non_streaming`): same as R1 non-streaming above.

**Contract** — R2 streaming (`test_responses_handler_anthropic_streaming`): same event-structure assertions as R1 streaming above.

#### 2. New test: `completion_handler` nvidia_nim passthrough

**File**: `src/proxy/handlers.rs`

**Intent**: Verify that a request routed to an nvidia_nim provider type (a) has `top_k`, `metadata`, `thinking` stripped from the body via `sanitize_for_nim`, and (b) receives `Authorization: Bearer` auth header.

**Contract**: Test using `test_app_with_nim_http_client`. Use httpmock to capture the forwarded request. Assert:
- Request has `Authorization: Bearer sk-test` header
- Request body does NOT contain `"top_k"`, `"metadata"`, `"thinking"` keys
- Request body contains `"model"` (overridden to provider model)
- Response returns 200 with valid OpenAI Chat body shape

#### 3. New test: `completion_handler` ollama passthrough

**File**: `src/proxy/handlers.rs`

**Intent**: Verify that a request routed to an ollama provider type (a) has no auth headers emitted, and (b) body passes through unmodified.

**Contract**: Test using `test_app_with_ollama_http_client`. Assert:
- Request does NOT have `Authorization` or `x-api-key` headers
- Request body contains the original user message unchanged
- Response returns 200

#### 4. New test: `messages_handler` nvidia_nim translation

**File**: `src/proxy/handlers.rs`

**Intent**: Verify the `messages_handler` path when `provider_type = "nvidia_nim"` (needs_translation=true + NIM sanitization). The handler translates Anthropic→OpenAI then applies NIM sanitization.

**Contract**: Test using `test_app_with_nim_http_client`. Mock on `/v1/chat/completions` with OpenAI Chat response. Send Anthropic Messages request. Assert:
- Response has Anthropic structural shape (type, role, content[], usage, stop_reason)
- No NIM-specific fields leak into response

#### 5. New test: `messages_handler` ollama translation

**File**: `src/proxy/handlers.rs`

**Intent**: Verify the `messages_handler` path when `provider_type = "ollama"` — translation path with no auth headers.

**Contract**: Test using `test_app_with_ollama_http_client`. Assert:
- Request has NO auth headers
- Translated request body has correct OpenAI Chat shape
- Response is translated back to Anthropic shape correctly

#### 6. New test: R5 passthrough (`codex-responses-api` F4 #5)

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify that when `provider_type = "openai_responses"`, the original Responses body is forwarded verbatim to the upstream (R5 path, no Chat translation). Reuses `test_app_with_openai_responses_http_client`.

**Contract**: New test `test_responses_handler_passthrough`. Mock on `/v1/responses`. Assert:
- Upstream receives the original Responses body unchanged (via httpmock request capture)
- Response has valid Responses shape: `object == "response"`, `id` starts with `"resp_"`
- Response content matches upstream response

#### 7. New test: Responses auth gate (`codex-responses-api` F4 #6)

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify the `/v1/responses` endpoint is behind the `proxy_auth_layer`.

**Contract**: New test `test_responses_handler_requires_auth`. Send request without `Authorization` header. Assert:
- Response status is 401

#### 8. New test: Responses upstream error envelope (`codex-responses-api` F4 #7)

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify that upstream errors (e.g., 429 rate limit) are translated to Responses-shaped error envelopes.

**Contract**: New test `test_responses_handler_upstream_error_forwards_body` using `test_app_with_http_client`. Mock returns 429. Assert:
- Response body contains `"error"` key
- Response body contains appropriate error code or message
- Response is valid JSON

#### 9. New test: Responses cache hit (`codex-responses-api` F4 #8)

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify that a repeated Responses request returns a cached response and the response_id is stable.

**Contract**: New test `test_responses_cache_hit_returns_cached_response` using `test_app_with_cache`. Send identical request twice. Assert:
- First request hits upstream (mock.hits() >= 1)
- Second request returns same response ID (not re-generated)
- Response body identical between first and second call

#### 10. New test: Responses header forwarding (`codex-responses-api` F4 #9)

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify that `openai-beta`, `openai-organization`, and `openai-project` headers are forwarded to the upstream.

**Contract**: New test `test_responses_handler_forwards_openai_headers` using `test_app_with_http_client`. Send request with `openai-beta: assistants=v2` header. Use httpmock to capture forwarded request. Assert:
- `openai-beta` header present in upstream request
- Header value matches original

#### 11. Dedicated test: Responses two-stage streaming

**File**: `src/proxy/responses_handler.rs`

**Intent**: Cover `handle_responses_anthropic_streaming_response` (`responses_streaming.rs:164`), the two-stage pipeline that chains Anthropic SSE → Chat SSE → Responses SSE. Feed a known full Anthropic SSE sequence through the pipeline and assert the Responses SSE event structure.

**Contract**: New test `test_responses_handler_anthropic_two_stage_streaming` using `test_app_with_anthropic_http_client`. Mock upstream returns a full Anthropic SSE sequence: `message_start` → `content_block_start(text)` → `content_block_delta(text)`×2 → `content_block_stop` → `message_delta(end_turn + usage)` → `message_stop`. Collect and parse the output as a sequence of SSE events. Assert:
- First event: `event: response.created` with `data: {"type":"response.created",...}`
- Contains `event: response.output_item.added` with `"type":"message"`
- Contains `event: response.content_part.added` with `"type":"output_text"`
- Contains `event: response.output_text.delta` with `"delta":` containing translated text
- Contains `event: response.output_text.done`
- Contains `event: response.content_part.done`
- Contains `event: response.output_item.done`
- Final event: `event: response.completed` with `"status":"completed"` and `"usage"` object containing `input_tokens`/`output_tokens`
- Events carry monotonically increasing `sequence_number`
- No raw Anthropic or Chat SSE event types leak into Responses output

#### 12. New test: Responses Anthropic buffered tool_use

**File**: `src/proxy/responses_handler.rs`

**Intent**: Verify the R2 buffered path with tool_use: Anthropic response with tool_use block → Chat translation → Responses envelope with function_call output.

**Contract**: Test using `test_app_with_anthropic_http_client`. Mock returns Anthropic response with `content[0].type = "tool_use"`. Assert Responses output:
- `json["output"][0]["type"] == "function_call"`
- `json["output"][0]["name"]` present
- `json["output"][0]["arguments"]` present

### Success Criteria

#### Automated Verification

- All new tests pass: `cargo test`
- Existing responses_handler tests extended with structural assertions (R1 non-streaming/streaming, R2 non-streaming/streaming)
- nvidia_nim: passthrough + sanitization + auth header (completion_handler + messages_handler)
- ollama: passthrough + no-auth-header (completion_handler + messages_handler)
- responses_handler F4 tests: R5 passthrough, auth gate, upstream error envelope, cache hit, header forwarding
- responses_handler: two-stage streaming structural assertions
- responses_handler: Anthropic buffered tool_use → function_call

#### Manual Verification

- Two-stage streaming test produces correct event ordering (no out-of-order events from stage boundary)
- nvidia_nim test correctly verifies sanitization without relying on hardcoded field positions
- Cache-hit test response_id stability verified across repeated requests

---

## Testing Strategy

### Integration Tests

All tests in this plan are integration tests — they exercise the full handler pipeline through `test_app()` + `httpmock` with the Axum router, middleware, and `AppState`. They live inline in the handler test modules per AGENTS.md convention.

### Test Naming Convention

`test_<handler>_<provider_type>_<mode>_<variant>` — e.g., `test_completion_handler_anthropic_streaming_full_sequence`.

### Edge Cases Covered

- Anthropic→OpenAI: text, thinking, tool_use content blocks; buffered + streaming; stop_reason mapping
- OpenAI→Anthropic: text content blocks; buffered + streaming; finish_reason mapping  
- Responses two-stage: full Anthropic SSE sequence → Responses events
- NIM: body sanitization (stripped fields), auth headers
- Ollama: no auth headers, body passthrough
- Error paths: existing Anthropic error tests (handlers.rs:2975, 2628) already cover error forwarding

## Performance Considerations

- Tests use `httpmock` (in-process HTTP mock) — no network I/O
- Each test uses `#[serial]` `EnvGuard` pattern for env var isolation
- Two-stage streaming test may need `slow_tests` classification if httpmock chunked delivery causes timing issues (per historical impl-review F1)

## References

- Frame brief: `context/changes/testing-proxy-translation-contracts/frame.md`
- Research: `context/changes/testing-proxy-translation-contracts/research.md`
- Test plan: `context/foundation/test-plan.md` §3 Phase 1
- Test harnesses: `src/app/test_helpers.rs:114-534`
- Handler injection points: `research.md` Finding 1
- Streaming state-machine gaps: `research.md` Finding 4
- Historical impl-review findings: `research.md` Finding 5

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Provider-Type Test Harnesses

#### Automated

- [x] 1.1 `test_app_with_nim_http_client` compiles and returns functional Router — d3a347e
- [x] 1.2 `test_app_with_ollama_http_client` compiles and returns functional Router — d3a347e
- [x] 1.3 Existing test suite passes: `cargo test` — d3a347e

#### Manual

- [ ] 1.4 Harnesses follow `test_app_with_anthropic_http_client` pattern (AppState fields, auth config, routing entries)
- [ ] 1.5 Provider type strings correct in routing entries

### Phase 2: Anthropic + OpenAI-Compatible Body-Contract Tests

#### Automated

- [x] 2.1 Extended Anthropic non-streaming test with structural response assertions — d3a347e
- [x] 2.2 New Anthropic streaming full-sequence test (text + thinking + tool_use + usage) — d3a347e
- [x] 2.3 New Anthropic buffered tool_use→tool_calls mapping test — d3a347e
- [x] 2.4 Extended messages_handler translation streaming test with structural assertions — d3a347e
- [x] 2.5 New messages_handler translation buffered test with response shape assertions — d3a347e
- [x] 2.6 New messages_handler translation request-body capture test — d3a347e
- [x] 2.7 New completion_handler openai_compatible buffered response shape test — d3a347e

#### Manual

- [ ] 2.8 Structural assertions survive model name/ID changes (no hardcoded model IDs)
- [ ] 2.9 Streaming tests work with single-chunk httpmock delivery

### Phase 3: Remaining Provider Types + Full Responses Coverage

#### Automated

- [ ] 3.1 Structural assertions on existing R1 non-streaming test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.2 Structural assertions on existing R1 streaming test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.3 Structural assertions on existing R2 non-streaming test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.4 Structural assertions on existing R2 streaming test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.5 completion_handler nvidia_nim passthrough + sanitization test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.6 completion_handler ollama passthrough test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.7 messages_handler nvidia_nim translation + sanitization test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.8 messages_handler ollama translation test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.9 R5 passthrough test (openai_responses verbatim forwarding) — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.10 Responses auth gate test (401 without token) — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.11 Responses upstream error envelope test (429 → error shape) — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.12 Responses cache hit test (response_id stability) — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.13 Responses header forwarding test (openai-beta passthrough) — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.14 Responses two-stage streaming structural assertions test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.15 Responses Anthropic buffered tool_use→function_call test — reverted by 894681a (was falsely checked off in d3a347e)
- [ ] 3.16 Full test suite passes: `cargo test` — reverted by 894681a (was falsely checked off in d3a347e)

#### Manual

- [ ] 3.17 Two-stage streaming produces correct event ordering
- [ ] 3.18 NIM sanitization test verifies field removal without hardcoded positions
- [ ] 3.19 Cache-hit test response_id stable across repeated requests
