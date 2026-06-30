<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Codex Responses API Shim (S-21)

- **Plan**: context/changes/codex-responses-api/plan.md
- **Scope**: Full plan, all 5 phases
- **Diff**: 46b6b72^..HEAD (6 commits, 3973 insertions)
- **Date**: 2025-06-30
- **Verdict**: REJECTED (8 critical findings; Phase 4 deliverable absent; major safety defects in SSE state machine; handler duplication violates plan)

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | FAIL ❌ |
| Scope Discipline | FAIL ❌ |
| Safety & Quality | FAIL ❌ |
| Architecture | WARNING ⚠️ |
| Pattern Consistency | WARNING ⚠️ |
| Success Criteria | WARNING ⚠️ |

## Findings

### F1 — TranscriptStore entirely absent (Phase 4.2 silently dropped)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural; rewires Phase 4 contract
- **Dimension**: Plan Adherence / Scope Discipline
- **Location**: (missing) `src/persistence/transcript.rs`
- **Plan §**: Phase 4.2 (lines 317-333)

**Detail**:
Plan required a NEW `TranscriptStore` trait + Postgres backend + AppState field + `responses` table in migration. None exist. Migration V2 only ALTERs `inferences`; no `responses` table; no `transcript_store` on `AppState`; no lookup/store calls in handler. The OpenAPI spec (`openapi/responses-shim.yaml:141`) still advertises TranscriptStore resolution. Progress rows 4.1/4.2/4.6 are marked `[x]` despite zero implementation. The plan's own "What We're NOT Doing" (line 30) says "transcripts are not persisted" — so the work was silently reclassified as "best-effort" without updating the plan. This contradicts the explicit Phase 4 deliverable.

**Fix A ⭐ Recommended**: Build the TranscriptStore now (plan contract).
- Strength: Honors the plan; enables real `previous_response_id` multi-turn; restores Phase 4 success criteria 4.6.
- Tradeoff: ~150-250 LOC; new Postgres round-trip per turn; needs V3 migration; config knob.
- Confidence: HIGH — plan §4.2 is the spec; migration pattern exists in V2.
- Blind spot: Multi-pod safety (already out of scope per plan §38).

**Fix B**: Update the plan to record the scope cut, remove TranscriptStore promises from OpenAPI/README, remove Phase 4.2 progress row.
- Strength: Aligns plan with reality; cheap.
- Tradeoff: Loses multi-turn functionality advertised in rest of plan + OpenAPI.
- Confidence: MED — depends on whether Codex users actually use `previous_response_id`.
- Blind spot: README caveat at line 228 still promises it.

**Decision**: PENDING

---

### F2 — x-codex-* headers never reach persistence layer

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff
- **Dimension**: Plan Adherence / Data Safety
- **Location**: `src/proxy/util.rs:355-358`, `src/persistence/sql_backend.rs:209,257-272`

**Detail**:
Migration V2 creates 4 new columns; `InferenceRecord` has matching `Option<String>` fields; but data path broken end-to-end:
1. `collect_forward_headers` (util.rs:498-512) allowlists only `anthropic-`, `x-claude-code-`, `openai-`, `x-openai-` — `x-codex-*` silently dropped.
2. `enqueue_inference_record` (util.rs:355-358) hardcodes `codex_installation_id: None, codex_turn_state: None, codex_window_id: None, codex_turn_metadata: None`.
3. SQLite `init_sqlite_schema` (sql_backend.rs:209) only adds `previous_response_id`; 4 `codex_*` columns missing on SQLite path.
4. `Inferences` enum + `insert_once_sql_backend` omit all 5 new fields.
Result: every `codex_*` column is `NULL` in production. Schema committed; migration runs; data never lands.

**Fix**: Thread the 4 headers end-to-end:
- Add `x-codex-` to allowlist in `collect_forward_headers`
- Capture headers in `log_classification_with_usage_and_prev` callers (e.g. responses_handler.rs) — mirror `session_id` capture at handlers.rs:196
- Extend `InferenceRecord` already has fields; ensure `enqueue_inference_record` passes them through
- Extend `Inferences` enum with variants for each field
- Update `init_sqlite_schema` to add the 4 columns
- Update `insert_once_sql_backend` column list to include the new fields
- Add tests that the headers appear in inserted rows

Strength: Honors plan §Phase 4.1+4.4; ~40 LOC.
Tradeoff: Refinery migrations are append-only — once shipped, column must stay.
Confidence: HIGH — capture pattern exists at handlers.rs:196; enum+INSERT pattern similar to `client_session_id`.
Blind spot: SQLite + Postgres paths must be kept in sync; if Postgres V2 migration missed them, a V3 migration is needed for Postgres.

**Decision**: PENDING

---

### F3 — responses_handler duplicates completion_handler cascade (Phase 1.2 drift)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural; long-term maintenance hazard
- **Dimension**: Architecture / Plan Adherence
- **Location**: `src/proxy/responses_handler.rs:194-532`
- **Plan §**: Implementation Approach (line 46) — *"The handler is **not** a copy of `completion_handler` — it translates at the boundary and delegates to the existing pipeline via an internal call pattern."*

**Detail**:
responses_handler.rs re-implements the entire provider cascade loop (~330 LOC): provider iteration, API-key resolution, classification-only short-circuit, upstream dispatch, error envelope construction, `log_classification` calls. Plan also said handler should be ~200 LOC; actual is 558. This is exactly the duplication hazard the plan called out: now `completion_handler`, `messages_handler`, and `responses_handler` are three copies that must stay in sync. Lesson `Re-run review after a follow-up change touches the same handler` already invoked for completion_handler rewrite; this third copy compounds the risk.

**Fix A ⭐ Recommended**: Extract a `run_cascade(state, classification, body, stream, on_success, on_error)` helper from `completion_handler` and call it from both handlers.
- Strength: Single source of truth for cascade; honors plan §Implementation Approach; eliminates drift.
- Tradeoff: ~half-day refactor; touches completion_handler (high-risk file) but extraction is mechanical and behavior-preserving.
- Confidence: HIGH — both cascade loops are structurally identical; Responses-specific bits (error envelopes, response wrapping) live at boundary and slot cleanly into callbacks.
- Blind spot: Must preserve OpenTelemetry metrics keys; careful with OTEL feature-gated sections.

**Fix B**: Keep duplicated cascade, document the deviation as a plan addendum.
- Strength: Cheapest; unblocks merge.
- Tradeoff: Three copies to maintain; future cascade changes will silently diverge. Repeats lesson violation already burned twice.
- Confidence: LOW — high probability next handler-touching commit drops a fix into one path only.

**Decision**: PENDING

---

### F4 — responses_handler has zero integration tests (9-cell matrix absent)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM
- **Dimension**: Success Criteria / Plan Adherence
- **Location**: `src/proxy/responses_handler.rs` (no `mod tests`)
- **Plan §**: Testing Strategy (lines 509-522)

**Detail**:
Plan listed 9 handler-level integration tests by name:
- R1: `test_responses_handler_openai_non_streaming`, `test_responses_handler_openai_streaming`
- R2: `test_responses_handler_anthropic_non_streaming`, `test_responses_handler_anthropic_streaming`
- R5: `test_responses_handler_passthrough`
- Auth/error/cache/header: `test_responses_handler_requires_auth`, `test_responses_handler_upstream_error_forwards_body`, `test_responses_cache_hit_returns_cached_response`, `test_responses_handler_forwards_openai_headers`

`responses_handler.rs` ends at line 558 with no `mod tests` block; 0 handler tests exist. The 52 passing `responses` tests live in `protocol::responses` / `protocol::responses_stream` — they stop at the protocol boundary and never exercise: classification, cascade fallback, auth boundary, cache hit path, upstream error mapping, header forwarding. Phase 1.6 / 2.4 / 3.5 Progress rows claim "Full suite: `cargo test` passes" — true — but the suite lacks the matrix the plan promised.

**Fix**: Add the 9 httpmock-backed tests in `src/proxy/responses_handler.rs` per plan §Testing Strategy.
- Strength: Verifies entire pipeline against spec; gives F3 a regression net for helper extraction.
- Tradeoff: ~300 LOC of test scaffolding; needs httpmock (already used in plan examples).
- Confidence: HIGH — pattern exists for `completion_handler` in `src/proxy/handlers.rs` tests; test factory pattern already exists for building test app with httpmock upstream.
- Blind spot: Anthropic reasoning path (Phase 3.2) needs a specific mock to emit `thinking_delta`; ensure at least one test covers that.

**Decision**: PENDING

---

### F5 — SSE item_id regenerated on every delta event (Phase 2 reliability)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — breaks Codex reassembly
- **Dimension**: Safety & Quality
- **Location**: `src/protocol/responses_stream.rs:319, 341, 372, 395, 416, 428, 448, 464, 480, 509, 525`
- **Evidence**: `format!("msg_{}", Uuid::new_v4())` and similar appear in delta events

**Detail**:
`ensure_reasoning_output_item`, `ensure_msg_output_item`, `ensure_tool_call_items` mint an item id (`rs_<uuid>`, `msg_<uuid>`, `fc_<uuid>`) and emit it in `output_item.added`. Subsequent delta events call `format!(..., Uuid::new_v4())` again — a *new* UUID each time. No field on `ResponsesStreamState` (lines 24-44) stores the minted id. The OpenAI Responses API contract requires clients to correlate delta events to the original output item by `item_id`. Codex will assemble zero coherent output items because every delta appears to belong to a different item. The unit tests (`test_translate_*`) only assert event *count*, not ID stability; this bug passes CI.

**Fix**: Extend `ResponsesStreamState`:
- Add `msg_id: Option<String>`, `fc_ids: Vec<String>` (parallel to `tool_call_ids`), `rs_id: Option<String>`
- In `ensure_msg_output_item`: when creating `output_item.added`, store the minted `msg_id` in `self.msg_id`
- In `ensure_reasoning_output_item`: store `rs_id`
- In `ensure_tool_call_items`: push each `fc_id` onto `self.fc_ids`
- In every delta emitter (content, reasoning, tool_calls arguments, refusal, done events), read from these stored fields:
  - Content delta: use `self.msg_id.as_ref().unwrap()` instead of fresh `format!("msg_{}", Uuid::new_v4())`
  - Reasoning delta: use `self.rs_id.as_ref().unwrap()`
  - Tool call arguments delta: use `self.fc_ids[i]` for `item_id`
  - Done events for content, reasoning, tool calls: use the stored IDs accordingly

Strength: Matches the contract; Codex reassembly works; IDs stable across a stream as plan promised.
Tradeoff: Trivial — adds 3-5 state fields; ~10 emission site edits.
Confidence: HIGH — same shape as existing `tool_call_ids: Vec<String>`; the `ensure_*` functions are the single source of truth for when an item is first created.
Blind spot: Tests must be updated (or new tests added) to assert that `item_id` is constant across deltas for a given item. Existing tests don't catch this bug.

**Decision**: PENDING

---

### F6 — response.completed never emitted on finish_reason-only streams

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — Codex CLI hangs
- **Dimension**: Safety & Quality
- **Location**: `src/protocol/responses_stream.rs:538` (finish_reason branch), `src/proxy/responses_streaming.rs:81-87` (terminal safety net)
- **Evidence**: finish_reason branch sets `state.finished = true` but emits no `response.completed`; safety net guarded by `if !responses_state.finished`

**Detail**:
When upstream sends `finish_reason` (e.g. `"stop"`) in a Chat chunk, `translate_chat_chunk_to_responses_events` emits per-item `.done` events and sets `state.finished = true`. It does NOT emit `response.completed`. The wrapper in `responses_streaming.rs` only emits `response.completed` when the upstream stream ends *and* `!responses_state.finished`. If the upstream closes after sending a chunk with `finish_reason` but no `data: [DONE]` (a common Anthropic and DeepSeek pattern), both sides think they're done: the state machine marked `finished=true` and the upstream byte stream ended, so the wrapper's `None` arm sees `responses_state.finished == true` and skips emitting the terminal event. The client never receives `response.completed` and hangs. Codex CLI's SSE consumer requires `response.completed` to consider the response final.

**Fix**: Emit `response.completed` inside the finish_reason branch, before setting `state.finished = true`. The `finalize_stream` function already exists (responses_stream.rs:545) and constructs the event with usage (currently zero). Call it and push the event to `events` before setting `state.finished = true`. Alternatively, drop the `!responses_state.finished` guard and always call `finalize_stream` on `None` arm (safer—for any completion path not already emitted), but then usage must be plumbed through the state until the end.
- Strength: Restores terminal-event contract; Codex streams complete cleanly; uses existing `finalize_stream`.
- Tradeoff: One or two line change in finish_reason branch; if using `finalize_stream`, ensure `state.finished` is set after call. Usage currently zero — consider whether to propagate actual usage (requires passing it through the state machine, out of scope for the minimal fix).
- Confidence: HIGH — `finalize_stream` already exists; just call it from the right place.
- Blind spot: Zero usage in `finalize_stream` payload should be addressed (Phase 3 usage tracking works, so we can accumulate it in the state from the final upstream message if needed, but that's a separate improvement).

**Decision**: PENDING

---

### F7 — SSE chunk boundaries not buffered; partial lines silently dropped

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff
- **Dimension**: Safety & Quality
- **Location**: `src/proxy/responses_streaming.rs:52-72` (loop body)
- **Evidence**: `for line in chunk_str.split('\n')` processes each TCP chunk in isolation; no accumulator

**Detail**:
`responses_streaming.rs` receives upstream byte chunks. For each chunk, it splits on `'\n'` and processes each line immediately. If a TCP packet splits mid-SSE line (e.g., chunk ends with `"data: {\"ch"` and next chunk starts with `oice\":...}\n\n"`), the first partial line becomes a string that does not start with `"data: "` (it starts with `"data: {\"ch"`) and is silently skipped. The second chunk starts with `oice\":...}\n\n"` and also does not start with `"data: "` so it is skipped as well. The entire SSE event is lost. This directly contradicts the proven pattern in `streaming.rs:246-261` (`handle_anthropic_streaming_response`) which buffers bytes in a `String` and uses a `parse_sse_events` function that handles split lines with a 1 MB cap. The existing lesson "Handle upstream error bodies without full buffering where possible" is satisfied if the cap is honored; this fix follows the same bounded-buffer pattern.

**Fix**: Mirror `streaming.rs` pattern:
- Add a `buffer: String` field on the spawn-task state (outside the mpsc loop)
- Instead of `for line in chunk_str.split('\n')`, append `chunk_str` to buffer, find last `\n\n` boundary, process complete events up to boundary, retain suffix (including partial line) in buffer.
- Use the same `parse_sse_events` approach: while buffer has `\n\n`, extract the event line (everything before first `\n\n`), feed to `translate_chat_chunk_to_responses_events`, drain; keep remainder after last `\n\n`.
- Honor a maximum buffer size (1 MB) and return an error if exceeded (mirror `streaming.rs:261` comment about cap).

Strength: Reuses proven pattern; handles split lines correctly; bounded memory via cap.
Tradeoff: ~30 LOC; one extra state field on the task; keepsafety-cap from streaming.rs.
Confidence: HIGH — pattern exists in streaming.rs; we just need to copy and adapt.
Blind spot: Keepalive comment lines (`: keepalive\n\n`) are single-line SSE events; the buffer parser must handle them correctly — `parse_sse_events` does, so reuse that exact logic.

**Decision**: PENDING

---

### F8 — Missing API key silently cascades without warning (lesson violation)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff
- **Dimension**: Safety & Quality (lesson-driven)
- **Location**: `src/proxy/responses_handler.rs:202-228` (missing env branch), `229-251` (None branch)
- **Plan/Evidence**: Lesson "Log operational failures before falling back" (lessons.md:40). `completion_handler` at `handlers.rs:323-326` logs: `warn!("API key env var '{:?}' is missing or empty for provider {}; cascading", ...)`

**Detail**:
When a provider's `api_key_env` is missing/empty or `api_key_env` is `None` (meaning no credential required, intended only for classification-only scenarios), `responses_handler` falls through to `continue` without any log. Operators have no signal that a credential is misconfigured until traffic fails downstream. `completion_handler` contains the correct logging pattern but `responses_handler.rs` imports nothing from `tracing` — so adding any log requires also adding the import.

**Fix**:
- Add `use tracing::{debug, warn};` to imports at top (mirror `handlers.rs:1-8`)
- In the `Some(env_name)` missing/empty branch, before the `continue` (and before the `if is_last` return), add:
  ```rust
  warn!("API key env var '{:?}' is missing or empty for provider {}; cascading", env_name, provider.model);
  ```
- In the `None` branch, before the `continue`, add:
  ```rust
  warn!("no api_key_env configured for provider {}; cascading", provider.model);
  ```
- Ensure these appear at the same indentation as the existing `if is_last { log_classification... } continue;` blocks.

Strength: Restores the operator signal; matches the canonical pattern; ~5 line change total.
Tradeoff: None.
Confidence: HIGH — copy-paste from `handlers.rs`.
Blind spot: In the `None` branch, `handlers.rs` logs at the same place with identical message. Ensure we add the warning there too.

**Decision**: PENDING

---

### F9 — response_id regenerated on every cache hit

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: `src/protocol/responses.rs:457`, `src/proxy/responses_handler.rs:100-117`
- **Evidence**: `CachedEntry` stores only `body: String`, `status: u16`; on hit, `response_from_chat` creates fresh `resp_{uuid}` at line 457

**Detail**:
The cache stores Chat response bodies. On a cache hit, `responses_handler.rs` re-parses the Chat JSON and calls `response_from_chat` which mints a fresh `resp_<uuid>` (responses.rs:457). Codex's multi-turn flow treats `response.id` as stable across retransmissions of the same turn; the ability to reference a previous response via `previous_response_id` relies on the ID being stable. Regenerating it on every identical request breaks the `previous_response_id` chain — a cached response cannot be referenced. This is particularly problematic given F1 (TranscriptStore absent): even if TranscriptStore were built, the cache would still hand out new IDs on every hit.

**Fix A ⭐ Recommended**: Persist the assigned `response_id` on `CachedEntry`.
- Extend `CachedEntry` in `src/proxy/cache.rs` (likely) to include `response_id: String` (or `Option<String>` for backward compatibility during cache warm-up; but cache entries are written freshly by this code so we can always store it)
- In the successful upstream path inside `responses_handler`, after generating the `responses_json`, also extract the `response_id` from it (or from the `extras` if that carries it) and include it in the stored entry.
- On cache hit, read the stored `response_id` and use it when constructing the `responses_json` response — i.e., avoid calling `response_from_chat` at all; just return the cached `responses_json` directly, or if we must re-wrap due to extras (e.g. `previous_response_id` from the *current* request), at least preserve the original `response_id` from the stored field.
Strength: Preserves `previous_response_id` chain for cache hits; small struct change; minimal runtime cost.
Tradeoff: Need to migrate in-memory `Cache` entries? In-process cache is ephemeral, so we can just change the struct and existing entries will miss after restart — acceptable.
Confidence: HIGH — `CachedEntry` is small; change is localized.
Blind spot: Cache key is currently `sha256(body)` where body is the original Responses request bytes. That key is fine for deduplication; we just need to preserve the response_id from the first synthesis across hits.

**Fix B**: Cache the synthesized Responses envelope, not the Chat body.
- Store `responses_json` directly in the cache instead of the Chat body.
- Then on hit we return it verbatim, preserving its `response_id` naturally.
- Strength: Simplifies cache-hit path; eliminates re-wrap step; naturally preserves ID.
- Tradeoff: Cache key currently computed on request body bytes; this still works. But the re-wrap approach was designed to handle cases where `previous_response_id` varies per request while the cache key is just the request body — the original design wanted to synthesize different Responses envelopes for cached Chat responses. If we cache the envelope, we lose that flexibility (but the extras that vary per request are minimal — mostly `previous_response_id` echo). Might be acceptable to not echo the request's `previous_response_id` on cache hit (it's a best-effort shim anyway).
- Confidence: MED — depends on whether any caller expects the cached response to reflect the current request's extras (like `previous_response_id`). The plan didn't emphasize that nuance.

**Decision**: PENDING

---

### F10 — input[] array has no DoS cap

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious
- **Dimension**: Safety & Quality
- **Location**: `src/protocol/responses.rs:273-280`
- **Evidence**: `for (idx, item) in items.iter().enumerate()` with no length check; sibling extractors cap at 1000 items (`src/persistence/types.rs:167`, `210`)

**Detail**:
`request_to_chat` walks the entire `input[]` array with no length cap. If a client sends an array with 1,000,000 items, the gateway will allocate memory for all of them in the `messages` vector before any validation error can be returned. This is an unbounded memory amplification path. Other input processing functions in the codebase (`extract_*`) enforce a 1000-item cap; this should adhere to the same policy.

**Fix**: Before iterating, check `items.len()` and if > 1000 (or some reasonable constant), return `Err(ResponsesRejection::bad_request("'input' array exceeds maximum length of 1000 items"))`. Use the same constant as elsewhere; define once if needed.
- Strength: Bounds memory; clear rejection message; consistent with codebase conventions.
- Tradeoff: One line + one constant (or reuse existing).
- Confidence: HIGH — pattern exists in `src/persistence/types.rs:167` and `210` (e.g., check for `prompt_messages.len() > 1000`).
- Blind spot: None; this is a defensive cap.

**Decision**: PENDING

---

## Observations (non-blocking)

- Header allowlist broadened to `openai-` + `x-openai-` prefixes (util.rs:502-503) — catches more headers than plan's three named ones, but achieves goal (+ `x-openai-internal-codex-responses-lite`).
- Migration filename `V2__add_codex_headers.sql` uses existing refinery convention (V<version>__) vs plan's date format (20260701...); acceptable.
- OpenAPI filename `openapi/responses-shim.yaml` drops `.openapi` segment vs plan's `responses-shim.openapi.yaml` — matches sibling `completions.yaml`.
- Cache key uses `sha256(&body)` of full bytes, not just `input[]`; broader but dedup still works.
- Cache check runs *after* `request_to_chat` (responses_handler.rs:88) vs plan's "before translation"; still correct due to re-wrap on hit.
- `request_to_chat` signature omits `headers: &HeaderMap` vs plan (responses.rs:71) — cosmetic, headers read in handler.
- `scripts/test-codex-e2e.sh` doesn't start a mock upstream; lighter scope than plan §5.5 but "no E2E test" carve-out already in plan.
- Dashboard renders `previous_response_id` as static `<span>` (inferences.html:79) not clickable link per plan §Phase 4.5.
- `tracing` import + warning pattern absent throughout responses_handler.rs (F8).
- `log_classification_with_usage_and_prev` (util.rs:276-301) is a 9-parameter wrapper that just forwards to `enqueue_inference_record` — parameter sprawl could be inlined.
- AGENTS.md lacks "Current test inventory" section referenced in plan §5.3; new modules not documented.
- Test coverage: `cargo test` passes (420 tests), but none of the 9 handler-level integration tests exist (F4).

---

## Manual verification items (unchecked)

Per plan Progress manual rows (all phases), these remain pending:

1.7–1.12 Phase 1 manual curl checks (6 items)
2.5–2.7 Phase 2 streaming curl checks (3 items)
3.6–3.8 Phase 3 reasoning + cache + warning checks (3 items)
4.4–4.6 Phase 4 dashboard + log + restart (3 items; 4.6 blocked by F1 TranscriptStore absent)
5.4–5.6 Phase 5 Codex CLI + README + OpenAPI verify (3 items)

All 18 manual items are unchecked. Combined with the CRITICAL findings, end-to-end Codex CLI usage will fail: SSE instability (F5), missing terminal event (F6), and TranscriptStore absence (F1) alone prevent functional multi-turn sessions.

---

## Summary

**Overall Verdict: REJECTED**

The implementation delivered a robust protocol-translation layer (52 unit tests pass) but failed on:

- **Architecture**: Duplicated cascade loop (F3) violates explicit plan constraint.
- **Safety**: Three critical SSE bugs (F5 item_id instability, F6 missing completion, F7 unbuffered chunks) that will break streaming Codex sessions.
- **Phase 4**: Entire TranscriptStore feature cut silently (F1); header wiring broken end-to-end (F2).
- **Testing**: Zero handler-level integration tests (F4) leave the pipeline unverified.
- **Reliability**: Silent fallback on missing API key (F8) violates operational logging lessons.
- **Cache semantics**: response_id regeneration on cache hits (F9) undermines multi-turn.
- **Security**: Unbounded input[] (F10) is a minor DoS surface.

Of the 10 findings, 6 are CRITICAL correctness issues (F1, F2, F3, F5, F6, F7, F8 — actually 7 if counting F2+F3+F5+F6+F7+F8+F1 = 7? let's recount) — wait, 8 CRITICAL total includes F4 as well. 8 CRITICAL, 2 WARNING.

Before any merge, the following must be addressed:
- F5, F6, F7: SSE state machine fixes (10-30 LOC each)
- F8: add warnings (5 LOC)
- F10: input cap (5 LOC)
- F9: cache response_id persistence (small)
- F2: header wiring end-to-end (~40 LOC)
- F3: refactor to shared cascade helper (significant but mechanical)
- F1: build TranscriptStore (~150-250 LOC) OR scope-cut plan/OpenAPI (a decision)
- F4: add 9 integration tests (~300 LOC)

Recommendation: Apply fixes in the order F8, F10, F5, F6, F7, F9, F2, then tackle F3 (refactor) and F1 (scope decision). F4 should be written alongside F3 to provide safety net for the refactored cascade.

**End of Report**
