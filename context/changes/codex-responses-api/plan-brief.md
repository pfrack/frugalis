# Codex Responses API Shim — Plan Brief

> Full plan: `context/changes/codex-responses-api/plan.md`
> Research: `context/changes/codex-responses-api/research.md`

## What & Why

Add a `POST /v1/responses` endpoint that translates the OpenAI Responses API protocol into Chat Completions, reusing the existing cascade, cache, streaming, and inference-logging infrastructure. Modern Codex CLI speaks **only** `/v1/responses` — its `wire_api = "chat"` path was removed. Without this shim, Codex CLI cannot use Frugalis at all. This closes Tier-1 competitive gap #5 from the roadmap.

## Starting Point

Frugalis already has a mature bidirectional OpenAI↔Anthropic translator (`src/protocol/{request,response,stream}.rs`, 3,084 LOC), a provider-cascade handler (`completion_handler` at `handlers.rs:155-948`), SSE streaming with keepalive (`streaming.rs:17-107`), a SHA256-keyed response cache, and async inference persistence. What's missing is the Responses protocol leg — a new endpoint that translates Responses-shaped request bodies into Chat Completions, delegates to the existing cascade pipeline, and translates Chat responses back into Responses shape.

## Desired End State

`POST /v1/responses` sits behind the same `proxy_auth_layer` as all other proxy routes. Codex CLI users point at Frugalis with `provider_type: "openai_compatible"` and get full functionality: text streaming, tool calls (function_call), reasoning display (best-effort from `reasoning_content`), and multi-turn conversations (via `previous_response_id` with full-transcript re-sends). The endpoint returns valid Responses JSON with synthesized IDs (`resp_<uuid>`), proper `output[]` items, and standard SSE events. All unsupported Responses features (built-in tools, background tasks, `prompt`, `conversation`, grammar format) are cleanly rejected with 400.

## Key Decisions Made

| Decision                       | Choice                                                                                     | Why (1 sentence)                                                                          | Source   |
| ------------------------------ | ------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- | -------- |
| Architecture                   | Path (a): Responses → Chat → core → Responses                                              | Reuses 100% of existing cascade/cache/streaming/logging; new code bounded to ~2,000 lines. | Research |
| `openai_responses` provider_type | Phase 1 (not deferred to Phase 2)                                                          | Enables native-Responses upstream passthrough tests from day one.                         | Plan     |
| `store: true` handling         | Log a `warn!`                                                                              | Warns operators that server-side state isn't stored without blocking Codex CLI (which defaults to `store: true`). | Plan     |
| `x-codex-responses-lite` header | Forward verbatim                                                                           | Upstream providers that understand it get correct behavior; providers that don't 400 on their side. | Plan     |
| `input` as bare string         | Wrap as single user message                                                                | Matches Chat provider expectations; simplest downstream path.                             | Plan     |
| Reasoning fidelity loss        | Log warning                                                                                | Operators can diagnose missing reasoning without rejecting valid requests.                | Plan     |
| Field rejection granularity    | Per-feature 400 with descriptive message                                                   | Users know which field caused rejection; better UX than generic errors.                   | Plan     |
| Phase 4 transcript store       | Postgres-backed (Option C)                                                                 | Survives restarts; aligns with existing Postgres infrastructure in the persistence layer. | Plan     |
| InferenceRecord extension      | All 4 Codex headers + previous_response_id                                                 | Rich per-request attribution for debugging; mirrors S-18's `client_session_id` capture.   | Plan     |
| Header allowlist               | Add `openai-beta`, `openai-organization`, `openai-project`                                 | Codex CLI sends these for Responses feature gating; dropped headers can silently downgrade behavior. | Research |

## Scope

**In scope:**
- `POST /v1/responses` endpoint behind `proxy_auth_layer`
- Request translation: Responses → Chat Completions (messages, instructions, tools, tool_choice, format, caching fields)
- Response synthesis: Chat → Responses (output[] items, usage, status mapping, ID generation)
- Streaming: ~10 of 41 Responses SSE event types emitted from upstream Chat SSE chunks
- Function call streaming (arguments.delta events)
- Reasoning summary (best-effort from `reasoning_content` + Anthropic `thinking_delta`)
- `previous_response_id` via re-send-full-transcript (no server-side store until Phase 4)
- Cache keying on `sha256(input[])` for identical re-sends
- Postgres-backed `TranscriptStore` (Phase 4)
- `InferenceRecord` extended with 5 new nullable fields (Phase 4)
- Dashboard inferences page column (Phase 4)
- OpenAPI spec, README, AGENTS.md, bash mock functions (Phase 5)

**Out of scope:**
- Built-in tools (web_search, code_interpreter, file_search, computer_use, image_generation, mcp_*, shell, apply_patch) — rejected with 400
- `conversation: {id}` API — rejected with 400
- `background: true` — rejected with 400
- `prompt` field — rejected with 400
- `text.format = "grammar"` — rejected with 400
- `tool_choice: {type: "allowed"}` — rejected with 400
- Server-side transcript storage honoring `store: true` (warning logged, not stored)
- Multi-pod-safe transcript store (single-instance Postgres in Phase 4)
- WebSocket upgrade (`wss://`) — Codex CLI falls back to HTTP

## Architecture / Approach

The `responses_handler` receives a Responses JSON body, calls `protocol::responses::request_to_chat()` to produce a Chat-Completions-shaped body, then delegates to `completion_handler`'s classification+cascade pipeline. Non-streaming responses go through `response_from_chat()` to synthesize a Responses JSON envelope. Streaming uses `protocol::responses_stream` (a state machine that maps Chat SSE chunks → Responses SSE events) wrapped by `proxy/responses_streaming.rs` (which prepends `response.created` and appends `response.completed` around the existing `handle_streaming_response` byte stream).

New code: `src/protocol/responses.rs` (~600-800 LOC), `src/protocol/responses_stream.rs` (~800-1000 LOC), `src/proxy/responses_handler.rs` (~200 LOC), `src/proxy/responses_streaming.rs` (~150 LOC). Modified files: `src/protocol/mod.rs`, `src/app/mod.rs`, `src/proxy/util.rs`, `src/proxy/handlers.rs` (test entries), `src/routing/routes.rs` (doc only), `src/persistence/types.rs` (Phase 4), `src/persistence/sql_backend.rs` (Phase 4).

## Phases at a Glance

| Phase                            | What it delivers                                      | Key risk                                    |
| -------------------------------- | ----------------------------------------------------- | ------------------------------------------- |
| 1. Protocol Translator + Handler | Non-streaming `POST /v1/responses` with auth, rejection, header forwarding | Body translation correctness for all `InputItem` variants |
| 2. Streaming Responses           | SSE event translation, chunk-by-chunk streaming       | SSE state machine correctness (event ordering, ID stability) |
| 3. Reasoning + Cache             | Reasoning summary + Anthropic support + cache dedup   | Reasoning fidelity inherently lossy via Chat |
| 4. Persistence + Dashboard       | Postgres transcript store + 5 new InferenceRecord fields + dashboard column | Migration compatibility with existing rows |
| 5. Documentation + E2E           | OpenAPI spec, README, AGENTS.md, bash mocks, E2E fixture | Documentation bit-rot if not verified against running server |

**Prerequisites:** Existing S-01e (cascade) and S-15 (openai→anthropic translation) are done. `cargo test auth` and `cargo test routes_auth` must be green before starting. Postgres available for Phase 4 migration testing.
**Estimated effort:** ~23 working days across 5 phases (~7 days per Phase 1, ~5 per Phase 2, ~5 per Phase 3, ~3 per Phase 4, ~3 per Phase 5).

## Open Risks & Assumptions

- **SSE state machine complexity** — the 10 Responses event types must be emitted in correct order with stable IDs across a stream that may include reasoning, text, and tool calls interleaved. The risk is managed by the explicit per-event translation table in the research and the phased rollout (Phase 1 is non-streaming only).
- **`completion_handler` reuse mechanics** — Phase 1 assumes the handler can either invoke `completion_handler`'s cascade loop via a shared helper or call it as an internal function. If the existing handler is too tightly coupled to its own request path, a refactor of the cascade loop into a reusable function may be needed — this could push Phase 1 scope.
- **Reasoning fidelity** — Chat Completions has no first-class reasoning wire field. `delta.reasoning_content` is a non-standard field used by DeepSeek/DeepInfra providers; most OpenAI-compatible providers do not emit it. The practical impact on Codex CLI (which targets OpenAI) is zero, but Anthropic-upstream reasoning reaches the client via the existing two-leg translator chain (Anthropic `thinking_delta` → Chat `reasoning_content` → Responses `reasoning_summary_text.delta`).
- **Anthropic upstream provider_type with Responses** — when `provider_type: "anthropic"` is configured, `completion_handler`'s Anthropic branch (`handlers.rs:407-677`) translates the (already-translated) Chat body to Anthropic Messages. This double-translation (Responses→Chat→Anthropic) preserves semantics but loses the `previous_response_id` field — documented as caveat.
- **S-18 review fix preservation** — S-21 touches `handlers.rs` and `streaming.rs`. The S-18 impl-review fixes (F1-F4) in those files must be verified intact before merging, per `lessons.md` rule #2.

## Success Criteria (Summary)

- Codex CLI configured with Frugalis successfully completes queries (streaming + non-streaming)
- All 38 new tests pass across the 9-cell matrix without regressing existing protocol tests
- Unsupported Responses features are cleanly rejected with descriptive 400 errors
- Bearer-token auth boundary is intact for the new endpoint
