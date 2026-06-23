# OpenAI → Anthropic Protocol Translation — Plan Brief

> Full plan: `context/changes/translate-openai-to-anthropic/plan.md`
> Research: `context/changes/translate-openai-to-anthropic/research.md`

## What & Why

Enhance `POST /v1/chat/completions` to transparently translate between OpenAI and Anthropic protocols. When the routed upstream speaks Anthropic (Claude API, DeepSeek /anthropic, Kimi, Z.ai, Fireworks), the proxy translates the request from OpenAI format to Anthropic Messages format, forwards it, and translates the response (including SSE streaming) back to OpenAI format. This lets existing OpenAI-speaking clients use Anthropic providers without any client changes.

## Starting Point

Today, cerebrum has two independent handler paths: `completion_handler` for OpenAI pass-through and `messages_handler` for Anthropic pass-through. Neither translates between protocols. The `provider_type` field on `RouteEntry` already exists and `auth_headers_for` already emits Anthropic-specific headers when `provider_type == "anthropic"`. The translation logic itself doesn't exist yet.

## Desired End State

A request to `POST /v1/chat/completions` routed to an Anthropic upstream automatically:
- Translates the OpenAI request body to Anthropic Messages format
- Forwards with correct `x-api-key` + `anthropic-version` headers
- Translates the Anthropic response (non-streaming or streaming) back to OpenAI format
- Translates Anthropic error responses to OpenAI error envelope

The client always sees OpenAI format — protocol is invisible.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Module placement | New `src/protocol_translation.rs` | ~200-300 lines of pure translation logic shouldn't clutter main.rs (already 2600+ lines) | Plan |
| Detection mechanism | `provider_type == "anthropic"` | Already exists in routing config and used by `auth_headers_for`; no new config needed | Plan |
| Error shape | Translate to OpenAI envelope | Client always sees consistent error format regardless of upstream protocol | Plan |
| Testing strategy | Unit tests + httpmock e2e | Unit tests cover edge cases; e2e tests verify handler integration | Plan |

## Scope

**In scope:**
- Request translation: OpenAI Chat → Anthropic Messages (messages, system, tools, tool_choice, max_tokens default)
- Response translation: Anthropic Messages → OpenAI Chat (non-streaming)
- Streaming translation: Anthropic SSE → OpenAI SSE chunks
- Error translation: Anthropic error → OpenAI error envelope
- Auth headers (already handled by existing `auth_headers_for`)

**Out of scope:**
- `/v1/messages` endpoint stays pass-through (no reverse translation)
- OpenAI-only fields dropped silently: `n`, `frequency_penalty`, `presence_penalty`, `logprobs`, `logit_bias`, `seed`, `response_format`, `stream_options`
- No new config format — uses existing `provider_type`

## Architecture / Approach

A new `src/protocol_translation.rs` module provides pure translation functions: `translate_request`, `translate_response`, `translate_error`, `translate_stream_event`. These are wired into `completion_handler` with a `provider_type == "anthropic"` check after classification. For streaming, the upstream SSE byte stream is intercepted and events are translated before forwarding to the client. The translation layer sits between body parsing and upstream request building.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Translation Module | Pure translation functions with unit tests | Message alternation edge cases (consecutive tool messages) |
| 2. Handler Integration | Wire translation into completion_handler | Streaming interception — translating SSE events in the byte stream |
| 3. Testing | E2E httpmock tests + comprehensive edge case coverage | Mock Anthropic response shapes must be accurate |

**Prerequisites:** None — research doc is complete, codebase patterns are established.
**Estimated effort:** ~2-3 sessions across 3 phases.

## Open Risks & Assumptions

- Streaming translation requires intercepting the byte stream and parsing SSE events — if the upstream sends malformed SSE, the translation layer must handle gracefully
- `arguments` (JSON string) → `input` (object) parsing: malformed JSON must fall back to `{"raw": "..."}` rather than failing the request
- Message alternation enforcement: the translation must merge consecutive tool messages, but edge cases with mixed content may need careful handling

## Success Criteria (Summary)

- An OpenAI-speaking client can send requests to `/v1/chat/completions` routed to an Anthropic upstream and receive valid OpenAI-format responses (both streaming and non-streaming)
- All existing tests continue to pass — no regressions in OpenAI pass-through or Anthropic pass-through paths
- Unit tests cover all translation edge cases from the research doc (§1-§4)
