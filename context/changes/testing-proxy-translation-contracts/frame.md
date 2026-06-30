# Frame Brief: Proxy Translation Contract Tests

> Framing step before /10x-plan. This document captures what is *actually*
> at issue, separated from what was initially assumed.

## Reported Observation

Protocol translation code across 3 directions (OpenAI↔Anthropic bidirectional + Responses→Chat) and 5 provider types has zero **integration-level** contract tests that verify the full handler pipeline produces correct translated output against known-good reference outputs. Streaming emitters have integration-level happy-path tests for the OpenAI-compatible path only — no Anthropic or Responses streaming integration tests. The user is worried about silent body-structure corruption.

## Initial Framing (preserved)

- **User's stated cause or approach**: Translation was built incrementally — each protocol direction and provider type was added without reference-output verification at the integration level.
- **User's proposed direction**: Write integration contract tests (known-good input→output pairs via `test_app()` + `httpmock`) and streaming edge-case tests — as Phase 1 of `context/foundation/test-plan.md`.
- **Pre-dispatch narrowing**: Leading concern is translation correctness (over streaming resilience). Most likely bug shape is body-structure mismatch — tool use blocks, thinking content, message roles lost or malformed in transit.

## Dimension Map

The observation could originate at any of these dimensions:

1. **Protocol unit translation correctness** — `protocol/request.rs:8` (`translate_request`), `protocol/response.rs:10` (`translate_response`), `protocol/stream.rs:268` (`translate_stream_event`), `protocol/responses_stream.rs` (Chat→Responses). If these functions silently drop fields (tool_use, thinking, cache_control, message roles), output looks correct but is wrong.
   - **Evidence**: 118 unit tests exist across `src/protocol/` (29 request, 19 response, 19 stream, 38 responses, 13 responses_stream). WELL-TESTED.

2. **Handler composition of protocol + provider dispatch** — `handlers.rs:407` (`completion_handler` Anthropic branch), `responses_handler.rs:315` (Responses Anthropic branch), `handlers.rs:1218` (`messages_handler` OpenAI→Anthropic translation branch). A protocol function passes unit tests but is called with wrong arguments at the handler level, or applied to the wrong provider type. ← **user's framing lands here**
   - **Evidence**: Zero integration tests exercise the Anthropic provider_type path through `completion_handler`. Zero integration tests exercise the OpenAI→Anthropic translation path through `messages_handler`. All 96 proxy-level tests use `openai_compatible` provider types only. STRONG signal.

3. **Auth header passthrough per provider type** — `classification/llm.rs` `auth_headers_for` emits different headers per type. Anthropic gets `x-api-key` + `anthropic-version`; NVIDIA gets `Authorization: Bearer`. A provider type mismatch causes wrong auth emission.
   - **Evidence**: `auth_headers_for` tests exist (`auth_headers_for_anthropic_no_provider_config` at `classification/llm.rs:413`) but test the function in isolation, not wired through a handler. WEAK signal at integration level.

4. **Streaming state-machine multi-event correctness** — `StreamTranslateState` (`stream.rs:8`), `AnthropicStreamState`, `ResponsesStreamState`. Individual event translation is tested; multi-event sequences that trigger state transitions are NOT.
   - **Evidence**: 19 unit tests on `translate_stream_event` test individual events in isolation. No test feeds a full Anthropic SSE sequence through `handle_anthropic_streaming_response` and asserts the complete OpenAI SSE output sequence. MODERATE signal.

5. **Responses API two-stage composability** — `responses_streaming.rs:164` (`handle_responses_anthropic_streaming_response`) chains Anthropic SSE → Chat SSE → Responses SSE. A bug in either stage compounds.
   - **Evidence**: 13 unit tests on `responses_stream.rs` test Chat→Responses translation only. The two-stage Anthropic→Chat→Responses path is untested at any level. STRONG signal.

## Hypothesis Investigation

| Hypothesis | Evidence | Verdict |
|---|---|---|
| Protocol unit translation correctness | 118 unit tests in `src/protocol/` covering `translate_request` (29), `translate_response` (19), `translate_stream_event` (19), `responses` (38), `responses_stream` (13). Protocol layer is well-tested at the unit level. | WEAK — the gap is not here |
| Handler composition (user's framing) | Zero integration tests in `src/proxy/` exercise the Anthropic provider_type path. All 96 proxy-level tests use openai_compatible. No test exercises `completion_handler` line 407 (`if provider.provider_type == "anthropic"`) or `messages_handler` line 1218 (`needs_translation = provider.provider_type != "anthropic"`). | **STRONG** |
| Auth header passthrough | `auth_headers_for` has unit tests but no integration tests wired through handlers with real provider-type dispatch. | MODERATE |
| Streaming state-machine multi-event | `translate_stream_event` unit tests exist (19) but no test feeds a full Anthropic SSE event sequence and asserts the complete OpenAI SSE output. | MODERATE |
| Responses two-stage composability | `handle_responses_anthropic_streaming_response` (`responses_streaming.rs:164`): two-stage pipeline (Anthropic→Chat→Responses), zero tests at any level. | **STRONG** |

## Narrowing Signals

Decisive observations that narrowed the hypothesis space:

- 118 unit tests already exist in `src/protocol/` — the test plan's framing of "zero tests" is wrong for the unit layer. The protocol functions ARE tested. The gap is exclusively at the **integration/composition** level.
- All 96 proxy-level tests (`src/proxy/streaming.rs`, `src/proxy/handlers.rs`, `src/proxy/responses_handler.rs`, `src/proxy/util.rs`) use `openai_compatible` provider type — the Anthropic, ollama, and local types have zero integration coverage.
- The Responses streaming two-stage path (`handle_responses_anthropic_streaming_response`) is untested at every level — not even unit tests on the stage composition.
- The user's leading concern (body-structure mismatch) aligns with the handler composition dimension, which IS the strongest-evidence gap.

## Cross-System Convention

Existing tests in this project follow a consistent pattern: build a `test_app()` with a `RegexClassifier` and `httpmock` upstream, send a request via `oneshot`, assert on status + body content. The Anthropic translation path is architecturally identical to the tested OpenAI path — it differs only in a boolean branch (`provider.provider_type == "anthropic"`). The gap is not technical but organizational: each handler was tested with the first provider type wired (openai_compatible) and then the other branches were never backfilled.

The `CountingClassifier` pattern (from `src/classification/types.rs`) and `test_app_with_classifier()` harness (from `src/app/test_helpers.rs`) provide the side-effect-observation tooling needed to validate chain→translation interaction without production traffic.

## Reframed Problem Statement

> **The actual problem to plan around is**: The handler-to-protocol integration layer — where `completion_handler`, `messages_handler`, and `responses_handler` compose protocol translation with provider-type dispatch — has zero integration tests for any path other than `openai_compatible`. Protocol unit tests exist (118) but can't catch handler-level wiring errors like wrong function arguments, missed provider-type branches, or incorrect auth-header composition.

The test plan's Phase 1 should scope to **handler-level integration/contract tests** — exercise the full handler pipeline through `test_app()` + `httpmock` with known-good reference input→output pairs for each provider type, NOT add more unit tests on protocol translation functions. The 118 existing protocol unit tests are sufficient for function-level correctness; the gap is verifying that those correctly-tested functions are wired correctly in the handler dispatch.

Additionally, the streaming edge-case tests (Risk #4) should be deferred or scoped narrowly: streaming emitters have extensive SSE error-formatting tests (format_sse_error_event has 10+ unit tests, status-passthrough has 4, oversize-body truncation has 1, JSON-injection escape has 1), and the keepalive/inline-mid-stream-error paths are tested via `slow_tests`. The remaining streaming gap is the **multi-event sequence correctness** of the Anthropic streaming emitters, which can be covered by the same handler-level contract tests (feed known Anthropic SSE → assert OpenAI SSE output sequence).

## Confidence

**HIGH** — strong evidence (96 proxy tests all use openai_compatible, protocol unit tests exist, two-stage Responses path untested at every level), matches user's stated concern (body-structure mismatch in handler composition), and aligns with the existing test harness patterns (test_app + httpmock + oneshot).

## What Changes for /10x-plan

1. **Scope to handler-level integration, not protocol unit tests.** Do not plan sub-phases that add unit tests on `translate_request`, `translate_response`, or `translate_stream_event` — those already have 118 tests. Plan sub-phases that exercise `completion_handler`, `messages_handler`, and `responses_handler` with the Anthropic provider_type path through `test_app()` + `httpmock`, asserting on the full translated output body and header shape.

2. **Streaming edge-case tests should be contract-driven, not standalone.** Instead of separate "malformed SSE injection" tests, cover streaming correctness by feeding known full-Anthropic-SSE-sequences via httpmock and asserting the complete translated SSE output — this catches multi-event sequence bugs (state-machine transitions) that single-event unit tests miss, AND edge-case behavior (missing events, unexpected event types) as a byproduct.

3. **Add a provider-type matrix as a single cross-cutting sub-phase.** One sub-phase that iterates over all 5 provider types (`nvidia_nim`, `openai_compatible`, `anthropic`, `ollama`, `local`) through the same handler and asserts correct translation/auth behavior per type. This is cheaper than writing separate tests for each.

4. **Responses streaming two-stage path needs its own sub-phase.** `handle_responses_anthropic_streaming_response` is the most complex translation surface (two stages, three state machines) and is untested at every level. It deserves a dedicated sub-phase with conversation-level (multi-turn) reference outputs.

## References

- Source files: `src/proxy/handlers.rs:407` (Anthropic branch), `src/proxy/handlers.rs:1218` (messages_handler translation branch), `src/proxy/responses_handler.rs:315` (Responses Anthropic branch), `src/proxy/streaming.rs:207` (handle_anthropic_streaming_response), `src/proxy/streaming.rs:360` (handle_translating_anthropic_stream), `src/proxy/responses_streaming.rs:13` (handle_responses_streaming_response), `src/proxy/responses_streaming.rs:164` (handle_responses_anthropic_streaming_response), `src/proxy/upstream.rs:142` (translate_anthropic_buffered_response), `src/proxy/upstream.rs:205` (translate_openai_buffered_to_anthropic), `src/protocol/request.rs:8` (translate_request), `src/protocol/response.rs:10` (translate_response), `src/protocol/stream.rs:8` (StreamTranslateState), `src/protocol/responses_stream.rs` (ResponsesStreamState)
- Test counts: `src/protocol/` = 118 unit tests (29 + 19 + 19 + 38 + 13), `src/proxy/` = 96 tests (all openai_compatible provider type), `src/proxy/streaming.rs::slow_tests` = keepalive/inline-error tests
- Harness: `src/app/test_helpers.rs` (`test_app_with_http_client`, `test_app_with_cache`, `test_app_with_classifier`), `src/classification/types.rs` (CountingClassifier pattern for side-effect observation)
- Test plan: `context/foundation/test-plan.md` §3 Phase 1
