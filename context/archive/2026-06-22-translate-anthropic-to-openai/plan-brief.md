# Anthropic → OpenAI Protocol Translation — Plan Brief

> Full plan: `context/changes/translate-anthropic-to-openai/plan.md`
> Research: `context/changes/translate-anthropic-to-openai/research.md`
> Sibling plan: `context/changes/translate-openai-to-anthropic/plan.md`

## What & Why

Enhance `POST /v1/messages` to transparently translate between Anthropic and OpenAI protocols. When the routed upstream speaks OpenAI (NVIDIA NIM, OpenRouter, Groq, Cerebras, Ollama), the proxy translates the request from Anthropic Messages format to OpenAI Chat Completions format, forwards it, and translates the response (including SSE streaming) back to Anthropic format. This lets Claude Code and other Anthropic-speaking clients use OpenAI-compatible providers without any client changes.

## Starting Point

Today, cerebrum has two independent handler paths: `completion_handler` for OpenAI pass-through and `messages_handler` for Anthropic pass-through. Neither translates between protocols. The `provider_type` field on `RouteEntry` already exists and `auth_headers_for` already emits `Authorization: Bearer` when `provider_type != "anthropic"`. The translation logic itself doesn't exist yet. The sibling plan (translate-openai-to-anthropic) establishes the `src/protocol_translation.rs` module.

## Desired End State

A request to `POST /v1/messages` routed to an OpenAI-compatible upstream automatically:
- Translates the Anthropic request body to OpenAI Chat Completions format
- Forwards with correct `Authorization: Bearer` headers
- Translates the OpenAI response (non-streaming or streaming) back to Anthropic format
- Translates OpenAI error responses to Anthropic error envelope

The client always sees Anthropic format — protocol is invisible.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Module placement | Extend existing `src/protocol_translation.rs` | Sibling plan already created the module; both directions belong together | Plan |
| Detection mechanism | `provider_type != "anthropic"` | Mirrors sibling plan's check; uses existing routing config field | Plan |
| Error shape | Translate to Anthropic envelope | Client always sees consistent error format regardless of upstream protocol | Plan |
| Streaming approach | Stateful emitter (block_index, open_block, tool_state) | OpenAI SSE → Anthropic SSE requires tracking which content block is open | Research §3 |
| Testing strategy | Unit tests + httpmock e2e | Unit tests cover edge cases; e2e tests verify handler integration | Plan |

## Scope

**In scope:**
- Request translation: Anthropic Messages → OpenAI Chat (messages, system, tools, tool_choice, stream_options)
- Response translation: OpenAI Chat → Anthropic Messages (non-streaming)
- Streaming translation: OpenAI SSE → Anthropic SSE (stateful emitter with block transitions)
- Error translation: OpenAI error → Anthropic error envelope
- Post-pass reasoning fix for DeepSeek/Kimi compatibility
- Auth headers (already handled by existing `auth_headers_for`)

**Out of scope:**
- `/v1/chat/completions` endpoint stays pass-through (no reverse translation)
- Anthropic-only fields dropped silently: `top_k`, `metadata`, `thinking`
- NIM field sanitization (can be added later)
- No new config format — uses existing `provider_type`

## Architecture / Approach

Extend `src/protocol_translation.rs` (created by sibling plan) with Anthropic → OpenAI translation functions: `anthropic_to_openai_request`, `anthropic_to_openai_response`, `anthropic_to_openai_error`, `anthropic_to_openai_stream_event`. These are wired into `messages_handler` with a `provider_type != "anthropic"` check after classification. For streaming, a stateful emitter tracks open content blocks and translates OpenAI SSE chunks to Anthropic SSE events.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Translation Module | Pure translation functions with unit tests | Post-pass reasoning fix edge cases |
| 2. Handler Integration | Wire translation into messages_handler | Streaming interception — stateful block transitions |
| 3. Testing | E2E httpmock tests + comprehensive edge case coverage | Streaming state machine correctness |

**Prerequisites:** Sibling plan (translate-openai-to-anthropic) must create `src/protocol_translation.rs` first, OR both plans can be implemented in parallel with the module created by whichever runs first.
**Estimated effort:** ~2-3 sessions across 3 phases.

## Open Risks & Assumptions

- Streaming translation requires a stateful emitter — block transitions must be tested thoroughly (close previous block before opening new one)
- `stream_options.include_usage = true` may not be supported by all OpenAI providers (emit 0 tokens)
- Post-pass reasoning fix: if ANY message has `reasoning_content`, ALL assistant messages with `tool_calls` but no reasoning need `reasoning_content: " "` — edge case for mixed conversations
- Usage deferred: some providers defer `finish_reason` until usage arrives or `[DONE]` flushes

## Success Criteria (Summary)

- An Anthropic-speaking client can send requests to `/v1/messages` routed to an OpenAI-compatible upstream and receive valid Anthropic-format responses (both streaming and non-streaming)
- All existing tests continue to pass — no regressions in OpenAI pass-through or Anthropic pass-through paths
- Unit tests cover all translation edge cases from the research doc (§1-§4)
