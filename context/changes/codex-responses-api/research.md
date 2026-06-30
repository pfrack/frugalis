---
date: 2026-06-30T08:09:36+02:00
researcher: opencode / MiniMax-M3 via /10x-research
git_commit: b4a154e1d8eff8e11d4de3c082eb7673897dd92a
branch: code-structure-reorg-ext
repository: pfrack/frugalis
topic: "codex-responses-api — S-21 OpenAI Responses API shim for Codex CLI"
tags: [research, s-21, openai-responses-api, codex-cli, protocol-translation, sse-streaming, httpmock, code-structure-reorg-ext]
status: complete
last_updated: 2026-06-30
last_updated_by: opencode / MiniMax-M3
---

# Research: codex-responses-api (S-21)

**Date:** 2026-06-30T08:09:36+02:00
**Researcher:** opencode / MiniMax-M3 via /10x-research
**Git Commit:** `b4a154e1d8eff8e11d4de3c082eb7673897dd92a`
**Branch:** `code-structure-reorg-ext`
**Repository:** `pfrack/frugalis`

## Research Question

> Research the codebase to inform `/10x-plan codex-responses-api` — a new
> `POST /v1/responses` endpoint so OpenAI Codex CLI can use Frugalis as a drop-in
> gateway. Comprehensive plan-grade research across: (1) the OpenAI Responses API
> spec + Codex CLI traffic patterns; (2) the Responses ↔ Chat Completions ↔
> Anthropic Messages translation matrix; (3) the current `src/` surface after
> the code-structure-reorg-ext reorganization; (4) the test strategy mirroring
> the S-18 (claude-code-compat) 4-way matrix.

## Summary

S-21 adds the missing protocol leg so Codex CLI — which now speaks **only**
`/v1/responses` (the `wire_api = "chat"` path was removed in Codex's `main`
branch) — can use Frugalis. The existing 3,084-line bidirectional
OpenAI↔Anthropic translator under `src/protocol/{request,response,stream}.rs`
plus the handler/cascade/cache/logging pipeline in `src/proxy/handlers.rs:155-948`
is the foundation. **Recommended architecture: path (a) — Responses →
Chat Completions → existing core → Responses.** This reuses the entire cascade,
keeps new code bounded to ~1,500-2,000 lines of a new `src/protocol/responses.rs`
+ `src/protocol/responses_stream.rs` + `src/proxy/responses_handler.rs`, and
matches the roadmap's "translation layer on top of the existing
`/v1/chat/completions` core" intent.

Key fidelity limits and how to handle them:
- **Reasoning streams are best-effort:** Chat Completions has no first-class
  reasoning wire field; `delta.reasoning_content` chunks from DeepSeek/DeepInfra-style
  providers accumulate into a single `summary_text` per `response.reasoning_summary_text.delta`
  burst. Anthropic upstreams get higher reasoning fidelity because
  `src/protocol/stream.rs:429-650` already translates Anthropic `thinking_delta`
  → Chat `reasoning_content` on the upstream leg.
- **`previous_response_id` is supported only via "re-send full transcript"
  mode in S-21's first ship.** Codex CLI does this by default with
  `store: true`, so no server-side transcript store is needed.
- **Responses-only fields are rejected with 400** in Phase 1 (built-in tools
  `web_search`/`code_interpreter`/`mcp_*`/`computer_use`/`image_generation`/
  `apply_patch`/`shell`, `text.format = grammar`, `prompt`, `conversation`,
  `background: true`, `truncation`, `include`, `prompt`, `tool_choice.allowed`,
  parallel `tool_choice` types). No upstream we control implements these in
  S-21's scope.
- **`store: true` is **not** honored by the gateway** in Phase 1; the gateway
  does not store transcripts. Document explicitly; future phases can add a
  Postgres-backed `TranscriptStore`.

Phased rollout: **5 phases**, each independently testable + shippable behind the
existing bearer-token `proxy_auth_layer`. Phase 1 is non-streaming only; Phase 2
adds streaming; Phase 3 wires reasoning + Anthropic routing + cache keying;
Phase 4 adds `previous_response_id` persistence + dashboard column; Phase 5
publishes OpenAPI + E2E tests. Test budget grows from S-18's ~30 tests to
~38 tests across a 9-cell matrix (3 client protocols × 3 upstream provider types,
2 modes × optional axes).

---

## Detailed Findings

### A. Current `src/` surface (post `code-structure-reorg-ext`)

After the recent code-structure-reorg and `code-structure-reorg-ext`, the
"2,763-line bidirectional translator" referenced by the S-18 plan now lives
across **three sibling modules** under `src/protocol/`:

| File | Lines (approx) | Role |
|---|---|---|
| `src/protocol/request.rs` | 1,332 | OAI↔Anth body translation; S-18 cache_control detection (`request.rs:528-571`) and auto-insert (`request.rs:122-124`) |
| `src/protocol/response.rs` | 834 | Non-streaming response + error translation; usage-token round-trip (`response.rs:108-149`, `:281-320`) |
| `src/protocol/stream.rs` | 918 | `StreamTranslateState` (Anth→OAI, `:8-54`) + `AnthropicStreamState` (OAI→Anth, `:385-425`) + SSE parsers/translators |

Handlers:

| Handler | Lines | Behaviour |
|---|---|---|
| `completion_handler` (POST `/v1/chat/completions`) | `proxy/handlers.rs:155-948` | Classify → cascade; if upstream Anthropic, rewrite body and emit via `handle_anthropic_streaming_response` (`streaming.rs:207-355`); if OpenAI-compat, byte-pipe via `handle_streaming_response` (`streaming.rs:17-107`). |
| `messages_handler` (POST `/v1/messages`) | `proxy/handlers.rs:963-1614` | Mirror: Anthropic→Anthropic pass-through (`:1272`) or Anthropic→OpenAI translation (`:1242`) via `handle_translating_anthropic_stream` (`streaming.rs:360-504`). |

Routes register in `src/app/mod.rs:318-330` (`proxy_routes` wrapped in
`routing::proxy_auth_layer(...)`). State is threaded as `State<Arc<AppState>>`
(`src/app/mod.rs:21-41`).

SSE translator coverage already in place that S-21 will reuse:
- `src/protocol/stream.rs:124-366` — Anthropic→OpenAI SSE (`message_start`,
  `content_block_start` (text/thinking/tool_use), `content_block_delta`
  (`text_delta`/`thinking_delta`/`input_json_delta`), `content_block_stop`,
  `message_delta` (stop_reason + usage), `message_stop`).
- `src/protocol/stream.rs:429-650` — OpenAI→Anthropic SSE: `[DONE]` parsing,
  reasoning_content → `thinking` block, content → `text` block, tool_calls →
  `tool_use` block with `input_json_delta`.

Header forwarding (`anthropic-*`, `x-claude-code-*`) is centralised in
`src/proxy/util.rs:462-475` (`collect_forward_headers`) and re-emitted via
`src/classification/llm.rs:251-312` (`auth_headers_for`) only when upstream
is Anthropic. **Allowlist omission flagged in §D:** S-21 needs `openai-beta`
to pass through for OpenAI feature-gated Responses traffic.

Persistence: `src/proxy/util.rs:218-340` builds an `InferenceRecord`
(`src/persistence/types.rs:106-134`) carrying the S-18 fields `input_tokens`,
`output_tokens`, `cache_read_tokens`, `cache_creation_tokens`,
`client_session_id`. Snippet extraction (`types.rs:148-237`) is JSON-aware but
**assumes a top-level `messages: [...]` field** — Responses-shaped bodies need
an adapted extractor.

#### File-touch summary (S-21's surface)

```
src/
├── app/mod.rs                       (route registration — additive 1 route)
├── proxy/handlers.rs                (existing untouched; test entries appended)
├── proxy/responses_handler.rs       (NEW — ~200 lines, Axum handler)
├── proxy/streaming.rs               (existing; reuse handle_streaming_response)
├── proxy/responses_streaming.rs     (NEW — ~150 lines, optional SSE prefix/suffix)
├── proxy/util.rs:466                (allowlist openai-beta)
├── protocol/mod.rs                  (declare new modules)
├── protocol/request.rs              (NEW fns: responses_request_to_chat, …)
├── protocol/response.rs             (NEW fns: chat_to_responses_response, …)
├── protocol/stream.rs               (NEW fns: parse_sse_events_typed, AnthropicToResponsesStreamState, ChatToResponsesStreamState)
├── protocol/responses.rs            (NEW ~600-800 lines — pure translator)
├── protocol/responses_stream.rs     (NEW ~800-1000 lines — SSE translator + state machine)
├── persistence/types.rs:106-134     (add previous_response_id: Option<String>)
├── persistence/{mod,sql_backend,memory}.rs   (Phase 4 only: TranscriptStore trait)
├── migrations/2026XXXX_add_previous_response_id.sql  (Phase 4 only)
└── routing/routes.rs:17             (add "openai_responses" to allowlist, Phase 2+)
```

This sits well within the AGENTS.md rule "Add new authentication schemes or
routes to existing modules rather than creating separate files" — except for
the two genuinely new top-level protocol modules, which the rule accommodates
("new components: look at existing components to see how they're written").
Co-location note from `context/foundation/lessons.md`: "Always group source
files into domain-named subdirectories ... when a module exceeds 2-3 files
or crosses subsystem boundaries" — `src/protocol/responses.rs` and
`src/protocol/responses_stream.rs` are co-located with sibling request/
response/stream files, which is the right domain.

### B. OpenAI Responses API spec + Codex CLI traffic patterns

Sources: `openai/openai-openapi` (v2.3.0), `openai/openai-python` SDK types,
`openai/codex` Rust source (current main), `platform.openai.com/docs`, and
`docs.anthropic.com` extended-thinking guide. Open issues #30403
(X-OpenAI-Internal-Codex-Responses-Lite regression), #29631 (local model
workaround), #30585 (xhigh rejection) confirmed against the current Codex
main branch.

#### Request shape — `POST /v1/responses`

**Required:** `model`, `input`. `input` is `string | InputItem[]`.
`InputItem` is a `oneOf` of ~30 variants including `message`,
`function_call`, `function_call_output`, `reasoning`, `item_reference`,
`web_search_call`, `file_search_call`, `computer_call`,
`code_interpreter_call`, `image_generation_call`, `local_shell_call`,
`local_shell_call_output`, `apply_patch_tool_call`,
`apply_patch_tool_call_output`, `mcp_list_tools`, `mcp_approval_request`,
`mcp_approval_response`, `mcp_tool_call`, `custom_tool_call`,
`custom_tool_call_output`, `compaction_summary`, and Codex-specific
`agent_message`. Discriminator: `type`.

**Notable optional fields** (with mapping notes for S-21):
- `instructions` (string OR array of items) — bound to Responses `system` field.
- `reasoning: {effort: none|minimal|low|medium|high|xhigh, summary: auto|concise|detailed}` — **no token budget**; OpenAI manages reasoning server-side.
- `text: {format: {type: text|json_object|json_schema, …}, verbosity: low|medium|high}`.
- `tools: Tool[]` discriminated by `type`: `function`, `custom`, `file_search`, `web_search`, `computer`, `code_interpreter`, `image_generation`, `mcp`, `shell`, `namespace`, `tool_search`, `apply_patch`.
- `tool_choice`: string (`auto|none|required`), object `{type:"function", function:{name}}`, `{type:"allowed", …}`, `{type:"<built-in-tool>", …}`.
- `previous_response_id` (string; mutually exclusive with `conversation`).
- `conversation: {id: "conv_…"}` (mutually exclusive with `previous_response_id`).
- `store: bool` (default true), `background: bool` (default false), `truncation: auto|disabled` (default disabled), `include: string[]` (`reasoning.encrypted_content` is the most-used value).
- `max_output_tokens`: integer, **minimum 16** (counts reasoning tokens too).
- `prompt_cache_key`, `prompt_cache_retention: in_memory|24h`, `safety_identifier`, `service_tier`, `top_logprobs`, `metadata`, `parallel_tool_calls`, `max_tool_calls`.

#### Response shape (non-streaming)

`Response` is `allOf` of `ModelResponseProperties`, `ResponseProperties`,
and per-spec extras. Top-level fields:

```jsonc
{
  "id": "resp_67ccd3a9…",           // generated server-side
  "object": "response",
  "status": "completed|failed|in_progress|cancelled|queued|incomplete",
  "created_at": 1718918400,          // unix seconds
  "completed_at": 1718918412,        // null until done
  "error": { "code": "string", "message": "string", "param": null } | null,
  "incomplete_details": { "reason": "max_output_tokens|content_filter" } | null,
  "output": [ /* ResponseOutputItem[] */ ],
  "instructions": "<echoed>",
  "model": "<upstream model>",
  "previous_response_id": null,
  "reasoning": { "effort": "medium", "summary": "auto" },
  "parallel_tool_calls": true,
  "temperature": null,
  "tool_choice": "auto",
  "tools": [ /* echoed Tool[] */ ],
  "top_p": null,
  "max_output_tokens": null,
  "truncation": "disabled",
  "metadata": null,
  "usage": {
    "input_tokens": 328,
    "input_tokens_details": { "cached_tokens": 0 },
    "output_tokens": 52,
    "output_tokens_details": { "reasoning_tokens": 0 },
    "total_tokens": 380
  },
  "output_text": "..."            // SDK-computed convenience; Frugalis should set
}
```

`output[]` items: `message` (`id`, `role: "assistant"`, `content: OutputMessageContent[]` of `output_text` or `refusal`, `status`, optional `phase` for `commentary|final_answer`), `reasoning` (`id`, `summary: SummaryTextContent[]`, `content: ReasoningTextContent[]`, `encrypted_content`), `function_call`, `web_search_call`, `file_search_call`, `computer_call`, `image_generation_call`, `local_shell_call`, `apply_patch_tool_call`, `mcp_*`, `custom_tool_call`, `tool_search_call`. Codex adds `agent_message`, `additional_tools` internally.

#### Streaming SSE events

Wire format: standard SSE — `event:` line (or just `data:`) with JSON payload,
blank-line terminator. **No `[DONE]` sentinel** — the stream ends after the
terminal `response.completed` / `response.failed` / `response.incomplete` event.
Every event carries a `sequence_number` integer (monotonic, starts at 0).

The ResponsesStreamEvent schema is a 41-variant `anyOf`. The events relevant
to S-21's Chat/Anthropic shim are ~10:

**Lifecycle:** `response.created` (first event, status=`in_progress`),
`response.in_progress` (optional), `response.queued` (background only),
`response.completed` (terminal, full `Response` payload),
`response.failed` (terminal, `response.error` populated),
`response.incomplete` (terminal with `incomplete_details.reason`),
`error` (out-of-band).

**Output items (paired added/done):**
- `response.output_item.added{output_index, item: OutputItem}` — fires per new item.
- `response.output_item.done{output_index, item: OutputItem}` — fires per finalized item.

**Assistant text content (per content-part pair):**
- `response.content_part.added{item_id, output_index, content_index, part}` — `part.type="output_text"`, empty `text`.
- `response.output_text.delta{item_id, output_index, content_index, delta, logprobs[]}`.
- `response.output_text.done{item_id, output_index, content_index}` (final text already in `output_item.done`).
- `response.content_part.done{item_id, output_index, content_index, part}`.

**Refusals (per content-part pair):** `response.refusal.delta`, `response.refusal.done`.

**Function calls (paired):**
- `response.function_call_arguments.delta{item_id, output_index, delta}` (JSON string fragment).
- `response.function_call_arguments.done{item_id, name, output_index, arguments}` (final JSON string).

**Reasoning (paired for both summary and raw content):**
- `response.reasoning_summary_part.added{type, item_id, output_index, summary_index, part}` (`part.type="summary_text"`, empty `text`).
- `response.reasoning_summary_text.delta{type, item_id, output_index, summary_index, delta}`.
- `response.reasoning_summary_text.done{type, item_id, output_index, summary_index, text}`.
- `response.reasoning_text.delta{type, item_id, output_index, content_index, delta}` (raw CoT).
- `response.reasoning_text.done{type, item_id, output_index, content_index, text}`.

**Built-in tool lifecycles** (not reached by S-21's Chat-Core shim path but
documented): `response.web_search_call.in_progress|searching|completed`,
`response.file_search_call.*`, `response.code_interpreter_call.in_progress|interpreting|completed`,
`response.code_interpreter_call_code.delta|done`, `response.image_generation_call.*`,
`response.mcp_call.*` + `response.mcp_list_tools.*`, `response.custom_tool_call_input.delta|done`.

**Observed Codex CLI sequence (text + tool case):**
```
response.created{status:in_progress}
response.output_item.added{reasoning, summary:[], content:[]}    ← if reasoning model
response.reasoning_summary_part.added{summary_text:""}
response.reasoning_summary_text.delta{…"think"…}
response.reasoning_summary_text.done
…
response.output_item.done{reasoning}
response.output_item.added{message, role:assistant, content:[]}
response.content_part.added{output_text:"", text:""}
response.output_text.delta{"Hi"}
response.output_text.delta{" there"}
response.output_text.done
response.content_part.done
response.output_item.done{message, content:[…text, annotations], status:completed}
[optional function_call items follow added → arguments.delta → arguments.done → done cycle]
response.completed{status:completed, usage:{…}}
```

#### Codex CLI in practice (most important findings)

Sources: `codex-rs/codex-api/src/common.rs:172-194`, `codex-rs/codex-api/src/provider.rs`, `codex-rs/codex-api/src/requests/headers.rs`, `codex-rs/model-provider-info/src/lib.rs:71-90`, `codex-rs/protocol/src/{models,openai_models}.rs`.

1. **Wire API is locked to Responses.** `pub enum WireApi { #[default] Responses }`
   in `codex-rs/model-provider-info/src/lib.rs:71-90` rejects `wire_api = "chat"`
   at deserialize time with: *"How to fix: set `wire_api = "responses"` in your
   provider config."* There is **no** Chat Completions path.
2. **The actual `ResponsesApiRequest` struct sent:**
   ```rust
   pub struct ResponsesApiRequest {
       pub model: String,
       pub instructions: String,                  // skip_serializing_if empty
       pub input: Vec<ResponseItem>,
       pub tools: Option<Vec<serde_json::Value>>,
       pub tool_choice: String,                   // always "auto" in Codex
       pub parallel_tool_calls: bool,             // false when use_responses_lite
       pub reasoning: Option<Reasoning>,
       pub store: bool,                           // always true
       pub stream: bool,                          // always true
       pub include: Vec<String>,                  // includes "reasoning.encrypted_content"
       pub service_tier: Option<String>,
       pub prompt_cache_key: Option<String>,
       pub text: Option<TextControls>,
   }
   ```
3. **Headers Codex sends** (`codex-rs/codex-api/src/requests/headers.rs`):
   - `Authorization: Bearer <OPENAI_API_KEY>` (or ChatGPT-account bearer)
   - `version: <CARGO_PKG_VERSION>`
   - `OpenAI-Organization`, `OpenAI-Project` (optional)
   - `OpenAI-Beta: responses_websockets=2026-02-06` (WebSocket handshake only)
   - `session-id: <uuid>` (always, when known)
   - `thread-id: <thread id>` (always, when known)
   - `x-codex-turn-state`, `x-codex-turn-metadata`, `x-codex-installation-id`,
     `x-codex-parent-thread-id`, `x-codex-window-id`, `x-openai-subagent`,
     `x-openai-memgen-request`, `x-responsesapi-include-timing-metrics`
   - `x-openai-internal-codex-responses-lite: true` (only when
     `model_info.use_responses_lite` is set; issue #30403 documents this header
     regresses 400 for some models — gateway should be lenient)
4. **Reasoning effort enum Codex actually emits:** `None | Minimal | Low |
   Medium | High | XHigh | Max | Ultra | Custom(String)`; **Ultra is
   internally remapped to Max before wire serialization** (per
   `reasoning_effort_for_request`). Wire values: `none`, `minimal`, `low`,
   `medium`, `high`, `xhigh`, `max`. Don't reject `xhigh`/`max`.
5. **Reasoning summary enum:** `Auto | Concise | Detailed | None` (None
   deprecated). Maps to `reasoning.summary` in the wire payload.
6. **Tool calls Codex actually issues:** `function_call`,
   `local_shell_call` (Codex's preferred shell tool — **not** a function
   tool; any shim that only knows `function_call` will misinterpret),
   `apply_patch_tool_call` (Codex's structured file-edit tool),
   `web_search_call`, `image_generation_call`, `custom_tool_call`,
   `reasoning`, `mcp_tool_call`, `tool_search_call`.
7. **Multi-turn state:** Codex uses both state mechanisms:
   - **Default (`store: true`, `previous_response_id`):** each turn sends
     only new user turn + previous `response.id`.
   - **Stateless fallback (`store: false`, `include: ["reasoning.encrypted_content"]`):**
     Codex sends full `input[]` each turn, with previous reasoning items
     carrying `encrypted_content` blobs.
8. **WebSocket fallback:** Codex attempts Responses-over-WebSocket first for
   OpenAI upstreams; if the gateway doesn't support it (no `wss://`), falls
   back to HTTP `/v1/responses`. Default WS connect timeout 15s.
9. **Compression:** Codex sends `Accept-Encoding: gzip` (enable_request_compression
   feature flag) on POST bodies.
10. **Built-in providers Codex ships with:**
    - `openai` (`https://api.openai.com/v1`, requires OpenAI auth,
      `supports_websockets: true`)
    - `amazon-bedrock` (`https://bedrock-mantle.us-east-1.api.aws/openai/v1`,
      SigV4 auth, `supports_websockets: false`, requires header
      `x-amzn-mantle-client-agent: codex`)
    - `ollama` (`http://localhost:11434/v1`, `requires_openai_auth: false`)
    - `lmstudio` (`http://localhost:1234/v1`, `requires_openai_auth: false`)
    - Ollama and LM Studio still use `WireApi::Responses` — Codex sends
      Responses-API requests even to local servers. **A local gateway is
      therefore indistinguishable from "OpenAI from Codex's POV" as long as
      it speaks `/v1/responses`**.

#### Anthropic asymmetry

Anthropic's reasoning model differs structurally:

| Aspect | Anthropic Messages | OpenAI Responses |
|---|---|---|
| Request-side | `thinking: {type:"enabled", budget_tokens: N}` (integer budget) | `reasoning: {effort, summary}` (qualitative enum) |
| Response-side | `thinking` content block with `signature` for redaction | `reasoning` output item with `summary[]` and `content[]` (raw CoT) and `encrypted_content` |
| Omission mode | `thinking: {type:"disabled"}` or `{type:"adaptive"}` | `reasoning.effort: "none"` (model-dependent availability) |
| Combined with `tool_choice: "any"`/`"tool"` | **Forbidden** (400) | `tool_choice: "required"` works normally |
| Cross-vendor signature portability | — | — | Anthropic `signature` ≠ OpenAI `encrypted_content`; cross-vendor round-tripping only works via server-side translation |

Reasoning-effort → budget_tokens heuristic (Sonnet 4.x-class; tune per model):
`none` → omit; `minimal` → 1024; `low` → 2048; `medium` → 8192; `high` →
16384; `xhigh`/`max` → 32768. Lossy — these are budgets, not effort levels.

### C. Translation matrix

#### Architectural decision

Three options compared:

| Path | Pros | Cons |
|---|---|---|
| **(a) Responses → Chat Completions → existing core → Responses** *(recommended)* | Reuses all of `src/protocol/{request,response,stream}.rs` (3,084 LOC); reuses cascade+cache+logging+streaming in `completion_handler` (`handlers.rs:155-948`); S-18 cache_control auto-insert (`request.rs:122-124`) still works for Anthropic upstreams; bounded new code (~1,500-2,000 lines) | Reasoning fidelity lossy — Chat Completions has no first-class reasoning wire field; `previous_response_id` must re-send full transcript; Responses-only fields (`built-in tools`, `prompt`, `include`, `conversation`, `background`, `truncation`, `grammar format`, `tool_choice.allowed`) must be rejected with 400 in Phase 1; SSE event-state machine is fancier than what's in `stream.rs` today |
| (b) Responses → Anthropic Messages directly when upstream is Anthropic, else (a) | Perfect reasoning fidelity on Anthropic leg | **Mixed-protocol problem**: cascade fallback would re-translate the Responses body two different ways per attempt; `reasoning.effort → budget_tokens` mapping is approximate; test surface balloons to ~18 distinct paths; duplicates a lot of the Chat↔Anthropic translator code |
| (c) Native `/v1/responses` handler parallel to Chat Core | Highest Responses-spec fidelity | Duplicates the entire classification+cascade+cache+keepalive+logging pipeline, or refactors `completion_handler`/`messages_handler` first (massive blast radius); bespoke 1,500+ line streaming emitter with ~30 distinct event types; two release trains forever |

**Recommendation: path (a).** Rationale: the roadmap's "translation layer on
top of the existing `/v1/chat/completions` core" survives the deeper requirements.
Codex CLI's primary use cases (tool calls, streaming text, function-call) are
well-served by Chat Completions. Reasoning is the one lossy axis and Codex CLI
tolerates one-accumulated-summary-per-turn. New code is bounded; no refactor of
existing handler.

#### Request: Responses → Chat Completions

The shim's `protocol::responses::request_to_chat` translates Responses into a
Chat-Completions-shaped body and hands off to `completion_handler`:

| Responses field | Chat field | Transformation / loss |
|---|---|---|
| `model` | `model` | Pass-through |
| `instructions: string \| items[]` | `messages: [{role: "system", content}]` | String pass-through; items → concatenate `text` parts (mirror `request.rs:419-431`) |
| `input: string \| InputItem[]` | `messages: [...]` | **Lossy.** Walker understands: `EasyInputMessage`, `Message`, `FunctionCallOutput`, `ItemReference`; `Reasoning` → elided (no Chat equivalent); built-in tool calls (`web_search`, `file_search`, `computer_use`, `code_interpreter`, `mcp_*`, `image_generation`, `apply_patch`, `shell`) → **rejected with 400 "tool not supported"** |
| `max_output_tokens` | `max_tokens` | Rename; null → omit |
| `temperature`, `top_p` | (same) | Pass-through |
| `parallel_tool_calls` | (same) | Pass-through |
| `tools[].function` | `tools[].function` | Filter union to `type: "function"`; reject built-in tools with 400 |
| `tool_choice` | `tool_choice` | Reduce to Chat's 4 shapes: `auto|none|required|{type:"function", function:{name}}`; other variants → 400 |
| `text.format` | `response_format` | `text` → omit; `json_object` → `response_format.json_object`; `json_schema` → `response_format.json_schema.{name,schema,strict}`; `grammar`/`python` → 400 |
| `text.verbosity` | — | Drop (no Chat equivalent) |
| `reasoning` | — | **Lossy.** Extract `reasoning.effort`/`reasoning.summary` into shim-local `ResponsesRequestExtras`; drop from Chat body. Will be used to synthesize `reasoning` output items + `response.reasoning_*` events |
| `stream` | `stream` | Pass-through |
| `previous_response_id` | — | **Lossy.** See §D statefulness |
| `conversation`, `store`, `background`, `truncation`, `include`, `prompt`, `prompt_cache_retention`, `max_tool_calls` | — | Drop (no Chat equivalent); `background: true` → 400 |
| `prompt_cache_key` | `user` | **Field rename** (Chat's `user` is OpenAI's cache-bucketing field) |
| `safety_identifier` | `safety_identifier` | Pass-through |
| `metadata` | `metadata` | Pass-through (clamp to Anthropic's max-4-key/64-char-key limit if it goes back through `request.rs`) |
| `service_tier`, `top_logprobs`, `user` (deprecated) | (same / pass-through) | Pass-through |

#### Request: Responses → Anthropic Messages (NOT recommended for Phase 1)

Documented here only because §1 referenced it. **Not implemented in S-21**. If
path (b)/(c) is adopted in a future change, the matrix is already mapped:
`instructions` → `system`; `input[]` walker → `messages[]` blocks including
`thinking` (from `ResponseReasoningItem`); `reasoning.effort` →
`thinking.budget_tokens` (table in §B); `tool_choice` → `{type:"auto"|"any"|"tool",name}`;
`prompt_cache_key` → drop (Anthropic has no equivalent; S-18's
cache_control auto-insert already covers the Anthropic side);
`max_output_tokens` → `max_tokens` (Anthropic requires it).

#### Response: Chat Completions / Anthropic → Responses

The shim's `protocol::responses::response_from_chat` (non-streaming) and
`protocol::responses::translate_*_stream_chunk_to_responses` (streaming)
produce Responses-shaped output.

**Non-streaming response synthesis:**

| Responses field | Chat source | Notes |
|---|---|---|
| `id` (e.g. `resp_<uuid>`) | **Synthesised** — `"resp_" + uuid7()` | Required so `previous_response_id` round-trips. Use `Uuid::now_v7()` (uuid crate, already in `Cargo.lock`) |
| `object` | Constant `"response"` | |
| `created_at` | `Utc::now().timestamp()` at shim entry | |
| `completed_at` | Same as `created_at` (non-streaming: complete on emission) | |
| `status` | `finish_reason` | `stop\|tool_calls` → `completed`; `length` → `incomplete{reason:"max_output_tokens"}`; `content_filter` → `incomplete{reason:"content_filter"}`; upstream 5xx → `failed` |
| `error` | Synthesised from upstream error body | Reuse existing `upstream_error_json`/`anthropic_error_json` (`util.rs:410-465`) |
| `incomplete_details` | `finish_reason: "length"` → `{reason:"max_output_tokens"}`; `finish_reason: "content_filter"` → `{reason:"content_filter"}` | |
| `instructions`, `metadata`, `model`, `parallel_tool_calls`, `temperature`, `tool_choice`, `tools`, `top_p`, `max_output_tokens`, `safety_identifier`, `service_tier`, `truncation`, `previous_response_id`, `reasoning` | **Echoed from request** | (OpenAI Responses returns the inputs it was called with) |
| `output[]` items | Chat `choices[0].message` synthesis (see below) | |
| `output_text` | Concatenated `output[]` `output_text` parts | SDK convenience; Codex CLI may inspect |
| `usage` | `usage` synthesis (see below) | |

**`output[]` items:**
| Chat piece | Responses `output[]` item |
|---|---|
| `reasoning_content: "..."` (non-empty, DeepSeek convention via `response.rs:77-80`) | `{type: "reasoning", id: "rs_<uuid>", summary: [{type: "summary_text", text: "..."}]}` |
| `content: "..."` | `{type: "message", id: "msg_<uuid>", role: "assistant", status: "completed", content: [{type: "output_text", text: "...", annotations: []}]}` |
| `content: null` + `tool_calls: [...]` | One message + one `{type: "function_call", id: "fc_<uuid>", call_id, name, arguments: "<json string>", status: "completed"}` per tool call |
| `refusal: "..."` | Message with `content: [{type: "refusal", refusal: "..."}]` |

**`usage` synthesis:**
| Responses field | Chat source |
|---|---|
| `input_tokens` | `usage.prompt_tokens - usage.prompt_tokens_details.cached_tokens` (saturating sub; mirrors `response.rs:298-303`) |
| `input_tokens_details.cached_tokens` | `usage.prompt_tokens_details.cached_tokens` |
| `output_tokens` | `usage.completion_tokens` |
| `output_tokens_details.reasoning_tokens` | **Set to 0** (cannot be reliably recovered from Chat Completions; `tracing::debug!` suppression). Anthropic upstreams will report it correctly via the round-trip chain. |
| `total_tokens` | `input_tokens + output_tokens` |

#### SSE event translation (critical path)

For Codex's primary Chat-Completions-upstream case, the shim's SSE emitter must
produce ~10 of the 41 Responses event types. The mapping table is the
**single source of truth** for the streaming test matrix:

| Upstream chunk | Responses event emitted by shim |
|---|---|
| Synthesised at shim entry (before first upstream chunk) | `response.created{response:{id: "resp_<uuid>", status: "in_progress", output: [], …}}` (once) |
| Same, optional | `response.in_progress` (once) |
| First Chat `delta.content` non-empty for an assistant message item | `response.output_item.added{output_index, item: {type: "message", id: "msg_<uuid>", status: "in_progress", content: []}}` (once per item) |
| Same moment | `response.content_part.added{item_id, output_index, content_index:0, part: {type: "output_text", text: "", annotations: []}}` |
| Every Chat `delta.content` non-empty | `response.output_text.delta{item_id, output_index, content_index:0, delta: "<text>", logprobs: []}` |
| First Chat `delta.tool_calls[i].id` non-null on a tool call | `response.output_item.added{output_index, item: {type: "function_call", id: "fc_<uuid>", call_id, name, arguments: "", status: "in_progress"}}` (once per tool call) |
| Every Chat `delta.tool_calls[i].function.arguments` non-empty | `response.function_call_arguments.delta{item_id, output_index, delta: "<partial JSON string>"}` |
| Chat chunk with `finish_reason: "stop"\|"tool_calls"\|"length"\|"content_filter"` | `response.output_text.done{item_id, output_index, content_index:0}` then `response.content_part.done{item_id, output_index, content_index:0, part: {…final text…}}` then `response.output_item.done{output_index, item: {type:"message", status:"completed", content:[…final…]}}`. For function calls: `response.function_call_arguments.done{item_id, name, arguments: "<final>"}` then `response.output_item.done{output_index, item: {type:"function_call", arguments: "<final>", status: "completed"}}` |
| Chat `data: [DONE]` (terminal for Chat) | `response.completed{sequence_number, response: {full Response with status: "completed", output: […], usage: {…}}}` |
| Every Chat `delta.reasoning_content` (when present) | `response.reasoning_summary_text.delta{…, delta: "<text>"}`. Before first delta: `response.output_item.added{item: {type: "reasoning", id: "rs_<uuid>", summary: []}}` + `response.reasoning_summary_part.added{summary_index:0, part: {type:"summary_text", text:""}}` |
| Chat `delta.refusal` content | `response.refusal.delta` / `response.refusal.done` (rare; mostly OpenAI-native) |
| Anthropic SSE upstream (the cascade's Anthropic-translated leg emits Chat SSE via `handle_anthropic_streaming_response` at `streaming.rs:207-355`): `content_block_start{type:"text", text:""}` | `response.content_part.added` (first content block) |
| Anthropic: `content_block_delta{type:"text_delta", text:"…"}` | `response.output_text.delta` |
| Anthropic: `content_block_start{type:"thinking", thinking:""}` + `content_block_delta{type:"thinking_delta", thinking:"…"}` | `response.reasoning_summary_part.added` + `response.reasoning_summary_text.delta` |
| Anthropic: `content_block_start{type:"tool_use", id, name, input:{}}` + `content_block_delta{type:"input_json_delta", partial_json:"…"}` | `response.output_item.added{type:"function_call"}` + `response.function_call_arguments.delta` |
| Anthropic: `message_delta{stop_reason, usage:{output_tokens,cache_read_input_tokens}}` | Synthesise terminal `response.completed` with `usage.input_tokens_details.cached_tokens` populated from the Anthropic `cache_read_input_tokens` |

**State machine:** the streaming translator needs a `ResponsesStreamState`
struct (see File Placement §8 below) mirroring `StreamTranslateState`
(`stream.rs:8-54`) and `AnthropicStreamState` (`stream.rs:385-425`) but with
multiple parallel output items + ID-keyed accumulation.

### D. Edge cases, statefulness, and header/cache_control interactions

#### `previous_response_id` and statefulness

OpenAI's Responses API supports two stateful-turn patterns:
`previous_response_id: "resp_..."` (server-side) and `conversation: {id: "conv_…"}`
(server-side, mutually exclusive). **Frugalis has no server-side transcript
store** — the persistence layer (`src/persistence/{mod,memory,sql_backend,types}.rs`)
is a one-shot inference logger keyed on `(category, prompt_snippet, timestamp, …)`,
not a transcript store. There is no "re-inject this turn's output into the next
turn's input" concept today.

Three storage options for S-21:

| Option | Storage | Cost | Fidelity |
|---|---|---|---|
| **A. Always re-send full transcript** *(recommended for S-21 ship)* | None — extract from `input[]` itself | Bandwidth ×2-3 per turn; no server state | High — Codex's default `store: true` path sends a sufficient `previous_response_id` for the gateway to resolve on the client side |
| B. In-memory LRU keyed by `response.id` | `HashMap<String, StoredResponse>` in `AppState` with TTL | O(turns) memory; lost on restart | High, when TTL > inter-turn gap |
| C. Postgres-backed transcript store | New `responses` table; new dashboard page; migration `006_*` | DB round-trip per turn | Highest; survives restart |

**Recommendation for S-21: option A.** Codex CLI sends the full `input[]` on
each new turn by default. Frugalis can either forward `previous_response_id`
verbatim (used by Codex for client-side history stitching) or treat it as a
cache key (since the existing `ResponseCache` keys on `sha256(input[])` —
`handlers.rs:218-230`, identical semantics). Either way, no new infrastructure
lands in S-21. **Document the bandwidth amplification explicitly.**

If `previous_response_id: "resp_<uuid>"` is set with a *partial* `input[]` (only
the new turn's messages, not the full transcript), the shim rejects with 400
(`previous_response_id is not supported on this gateway; send the full
transcript`) using the existing `upstream_error_json` envelope
(`util.rs:410-465`).

The `Conversation` API is **dropped** in Phase 1 (no `conversation: {id}`
support). Document it as "use `previous_response_id` instead" in the OpenAPI
doc Phase 5 ships.

**Persistence impact:** `InferenceRecord` (`persistence/types.rs:106-134`)
gains one field — `previous_response_id: Option<String>` — in Phase 4 only.
The migration `migrations/2026XXXX_add_previous_response_id.sql` is additive
(nullable). The dashboard's inferences page (`dashboard/handlers.rs:75-142`)
renders it as a clickable link when present; column can ship in a follow-up.

#### Tool-call argument encoding

Chat Completions streams arguments as **partial JSON string fragments**
concatenated into `function.arguments`. Responses does the same on
`response.function_call_arguments.delta`. **Mapping is direct — both sides are
JSON string deltas**, no conversion needed. Same for Anthropic `input_json_delta`.

If a provider emits tool-call arguments as a single accumulated JSON object
(no streaming), the shim still emits `response.function_call_arguments.delta`
events — one delta with the full arguments. `accumulated_tool_args` accumulator
handles it trivially.

#### Refusal handling

| Upstream behaviour | Responses emission |
|---|---|
| OpenAI native refusal (`message.refusal` + `finish_reason: "stop"`) | `response.output_item.added` (message with `output_text=""` part) → `response.content_part.added` (`{type:"refusal", refusal:""}`) → `response.refusal.delta` ×N → `response.refusal.done` → `response.output_item.done` |
| OpenAI `finish_reason: "content_filter"` | `response.incomplete` with `incomplete_details.reason: "content_filter"`, `status: "incomplete"`, no output items |
| Anthropic upstream filter (400 returned) | `response.failed` with `error.code = <status>`, `error.message = <translated message>` |
| Anthropic upstream returns empty content | Synthesise empty `output_text` part (existing fallback at `response.rs:264-266`) |

**Risk:** providers that 200-OK with empty content + no filter signal. No fix —
document.

#### Streaming backpressure, S-17 cascade, S-18 cache_control interactions

- **Backpressure:** Chat SSE and Responses SSE share byte-delimited `\n\n`
  convention. The shim's per-chunk emit overhead is constant-time; existing
  `streaming.rs:30` mpsc channel carries the larger Responses envelopes without
  capacity change.
- **S-17 cascade:** per-attempt body is always the same Chat-Completions body
  (since the shim translates once at handler entry); cascade cost unchanged.
- **S-18 cache_control + Responses:** OAI→Anth auto-insert (`request.rs:122-124`)
  still fires on Anthropic attempts — orthogonal to `previous_response_id`.
  Prompt-cache-key forwarded as Chat `user` field; Anthropic attempts ignore it
  (Anthropic's caching is content-based, not key-based) — correct behavior.
- **S-18 header forwarding:** `collect_forward_headers` (`util.rs:462-475`)
  only forwards `anthropic-*` and `x-claude-code-*`. OpenAI-shape headers are
  dropped, which is correct for Anthropic upstreams. For Responses API,
  Codex-CLI-sent `OpenAI-Beta: responses=v1` (and `OpenAI-Organization`,
  `OpenAI-Project`) need a **2-line addition** to the allowlist in
  `util.rs:466`: add `openai-beta`, `openai-organization`, `openai-project`.
  Otherwise feature-gated Responses traffic from Codex may be silently
  downgraded.
- **Routes registered behind same `proxy_auth_layer`:** `src/app/mod.rs:318-330`
  has the auth boundary already; adding `.route("/responses", post(…))`
  before the `.route_layer(...)` ensures `cargo test auth` and
  `cargo test routes_auth` remain green — no auth regression.

### E. Test strategy

The S-18 plan established a 4-way matrix (2 client protocols × 2 upstream
protocols) as the "good test coverage" bar. S-21 raises the bar to a 9-cell
matrix.

#### Current test inventory (vs. AGENTS.md)

Note: `AGENTS.md` says `mod tests`/`mod slow_tests` live in `src/main.rs`; that
arrangement is **stale** — the modules moved file-scoped during the reorg.
Update `AGENTS.md` when S-21 lands.

| Test module | File:line |
|---|---|
| `proxy::handlers::tests` | `src/proxy/handlers.rs:1700` |
| `proxy::handlers::tests::slow_tests` | `src/proxy/streaming.rs:1019` |
| `proxy::streaming::tests` | `src/proxy/streaming.rs:506` |
| `proxy::util::tests` | `src/proxy/util.rs:490` |
| `protocol::request::tests` | `src/protocol/request.rs:700` |
| `protocol::response::tests` | `src/protocol/response.rs:331` |
| `protocol::stream::tests` | `src/protocol/stream.rs:652` |
| `classification::llm::tests` | `src/classification/llm.rs:489` (httpmock) |
| `classification::chain::tests` | `src/classification/chain.rs:99` |
| `classification::regex::tests` | `src/classification/regex.rs:247` |
| `classification::fewshot::tests` | `src/classification/fewshot.rs:376` |
| `routing::auth::tests` | `src/routing/auth.rs:222` |
| `routing::routes::tests` | `src/routing/routes.rs:154` |
| `persistence::tests` | `src/persistence/mod.rs:60` |
| `persistence::types::tests` | `src/persistence/types.rs:254` |
| `cache::tests` | `src/cache.rs` |
| `dashboard::handlers::tests` | `src/dashboard/handlers.rs` |
| `config::tests` | `src/config/mod.rs:394` |
| `app::quickstart::tests` | `src/app/quickstart.rs:303` |

#### Helpers to reuse

| Helper | File:line |
|---|---|
| `test_app()` | `src/app/test_helpers.rs:260` |
| `test_app_with_classifier()` | `src/app/test_helpers.rs:292` |
| `test_app_with_http_client(env, max)` | `src/app/test_helpers.rs:114` (OpenAI-shaped) |
| `test_app_with_anthropic_http_client(env, max)` | `src/app/test_helpers.rs:187` |
| `test_app_with_cache(ttl, max)` | `src/app/test_helpers.rs:355` |
| `test_app_with_openai_translation(env)` | `src/proxy/handlers.rs:3001` |
| `build_app_with_persistence_backend(…)` | `src/persistence/mod.rs:77` (in-memory `MemoryBackend`) |
| `parse_json_body(response)` | `src/app/test_helpers.rs:455` |
| `parse_sse_events(bytes) -> Vec<(String,String)>` | `src/protocol/stream.rs:68` |
| `EnvGuard(&'static str)` | `src/test_util.rs:1` (RAII env-var isolation) |

#### Existing httpmock tests S-21 mirrors (the 4-way matrix reference)

| Test | Pair | Asserts |
|---|---|---|
| `test_messages_handler_non_streaming_passthrough` (`handlers.rs:2489`) | Anth→Anth | Body & `msg_1` id round-trip |
| `test_messages_handler_streaming_passthrough` (`handlers.rs:2591`) | Anth→Anth SSE | SSE bytes contain `message_start`, `content_block_delta` |
| `test_messages_handler_anthropic_passthrough_preserves_cache_control` (`handlers.rs:3089`) | Anth→Anth | `cache_control` block survives translation |
| `test_messages_handler_forwards_anthropic_client_headers` (`handlers.rs:2516`) | Anth→Anth | Inbound `anthropic-beta`/`anthropic-version`/`x-claude-code-session-id` reach upstream |
| `test_messages_handler_openai_translation_strips_cache_control` (`handlers.rs:3060`) | Anth→OAI | Upstream receives body **without** `cache_control` (canary mock asserts `hits() == 0`) |
| `test_messages_handler_openai_translation_non_streaming` (`handlers.rs:3110`) | Anth→OAI | Translated body shape matches Anthropic Messages |
| `test_messages_handler_openai_translation_streaming` (`handlers.rs:3150`) | Anth→OAI | OAI SSE chunks → Anthropic SSE events emitted |
| `test_messages_handler_openai_translation_error` (`handlers.rs:3185`) | Anth→OAI | Upstream 429 → Anthropic-shaped error body |
| `test_messages_handler_upstream_error_forwards_body` (`handlers.rs:2625`) | Anth→Anth | Upstream 429 body verbatim-passes-through |
| `test_completion_handler_anthropic_translation` (`handlers.rs:2827`) | OAI→Anth | OAI Chat Completions body becomes Anthropic shape upstream |
| `test_completion_handler_anthropic_translation_inserts_cache_control` (`handlers.rs:2878`) | OAI→Anth | Auto-inserted `cache_control` reaches upstream |
| `test_completion_handler_translates_cache_tokens_in_usage` (`handlers.rs:2899`) | OAI→Anth | `cache_read_input_tokens` → `usage.prompt_tokens_details.cached_tokens` |
| `test_completion_handler_anthropic_streaming` (`handlers.rs:2935`) | OAI→Anth SSE | Anthropic SSE → OAI `chatcmpl-…` + `[DONE]` chunks |
| `test_completion_handler_anthropic_error` (`handlers.rs:2972`) | OAI→Anth | Upstream 429 → OAI error envelope |
| `test_completion_handler_does_not_forward_anthropic_headers_to_openai` (`handlers.rs:2541`) | OAI→OAI | Negative canary: `anthropic-*` does NOT reach OAI upstreams |
| `test_streaming_handler_returns_sse_content_type` (`streaming.rs:524`) | OAI→OAI SSE | `Content-Type: text/event-stream`, `Cache-Control: no-cache` |
| `test_streaming_handler_forwards_upstream_bytes` (`streaming.rs:577`) | OAI→OAI SSE | Upstream bytes forwarded verbatim |
| `test_streaming_handler_non_2xx_returns_sse_error_event` (`streaming.rs:615`) | OAI→OAI SSE | Upstream 503 → `event: error\ndata: …` frame |
| `test_cache_hit_returns_cached_response` (`cache.rs:175`) | OAI→OAI | Second request hits cache, mock served once |

#### 9-cell test matrix for S-21

| # | Inbound | Upstream provider_type | Phase | Notes |
|---|---|---|---|---|
| **R1** | `/v1/responses` | `openai_compatible` | 1 | Responses → Chat protocol translates |
| **R2** | `/v1/responses` | `anthropic` | 1 | Responses → Anthropic Messages |
| **R3** | `/v1/chat/completions` | `openai_responses` | 3 | Bidirectional — Chat → Responses |
| **R4** | `/v1/messages` | `openai_responses` | 3 | Bidirectional — Anthropic → Responses |
| **R5** | `/v1/responses` | `openai_responses` | 2 | Passthrough; validates new provider_type |
| L1 | `/v1/chat/completions` | `openai_compatible` | — | S-15 regression |
| L2 | `/v1/messages` | `anthropic` | — | Passthrough regression |
| L3 | `/v1/messages` | `openai_compatible` | — | S-16 regression |
| L4 | `/v1/chat/completions` | `anthropic` | — | S-15 regression |

Phase priority: **add `/v1/responses` inbound + R1/R2 first** (Phase 1, ~16 tests).
**Add `openai_responses` provider_type + R5 passthrough second** (Phase 2, ~5 tests).
**Add R3 + R4 cross-protocol third** (Phase 3, ~12 tests). Statefulness Phase 4
adds ~5 tests. **Cumulative: ~38 new tests across 4 phases** — proportionate
to S-18's ~30 because the matrix is 50% larger (3 vs 2 in/out × optional axes).

#### Streaming-test design

Reuse the canonical httpmock pattern: `test_app_with_anthropic_http_client`
(`test_helpers.rs:187`) → httpmock `then.status(200).header("content-type",
"text/event-stream").body(sse_str)` → assert on aggregated SSE body bytes.

**Recommended extension:** add a typed SSE event struct to `src/protocol/stream.rs`
(currently returns `(String, String)` tuples):

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: serde_json::Value,
    pub raw: String,
}

pub fn parse_sse_events_typed(bytes: &[u8]) -> Vec<SseEvent> { … }
```

Tests then become:
```rust
let events = parse_sse_events_typed(&body);
assert_eq!(events[0].event, "response.created");
assert_eq!(events[0].data["response"]["status"], "in_progress");
```

#### Manual verification (per phase; bash mocks in `scripts/test.sh`)

```bash
# Phase 1: R1 non-streaming
curl -sS -X POST http://127.0.0.1:10000/v1/responses \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","input":[{"role":"user","content":"hello"}]}' | jq
# expect: .output[0].content[0].text == "…" (after upstream mock returns)

# Phase 1: R2 streaming
curl -N -sS -X POST http://127.0.0.1:10000/v1/responses \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"model":"claude-sonnet-4-6","stream":true,
       "input":[{"role":"user","content":"hello"}]}' | grep -c '^event: response\.'
# expect: ≥ 4 (created, output_item.added, output_text.delta, completed)
```

Add 5 bash-mock functions to `scripts/test.sh` and append to the `--auto` list.

#### CI integration

`/.github/workflows/ci.yml` needs only minor additions; no structural changes:

```yaml
- name: Run responses-API protocol tests
  run: |
    cargo test responses
    cargo test responses_handler
```

Existing `cargo test auth` and `cargo test routes_auth` must remain green;
add `test_responses_handler_requires_auth` to confirm the new route sits behind
the same `proxy_auth_layer`. Phase 4 transcript tests join the existing
`cargo test persistence_integration` block (already gated on `DATABASE_URL`).

No new Cargo dev-deps needed; `httpmock`, `serial_test`, `testcontainers`
already present. The SSE parser crate isn't needed — extend `parse_sse_events`
in-tree.

### F. Recommended file layout & 5-phase breakdown

See file-touch summary in §A. **Phase plan:**

- **Phase 1 — Non-streaming Responses (R1, R2).** Build `protocol::responses`
  (request + response translators), `proxy/responses_handler`, route registration,
  header allowlist tweak. ~16 new tests, ship behind `proxy_auth_layer`. Deliverable:
  Codex CLI can send `POST /v1/responses` non-streaming and receive a complete
  Response. ~7 days.

- **Phase 2 — Streaming Responses (OAI-compat).** Build `protocol::responses_stream`,
  `proxy/responses_streaming` (or extend `handle_streaming_response` with optional
  prefix/suffix envelopes). ~6 new tests. Deliverable: streaming Codex CLI works
  against OpenAI-compat upstreams end-to-end. ~5 days.

- **Phase 3 — Reasoning + Anthropic + cache keying.** Reasoning summary emission
  (best-effort from `delta.reasoning_content` and from Anthropic `thinking_delta`
  via the existing `stream.rs:429-650` chain). `ResponseCache` cache key as
  `sha256(input[])` extends the existing `cache.rs`. ~12 new tests. Deliverable:
  reasoning stream + Anthropic + cache hit on identical re-sends. ~5 days.

- **Phase 4 — Persistence + dashboard.** `InferenceRecord.previous_response_id`
  optional field + migration; optional `responses` transcript store (Phase 4
  priority: B in-memory LRU, C Postgres if scope allows). Dashboard inferences
  page new column. ~5 new tests. Deliverable: previous-turn attribution in the
  inference log. ~3 days.

- **Phase 5 — Documentation + E2E.** Publish `openapi/responses-shim.openapi.yaml`,
  README update, E2E test fixture. Optional Codex-CLI-on-mock end-to-end. ~3 days.

**Total: ~23 working days, comparable to S-18's runtime.**

---

## Code References (GitHub permalinks)

All permalinks anchor to commit `b4a154e1d8eff8e11d4de3c082eb7673897dd92a` on
`pfrack/frugalis` (the working branch `code-structure-reorg-ext` is not yet
merged to `main`).

### Existing surface (post-reorg) — what S-21 reuses or extends

- `src/main.rs` — entry point (untouched)
- `src/app/mod.rs:318-330` — route registration, `proxy_auth_layer` boundary — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/app/mod.rs#L318
- `src/app/test_helpers.rs:114, :187, :260, :292, :355, :455` — app test helpers
- `src/proxy/handlers.rs:155-948` — `completion_handler` (POST `/v1/chat/completions`) — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/proxy/handlers.rs#L155
- `src/proxy/handlers.rs:963-1614` — `messages_handler` (POST `/v1/messages`) — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/proxy/handlers.rs#L963
- `src/proxy/handlers.rs:1700` — `mod tests` entry point
- `src/proxy/streaming.rs:17-107` — `handle_streaming_response` (OAI SSE) — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/proxy/streaming.rs#L17
- `src/proxy/streaming.rs:207-355` — `handle_anthropic_streaming_response` — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/proxy/streaming.rs#L207
- `src/proxy/streaming.rs:360-504` — `handle_translating_anthropic_stream`
- `src/proxy/streaming.rs:506` — `proxy::streaming::tests` module
- `src/proxy/streaming.rs:1019` — `proxy::handlers::tests::slow_tests` module
- `src/proxy/util.rs:188-195` — `parse_usage_from_body`
- `src/proxy/util.rs:200-205` — `x-claude-code-session-id` capture for `client_session_id`
- `src/proxy/util.rs:218-340` — `enqueue_inference_record`
- `src/proxy/util.rs:248-271` — `log_classification_with_usage` (10-param sibling from S-18 impl-review)
- `src/proxy/util.rs:410-465` — `upstream_error_json`
- `src/proxy/util.rs:426-435` — `anthropic_error_json`
- `src/proxy/util.rs:462-475` — `collect_forward_headers`
- `src/proxy/util.rs:490` — `mod tests`
- `src/proxy/upstream.rs:21-28` — cascade model overrides
- `src/proxy/upstream.rs:41-46` — `auth_headers_for` chain
- `src/cache.rs:175` — `test_cache_hit_returns_cached_response`
- `src/protocol/request.rs:8` — `translate_request` signature (OAI→Anth)
- `src/protocol/request.rs:122-124` — top-level `cache_control: {type:"ephemeral"}` auto-insert (OAI→Anth)
- `src/protocol/request.rs:418-431` — `instructions` / system-message flattening
- `src/protocol/request.rs:528-571` — `anthropic_to_openai_request_with_cache_signal` (cache_control detection for Anth→OAI)
- `src/protocol/request.rs:649-697` — Anthropic thinking block synthesis
- `src/protocol/request.rs:700` — `mod tests`
- `src/protocol/request.rs:900` — `test_consecutive_tool_messages_merged`
- `src/protocol/response.rs:10` — `translate_response` signature (Anth→OAI)
- `src/protocol/response.rs:33-67` — Anthropic thinking block parsing
- `src/protocol/response.rs:77-80` — `reasoning_content` emission for Chat downstream (DeepSeek convention)
- `src/protocol/response.rs:108-149` — usage translation Anthropic → OpenAI
- `src/protocol/response.rs:175-191` — Anthropic error envelope mapping
- `src/protocol/response.rs:264-266` — empty Anthropic content fallback
- `src/protocol/response.rs:281-320` — usage translation OpenAI → Anthropic (saturating_sub invariant)
- `src/protocol/response.rs:298-303` — `saturating_sub` invariant on `cache_read_input_tokens + cache_creation_input_tokens + input_tokens`
- `src/protocol/response.rs:331` — `mod tests`
- `src/protocol/response.rs:683` — `test_a2o_response_text`
- `src/protocol/stream.rs:8-54` — `StreamTranslateState` (Anth→OAI SSE state)
- `src/protocol/stream.rs:68-119` — `parse_sse_events` existing helper
- `src/protocol/stream.rs:124-366` — `translate_anthropic_stream_chunk_to_openai`
- `src/protocol/stream.rs:233` — `reasoning_content` emit for Chat downstream
- `src/protocol/stream.rs:282-287` — `tool_index` advancement on block transitions (existing state-machine model)
- `src/protocol/stream.rs:385-425` — `AnthropicStreamState` (OAI→Anth SSE state)
- `src/protocol/stream.rs:429-650` — `openai_to_anthropic_stream_event` (Anthropic thinking → Chat reasoning_content)
- `src/protocol/stream.rs:435-442` — `[DONE]` terminator handling
- `src/protocol/stream.rs:652` — `mod tests`
- `src/protocol/stream.rs:787` — `test_parse_sse_events`
- `src/protocol/stream.rs:795` — `test_parse_sse_events_no_event_type`
- `src/protocol/stream.rs:828-842` — `test_a2o_stream_reasoning_delta`
- `src/protocol/stream.rs:898` — `test_a2o_stream_block_transition`
- `src/classification/llm.rs:251-312` — `auth_headers_for` re-emit
- `src/classification/llm.rs:307` — classifier self-probe call
- `src/classification/llm.rs:489` — `mod tests`
- `src/classification/llm.rs:558` — `llm_classifier_success` httpmock test (canonical pattern)
- `src/persistence/types.rs:106-134` — `InferenceRecord` struct — https://github.com/pfrack/frugalis/blob/b4a154e1d8eff8e11d4de3c082eb7673897dd92a/src/persistence/types.rs#L106
- `src/persistence/types.rs:133` — `client_session_id` field (analog for `previous_response_id`)
- `src/persistence/types.rs:148-175` — snippet extraction (OAI, JSON-array aware, assumes `messages: [...]`)
- `src/persistence/types.rs:191-237` — snippet extraction (Anthropic)
- `src/persistence/types.rs:254` — `mod tests`
- `src/persistence/mod.rs:60` — `persistence::tests` module entry
- `src/persistence/mod.rs:77` — `build_app_with_persistence_backend`
- `src/persistence/mod.rs:186` — `test_snippet_path_truncates_to_200_chars`
- `src/routing/auth.rs:74-89` — `ProxyBearerAuth` token consumption
- `src/routing/auth.rs:222` — `mod tests`
- `src/routing/routes.rs:17` — `ProviderEntry.provider_type` field (allowlist enum)
- `src/routing/routes.rs:154` — `mod tests`
- `src/routing/routes.rs:159` — `RouteEntry` deserialization
- `src/dashboard/handlers.rs:75-142` — dashboard inferences page handler

### New files S-21 lands

- `src/protocol/responses.rs` — pure translator functions (request + response)
- `src/protocol/responses_stream.rs` — streaming translator + `ResponsesStreamState`
- `src/proxy/responses_handler.rs` — Axum handler, ~200 lines
- `src/proxy/responses_streaming.rs` — SSE wrapper, ~150 lines (or prefix/suffix extension to `handle_streaming_response`)
- `migrations/2026XXXX_add_previous_response_id.sql` — Phase 4 only
- `openapi/responses-shim.openapi.yaml` — Phase 5 only

### Modified files S-21 touches

- `src/proxy/handlers.rs` — add `responses_handler`; add tests module entries
- `src/proxy/streaming.rs` — optional extension for Responses prefix/suffix envelopes
- `src/proxy/util.rs:466` — add `openai-beta`, `openai-organization`, `openai-project` to allowlist
- `src/protocol/request.rs` — add `responses_request_to_chat` (and optionally `responses_request_to_anthropic` per the §3 matrix)
- `src/protocol/response.rs` — add `chat_to_responses_response` (and optionally `anthropic_to_responses_response`)
- `src/protocol/stream.rs` — add `parse_sse_events_typed`, `AnthropicToResponsesStreamState`, `ChatToResponsesStreamState`
- `src/persistence/types.rs:106-134` — add `previous_response_id: Option<String>` (Phase 4)
- `src/persistence/{mod,sql_backend,memory}.rs` — Phase 4 only: `TranscriptStore` trait
- `src/routing/routes.rs:17` — Phase 2 only: add `"openai_responses"` allowlist value
- `src/app/mod.rs:318-330` — add `.route("/responses", post(responses_handler))` before `.route_layer(routing::proxy_auth_layer(...))`
- `scripts/test.sh` — add 5 bash-mock test functions
- `AGENTS.md` — update test-modules table (§1.1)
- `openapi/` directory — Phase 5 only: add `responses-shim.openapi.yaml`
- `README.md` — Phase 5 only: shim documentation + caveats

---

## Architecture Insights

1. **Path (a) — re-using `completion_handler` end-to-end — is the cheapest path
   to Codex-CLI compatibility.** The existing translator is purpose-built for
   Chat↔Anthropic; extending it with one more inbound protocol (Responses) at
   the boundary, with bidirectional translation, reuses 100% of the existing
   routing/cache/streaming/inference-logging infrastructure. The new code is
   bounded to ~1,500-2,000 lines across 4 new modules + 6 existing-file edits.

2. **The reasoning-fidelity tax is bounded and acceptable.** Chat Completions
   has no first-class reasoning wire field, so the shim's Responses
   `reasoning_*` events are best-effort accumulated from upstream
   `delta.reasoning_content` (DeepSeek/DeepInfra-style providers) or from
   Anthropic `thinking_delta` (via the existing two-leg translator chain).
   Codex's primary use case is OpenAI-direct (which doesn't emit reasoning on
   Chat Completions anyway), so the practical fidelity loss for Codex is zero.
   Reasoning streaming on Anthropic upstreams reaches the shim via the existing
   `src/protocol/stream.rs:429-650` reverse translator, so end-to-end fidelity
   is good.

3. **`previous_response_id` requires no new infrastructure if we accept the
   bandwidth amplification.** Codex CLI's default behavior (`store: true`,
   sends full `input[]` with `previous_response_id`) means the existing
   `ResponseCache` cache-key-on-`sha256(input[])` semantics already provide
   correct de-duplication. Phase 4 can add a `TranscriptStore` for an
   enterprise-tier "true stateful" experience; the MVP works without it.

4. **Header forwarding needs a 2-line tweak to support OpenAI Responses
   feature gating.** Codex CLI sends `OpenAI-Beta: responses=v1` (and
   `OpenAI-Organization`/`OpenAI-Project`). The existing allowlist
   (`util.rs:462-475`) drops them — add them to `util.rs:466` and Codex's
   feature-gated Responses traffic reaches the upstream intact.

5. **S-21 lowers the S-18 implementation risk profile.** The existing
   bidirectional translator already handles the hard cases (cache_control,
   thinking blocks, multi-leg protocol crossings, streaming). S-21 only adds
   the wrapper layer on top. The risk stays "high" only because of the SSE
   state-machine complexity — manageable with the typed `SseEvent` parser
   extension and the explicit per-event translation table.

6. **Lessons already in `context/foundation/lessons.md` that apply directly:**
   - "Handle upstream error bodies without full buffering where possible"
     (apply to upstream 5xx → Responses error envelope mapping).
   - "Document guard points with self-describing comments" (apply to the
     body-shape rejection points where Responses-only fields are 400'd).
   - "Re-run review after a follow-up change touches the same handler" — S-21
     touches `src/proxy/handlers.rs` and `src/proxy/streaming.rs`. The
     existing handlers' S-18 review fixes (F1-F4) must remain intact; the
     S-21 change must grep for and preserve them.
   - "Organize src/ into domain subdirectories" — the new files co-located
     under `src/protocol/` (request/response/stream + responses/responses_stream)
     is the right domain.

---

## Historical Context (from prior changes)

The S-21 change is the third leg of the proxy-protocol-completeness arc that
began with F-01 (auth) and ran through S-15 (OpenAI→Anthropic translation),
S-16 (Anthropic→OpenAI translation), S-17 (cascade), S-18 (Claude Code
compat — `cache_control` headers, `display_name`, cache-token usage, attribution
columns in `InferenceRecord`). Each prior leg left behind patterns S-21 must
honor:

- **S-15 (`translate-openai-to-anthropic`) — `context/archive/2026-06-22-translate-openai-to-anthropic/`.**
  Established the body-translation strategy: `serde_json::Value` with explicit
  per-field allowlists, no `#[serde(flatten)]` catch-alls. S-21 must follow
  the same pattern: explicit allowlist with rejections, no implicit pass-through.

- **S-16 (`translate-anthropic-to-openai`) — `context/archive/.../translate-anthropic-to-openai/`.**
  Added the `/v1/messages` endpoint. This is the closest analogue to S-21:
  a "make this client work" change that translates a wire format onto Chat
  Completions core. S-21's handler pattern (one new endpoint + body-shape
  translation at the boundary) is a direct copy of S-16's pattern.

- **S-17 (`provider-fallback-cascade`) — `context/archive/2026-06-24-provider-fallback-cascade/`.**
  Codified that each cascade attempt sends a fresh body to a new upstream.
  S-21 fits cleanly: the shim produces a Chat body at handler entry, and each
  cascade attempt reuses that body with no per-attempt re-translation.

- **S-18 (`claude-code-compat`) — `context/archive/2026-06-27-claude-code-compat/`.**
  Established the 4-way httpmock matrix that is the proof-of-completeness bar
  for protocol-completeness changes. S-21 raises the bar to a 9-cell matrix.
  S-18 also added: `cache_control` auto-insert (`request.rs:122-124`),
  `display_name` on `/v1/models`, cache-token usage translation
  (`response.rs:281-320`), and `client_session_id` capture
  (`util.rs:200-205`) into `InferenceRecord` (`persistence/types.rs:133`).
  S-21 extends these to Responses-shape: `previous_response_id` is the
  conceptual sibling of `client_session_id`.

- **Competitive-gap landscape research — `context/archive/2026-06-24-competitive-gap-model-routing/research.md`.**
  Established in its Tier-1 #5 that "*modern Codex CLI (Responses-API-only)
  cannot use Frugalis.*" S-21 closes that gap.

- **Open questions worth re-stating for S-21:**
  - **Storage backend for Phase 4 transcript store** — in-memory (Option B)
    vs. Postgres (Option C) vs. S3/Redis. The roadmap's small-scale, after-hours
    budget tilts toward Option B unless an existing Redis/Postgres dependency
    makes Option C free.
  - **`x-amzn-mantle-client-agent: codex` header** — required by AWS Bedrock
    Mantle gateway. If Frugalis ever proxies to Bedrock Mantle, this header
    must be forwarded. Document; defer unless Bedrock becomes a routing target.
  - **Codex's `x-openai-internal-codex-responses-lite` header** —
    `use_responses_lite: true` triggers it; per issue #30403 some backends
    reject it. Frugalis should be lenient: ignore if not applicable, forward
    if claims `use_responses_lite`. Decide whether to plumb this through
    `provider_type = "openai_responses"` config or keep it implicit.

---

## Related Research

- `context/archive/2026-06-24-competitive-gap-model-routing/research.md` —
  Tier-1 competitive gap analysis; item #5 is exactly S-21's origin.
- `context/archive/2026-06-27-claude-code-compat/plan.md` — S-18 plan,
  the closest sibling; its §"Testing Strategy" (lines 253-274) sets the
  template S-21 follows.
- `context/archive/2026-06-22-translate-openai-to-anthropic/research.md` —
  S-15 research; protocol-translation precedent.
- `context/archive/.../translate-anthropic-to-openai/research.md` — S-16
  research; endpoint-addition precedent.
- `context/foundation/roadmap.md` (S-21 entry, lines 519-529 + 60 + 698,
  Tier-1 #5) — the canonical S-21 definition.
- `context/foundation/lessons.md` — applies directly to S-21 (see §
  Architecture Insights #6).
- `context/changes/code-structure-reorg-ext/` — the most recent reorg;
  sets the post-reorg src/ tree layout that S-21 fits into.

---

## Open Questions

1. **`previous_response_id` storage backend (Phase 4 design).** In-memory LRU
   (Option B) vs. Postgres-backed `responses` table (Option C) vs.
   external store. Affects `TranscriptStore` trait surface, migration count,
   and dashboard page scope. Recommend deferring to `/10x-plan codex-responses-api`
   prep work.
2. **`x-amzn-mantle-client-agent` header forwarding.** Required only if Frugalis
   routes to AWS Bedrock Mantle. Decide in `/10x-plan` whether S-21 must
   forward it conditionally, or punt.
3. **`codex_session_id`-style extension for `x-codex-turn-state` /
   `x-codex-installation-id` / `x-codex-window-id` into `InferenceRecord`.**
   Codex sends these; should we capture them like S-18 captured
   `x-claude-code-session-id`? Recommend: yes for the installation-id
   (parallel to `client_session_id`), defer the rest.
4. **`x-openai-internal-codex-responses-lite` header behavior.** Per issue
   #30403, some backends 400 on this header. Should the shim strip, forward,
   or 400? Recommend: forward verbatim (lenient), document the choice.
5. **SSE state-machine sharability.** Should `ResponsesStreamState` mirror the
   `StreamTranslateState` / `AnthropicStreamState` pair and live in
   `src/protocol/responses_stream.rs`, or should it consume those structs
   directly as fields? Recommend a fresh struct with shared state primitives.
6. **`output_text.format = "grammar"` rejection vs. error.granular.** Currently
   recommended 400 — consider whether to offer a per-field 400 vs. a single
   "Responses-only fields not supported" envelope.
7. **Multi-pod statefulness.** If Frugalis ever runs multi-pod (Render
   autoscaling), in-memory transcript storage (Option B) doesn't work. Path A
   (re-send full transcript) is multi-pod-safe by construction — but is the
   bandwidth amplification acceptable at Render's tier pricing? Out of scope
   for S-21; track if/when S-31 (multi-tenant-keys-budgets) lands.
