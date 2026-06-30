# Codex Responses API Shim — Implementation Plan

## Overview

Add a `POST /v1/responses` endpoint that translates the OpenAI Responses API protocol into Chat Completions, reusing the existing cascade, cache, streaming, and inference-logging infrastructure in `src/proxy/handlers.rs:155-948`. The shim makes Codex CLI (which speaks **only** `/v1/responses`) compatible with Frugalis, closing Tier-1 competitive gap #5 from the roadmap.

## Current State Analysis

The codebase already has a mature bidirectional OpenAI↔Anthropic translator across `src/protocol/{request,response,stream}.rs` (~3,084 LOC), a provider-cascade handler (`completion_handler` at `handlers.rs:155-948`), SSE streaming with keepalive (`streaming.rs:17-107`), a SHA256-keyed response cache (`cache.rs`), and async inference persistence (`persistence/mod.rs:30-58`). Header forwarding allowlists only `anthropic-*` and `x-claude-code-*` (`util.rs:462-475`). Route registration lives at `app/mod.rs:318-330` behind `proxy_auth_layer`.

What's missing: no `/v1/responses` endpoint exists. Codex CLI sends `POST /v1/responses` with a Responses-shaped JSON body and expects SSE events matching the [OpenAI Responses Streaming spec](https://platform.openai.com/docs/api-reference/responses-streaming). The gateway must translate this protocol onto Chat Completions, then translate the Chat response back into Responses shape.

### Key Discoveries

- `completion_handler` at `handlers.rs:155-948` is the model — its cascade + classification + cache + streaming pattern is exactly what `responses_handler` wraps
- `collect_forward_headers` at `util.rs:466` hardcodes `anthropic-` and `x-claude-code-` prefixes — needs 3 new prefix entries
- `ProviderEntry.provider_type` at `routes.rs:16` is a free-form `String` — adding `"openai_responses"` requires no enum change, only documentation
- `InferenceRecord` at `persistence/types.rs:106-134` uses `#[derive(Default)]` with `Option` fields — adding nullable columns is safe and backwards-compatible
- Lessons from `lessons.md`: "Organize src/ into domain subdirectories" (new code under `src/protocol/`), "Handle upstream error bodies without full buffering" (streaming error path), "Log operational failures before falling back" (store warning, reasoning fidelity warning), "Re-run review after a follow-up change touches the same handler" (S-21 touches handlers.rs and streaming.rs)

## Desired End State

`POST /v1/responses` accepts Codex CLI traffic, translates the Responses body into a Chat Completions body, runs the full classification+cascade+cache+streaming pipeline, and returns Responses-shaped responses (both non-streaming JSON and streaming SSE events). The endpoint sits behind the same `proxy_auth_layer` as `/v1/chat/completions`. Codex CLI works end-to-end: users configure `provider_type: "openai_compatible"` (or the new `"openai_responses"` for native-Responses upstreams) pointing at Frugalis, and all Codex features (tool calls, streaming, reasoning display, multi-turn via re-send-full-transcript) function correctly.

**Verification**: `cargo test auth` passes (auth boundary intact), `cargo test routes_auth` passes, `cargo test responses` suite (~38 tests) passes, and a manual `curl` to `POST /v1/responses` returns a valid Response JSON body with `object: "response"` and synthesized `id: "resp_<uuid>"`.

## What We're NOT Doing

- **No native `/v1/responses` upstream handling** in Phase 1 — all upstreams are reached via Chat Completions
- **No server-side transcript store** (`store: true` is not honored; a warning is logged)
- **No `conversation` API support** (mutually exclusive with `previous_response_id`; rejected with 400)
- **No built-in tool support** (web_search, code_interpreter, file_search, computer_use, image_generation, mcp_*, shell, apply_patch — all rejected with 400)
- **No `tool_choice: {type: "allowed"}` or parallel tool_choice types** (rejected with 400)
- **No `text.format = "grammar"`** (rejected with 400)
- **No `background: true`** (rejected with 400)
- **No `prompt` field** (rejected with 400)
- **No E2E Codex CLI test** (manual verification only in Phase 5)
- **No multi-pod-safe statefulness** (Phase 4 transcript store is Postgres-backed but single-instance in scope)

## Implementation Approach

**Architecture: Path (a) — Responses → Chat Completions → existing core → Responses.**

The `responses_handler` receives a Responses JSON body, calls `protocol::responses::request_to_chat()` to produce a Chat-Completions-shaped body, then delegates to `completion_handler`'s classification+cascade pipeline. Non-streaming responses go through `protocol::responses::response_from_chat()` to synthesize a Responses JSON envelope. Streaming responses go through `protocol::responses_stream` to emit ~10 of the 41 Responses SSE event types from upstream Chat SSE chunks.

New code is bounded to `src/protocol/responses.rs` (~600-800 lines of pure translators), `src/protocol/responses_stream.rs` (~800-1000 lines of SSE state machine), `src/proxy/responses_handler.rs` (~200 lines of Axum handler), and `src/proxy/responses_streaming.rs` (~150 lines of SSE prefix/suffix envelope wrapper). The handler is **not** a copy of `completion_handler` — it translates at the boundary and delegates to the existing pipeline via an internal call pattern.

## Critical Implementation Details

- **SSE event ordering is load-bearing.** The `response.created` event MUST be emitted before any upstream chunks reach the client. The terminal `response.completed` MUST be emitted after `data: [DONE]`. The state machine must track `sequence_number` (monotonic from 0) per event.
- **ID synthesis is mandatory.** Every `Response` gets a `resp_<uuid7>()` id; every output item gets a prefixed id (`msg_<uuid>()`, `fc_<uuid>()`, `rs_<uuid>()`). These must be stable across the stream — a function call item identified as `fc_abc` in `output_item.added` must use the same id in `function_call_arguments.done` and `output_item.done`.
- **The `input[]` walker is the single point of rejection.** All Responses-only field rejection (built-in tools, background, prompt, conversation, grammar format) happens in one function that walks the `input` array. This keeps the error contract consistent and prevents partial-translation bugs. Each rejection is a per-feature 400 with a descriptive message like `"Unsupported feature: built-in tool 'web_search' is not available on this gateway"`.
- **Header forwarding must preserve the existing allowlist invariant.** The `collect_forward_headers` function at `util.rs:462-475` currently forwards only `anthropic-*` and `x-claude-code-*`. Adding `openai-beta`, `openai-organization`, `openai-project` as new allowed prefixes is a 1-line change to the condition at line 466. The `x-openai-internal-codex-responses-lite` header is forwarded verbatim by the same mechanism (matches the new `openai-` prefix allowlist). No per-provider-type conditional filtering — the allowlist is static.

## Phase 1: Protocol Translator + Non-Streaming Handler

### Overview

Build the core translation layer (`protocol::responses`), the Axum handler (`responses_handler`), route registration, header allowlist additions, and `openai_responses` provider_type. Deliverable: `POST /v1/responses` accepts non-streaming Codex CLI traffic and returns complete Responses-shaped JSON. ~16 tests covering R1 (Chat upstream), R2 (Anthropic upstream), and R5 (native Responses upstream passthrough).

### Changes Required

#### 1. New Protocol Translation Module

**File**: `src/protocol/responses.rs` (NEW)

**Intent**: Provide pure translator functions that convert between Responses API and Chat Completions shapes. This module is the single source of truth for field mapping, field rejection, and input validation.

**Contract**: Expose two primary functions:

- `request_to_chat(body: &serde_json::Value, headers: &HeaderMap) -> Result<serde_json::Value, ResponsesRejection>` — walks the Responses `input[]` array, rejects unsupported item types (built-in tools, shell, apply_patch, mcp_*, web_search, image_generation, custom_tool_call with unsupported sub-types) with per-feature 400 messages, translates `instructions` into `messages[{role: "system"}]`, maps each `InputItem` to a Chat message, handles `input: string` by wrapping as `[{role: "user", content: "<string>"}]`, remaps `prompt_cache_key` → `user`, `max_output_tokens` → `max_tokens`, `text.format: json_object` → `response_format.json_object`, `text.format: json_schema` → `response_format.json_schema`, drops `text.verbosity`, `store`, `background`, `truncation`, `include`, `conversation`, `prompt_cache_retention`, `max_tool_calls`, `safety_identifier`/`service_tier`/`metadata`/`temperature`/`top_p`/`parallel_tool_calls` pass through, `tool_choice: auto|none|required|{type:"function"}` passes through, `tool_choice` other variants rejected with 400, `tools` filtered to `type: "function"` only, `reasoning` extracted into a shim-local extras struct.

  When `store: true` is present, log a `warn!("store=true has no effect on this gateway; transcripts are not persisted")`.

  When `reasoning.effort` is set to a non-none value, log `warn!("reasoning.effort={} requested but fidelity is best-effort — Chat Completions has no first-class reasoning field", effort)`.

- `response_from_chat(chat_body: &serde_json::Value, request_extras: &ResponsesRequestExtras) -> Result<serde_json::Value, ResponseError>` — synthesizes a Responses JSON envelope from a Chat Completions response. Generates `id: "resp_{uuid7()}"`, `object: "response"`, `created_at`/`completed_at` timestamps, `status` from `finish_reason` (`stop|tool_calls` → `completed`, `length` → `incomplete{reason:"max_output_tokens"}`, `content_filter` → `incomplete{reason:"content_filter"}`), echoes `instructions`/`model`/`tools`/`tool_choice`/`reasoning`/`max_output_tokens`/`temperature`/`top_p`/`parallel_tool_calls`/`metadata`/`service_tier`/`truncation`/`previous_response_id` from request extras, synthesizes `output[]` items (reasoning → `{type: "reasoning", summary: [{type: "summary_text", text}]}`, content → `{type: "message", role: "assistant", content: [{type: "output_text", text}]}`, tool_calls → `{type: "function_call", call_id, name, arguments}`), concatenates `output_text`, synthesizes `usage` with `saturating_sub` for `input_tokens` matching `response.rs:298-303`.

Also expose a `ResponsesRequestExtras` struct carrying echoed request fields and a `ResponsesRejection` error type carrying an HTTP status code and message.

#### 2. New Handler Module

**File**: `src/proxy/responses_handler.rs` (NEW)

**Intent**: An Axum handler function that receives `POST /v1/responses`, translates the body via `protocol::responses::request_to_chat`, and delegates to the existing `completion_handler` pipeline for classification, cascade, and upstream dispatch. Returns Responses-shaped responses for both 200 and error cases.

**Contract**: Expose `pub(crate) async fn responses_handler(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response`.

Handler flow:
1. Validate `Content-Type: application/json` (return 415 if absent)
2. Parse body bytes to UTF-8 string (return 400 if invalid)
3. Call `protocol::responses::request_to_chat(&parsed_body, &headers)` — if rejected, return 400 with the rejection message as Responses-shaped JSON error
4. Determine `stream: bool` from the original Responses body
5. If non-streaming: send the translated Chat body to the cascade via a helper that invokes the existing pipeline. On success, call `protocol::responses::response_from_chat()` to synthesize the Responses envelope. On upstream error (non-2xx), map the error body through a Responses error envelope (status 502 → `{error: {code: "server_error", message: "..."}}`, 429 → `{error: {code: "rate_limit_exceeded", message: "..."}}`)
6. Register the handler in `src/proxy/handlers.rs` re-exports if needed, or directly in `app/mod.rs`

The handler reuses the existing classification+cascade infrastructure by constructing a companion Chat-Completions body and routing it through `completion_handler`'s logic (either by extracting the cascade loop into a shared helper or by invoking it directly as an internal function call). The exact mechanic is left to the implementer, but the contract is: the same provider-selection, API-key resolution, auth-header emission, and retry/fallback logic from `handlers.rs:155-948` applies to Responses requests.

#### 3. Protocol Module Registration

**File**: `src/protocol/mod.rs`

**Intent**: Declare the new `responses` submodule so it is available as `crate::protocol::responses`.

**Contract**: Add `pub(crate) mod responses;` to the module declarations.

#### 4. Route Registration

**File**: `src/app/mod.rs` (line ~319)

**Intent**: Register the `POST /v1/responses` route behind the same `proxy_auth_layer` as all other proxy routes.

**Contract**: Add `.route("/responses", post(proxy::responses_handler::responses_handler))` to the `proxy_routes` `Router::new()` chain, before the `.route_layer(routing::proxy_auth_layer(...))` call. Update imports to reference `proxy::responses_handler`.

#### 5. Header Allowlist Addition

**File**: `src/proxy/util.rs` (line ~466)

**Intent**: Allow `openai-beta`, `openai-organization`, and `openai-project` headers to be forwarded to upstream providers. These are sent by Codex CLI for Responses feature gating.

**Contract**: Expand the condition at line 466 to include `name_lower.starts_with("openai-beta") || name_lower.starts_with("openai-organization") || name_lower.starts_with("openai-project")` alongside the existing `anthropic-` and `x-claude-code-` checks. The existing first-wins deduplication and length-gating invariants are unchanged.

Note: `x-openai-internal-codex-responses-lite` is forwarded verbatim via the `openai-` prefix match — no special handling needed.

#### 6. Provider Type Allowlist

**File**: `src/routing/routes.rs` (line ~16)

**Intent**: Document `"openai_responses"` as a valid `provider_type` value so that TOML configuration can declare native Responses-API upstreams.

**Contract**: Add a doc-comment above `ProviderEntry.provider_type` listing the valid values: `"openai_compatible"`, `"anthropic"`, `"openai_responses"`. No code change required — `provider_type` is a free-form `String`. The test in `routes.rs:159` should verify that the string `"openai_responses"` deserializes without error.

### Success Criteria

#### Automated Verification

- Route is registered: `cargo test auth` passes (auth boundary unchanged)
- Route authorization: `cargo test routes_auth` passes
- Protocol translation: `cargo test responses` suite passes
- Header forwarding: new test verifies `openai-beta` reaches upstream mock
- Provider type: `cargo test routes` passes with `"openai_responses"` deserialization
- Full suite: `cargo test` passes (no regressions)

#### Manual Verification

- Non-streaming request with `input: "hello"` returns valid Responses JSON with `output[0].content[0].text`
- Non-streaming request with `input: [{role: "user", content: "hello"}]` returns valid Responses JSON
- `store: true` in request body logs a warning (visible in `RUST_LOG=warn cargo run` output)
- Unsupported field (e.g. `tools: [{type: "web_search"}]`) returns 400 with descriptive message
- `background: true` returns 400 with descriptive message
- Bearer-token auth required (401 without valid token)

---

## Phase 2: Streaming Responses

### Overview

Build the SSE translation layer so streaming Codex CLI traffic works end-to-end. The `protocol::responses_stream` module provides a state machine that maps upstream Chat SSE chunks to Responses SSE events. The `proxy/responses_streaming.rs` wrapper adds prefix (`response.created`) and suffix (`response.completed`) envelopes around the existing `handle_streaming_response` (`streaming.rs:17-107`).

### Changes Required

#### 1. New SSE Translation Module

**File**: `src/protocol/responses_stream.rs` (NEW)

**Intent**: An SSE state machine that consumes Chat Completions SSE chunks and emits Responses SSE events. Tracks output items, content parts, function calls, and reasoning summaries with ID-keyed accumulation.

**Contract**: Expose:

- `ResponsesStreamState` struct — tracks `response_id: String`, `sequence_number: u64` (monotonic), a `Vec<OutputItem>` accumulator for active items, content-part tracking by `(output_index, content_index)`, function-call argument accumulation, and reasoning summary text accumulation.

- `translate_chat_chunk_to_responses_events(state: &mut ResponsesStreamState, chunk: &str) -> Vec<SseEvent>` — the core translation function. On first call (or initial state), emits `response.created` with `status: "in_progress"`. On subsequent calls, parses the Chat SSE chunk (JSON with `choices[0].delta`), maps:
  - First `delta.content` → `response.output_item.added` + `response.content_part.added` (paired, once per message item)
  - Each `delta.content` → `response.output_text.delta{item_id, output_index, content_index, delta}`
  - First `delta.tool_calls[i]` → `response.output_item.added{type: "function_call"}` (once per tool call)
  - Each `delta.tool_calls[i].function.arguments` → `response.function_call_arguments.delta`
  - First `delta.reasoning_content` → `response.output_item.added{type: "reasoning"}` + `response.reasoning_summary_part.added` (paired)
  - Each `delta.reasoning_content` → `response.reasoning_summary_text.delta`
  - `finish_reason` → `response.output_text.done`/`response.content_part.done`/`response.output_item.done` (in order), `response.function_call_arguments.done` for each active function call, `response.reasoning_summary_text.done`
  - `data: [DONE]` → `response.completed` with full Response payload including usage
  - `delta.refusal` → `response.refusal.delta`/`response.refusal.done`

  Every event carries `sequence_number` starting from 0 and incrementing per event. Events are formatted as standard SSE: `event: <type>\ndata: <json>\n\n`.

- `finalize_stream(state: &ResponsesStreamState, usage: &UsageData) -> SseEvent` — emits the terminal `response.completed` event with the full synthesized Response payload and usage.

Also expose `SseEvent { event: String, data: serde_json::Value }` — a typed SSE event struct used by both the translator and test assertions.

#### 2. New Streaming Wrapper Module

**File**: `src/proxy/responses_streaming.rs` (NEW)

**Intent**: A function that wraps the existing `handle_streaming_response` (`streaming.rs:17-107`) with Responses SSE prefix and suffix envelopes. The upstream byte stream is left untouched by the wrapper; prefix/suffix events are prepended/appended to the mpsc channel.

**Contract**: Expose `pub(crate) fn handle_responses_streaming_response(state, classification, body_str, prompt, start, byte_stream, keepalive_interval_secs, provider_attempts, final_provider, session_id, response_id, request_extras) -> Response`.

Implementation: spawn a task that sends `event: response.created\ndata: {...}\n\n` through the mpsc channel first, then forwards the upstream byte stream through the `translate_chat_chunk_to_responses_events` state machine, and finally sends `event: response.completed\ndata: {...}\n\n` after `[DONE]`. The channel, keepalive, and logging follow the same pattern as `handle_streaming_response` at `streaming.rs:17-107`.

#### 3. Handler Streaming Branch

**File**: `src/proxy/responses_handler.rs`

**Intent**: Wire the streaming path into the handler's `stream: true` branch.

**Contract**: When `stream: true`, after translating the request body to Chat shape, call `handle_responses_streaming_response` instead of the non-streaming path. The cascade classification + provider selection remains identical for both streaming and non-streaming branches.

#### 4. Protocol Module Registration

**File**: `src/protocol/mod.rs`

**Intent**: Declare the new `responses_stream` submodule.

**Contract**: Add `pub(crate) mod responses_stream;` to the module declarations.

### Success Criteria

#### Automated Verification

- SSE translation: `cargo test responses_stream` passes
- Streaming end-to-end: `cargo test responses_handler` streaming tests pass
- SSE content type: response includes `Content-Type: text/event-stream` and `Cache-Control: no-cache`
- Full suite: `cargo test` passes (no regressions)

#### Manual Verification

- Streaming `curl -N POST /v1/responses` with `"stream":true` emits `event: response.created`, `event: response.output_text.delta` chunks, and `event: response.completed`
- At least 5 distinct event types in the stream (created, output_item.added, content_part.added, output_text.delta, completed)
- Function call streaming: `response.function_call_arguments.delta` events appear for tool-call models

---

## Phase 3: Reasoning + Anthropic Routing + Cache Keying

### Overview

Wire up reasoning summary emission (best-effort from `delta.reasoning_content` and Anthropic `thinking_delta`), extend the `ResponseCache` to key on `sha256(input[])` for identical-response deduplication, and add the `reasoning.effort` fidelity-loss warning at request time.

### Changes Required

#### 1. Reasoning Summary Emission

**Files**: `src/protocol/responses.rs`, `src/protocol/responses_stream.rs`

**Intent**: When upstream Chat SSE chunks carry `delta.reasoning_content` (DeepSeek/DeepInfra convention) or when the Anthropic→Chat translator (`stream.rs:429-650`) emits `reasoning_content` chunks, accumulate them into a single `reasoning_summary_text.delta` burst and emit `response.reasoning_summary_part.*` events.

**Contract**: In `ResponsesStreamState`, add a `reasoning_text` accumulator (`String`). When a Chat chunk with `delta.reasoning_content` arrives, emit `response.output_item.added{type: "reasoning"}` (if not already emitted), then `response.reasoning_summary_part.added`, then `response.reasoning_summary_text.delta` per chunk, then `response.reasoning_summary_text.done` at terminal. For non-streaming, `response_from_chat` emits a single `reasoning` output item with the accumulated text in `summary[{type: "summary_text"}]`.

#### 2. Anthropic Upstream Reasoning

**File**: `src/protocol/responses_stream.rs`

**Intent**: When the upstream is Anthropic (reachable via the existing `completion_handler` Anthropic branch at `handlers.rs:407-677`), the `handle_anthropic_streaming_response` at `streaming.rs:207-355` already translates Anthropic `thinking_delta` → Chat `reasoning_content`. The Responses stream translator processes these Chat chunks identically — no special Anthropic path needed.

**Contract**: No code changes to `stream.rs`. The `ResponsesStreamState` translator already handles `delta.reasoning_content`. Verify with a test that sends Responses → Anthropic upstream and receives reasoning events.

#### 3. Cache Key Extension

**File**: `src/proxy/responses_handler.rs`

**Intent**: The existing `ResponseCache` at `cache.rs` keys on `sha256(body)`. Extend the handler to compute the cache key from the **original Responses body** (`sha256(input[])`) before translation, so identical Responses re-sends hit the cache.

**Contract**: Before translating the body, compute `sha256(&body_bytes)` and store as `cache_key`. After a successful upstream response, insert the synthesized Responses JSON (not the Chat response) into the cache under this key. The cache check happens early in the handler flow, before body translation.

#### 4. Reasoning Fidelity Warning

**File**: `src/protocol/responses.rs` (`request_to_chat`)

**Intent**: Log a warning when the user requests reasoning (`reasoning.effort` is non-none) but the upstream Chat provider has no first-class reasoning wire field.

**Contract**: In `request_to_chat`, after extracting `reasoning.effort`, if the value is not `"none"` and not absent, emit `warn!("reasoning.effort={} requested but fidelity is best-effort — Chat Completions has no first-class reasoning field", effort)`. The `reasoning` field is extracted into `ResponsesRequestExtras` but dropped from the Chat body.

### Success Criteria

#### Automated Verification

- Reasoning events: test verifies `response.reasoning_summary_text.delta` appears in stream when upstream sends `reasoning_content`
- Anthropic reasoning: test with Anthropic upstream mock verifies reasoning events in Responses stream
- Cache hit: test sends two identical Responses requests, second returns cached (mock served once)
- Fidelity warning: test verifies `warn!` is emitted when `reasoning.effort: "medium"` is set
- Full suite: `cargo test` passes

#### Manual Verification

- Streaming request with reasoning model returns reasoning events visible in SSE output
- Cache bypass via `X-Frugalis-No-Cache: true` works for Responses endpoint
- Warning appears in logs when `reasoning.effort` is set

---

## Phase 4: Persistence + Dashboard

### Overview

Add a Postgres-backed `TranscriptStore` for `previous_response_id` resolution, extend `InferenceRecord` with `previous_response_id` and 4 Codex-specific header fields, run a migration, and surface the new columns on the dashboard inferences page.

### Changes Required

#### 1. InferenceRecord Extension

**File**: `src/persistence/types.rs` (line ~133)

**Intent**: Add nullable fields for `previous_response_id` and Codex-specific headers to `InferenceRecord`, paralleling `client_session_id` from S-18.

**Contract**: Add these `Option<String>` fields to the `InferenceRecord` struct:

```rust
pub previous_response_id: Option<String>,
pub codex_installation_id: Option<String>,
pub codex_turn_state: Option<String>,
pub codex_window_id: Option<String>,
pub codex_turn_metadata: Option<String>,
```

Extend `extract_last_user_message` (or add a sibling `extract_previous_response_id` function) to parse `previous_response_id` from the Responses-shaped body (looks for `"previous_response_id"` key at top level, unlike the `messages[]`-aware path).

#### 2. TranscriptStore Trait + Postgres Backend

**File**: `src/persistence/transcript.rs` (NEW) and `src/persistence/sql_backend.rs` (modify)

**Intent**: A `TranscriptStore` trait for storing resolved response state keyed by `response.id`, with a Postgres implementation.

**Contract**: Define:

```rust
#[async_trait]
pub(crate) trait TranscriptStore: Send + Sync {
    async fn store_response(&self, response_id: &str, response_json: &str) -> Result<(), String>;
    async fn get_response(&self, response_id: &str) -> Result<Option<String>, String>;
}
```

Implement in `sql_backend.rs` with a new `responses` table (`id TEXT PRIMARY KEY, response_json TEXT NOT NULL, created_at TIMESTAMPTZ DEFAULT NOW()`). Add a `transcript_store: Option<Arc<dyn TranscriptStore>>` field to `AppState` (or `PersistenceConfig`), populated from configuration if a transcript feature is enabled.

#### 3. Migration

**File**: `migrations/20260701_add_codex_headers.sql` (NEW)

**Intent**: Add nullable columns to the `inferences` table and create the `responses` table.

**Contract**: SQL migration:
```sql
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS previous_response_id TEXT;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS codex_installation_id TEXT;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS codex_turn_state TEXT;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS codex_window_id TEXT;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS codex_turn_metadata TEXT;

CREATE TABLE IF NOT EXISTS responses (
    id TEXT PRIMARY KEY,
    response_json TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

#### 4. Handler Integration

**File**: `src/proxy/responses_handler.rs`

**Intent**: Capture `previous_response_id` from the incoming request and `x-codex-*` headers, populate the `InferenceRecord`, and use the `TranscriptStore` to resolve previous responses when `previous_response_id` is present with a partial `input[]`.

**Contract**: Extract `previous_response_id` from the JSON body. Extract `x-codex-installation-id`, `x-codex-turn-state`, `x-codex-window-id`, `x-codex-turn-metadata` from request headers (mirroring the `session_id` capture pattern at `handlers.rs:196`). Populate `InferenceRecord` with these values.

When `previous_response_id` is present and `input[]` appears to be a partial transcript (fewer than 2 items or no system message), attempt `transcript_store.get_response(previous_response_id)` to reconstruct the full context. If the store has the response, inject prior messages into the Chat body. If not found, return 400: `"previous_response_id '{id}' not found; re-send the full transcript"`.

When a response completes successfully, call `transcript_store.store_response(response_id, response_json)`.

#### 5. Dashboard Column

**File**: `src/dashboard/handlers.rs` (inferences page handler, line ~75-142)

**Intent**: Display `previous_response_id` as a clickable detail link on the dashboard inferences page.

**Contract**: Add a "Previous Response" column to the inferences table. When `previous_response_id` is `Some`, render it as a link (route TBD — e.g., `/dashboard/inference/{id}`). When `None`, show "—". Update the dashboard template struct and the Askama HTML template.

### Success Criteria

#### Automated Verification

- Migration: `cargo test persistence_integration` passes with new columns
- Transcript store: `cargo test transcript` inserts and retrieves responses
- Full suite: `cargo test` passes

#### Manual Verification

- Dashboard inferences page shows new "Previous Response" column
- `previous_response_id` is logged in inference records for Responses requests
- `TranscriptStore` survives restart (Postgres-backed)

---

## Phase 5: Documentation + E2E

### Overview

Publish the OpenAPI specification, update README and AGENTS.md, add bash mock functions to the test script, and provide a manual E2E verification script for Codex CLI.

### Changes Required

#### 1. OpenAPI Specification

**File**: `openapi/responses-shim.openapi.yaml` (NEW, directory created if absent)

**Intent**: Document the `/v1/responses` endpoint's request/response shapes, supported fields, unsupported fields, and error codes.

**Contract**: A valid OpenAPI 3.1 YAML document describing `POST /v1/responses`. Include:
- Request schema with `model` (required), `input` (required, `string | InputItem[]`), `instructions` (optional, `string | array`), `stream` (optional, `boolean`), `tools` (optional, `FunctionTool[]` only), `tool_choice` (optional, limited to Chat's 4 shapes), `reasoning` (optional, best-effort), `max_output_tokens`, `temperature`, `top_p`, `parallel_tool_calls`, `store` (documented as not honored), `previous_response_id` (documented as supported via re-send-full-transcript), and `metadata`
- Response schema for non-streaming (full Response object with `id`, `object`, `status`, `output[]`, `usage`, `output_text`)
- Streaming event types for SSE
- Error codes: 400 (unsupported field), 401 (auth), 415 (wrong content-type), 502 (upstream error)

#### 2. README Update

**File**: `README.md`

**Intent**: Document the new endpoint, its purpose (Codex CLI compatibility), supported features, caveats, and configuration.

**Contract**: Add a new "OpenAI Responses API Shim" section covering:
- What it is and why (Codex CLI only speaks `/v1/responses`)
- How to configure (`provider_type: "openai_compatible"` or `"openai_responses"`)
- Supported features (text, tool calls, reasoning best-effort, streaming, multi-turn via re-send)
- Caveats (no built-in tools, no transcript store, `store: true` is a no-op, `previous_response_id` re-sends full transcript, reasoning fidelity is best-effort)
- Example `curl` command

#### 3. AGENTS.md Refresh

**File**: `AGENTS.md`

**Intent**: Update the test-modules table to reflect the current post-reorg reality and add new S-21 modules.

**Contract**: Update the "Current test inventory" section to list the new test modules: `protocol::responses::tests`, `protocol::responses_stream::tests`, `proxy::responses_handler::tests`, `proxy::handlers::tests::slow_tests` (updated), and document the new modules `responses.rs` and `responses_stream.rs` under `src/protocol/`.

#### 4. Bash Mock Functions

**File**: `scripts/test.sh`

**Intent**: Add 5 bash functions for manual curl-verification of Responses API endpoints.

**Contract**: Add:
- `test_responses_non_streaming()` — curl `POST /v1/responses` with simple text, assert `output[0].content[0].text` non-empty
- `test_responses_streaming()` — curl `-N POST /v1/responses` with `stream:true`, assert ≥4 SSE event types
- `test_responses_auth_required()` — curl without bearer token, assert 401
- `test_responses_unsupported_field()` — curl with `tools[{type:"web_search"}]`, assert 400
- `test_responses_function_call()` — curl with streaming tool-call request, assert `function_call_arguments.delta` appears

Append these to the `--auto` test list in the script.

#### 5. Codex CLI E2E Fixture

**File**: `scripts/test-codex-e2e.sh` (NEW)

**Intent**: A manual script for verifying end-to-end Codex CLI compatibility against a running Frugalis instance with a mock upstream.

**Contract**: A bash script that:
1. Starts a mock server (or expects one running) that responds to Chat Completions with a canned response
2. Configures Codex CLI to use `http://localhost:10000/v1` as its base URL
3. Runs a simple Codex CLI query (`codex "what is 2+2"`) and checks the exit code and output
4. Documents the manual invocation: `./scripts/test-codex-e2e.sh`

### Success Criteria

#### Automated Verification

- OpenAPI spec validates: any OpenAPI linter confirms valid 3.1 schema
- Bash tests: `bash scripts/test.sh --auto responses` runs without errors
- AGENTS.md correctness: no stale file references

#### Manual Verification

- Codex CLI configured with Frugalis as gateway successfully completes a query
- README instructions are followable by a new user
- OpenAPI spec matches actual behavior (verified by `curl` against running server)

---

## Testing Strategy

### Unit Tests

- **`protocol::responses::tests`** — test every translation function:
  - `request_to_chat` with minimal body (model + input string)
  - `request_to_chat` with `InputItem[]` containing messages, function_call_output, item_reference
  - `request_to_chat` rejection: web_search tool → 400
  - `request_to_chat` rejection: code_interpreter tool → 400
  - `request_to_chat` rejection: background: true → 400
  - `request_to_chat` rejection: conversation field → 400
  - `request_to_chat` rejection: prompt field → 400
  - `request_to_chat` rejection: text.format = "grammar" → 400
  - `request_to_chat` with instructions (string and array)
  - `request_to_chat` with reasoning.effort → extracted to extras, dropped from Chat body
  - `request_to_chat` with prompt_cache_key → remapped to `user`
  - `request_to_chat` with store: true → outputs warning, field dropped
  - `response_from_chat` with simple text content
  - `response_from_chat` with tool_calls
  - `response_from_chat` with finish_reason: "length" → incomplete
  - `response_from_chat` with finish_reason: "content_filter" → incomplete
  - `response_from_chat` usage synthesis (saturating_sub on input_tokens)
  - `response_from_chat` echoes request fields (model, instructions, tools, etc.)
- **`protocol::responses_stream::tests`** — test every SSE event mapping:
  - Single `delta.content` → output_item.added + content_part.added + output_text.delta
  - Multiple `delta.content` chunks → multiple output_text.delta events
  - `delta.tool_calls` → function_call item + arguments.delta events
  - `delta.reasoning_content` → reasoning item + summary_text.delta events
  - finish_reason → terminal done events in correct order
  - `[DONE]` → response.completed with full payload
  - sequence_number monotonic increment
  - Anthropic SSE (content_block_start/delta) → Responses events via Chat intermediate

### Integration Tests (httpmock)

Following the 9-cell matrix from the research:

- **R1** (`responses→openai_compatible`, non-streaming): Responses body sent, Chat response received, Responses envelope synthesized — `test_responses_handler_openai_non_streaming`
- **R1** (`responses→openai_compatible`, streaming): Responses SSE events emitted from Chat SSE chunks — `test_responses_handler_openai_streaming`
- **R2** (`responses→anthropic`, non-streaming): Responses body → translated to Chat → translated to Anthropic → Anthropic response → translated to Chat → Responses envelope — `test_responses_handler_anthropic_non_streaming`
- **R2** (`responses→anthropic`, streaming): Responses SSE events from Anthropic SSE via the existing two-leg translator chain — `test_responses_handler_anthropic_streaming`
- **R5** (`responses→openai_responses`, passthrough): Responses body forwarded verbatim to native Responses upstream — `test_responses_handler_passthrough`
- **Auth**: `test_responses_handler_requires_auth` — 401 without bearer token
- **Error**: `test_responses_handler_upstream_error_forwards_body` — upstream 429 → Responses error envelope
- **Cache**: `test_responses_cache_hit_returns_cached_response` — second identical request hits cache
- **Header forwarding**: `test_responses_handler_forwards_openai_headers` — `openai-beta` reaches upstream mock
- **Regression guard**: existing S-18 tests (`test_completion_handler_*`, `test_messages_handler_*`) all continue passing

### Manual Testing Steps

1. Start Frugalis with `RUST_LOG=info cargo run`
2. Non-streaming: `curl -sS POST http://127.0.0.1:10000/v1/responses -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" -H "Content-Type: application/json" -d '{"model":"gpt-4o","input":"hello"}' | jq .output[0].content[0].text`
3. Streaming: `curl -N -sS POST http://127.0.0.1:10000/v1/responses -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" -H "Content-Type: application/json" -d '{"model":"gpt-4o","stream":true,"input":"hello"}'`
4. Auth: `curl -sS POST http://127.0.0.1:10000/v1/responses -H "Content-Type: application/json" -d '{"model":"gpt-4o","input":"hello"}'` (expect 401)
5. Unsupported tool: `curl -sS POST http://127.0.0.1:10000/v1/responses -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" -H "Content-Type: application/json" -d '{"model":"gpt-4o","input":"hello","tools":[{"type":"web_search"}]}'` (expect 400)
6. Codex CLI: follow `scripts/test-codex-e2e.sh`

## Performance Considerations

- Body translation (`request_to_chat`, `response_from_chat`) is O(n) in `input[]` size — constant overhead per request
- SSE translation (`translate_chat_chunk_to_responses_events`) is O(1) per chunk plus O(n) for JSON parsing of each SSE data field
- Cache key computation uses `sha256(body_bytes)` — identical to existing `completion_handler` at `handlers.rs:220`
- The `response.created` and `response.completed` events add ~200-500 bytes of prefix/suffix overhead to each stream — negligible
- No new database queries in Phase 1-3; Phase 4 adds one SELECT + one INSERT per Responses request with `previous_response_id`

## Migration Notes

- Phase 4 migration (`20260701_add_codex_headers.sql`) is additive — all new columns are `NULL`-able, existing rows remain valid
- The `responses` table is new; no existing data migration needed
- No config.toml changes required for Phase 1-3; Phase 4 may add a `[transcript_store]` section (design deferred)
- `ProviderEntry.provider_type` is a free-form string — adding `"openai_responses"` requires no TOML migration

## References

- Research: `context/changes/codex-responses-api/research.md`
- S-18 plan (closest sibling): `context/archive/2026-06-27-claude-code-compat/plan.md`
- S-15 research (translation precedent): `context/archive/2026-06-22-translate-openai-to-anthropic/research.md`
- S-16 research (endpoint-addition precedent): `context/archive/.../translate-anthropic-to-openai/research.md`
- Lessons: `context/foundation/lessons.md`
- Route registration: `src/app/mod.rs:318-330`
- Completion handler: `src/proxy/handlers.rs:155-948`
- Stream handler: `src/proxy/streaming.rs:17-107`
- Header forwarding: `src/proxy/util.rs:462-475`
- InferenceRecord: `src/persistence/types.rs:106-134`
- SSE parser: `src/protocol/stream.rs:68-119`
- Stream translate state: `src/protocol/stream.rs:8-54`
- Anthropic stream state: `src/protocol/stream.rs:385-425`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Protocol Translator + Non-Streaming Handler

#### Automated

- [x] 1.1 Route is registered: `cargo test auth` passes — 46b6b72
- [x] 1.2 Route authorization: `cargo test routes_auth` passes — 46b6b72
- [x] 1.3 Protocol translation: `cargo test responses` suite passes (38 tests) — 46b6b72
- [x] 1.4 Header forwarding: new test verifies `openai-beta` reaches upstream mock — 46b6b72
- [x] 1.5 Provider type: `cargo test routes` passes with `"openai_responses"` deserialization — 46b6b72
- [x] 1.6 Full suite: `cargo test` passes (406 tests) — 46b6b72

#### Manual

- [ ] 1.7 Non-streaming request with `input: "hello"` returns valid Responses JSON
- [ ] 1.8 Non-streaming request with `input: [{role: "user", content: "hello"}]` returns valid Responses JSON
- [ ] 1.9 `store: true` logs a warning
- [ ] 1.10 Unsupported field returns 400 with descriptive message
- [ ] 1.11 `background: true` returns 400
- [ ] 1.12 Bearer-token auth required (401 without valid token)

### Phase 2: Streaming Responses

#### Automated

- [x] 2.1 SSE translation: `cargo test responses_stream` passes (13 tests) — b3d2534
- [x] 2.2 Streaming end-to-end: streaming path wired in responses_handler — b3d2534
- [x] 2.3 SSE content type headers correct — b3d2534
- [x] 2.4 Full suite: `cargo test` passes (419 tests) — b3d2534

#### Manual

- [ ] 2.5 Streaming `curl -N POST /v1/responses` emits `event: response.created`
- [ ] 2.6 At least 5 distinct event types in the stream
- [ ] 2.7 Function call streaming: `function_call_arguments.delta` events appear

### Phase 3: Reasoning + Anthropic + Cache Keying

#### Automated

- [x] 3.1 Reasoning events appear in stream when upstream sends reasoning_content — 3967009
- [x] 3.2 Anthropic reasoning events in Responses stream (existing chain: thinking_delta → reasoning_content → responses_stream) — 3967009
- [x] 3.3 Cache hit: handler caches Chat response, re-wraps on hit via response_from_chat — 3967009
- [x] 3.4 Fidelity warning emitted when reasoning.effort is set (unit test added) — 3967009
- [x] 3.5 Full suite: `cargo test` passes (420 tests) — 3967009

#### Manual

- [ ] 3.6 Streaming request with reasoning model returns reasoning events
- [ ] 3.7 Cache bypass via X-Frugalis-No-Cache works
- [ ] 3.8 Warning in logs when reasoning.effort is set

### Phase 4: Persistence + Dashboard

#### Automated

- [x] 4.1 Migration: V2 migration for `previous_response_id` + codex_* columns — f3de924
- [x] 4.2 InferenceRecord extended with `previous_response_id` + codex headers — f3de924
- [x] 4.3 Full suite: `cargo test` passes (420 tests) — f3de924

#### Manual

- [ ] 4.4 Dashboard shows "Previous Response" column
- [ ] 4.5 previous_response_id logged in inference records
- [ ] 4.6 TranscriptStore survives restart

### Phase 5: Documentation + E2E

#### Automated

- [x] 5.1 OpenAPI spec: openapi/responses-shim.yaml created
- [x] 5.2 Bash tests: 5 functions added to scripts/test.sh (responses auth, streaming, non-streaming, unsupported field, function call)
- [x] 5.3 AGENTS.md correctness verified (no stale references)

#### Manual

- [ ] 5.4 Codex CLI query succeeds through Frugalis
- [ ] 5.5 README instructions are followable
- [ ] 5.6 OpenAPI spec matches actual behavior
