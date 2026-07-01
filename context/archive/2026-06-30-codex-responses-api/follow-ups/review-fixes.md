# Follow-up tasks from Implementation Review: codex-responses-api

This file queues the post-review work that was either deferred or requires manual planning. Each task corresponds to a finding from `reviews/impl-review.md`.

---

## F3 — Extract shared cascade helper (Plan violation: duplicated handler)

**Status**: Pending  
**Finding**: F3 (CRITICAL)

### Task

Refactor `completion_handler` and `responses_handler` to share a single cascade provider loop, eliminating ~330 lines of duplicate logic.

### Approach

1. Extract a helper function `run_cascade(...)` from `completion_handler.rs:194-...` that:
   - Accepts: state, classification, request_body (as Chat JSON), stream flag, and two closures: `on_success(upstream_response)` and `on_error(status, body)` that define the handler-specific response wrapping.
   - Contains the provider iteration, API key resolution, cache check, classification-only short-circuit, upstream dispatch, retry/fallback, and error envelope logging.
2. Modify `completion_handler` to call `run_cascade` with appropriate closures that return Chat JSON (non-streaming) or stream via `handle_streaming_response`.
3. Modify `responses_handler` to call `run_cascade` with closures that:
   - On success for non-streaming: call `response_from_chat` to synthesize Responses envelope.
   - On success for streaming: call `handle_responses_streaming_response`.
   - On error: map upstream status/body to Responses error envelope.
4. Ensure `log_classification` and OTEL metrics are invoked inside the helper (or from closure after success, preserving call sites).
5. Add tests verifying that a change to the cascade in one path is reflected in the other (perhaps a unit test exercising helper directly).

### Files to modify

- `src/proxy/handlers.rs` — extract helper
- `src/proxy/responses_handler.rs` — simplify to thin wrapper around `run_cascade`
- possibly `src/proxy/mod.rs` to export the helper

### Plan reference

Plan §Implementation Approach (line 46): *"The handler is **not** a copy of `completion_handler` — it translates at the boundary and delegates to the existing pipeline via an internal call pattern."*

---

## F1 — Build TranscriptStore (Phase 4.2 deliverable entirely missing)

**Status**: Pending  
**Finding**: F1 (CRITICAL)

### Task

Implement the server-side transcript store so that `previous_response_id` enables multi-turn Codex sessions.

### Scope

1. Create `src/persistence/transcript.rs` with:
   ```rust
   #[async_trait]
   pub(crate) trait TranscriptStore: Send + Sync {
       async fn store_response(&self, response_id: &str, response_json: &str) -> Result<(), String>;
       async fn get_response(&self, response_id: &str) -> Result<Option<String>, String>;
   }
   ```
2. Implement in `src/persistence/sql_backend.rs`:
   - Add `responses` table (iff not exists, migration needed):
     ```sql
     CREATE TABLE IF NOT EXISTS responses (
         id TEXT PRIMARY KEY,
         response_json TEXT NOT NULL,
         created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
     );
     ```
   - Implement `TranscriptStore` for `SqlBackend` using two simple queries (INSERT, SELECT).
   - Add `transcript_store: Option<Arc<dyn TranscriptStore>>` to `AppState` (or `PersistenceConfig`), populated from config if `[transcript_store] enabled = true`.
3. Add a new migration for Postgres (V3), since V2 already shipped. The migration must be additive and safe to run.
4. In `responses_handler.rs`:
   - After translating request, if `previous_response_id` is present and input appears to be a partial transcript (fewer than 2 items or no system message), call `transcript_store.get_response()` and inject prior messages into the Chat body.
   - After a successful response, call `transcript_store.store_response(response_id, &responses_json)`.
   - On `get_response` error or not found, return 400 with a helpful message.
5. Ensure `previous_response_id` is correctly extracted from request JSON (already done via `request_to_chat`) and passed to `extras`.
6. Update OpenAPI and README if necessary (they already mention TranscriptStore; verify actual behavior matches).
7. Add tests for `TranscriptStore` and for the handler's reconstruction logic.

### Plan reference

Plan §Phase 4.2 — 4.6.

### Notes

- The existing V2 migration already added `previous_response_id` and codex headers to `inferences`. Phase 4.4 should also extract codex headers (F2) and this review triaged that as "Fix now" — ensure that is done before Finalize.
- If multi-pod safety is out of scope, single-instance assumption remains.

---

## F4 — Add handler-level integration tests for responses_handler

**Status**: Pending  
**Finding**: F4 (CRITICAL)

### Task

Write the 9 tests listed in the plan's Testing Strategy to cover the entire request pipeline.

### Required tests

1. `test_responses_handler_openai_non_streaming` (R1: Responses → OpenAI-compatible upstream)
2. `test_responses_handler_openai_streaming` (R1 streaming)
3. `test_responses_handler_anthropic_non_streaming` (R2: Responses → Anthropic)
4. `test_responses_handler_anthropic_streaming` (R2 streaming, ensure reasoning events appear)
5. `test_responses_handler_passthrough` (R5: `provider_type: openai_responses` native passthrough)
6. `test_responses_handler_requires_auth` (401 without bearer token)
7. `test_responses_handler_upstream_error_forwards_body` (upstream 429 → Responses error envelope)
8. `test_responses_cache_hit_returns_cached_response` (verify cache hit bypasses upstream; response_id stability from F9)
9. `test_responses_handler_forwards_openai_headers` (e.g., `openai-beta` appears in upstream request)

### Implementation hints

- Use httpmock to spin up a mock upstream server.
- Use the existing test harness patterns from `src/proxy/handlers.rs` tests (see `test_completion_handler_*` for httpmock setup, building test app with `test_app_with_cache` if needed).
- Tests should be placed in `src/proxy/responses_handler.rs` under `#[cfg(test)] mod tests`.
- Follow naming convention `test_<component>_<case>`.

### Plan reference

Plan §Testing Strategy (lines 509-522) lists these tests explicitly.

---

## Optional follow-ups

- Update `AGENTS.md` to document new modules under `Source layout / Testing this module` (see F4 observation).
- Update `README.md` to clarify that `store: true` is a no-op and that multi-turn via `previous_response_id` requires Phase 4 transcript store (currently unimplemented; note the gap if not yet implemented after F1).
- Add sanity check test for `responses_stream.rs` item_id stability across multiple delta events (currently unit tests only count events, not IDs).

---

**All applied fixes** (F2, F5-F10, F9) have been committed to the working tree and verified with `cargo test` (420 tests pass). The pending tasks above must be completed before the change can be considered fully compliant with its own plan.
