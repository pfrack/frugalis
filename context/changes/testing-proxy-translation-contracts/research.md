---
date: 2026-06-30T00:00:00+00:00
researcher: opencode
git_commit: 8e8a5e24fa18ae0ecd8621ea74edadb60151a790
branch: main
repository: frugalis
topic: "Ground handler composition surfaces for proxy translation contract tests (Phase 1 of test-plan.md)"
tags: [research, testing, proxy, translation, protocol, streaming]
status: complete
last_updated: 2026-06-30
last_updated_by: opencode
---

# Research: Proxy Translation Handler Injection Points & Test Coverage

**Date**: 2026-06-30
**Researcher**: opencode
**Git Commit**: 8e8a5e24fa18ae0ecd8621ea74edadb60151a790
**Branch**: main
**Repository**: frugalis

## Research Question

Where are the exact injection points for handler-level integration tests of the Anthropic-provider translation path? What harness support already exists? What streaming state-machine gaps remain? What historical decisions shaped the current test coverage?

## Summary

1. **Correction to frame brief**: The frame brief stated "all 96 proxy tests use openai_compatible." This is wrong — **13 integration tests** already use `provider_type = "anthropic"` via `test_app_with_anthropic_http_client` at `src/app/test_helpers.rs:187`. The gap is qualitatively different from "zero coverage": the existing tests verify passthrough behavior (byte-forward, auth headers, error forwarding, model override, cache_control), but NOT the full translated-body contract against known-good reference outputs.

2. **Harness is already in place**: `test_app_with_anthropic_http_client` wires httpmock on `/v1/messages` with `provider_type = "anthropic"` routing. No new harness needed — existing tests can be extended with body-contract assertions.

3. **Two critical untested layers identified**: (a) `completion_handler` Anthropic branch — 5 existing integration tests but none assert the full translated body shape against a reference; (b) `messages_handler` translation path (`needs_translation = true`) — 1 existing integration test (`test_messages_handler_openai_translation_streaming` at handlers.rs:3151) with basic substring checks.

4. **Responses streaming two-stage pipeline**: `handle_responses_anthropic_streaming_response` at `responses_streaming.rs:164` chains Anthropic SSE → Chat SSE → Responses SSE — **zero tests at any level**.

5. **Streaming state-machine gaps**: Three HIGH-risk gaps: (a) no full tool_use stream test for Anthropic→OpenAI, (b) no multi-block stream test (thinking→tool→text), (c) no reasoning+tool finish with ResponsesStreamState. Six MEDIUM-risk gaps including usage-only chunk handling and block-type transitions.

6. **Historical warnings unheeded**: The Jun 22 `translate-openai-to-anthropic` impl-review flagged multi-chunk SSE delivery as untested ("httpmock delivers full SSE in a single chunk") — a gap that persists. The `codex-responses-api` change has 9 pending integration tests from its plan's testing matrix (review F4).

## Detailed Findings

### Finding 1: Handler Injection Point Map

Each of the three handlers has a distinct provider-type trigger and set of injection points for integration test mocking.

#### `completion_handler` (`src/proxy/handlers.rs:155`) — PUT `/v1/chat/completions`

| Injection Point | Anthropic Path | Non-Anthropic Path |
|---|---|---|
| **Trigger** | `handlers.rs:407` — `if provider.provider_type == "anthropic"` | `handlers.rs:680` — else block (openai_compatible, nvidia_nim, local, ollama) |
| **Mock URL** | `/v1/messages` (set via `provider.endpoint`) | `/v1/chat/completions` |
| **Auth headers** | Inline `handlers.rs:463-467` — `auth_headers_for(..., "anthropic", ..., &forward_headers)` → emits `x-api-key` + `anthropic-version` + forwarded `anthropic-*`/`x-claude-code-*` | Via `build_upstream_request` at `handlers.rs:693` → `upstream.rs:41` → `auth_headers_for` → emits `Authorization: Bearer {key}` |
| **Request body translation** | `handlers.rs:433` — `protocol::request::translate_request(&parsed_body)` (OpenAI Chat → Anthropic Messages JSON) | `handlers.rs:689` — `body.clone()` (passthrough, except `sanitize_for_nim` at `handlers.rs:681-688`) |
| **Response body translation (buffered)** | `handlers.rs:574-575` — `translate_anthropic_buffered_response` → `protocol::response::translate_response` (Anthropic Messages → OpenAI Chat Completions) | `handlers.rs:818` — `handle_buffered_response(_, _, false)` (passthrough) |
| **Streaming response** | `handlers.rs:560` — `handle_anthropic_streaming_response` (Anthropic SSE → OpenAI SSE) | `handlers.rs:804` — `handle_streaming_response` (byte passthrough) |
| **Streaming error** | `handlers.rs:503-504` — `handle_anthropic_streaming_error` (translates error + wraps in SSE) | `handlers.rs:753` — `handle_streaming_error` (passthrough error in SSE) |
| **Cache insertion** | `handlers.rs:605-617` — writes to `state.response_cache` on success | `handlers.rs:848-861` — same logic |

#### `messages_handler` (`src/proxy/handlers.rs:965`) — PUT `/v1/messages`

| Injection Point | Anthropic Passthrough (`needs_translation = false`) | Non-Anthropic Translation (`needs_translation = true`) |
|---|---|---|
| **Trigger** | `handlers.rs:1218` — `let needs_translation = provider.provider_type != "anthropic"` → false | Same line → true |
| **Mock URL** | `/v1/messages` (Anthropic→Anthropic passthrough) | `/v1/chat/completions` (Anthropic client → OpenAI upstream) |
| **Auth headers** | Via `build_upstream_request` at `handlers.rs:1309` → emits `x-api-key` + `anthropic-version` | Same path → emits `Authorization: Bearer {key}` |
| **Request body** | `handlers.rs:1293` — `body.clone()` raw bytes (preserves cache_control, thinking, context_management) | `handlers.rs:1244` — `protocol::request::anthropic_to_openai_request_with_cache_signal(&parsed)` → strips cache_control, translates to OpenAI Chat JSON |
| **Response body (buffered)** | `handlers.rs:1477` — `handle_buffered_response(_, _, true)` verbatim Anthropic | `handlers.rs:1470` — `translate_openai_buffered_to_anthropic` → `protocol::response::openai_to_anthropic_response` (OpenAI → Anthropic) |
| **Streaming response** | `handlers.rs:1456` — `handle_streaming_response` verbatim SSE passthrough | `handlers.rs:1442` — `handle_translating_anthropic_stream` (OpenAI SSE → Anthropic SSE) |
| **Streaming error** | `handlers.rs:1378` — `handle_streaming_error` (verbatim) | `handlers.rs:1368-1376` — `handle_streaming_error_with_transform(..., openai_to_anthropic_error)` |

#### `responses_handler` (`src/proxy/responses_handler.rs:13`) — PUT `/v1/responses`

| Injection Point | Anthropic (R2) | OpenAI Responses Passthrough (R5) | Default Chat (R1) |
|---|---|---|---|
| **Trigger** | `responses_handler.rs:315` — `if provider.provider_type == "anthropic"` | `responses_handler.rs:361` — `else if provider.provider_type == "openai_responses"` | `responses_handler.rs:398` — `else` |
| **Mock URL** | `/v1/messages` | `/v1/responses` | `/v1/chat/completions` |
| **Auth headers** | Inline `responses_handler.rs:344-348` — `auth_headers_for(..., "anthropic", ...)` | Via `build_upstream_request` `responses_handler.rs:363-370` | Via `build_upstream_request` `responses_handler.rs:400-407` |
| **Request body** | `responses_handler.rs:317` — `protocol::request::translate_request(&chat_body)` (Chat→Anthropic, after initial Responses→Chat at line 67) | `body` raw (original Responses bytes, `responses_handler.rs:367`) | `chat_body_bytes` (Chat translation of Responses at `responses_handler.rs:404`) |
| **Response body (buffered)** | `responses_handler.rs:564-566` — `translate_anthropic_buffered_response` → then `responses_handler.rs:635` `response_from_chat` (Anthropic→Chat→Responses) | `responses_handler.rs:543` — `handle_buffered_response` passthrough | `responses_handler.rs:571` — `handle_buffered_response` → then `response_from_chat` (Chat→Responses) |
| **Streaming response** | `responses_handler.rs:502-503` — `handle_responses_anthropic_streaming_response` (two-stage: Anthropic→Chat→Responses) | `responses_handler.rs:520` — `handle_responses_streaming_response` (Chat→Responses) | Same as R5 |
| **Streaming error** | `responses_handler.rs:447-454` — `handle_streaming_error_with_transform(..., map_upstream_error_to_responses)` | Same | Same |

### Finding 2: Existing Test Harness Support

#### Corrections to Frame Brief Findings

The frame brief at `context/changes/testing-proxy-translation-contracts/frame.md` stated:
> "All 96 proxy-level tests use `openai_compatible` provider type — the Anthropic, ollama, and local types have zero integration coverage."

**This is incorrect.** `test_app_with_anthropic_http_client` exists at `src/app/test_helpers.rs:187` and is used in **13 integration tests**:

**`completion_handler` Anthropic translation tests** (5 tests, `src/proxy/handlers.rs`):
- `test_completion_handler_anthropic_translation` (line 2830) — non-streaming round-trip, asserts status + body content
- `test_completion_handler_anthropic_translation_inserts_cache_control` (line 2881) — cache_control injection in translated body
- `test_completion_handler_translates_cache_tokens_in_usage` (line 2902) — cache token usage translation
- `test_completion_handler_anthropic_streaming` (line 2938) — streaming SSE round-trip
- `test_completion_handler_anthropic_error` (line 2975) — upstream error forwarding

**`messages_handler` Anthropic passthrough tests** (5 tests, `src/proxy/handlers.rs`):
- `test_messages_handler_non_streaming_passthrough` (line 2492) — non-streaming round-trip
- `test_messages_handler_forwards_anthropic_client_headers` (line 2519) — header passthrough
- `test_messages_handler_streaming_passthrough` (line 2594) — streaming passthrough
- `test_messages_handler_upstream_error_forwards_body` (line 2628) — error forwarding
- `test_messages_handler_overrides_model_to_classifier_choice` (line 2670) — model override
- `test_messages_handler_anthropic_passthrough_preserves_cache_control` (line 3092) — cache_control passthrough

**`responses_handler` Anthropic tests** (2 tests, `src/proxy/responses_handler.rs`):
- `test_responses_handler_anthropic_non_streaming` (line 858) — R2 non-streaming
- `test_responses_handler_anthropic_streaming` (line 901) — R2 streaming

**Qualitative assessment of existing coverage**: These tests assert on HTTP status codes, Content-Type headers, substring presence in response bodies, and cache_control passthrough. They do NOT assert that the fully translated body shape matches a known-good reference output. They verify "the pipeline didn't crash" rather than "the pipeline produced the correct output."

#### Harness Inventory

| Harness | Location | Provider Type | httpmock? | Used For |
|---|---|---|---|---|
| `test_app_with_http_client` | `test_helpers.rs:114` | `openai_compatible` | `/v1/chat/completions` | OpenAI Chat proxy tests |
| `test_app_with_anthropic_http_client` | `test_helpers.rs:187` | `anthropic` | `/v1/messages` | Anthropic passthrough + translation tests |
| `test_app_with_cache` | `test_helpers.rs:355` | `openai_compatible` | `/v1/chat/completions` | Cache tests |
| `test_app_with_openai_responses_http_client` | `test_helpers.rs:457` | `openai_responses` | `/v1/responses` | R5 passthrough tests |
| `test_app_with_openai_translation` (local) | `handlers.rs:3004` | `openai_compatible` | `/v1/chat/completions` | Anthropic→OpenAI translation tests |

**Missing harnesses**: No harness for `nvidia_nim`, `ollama`, `local`, or `anthropic + cache` combinations. These would need new harness variants or parameterization of an existing one.

### Finding 3: Protocol Unit Test Inventory

The `src/protocol/` directory has **118 unit tests** across 5 files, covering individual translation functions:

| File | Tests | What's Covered |
|---|---|---|
| `protocol/request.rs` | 29 | `translate_request` (OpenAI→Anthropic): messages, system prompt, tool definitions, tool_choice, reasoning, images, max_tokens default, stop sequences. `anthropic_to_openai_request`: same direction in reverse. |
| `protocol/response.rs` | 19 | `translate_response` (Anthropic→OpenAI): text content, thinking→reasoning_content, tool_use→tool_calls, stop_reason mapping, usage translation. `openai_to_anthropic_response`: inverse direction. Error translation both ways. |
| `protocol/stream.rs` | 19 | `translate_stream_event` (Anthropic SSE→OpenAI SSE): message_start, content_block_start, text_delta, thinking_delta, input_json_delta, content_block_stop, message_delta, message_stop. `openai_to_anthropic_stream_event`: inverse direction. |
| `protocol/responses.rs` | 38 | `request_to_chat`, `response_from_chat`, validation, error wrapping. |
| `protocol/responses_stream.rs` | 13 | `translate_chat_chunk_to_responses_events`: content, reasoning, tool_calls, refusal, finish_reason, sequence-number monotonicity, done terminator. |

**Verdict**: Protocol unit tests are adequate. The gap is not here. The handler integration layer is what needs testing.

### Finding 4: Streaming State-Machine Coverage Gaps

#### Anthropic SSE → OpenAI SSE (`StreamTranslateState`, used by `handle_anthropic_streaming_response`)

**Covered by existing integration test** (`test_completion_handler_anthropic_streaming` at handlers.rs:2938):
- Sequence: `message_start` → `content_block_start(text)` → 2× `content_block_delta(text)` → `content_block_stop` → `message_delta(end_turn)` → `message_stop`
- Assertions: substring checks for `"Hello "`, `"finish_reason":"stop"`, `"[DONE]"`, `"chatcmpl-"`, `"role":"assistant"`

**HIGH-risk gaps**:
1. **Full tool_use stream**: `message_start` → `content_block_start(tool_use)` → `content_block_delta(input_json)` × N → `content_block_stop` → `message_delta(stop_reason="tool_use")` → `message_stop`. Untested — exercises `tool_index` counter, `has_tool_use` toggle, `finish_reason="tool_calls"` mapping.
2. **Multi-block stream** (thinking + tool_use + text): Tests block-type transitions, `content_block_stop` for non-tool blocks, tool_index with interleaved text. Untested.

**MEDIUM-risk gaps**:
- `message_delta` without usage field
- `message_delta` with `stop_reason="tool_use"` or `"max_tokens"` mapping
- `message_start` missing `message` field (returns `None`, no crash — but untested)

#### OpenAI SSE → Anthropic SSE (`AnthropicStreamState`, used by `handle_translating_anthropic_stream`)

**Covered by existing integration test** (`test_messages_handler_openai_translation_streaming` at handlers.rs:3151):
- Sequence: role chunk → content chunk → finish_reason(stop) → [DONE]
- Assertions: substring checks for `message_start`, `content_block_start`, `text_delta`, `message_delta`, `end_turn`, `message_stop`

**MEDIUM-risk gaps**:
- Usage-only terminal chunk (when `stream_options.include_usage` is set — lines 492-523 in `openai_to_anthropic_stream_event`)
- Text→tool_use and tool_use→text block transitions
- `finish_reason="length"`/`"tool_calls"`/`"content_filter"` mapping

#### Chat SSE → Responses SSE (`ResponsesStreamState`, used by `handle_responses_streaming_response` and chain-called by `handle_responses_anthropic_streaming_response`)

**MEDIUM-risk gaps**:
- Reasoning+tool+content finish in one stream (tests all done-sequence branching)
- Usage accumulation from terminal chunk (field `accumulated_usage`)
- `[DONE]` sent without any content (only `response.created` emitted)

**LOW-risk gaps**: Multiple tool_calls in single chunk, non-`data:` SSE lines, invalid JSON error paths.

### Finding 5: Historical Translation-Test Decisions

| Date | Change | Finding |
|---|---|---|
| Jun 22 | `translate-openai-to-anthropic` | **impl-review F1**: Multi-chunk SSE delivery untested — httpmock delivers full SSE body in single chunk, masking partial-event buffer discarding. Gap persists. |
| Jun 22 | `translate-openai-to-anthropic` | Plan Phase 3 specified comprehensive translation tests — all implemented and marked complete. |
| Jun 22 | `translate-anthropic-to-openai` | Same comprehensive test plan. All completed. No test gaps found in review. |
| Jun 27 | `claude-code-compat` | **impl-review F1**: 2 of 3 streaming paths untested for token usage capture — silent observability gap. `handle_anthropic_streaming_response` updated but `handle_translating_anthropic_stream` and passthrough `handle_streaming_response` still call plain `log_classification` without usage. |
| Jun 27 | `claude-code-compat` | **impl-review F7**: 4 manual verification items deferred to live-environment testing (needs Claude Code, Anthropic upstream, DB). |
| Jun 30 | `codex-responses-api` | **review-fixes.md F4**: 9 handler-level integration tests from the plan's testing matrix are still Pending — responses_handler remains test-light at the integration level. Two-stage Responses→Anthropic streaming path completely untested. |

## Architecture Insights

1. **The `provider_type` string is the single dispatch point** for all translation decisions. Changing provider_type in routing config changes which translation path is taken, which auth headers are emitted, and which upstream endpoint is called. The Anthropic branch is the most complex (6 protocol translation functions involved), while `openai_compatible` is effectively passthrough with optional NIM sanitization.

2. **The test harness pattern is extensible**: `test_app_with_anthropic_http_client` demonstrates the pattern for any provider type — set endpoint URL, set provider_type, provide env var for API key. Adding an `nvidia_nim` or `ollama` harness would follow mechanically. The harder gap is the `messages_handler` non-Anthropic path (`needs_translation = true`), which needs an `openai_compatible` upstream but an Anthropic client body — this is covered by the local `test_app_with_openai_translation` helper at handlers.rs:3004.

3. **CountingClassifier pattern doesn't apply here**: The phase needs to test translation correctness, not chain routing. The CountingClassifier (`src/classification/chain.rs:62-97`) observes which backend fired in the classifier chain. For translation tests, the classifier is pre-configured to route to the test provider; the assertion is on the translated output, not which tier fired.

4. **Cache bypasses translation tests**: `test_app_with_cache` uses `openai_compatible` only. If an Anthropic+cache test is needed (e.g., verifying cache insertion after Anthropic translation in `completion_handler` lines 605-617), a new harness `test_app_with_anthropic_cache` would need to be created following the pattern of `test_app_with_cache`.

## Historical Context (from Prior Changes)

- `context/archive/2026-06-22-translate-openai-to-anthropic/plan.md` — First comprehensive translation test plan. Explicitly listed every edge case for unit tests + httpmock e2e. All implemented and verified. But impl-review F1 flagged multi-chunk SSE as untested.
- `context/archive/2026-06-22-translate-anthropic-to-openai/plan.md` — Same pattern. All tests implemented.
- `context/archive/2026-06-27-claude-code-compat/reviews/impl-review.md` — F1: "2 of 3 streaming paths untested for token usage and session attribution." Still open.
- `context/changes/codex-responses-api/follow-ups/review-fixes.md` — F4: "Add handler-level integration tests for responses_handler." 9 tests pending. Two-stage streaming path completely untested.
- `context/foundation/lessons.md:12-17` — "Re-run review after a follow-up change touches the same handler" — the F1-F4 review fixes were lost twice, evidence that handler-level tests are needed as regression guards.

## Open Questions

1. Should the Anthropic+cache path (cache insertion after Anthropic translation at `handlers.rs:605-617`) be tested now, or deferred to a cache-specific test phase?
2. Should `nvidia_nim` sanitization tests be included in this phase (it's a distinct provider_type with body-modification logic at `handlers.rs:681-688` and `handlers.rs:1296-1305`)?
3. Should the `ollama` and `local` provider types be tested (they emit no auth headers at all — `auth_headers_for` returns empty vec)?
4. Should the `codex-responses-api` pending F4 tests (9 handler-level integration tests) be backfilled in this change or tracked separately?
