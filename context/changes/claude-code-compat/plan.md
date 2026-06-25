# Claude Code Compatibility Implementation Plan

## Overview

Make Cerebrum a true drop-in gateway for Claude Code by (a) forwarding `anthropic-beta` / `anthropic-version` / `x-claude-code-*` headers to Anthropic upstreams as an open list, (b) translating `cache_control` prompt-caching blocks across all four protocol crossings (with auto-insert on OpenAI→Anthropic), (c) translating cache tokens in responses and logging them to `InferenceRecord`, and (d) serving the Anthropic `/v1/models` shape with `display_name`.

## Current State Analysis

Cerebrum already has a 2,763-line bidirectional OpenAI↔Anthropic translator (`src/protocol_translation.rs`) and both `/v1/chat/completions` and `/v1/messages` endpoints. But several Claude Code contract requirements are unmet:

- **Headers are dropped.** Both handlers capture `headers: HeaderMap` (`src/main.rs:1783`, `:2189`) but never thread it into `build_upstream_request` (`src/main.rs:1039-1080`) or `auth_headers_for` (`src/intent_classifier.rs:436-475`). Outgoing headers are built entirely from config — `Content-Type` + auth-provider tuples + optional timeout. `anthropic-beta` appears **nowhere** in `src/`; `anthropic-version: 2023-06-01` is hardcoded in two spots inside `auth_headers_for` (`src/intent_classifier.rs:450`, used at `:461` and `:471`). `x-claude-code-session-id` / `-agent-id` / `-parent-agent-id` are never read.
- **`cache_control` is stripped on translation.** Translation builds a fresh `serde_json::Map` via explicit field allowlist (no `#[serde(flatten)]` catch-all, no typed request structs). `translate_request` (OAI→Anth, `src/protocol_translation.rs:37-144`) and `anthropic_to_openai_request` (Anth→OAI, `:860-1013`) emit only ~10 known fields; everything else — including `cache_control`, `thinking`, `context_management`, `output_config` — is silently dropped. `cache_control` survives **only** on same-protocol byte-passthrough paths (`body.clone()` at `src/main.rs:2366`; OpenAI passthrough via `build_upstream_request`).
- **Cache tokens are dropped in usage translation.** `translate_response` (`:382-502`, usage read `:474-481`), `translate_stream_event` (`:624-841`, `message_delta` usage `:803-823`), and `openai_to_anthropic_response` (`:1146-1239`, emit `:1237`) read only `input_tokens`/`output_tokens`. `cache_read_input_tokens` and `cache_creation_input_tokens` return zero matches across `src/`.
- **`/v1/models` returns the OpenAI shape.** `models_handler` (`src/main.rs:862-872`) emits a hardcoded static JSON (`:864`) with three `claude-`-prefixed IDs, `object: "model"`, `owned_by`, but **no `display_name`**. Route is intentionally unauthenticated (`src/main.rs:2592-2593`) so Claude Code's pre-auth discovery probe succeeds.
- **`InferenceRecord` has no token/attribution fields.** Struct at `src/persistence.rs:1107-1118` carries category, model, duration, snippet, provider cascade info — no `input_tokens`/`output_tokens`, no cache-token fields, no session/agent-id. Built at `src/main.rs:901-912`; logged through `log_classification` (`src/main.rs:881-919`) from ~20 call sites.

### Key Discoveries:

- **Prompt caching is GA** (verified against `docs.anthropic.com/en/docs/build-with-claude/prompt-caching`, Jun 2026): no `anthropic-beta` header required — only `anthropic-version: 2023-06-01` + the `cache_control` body field. This means Cerebrum's auto-insert on OpenAI→Anthropic needs **no** beta header injection.
- **Anthropic "automatic caching"** simplifies insertion dramatically: a single top-level `"cache_control": {"type": "ephemeral"}` field makes the API auto-place the breakpoint on the last cacheable block and move it forward as conversations grow (`protocol_translation.rs` translates bodies as `serde_json::Value`, so adding a top-level key is trivial). No per-block surgery required for the common case.
- **Translation uses `serde_json::Value` with explicit allowlists** — there is no free pass-through. Preserving `cache_control` (or any unknown field) requires explicit per-key code at the tail of each `translate_*` function (`protocol_translation.rs:141-143` and `:1010-1012`).
- **`src/translate/` is a dead stub** (`mod.rs` declares submodules whose files don't exist; never declared in `main.rs`). All real translation is in `src/protocol_translation.rs`. Do not add code to `src/translate/`.
- **Header forwarding is a signature change across 3 call sites**, not a local edit: `auth_headers_for` (`src/intent_classifier.rs:436`) is called from `src/main.rs:1066` (`build_upstream_request`), `:1971` (completion Anthropic branch), and `src/intent_classifier.rs:307` (classifier self-probe).
- **OpenAI represents cached tokens** as `usage.prompt_tokens_details.cached_tokens`; Anthropic uses `usage.cache_read_input_tokens` + `usage.cache_creation_input_tokens`. Total Anthropic input = `cache_read_input_tokens + cache_creation_input_tokens + input_tokens`.

## Desired End State

After this plan, pointing Claude Code at Cerebrum (`ANTHROPIC_BASE_URL`) works fully: prompt caching activates and reports real cache-hit tokens, beta-gated features (context management, interleaved thinking, extended context) reach the upstream intact, model discovery shows friendly names, and the operator dashboard/inference log reflects per-request cache savings and per-session attribution.

Verification: a real Claude Code client routed through Cerebrum to an Anthropic upstream shows `cache_read_input_tokens > 0` on repeated turns; the inference log row for that request carries the cache-token counts and the Claude Code session id; `GET /v1/models` returns entries with `display_name`; sending an `anthropic-beta` header reaches the upstream unchanged (verified via httpmock capturing the forwarded header).

## What We're NOT Doing

- **No Codex CLI `/v1/responses` support** — that is a separate change (`codex-responses-api`).
- **No response caching / semantic cache** — separate change (`add-response-cache`). This plan only makes *upstream* prompt caching work; it does not cache Cerebrum's own responses.
- **No learned/embedding router** — deferred to an enterprise-tier change.
- **No error-body verbatim-forwarding refactor** beyond what's needed to not wrap anthropic responses; a full error-envelope audit is out of scope (flagged as an open risk).
- **No `src/translate/` resurrection** — all work stays in `src/protocol_translation.rs`.
- **No per-client budgets/RBAC** — the attribution headers are *captured* into logs but not used for access control or quota enforcement (separate enterprise change).

## Implementation Approach

Four phases, ordered for low-risk-first and dependency flow. Phase 1 is a trivial independent win. Phase 2 establishes the header plumbing every later phase leans on. Phase 3 adds the request-side `cache_control` translation. Phase 4 adds response-side cache-token translation and the observability migration. Each phase is independently testable and shippable; the build stays green after each.

## Critical Implementation Details

- **Header ordering / override semantics.** Client-forwarded headers must NOT override the upstream auth credential. `build_upstream_request` sets auth headers last today (`src/main.rs:1072-1074`); insert forwarded client headers *before* the auth-headers loop so an inbound `authorization`/`x-api-key` can never replace the resolved upstream key. Only forward the explicit `anthropic-*` and `x-claude-code-*` prefixes — never blindly copy all inbound headers.
- **`anthropic-beta` only crosses to Anthropic upstreams.** On cross-protocol paths (e.g. Anthropic client routed to an OpenAI upstream) do NOT forward `anthropic-beta` — it is meaningless noise to an OpenAI provider. Cerebrum injects betas only for Anthropic features *it* adds. Because prompt caching is GA, the auto-inserted `cache_control` requires no beta header at all — do not inject one.
- **Automatic caching is the insertion primitive.** For OpenAI→Anthropic, add a single top-level `"cache_control": {"type":"ephemeral"}` to the translated body rather than surgically marking individual content blocks. Reserve block-level breakpoints for a future enhancement; the automatic mode covers the multi-turn-conversation use case that is Claude Code's primary traffic.
- **Streaming usage arrives in `message_start` + `message_delta`.** Anthropic reports `input_tokens`/`cache_creation_input_tokens` in `message_start` and `output_tokens`/`cache_read_input_tokens` in `message_delta`. The translator must read both events and translate the full set; do not assume all usage lands in one event.
- **`log_classification` is called before response parse on some streaming paths** (`src/main.rs:1181-1190`). Capturing response usage into `InferenceRecord` requires finalizing the record *after* the stream closes, not at stream open. Phase 4 must restructure the streaming log emission to update token fields at finalization — this is the single trickiest ordering change.

## Phase 1: `/v1/models` Anthropic shape + `display_name`

### Overview

Serve the Anthropic `/v1/models` contract (with `display_name`) so Claude Code's model discovery shows friendly names. Keep the endpoint unauthenticated.

### Changes Required:

#### 1. Anthropic-shaped models response

**File**: `src/main.rs`

**Intent**: Replace the hardcoded OpenAI-shape static JSON at `src/main.rs:864` with a response that includes `display_name` and the Anthropic entry shape, while keeping `claude`/`anthropic`-prefixed IDs (Claude Code's discovery filter requires IDs beginning with `claude` or `anthropic`).

**Contract**: `models_handler` (`src/main.rs:862-872`) returns a JSON list whose entries each carry `id`, `display_name`, and `type`/`object`. Derive the list from routing config rather than a hardcoded triple where feasible, but a static Anthropic-shape map keyed on the existing `claude-*` IDs is acceptable for this phase. The route stays unauthenticated (`src/main.rs:2592-2593`). Optional: when the inbound request carries `anthropic-version`, return the Anthropic shape; otherwise return the OpenAI shape — this lets both OpenAI and Anthropic clients use the same endpoint.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes (existing models tests updated for the new shape).
- New unit test asserts each entry has `display_name` and a `claude`/`anthropic`-prefixed `id`.
- `cargo test auth` and `cargo test routes_auth` pass (endpoint remains unauthenticated — verify no auth regression).

#### Manual Verification:

- `curl http://127.0.0.1:10000/v1/models` returns entries with `display_name`.
- Claude Code with `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` lists the friendly model names.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Header pass-through plumbing

### Overview

Thread inbound `headers` through to upstream construction and forward `anthropic-*` / `x-claude-code-*` headers to Anthropic upstreams as an open list. Prefer the client-supplied `anthropic-version`, falling back to the hardcoded `2023-06-01`.

### Changes Required:

#### 1. Extract forwardable headers in handlers

**File**: `src/main.rs`

**Intent**: In `completion_handler` (`src/main.rs:1781`) and `messages_handler` (`src/main.rs:2189`), collect inbound headers whose names start with `anthropic-` or `x-claude-code-` into a small `Vec<(String, String)>` (or a typed struct). This is the single extraction point both handlers reuse.

**Contract**: A helper `fn collect_forward_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)>` returning only the two prefixed families. Never include `authorization` or `x-api-key` (those are the proxy's own credential, consumed by `ProxyBearerAuth` at `src/auth.rs:64-91`).

#### 2. Thread headers into upstream construction

**File**: `src/main.rs`

**Intent**: Give `build_upstream_request` (`src/main.rs:1039-1080`) an additional parameter carrying the forwardable headers, and apply them to the outgoing reqwest request *before* the auth-headers loop at `src/main.rs:1072-1074` so they cannot override the upstream credential. Update the three call sites (`src/main.rs:2074` completion OpenAI branch, `:2381` messages, and the inline Anthropic branch at `:1976-1982`).

**Contract**: New signature param `forward_headers: &[(HeaderName, HeaderValue)]` (empty vec when none). Insert between the `Content-Type`/body set (`src/main.rs:1068-1071`) and the auth loop. The inline completion Anthropic branch (`src/main.rs:1976-1982`) must apply the same forwarding in its own header-building loop.

#### 3. Forward headers + client `anthropic-version` in `auth_headers_for`

**File**: `src/intent_classifier.rs`

**Intent**: Extend `auth_headers_for` (`src/intent_classifier.rs:436-475`) to (a) accept the forwardable headers and append them when the target provider is `anthropic`, and (b) prefer a client-supplied `anthropic-version` over the hardcoded literal at `src/intent_classifier.rs:450` when one is present in the forwarded set. Update all three call sites (`src/main.rs:1066`, `:1971`, `src/intent_classifier.rs:307`).

**Contract**: New param `forward_headers: &[(HeaderName, HeaderValue)]` (or `Option`). Logic: always emit the resolved auth header; if `pt == "anthropic"`, emit `anthropic-version` = client value if present else `2023-06-01`, then append any remaining `anthropic-beta` / `x-claude-code-*` entries verbatim. For non-anthropic providers, drop `anthropic-*` entirely (meaningless to OpenAI upstreams). De-duplicate so a client `anthropic-version` doesn't collide with the one emitted here.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes.
- New httpmock test: a request to the Anthropic upstream carries the inbound `anthropic-beta` and `anthropic-version` values unchanged (OpenAI-upstream variant: `anthropic-*` is absent).
- `cargo test auth`, `cargo test routes_auth` pass.

#### Manual Verification:

- Point Claude Code at Cerebrum; send a request that sets an `anthropic-beta` capability; confirm (via debug logging or a capture) it reaches the Anthropic upstream.
- Confirm OpenAI-upstream routing does not forward `anthropic-beta`.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: `cache_control` translation across all four crossings

### Overview

Preserve or insert `cache_control` so prompt caching is effective regardless of protocol path: auto-insert on OpenAI→Anthropic, strip-and-account on Anthropic→OpenAI, and explicit preservation on same-protocol paths (replacing reliance on byte-passthrough luck).

### Changes Required:

#### 1. OpenAI→Anthropic: auto-insert via automatic caching

**File**: `src/protocol_translation.rs`

**Intent**: In `translate_request` (`src/protocol_translation.rs:37-144`), after building the output map and before `Ok(...)` at `:143`, insert a top-level `"cache_control": {"type":"ephemeral"}` key if not already present. This activates Anthropic automatic caching (GA; no beta header needed) for OpenAI clients routed to Anthropic upstreams, with zero block-level surgery.

**Contract**: Add the key only when absent (respect an explicit `cache_control` if the OpenAI body somehow carried one). Guard so it is only added when the destination is an Anthropic body (the function already targets Anthropic shape). No new config flag for this phase — automatic insertion is the default; a per-route toggle can follow if operators want to opt out.

#### 2. Anthropic→OpenAI: strip `cache_control`, surface a caching signal

**File**: `src/protocol_translation.rs`

**Intent**: In `anthropic_to_openai_request` (`src/protocol_translation.rs:860-1013`), `cache_control` is already dropped by the `_ => {}` arms in the block loops (`:1027-1064`, `:1096-1125`) and system handling (`:913-926`). Make this explicit and lossless-for-logging: capture whether the source had cache breakpoints so Phase 4 can account for expected cache usage. OpenAI Chat Completions has no native `cache_control` equivalent, so the field is correctly absent from the translated body.

**Contract**: No output change for the upstream body (still no `cache_control`). Add an out-parameter or return-side signal (e.g. extend the return to a small struct `(Value, CacheSignals)` or a sidecar `bool had_cache_control`) so downstream logging knows cache was requested. Keep the function signature backward-compatible by wrapping in a new helper if needed.

#### 3. Same-protocol paths: explicit preservation

**File**: `src/main.rs`, `src/protocol_translation.rs`

**Intent**: Today Anthropic→Anthropic uses `body.clone()` (`src/main.rs:2366`) and OpenAI→OpenAI parses-and-reemits via `build_upstream_request`. Both already preserve `cache_control`. To remove "byte-passthrough luck," add a lightweight normalization that parses the body, verifies `cache_control` survives, and re-serializes — but only if it can be done without breaking byte-identical passthrough for fields Cerebrum doesn't understand. If parse-normalize risks dropping unknown fields, keep byte passthrough and instead add a debug assertion / log that `cache_control` is present when expected.

**Contract**: Prefer the lowest-risk option: keep `body.clone()` passthrough for same-protocol, and add a `tracing::debug!` noting `cache_control` presence. Document the decision in a comment. Do not introduce a regression where unknown Anthropic fields get dropped on the passthrough path.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes.
- httpmock test (OAI→Anthropic): the translated upstream body contains a top-level `cache_control`.
- httpmock test (Anth→OAI): the translated upstream body contains no `cache_control` and the function reports `had_cache_control = true` when the source had it.
- httpmock test (Anth→Anthropic passthrough): `cache_control` in the inbound body reaches the upstream unchanged.

#### Manual Verification:

- Send two consecutive identical Claude Code requests (Anthropic client → Anthropic upstream) through Cerebrum; the second response's usage shows `cache_read_input_tokens > 0`.
- Send an OpenAI-client request routed to an Anthropic upstream; confirm the upstream receives `cache_control`.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: Cache-token usage translation + `InferenceRecord` logging

### Overview

Translate cache tokens in responses so clients see accurate usage in both protocols and streaming modes, then capture token counts and Claude Code attribution into `InferenceRecord` (DB migration + call-site threading).

### Changes Required:

#### 1. Translate cache tokens in non-streaming responses

**File**: `src/protocol_translation.rs`

**Intent**: In `translate_response` (Anth→OAI, `src/protocol_translation.rs:382-502`) read `cache_read_input_tokens` and `cache_creation_input_tokens` alongside `input_tokens`/`output_tokens` (`:474-481`) and emit them as OpenAI `usage.prompt_tokens_details.cached_tokens` (`:496-500`). In `openai_to_anthropic_response` (OAI→Anth, `:1146-1239`) read `prompt_tokens_details.cached_tokens` and emit Anthropic `cache_read_input_tokens` (`:1237`).

**Contract**: Preserve the `total = cache_read + cache_creation + input_tokens` invariant on the Anthropic side. On the OpenAI side, `cached_tokens` maps from `cache_read_input_tokens` (cache reads are the OpenAI-equivalent of cached input).

#### 2. Translate cache tokens in streaming events

**File**: `src/protocol_translation.rs`

**Intent**: In `translate_stream_event` (`src/protocol_translation.rs:624-841`), the `message_start` branch carries `input_tokens` + `cache_creation_input_tokens` and the `message_delta` branch (`:776-831`, usage read `:803-811`) carries `output_tokens` + `cache_read_input_tokens`. Read the full set across both events and translate into the OpenAI streaming-usage chunk (`:812-823`).

**Contract**: Do not assume all usage lands in one event. Accumulate cache fields seen across `message_start`/`message_delta` and emit them once in the terminal usage chunk.

#### 3. Add token + attribution fields to `InferenceRecord`

**File**: `src/persistence.rs`

**Intent**: Extend `InferenceRecord` (`src/persistence.rs:1107-1118`) with nullable columns: `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, and `client_session_id` (from `x-claude-code-session-id`). Keep all new fields optional so existing rows and the memory backend stay valid.

**Contract**: Struct gains `Option<i32>` token fields and `Option<String> session_id`. The SQL insert (`fetch_inferences`/insert path in `persistence.rs`) and the memory backend gain the columns. Update `LatencySummary`/`SavingsEstimate` only if the dashboard will display cache savings in this phase (optional — see Testing).

#### 4. DB migration

**File**: `migrations/005_add_token_and_attribution_columns.sql`

**Intent**: Add the new nullable columns to the `inferences` table.

**Contract**: `ALTER TABLE inferences ADD COLUMN input_tokens INTEGER;` (and likewise for `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `client_session_id TEXT;`). All nullable. Follows the embedded-`sqlx::migrate!` pattern (`migrations/001`–`004`). Add a sibling sqlite path if the sqlite backend's schema is maintained separately.

#### 5. Thread usage capture through `log_classification`

**File**: `src/main.rs`

**Intent**: Update `log_classification` (`src/main.rs:881-919`) and the struct literal (`:901-912`) to accept the token counts and session id, and update all ~20 call sites (`:1229, 2289, 2297, 2315, 2325, 2336, 2350, 2386, 2421, 2436, 2458, 2471, 2497`; streaming `:1181, 1403, 1492`) to pass them. Critically, on streaming paths the record is emitted at stream open (`:1181-1190`) before usage is known — restructure so token fields are finalized when the stream closes (where usage is parsed), not at open.

**Contract**: New `log_classification` params: `usage: Option<UsageBreakdown>` and `session_id: Option<&str>`. For streaming, split into an open call (current behavior) and a finalization update that sets token fields from the parsed terminal usage. Non-streaming paths already parse the body before logging — pass usage directly. This is the highest-risk change; lean on the existing test suite and the lessons.md rule about handler-rewrite regressions.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes; `cargo test persistence` passes (with `DATABASE_URL` set for the migration; `SQLX_OFFLINE=true` otherwise).
- New migration applies cleanly and is idempotent.
- httpmock tests assert the translated OpenAI usage chunk carries `cached_tokens` from an Anthropic `cache_read_input_tokens`, and vice-versa.
- Unit test: an `InferenceRecord` with token fields round-trips through the Postgres and memory backends.

#### Manual Verification:

- A real Claude Code request through Cerebrum shows correct `usage` in the client (matching a direct Anthropic call) and an inference-log row populated with token counts + session id.
- Dashboard (if extended) shows cache savings; otherwise verify via direct DB/`/dashboard/inferences` inspection that the new columns are populated.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation that the manual testing was successful before proceeding.

---

## Testing Strategy

### Unit Tests:

- Header collection helper returns only `anthropic-*` / `x-claude-code-*`, never `authorization`/`x-api-key`.
- `auth_headers_for` prefers client `anthropic-version`, falls back to `2023-06-01`, and omits `anthropic-*` for non-Anthropic providers.
- `translate_request` inserts top-level `cache_control` when absent; respects an existing one.
- `anthropic_to_openai_request` strips `cache_control` and reports `had_cache_control`.
- Usage translation: cache-token round-trip in both directions, streaming and non-streaming; `total` invariant holds.
- `models_handler` returns Anthropic-shape entries with `display_name` and prefixed IDs.

### Integration Tests:

- httpmock 4-way matrix: {OpenAI-client, Anthropic-client} × {OpenAI-upstream, Anthropic-upstream}, each in streaming and non-streaming mode, asserting header forwarding, `cache_control` handling, and usage translation.
- Migration test: `005_add_token_and_attribution_columns.sql` applies on a fresh Postgres (testcontainers) and a sqlite path.

### Manual Testing Steps:

1. Point real Claude Code at Cerebrum (`ANTHROPIC_BASE_URL` + key); run a multi-turn task; confirm `cache_read_input_tokens > 0` on turn 2+.
2. Send a request with an `anthropic-beta` header; confirm it reaches the upstream (debug log or upstream capture).
3. `GET /v1/models` shows friendly `display_name` values in Claude Code's model picker.
4. Inspect `/dashboard/inferences` (or the DB) for a row with populated token counts and session id.

## Performance Considerations

- Header collection is a linear scan of a small `HeaderMap` per request — negligible.
- `cache_control` insertion is a single `serde_json::Map::insert` — negligible.
- Usage translation adds a handful of `.get()` calls per response/stream event — negligible.
- The Phase 4 streaming-log finalization restructure is the only path with latency sensitivity; ensure the finalization update is fire-and-forget (bounded semaphore, as today) so it never blocks the response.

## Migration Notes

- Migration `005_add_token_and_attribution_columns.sql` is additive and nullable — zero downtime, no backfill. Existing rows have NULL token columns.
- The memory backend (`persistence.rs`) needs the same new fields defaulted to `None`.
- No env-var changes; no `config.toml` schema change required for Phase 1–4 (a future per-route cache-insert toggle would add config, deferred).

## References

- Related research: `context/changes/competitive-landscape-gaps/research.md` (Tier-1 #3, #4)
- Prompt caching docs (verified GA): `https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching`
- Claude Code gateway protocol: `https://docs.claude.com/en/docs/claude-code/llm-gateway-protocol`
- Translation module: `src/protocol_translation.rs:37-144` (`translate_request`), `:860-1013` (`anthropic_to_openai_request`), `:382-502` (`translate_response`), `:624-841` (`translate_stream_event`), `:1146-1239` (`openai_to_anthropic_response`)
- Header plumbing: `src/main.rs:1039-1080` (`build_upstream_request`), `src/intent_classifier.rs:436-475` (`auth_headers_for`)
- Logging: `src/persistence.rs:1107-1118` (`InferenceRecord`), `src/main.rs:881-919` (`log_classification`)
- `context/foundation/lessons.md` — handler-rewrite regression rule (re-verify fixes after touching `completion_handler`/`messages_handler`)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: `/v1/models` Anthropic shape + `display_name`

#### Automated

- [x] 1.1 `cargo test` passes with updated models tests
- [x] 1.2 Unit test asserts entries have `display_name` and `claude`/`anthropic`-prefixed `id`
- [x] 1.3 `cargo test auth` and `cargo test routes_auth` pass (endpoint stays unauthenticated)

#### Manual

- [ ] 1.4 `curl /v1/models` returns entries with `display_name`
- [ ] 1.5 Claude Code discovery lists friendly model names

### Phase 2: Header pass-through plumbing

#### Automated

- [ ] 2.1 `cargo test` passes
- [ ] 2.2 httpmock test: Anthropic upstream receives inbound `anthropic-beta`/`anthropic-version` unchanged; OpenAI upstream does not
- [ ] 2.3 `cargo test auth`, `cargo test routes_auth` pass

#### Manual

- [ ] 2.4 Claude Code request with `anthropic-beta` reaches the Anthropic upstream
- [ ] 2.5 OpenAI-upstream routing does not forward `anthropic-beta`

### Phase 3: `cache_control` translation across all four crossings

#### Automated

- [ ] 3.1 `cargo test` passes
- [ ] 3.2 httpmock: OAI→Anthropic translated body has top-level `cache_control`
- [ ] 3.3 httpmock: Anth→OpenAI body has no `cache_control`, reports `had_cache_control`
- [ ] 3.4 httpmock: Anth→Anthropic passthrough preserves `cache_control`

#### Manual

- [ ] 3.5 Two consecutive Claude Code turns show `cache_read_input_tokens > 0` on turn 2
- [ ] 3.6 OpenAI-client → Anthropic upstream receives `cache_control`

### Phase 4: Cache-token usage translation + `InferenceRecord` logging

#### Automated

- [ ] 4.1 `cargo test` passes; `cargo test persistence` passes
- [ ] 4.2 Migration `005` applies cleanly and is idempotent
- [ ] 4.3 httpmock: OpenAI usage chunk carries `cached_tokens` from Anthropic `cache_read_input_tokens` (and reverse)
- [ ] 4.4 Unit test: `InferenceRecord` token fields round-trip through Postgres + memory backends

#### Manual

- [ ] 4.5 Real Claude Code request shows correct client `usage` and a populated inference-log row (tokens + session id)
- [ ] 4.6 Dashboard/DB inspection confirms new columns are populated
