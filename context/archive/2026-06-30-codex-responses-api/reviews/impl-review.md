<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Codex Responses API Shim

- **Plan**: context/changes/codex-responses-api/plan.md
- **Scope**: Phases 1–5 of 5 (all phases)
- **Date**: 2026-06-30
- **Verdict**: REJECTED (pre-triage) → APPROVED (post-triage)
- **Findings**: 1 critical  5 warnings  4 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING |
| Scope Discipline | PASS |
| Safety & Quality | FAIL |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | FAIL |

## Findings

### F1 — OOB index panic in tool-call SSE path

- **Severity**: ❌ CRITICAL
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/protocol/responses_stream.rs:384
- **Detail**: `state.fc_ids[i]` is a direct index with no bounds check. `ensure_tool_call_items` grows `fc_ids` from the current delta's tool_calls array, but an upstream may send partial deltas that skip index 0 or use non-sequential indices. Any such chunk panics the spawned streaming task and terminates the SSE stream silently.
- **Fix**: Replace `state.fc_ids[i]` with `state.fc_ids.get(i).map(|s| s.as_str()).unwrap_or("").to_string()`.
- **Decision**: FIXED

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/proxy/responses_handler.rs:504
- **Detail**: `responses_json.get("response").and_then(|r| r.get("id"))` — no `"response"` wrapper key exists. `response_from_chat()` returns a flat object with `"id"` at the top level. This always yields `""`, so the cache discriminator (`if !entry.response_id.is_empty()`) never fires for Responses entries. Plan required stable response_id across cache hits.
- **Fix**: Change `responses_json.get("response").and_then(|r| r.get("id"))` to `responses_json.get("id")`.
- **Decision**: FIXED

### F3 — Test uses `match_header` instead of `header` (compile error)

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency / Success Criteria
- **Location**: src/proxy/responses_handler.rs:955
- **Detail**: `.match_header("openai-beta", "test-beta")` does not exist on `httpmock::When`; correct method is `.header(…)`. All sibling tests in handlers.rs use `.header(…)`. This is a compile error that blocks the entire test suite — `cargo test auth` and `cargo test` abort with E0599.
- **Fix**: Change `.match_header(` to `.header(` at line 955.
- **Decision**: FIXED

### F4 — Streaming upstream error not wrapped in Responses envelope

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/proxy/responses_handler.rs:348
- **Detail**: Streaming error path calls `handle_streaming_error()` and returns raw upstream error shape. Non-streaming error path (lines 378–395) correctly calls `map_upstream_error_to_responses`. Plan requires all error responses to use the Responses envelope shape.
- **Fix A ⭐ Recommended**: Wrap the streaming error response body through `map_upstream_error_to_responses` before returning.
  - Strength: Consistent error shape for both paths; Codex CLI expects Responses envelope everywhere.
  - Tradeoff: Requires buffering the error body before returning — small allocation for error cases only.
  - Confidence: HIGH — plan is explicit; non-streaming path is already correct.
  - Blind spot: None significant.
- **Fix B**: Document the divergence in the plan as an accepted deviation.
  - Strength: Zero code change.
  - Tradeoff: Codex CLI may fail to parse non-Responses error; clients need two error-handling branches.
  - Confidence: LOW — Codex CLI likely requires Responses error shape everywhere.
  - Blind spot: Haven't tested Codex CLI's actual error-handling path.
- **Decision**: FIXED via Fix A

### F5 — `unwrap()` on `rs_id`/`msg_id` will panic if invariant breaks

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/protocol/responses_stream.rs:120, 168, 353
- **Detail**: `self.rs_id.as_ref().unwrap()` and `self.msg_id.as_ref().unwrap()` rely on a logical invariant (id set before flag raised) not enforced by the type system. A future refactor that sets `*_output_item_emitted = true` without the id will panic in production.
- **Fix**: Replace `.as_ref().unwrap()` with `.as_deref().unwrap_or("unknown_id")` at lines 120, 168, and 353.
- **Decision**: FIXED

### F6 — SSE drain off-by-one can corrupt event framing

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/proxy/responses_streaming.rs:71
- **Detail**: `buffer.drain(..=pos)` where `pos` is the index of the first `\n` in `\n\n`. Drains through the first newline only, leaving the second `\n` at the front of the buffer. Next call to `buffer.find("\n\n")` immediately matches the residual `\n` plus the first `\n` of the next event boundary, splitting events at wrong offsets on tightly-packed chunks.
- **Fix A ⭐ Recommended**: Change `buffer.drain(..=pos)` to `buffer.drain(..pos+2)`.
  - Strength: One-character change; directly fixes the off-by-one. SSE spec requires consuming both `\n`s.
  - Tradeoff: None.
  - Confidence: HIGH.
  - Blind spot: Whether existing tests cover tightly-packed chunks.
- **Fix B**: Refactor to use the line-iterator approach from `stream.rs`.
  - Strength: Aligns two SSE parsers; future-proof.
  - Tradeoff: ~20-30 lines of change.
  - Confidence: MED.
  - Blind spot: None significant.
- **Decision**: FIXED via Fix A

### F7 — Migration missing `CREATE TABLE responses`

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: migrations/V2__add_codex_headers.sql
- **Detail**: Phase 4 specified `CREATE TABLE IF NOT EXISTS responses (...)`. Table absent. `TranscriptStore` also not implemented. Scope guardrail "No server-side transcript store" overrides — harmless now, but plan inconsistency.
- **Fix**: Add a comment in the migration noting the `responses` table was deferred per scope guardrail.
- **Decision**: FIXED

### F8 — `response.completed` always emits zero usage tokens

- **Severity**: OBSERVATION
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/protocol/responses_stream.rs:246–300
- **Detail**: `ResponsesStreamState` has no usage accumulator. `finalize_stream` emits `"usage": {"input_tokens": 0, "output_tokens": 0}`. OpenAI Chat SSE streams emit usage in the last delta chunk; Responses streaming clients use this for cost attribution.
- **Fix**: Add `accumulated_usage: Option<UsageData>` to `ResponsesStreamState`; populate it from final-chunk `"usage"` field; propagate to `finalize_stream`.
- **Decision**: FIXED

### F9 — Empty `input` array not rejected upfront

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/protocol/responses.rs:273
- **Detail**: `input: []` passes validation and sends a zero-message request to upstream, which responds with its own 400. Client sees a generic upstream error rather than a clear Frugalis-originated 400.
- **Fix**: Add `if items.is_empty() { return Err(bad_request("'input' array must not be empty")) }` before the loop.
- **Decision**: FIXED

### F10 — `x-codex-*` headers forwarded to all upstream providers

- **Severity**: OBSERVATION
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/proxy/util.rs:518–533
- **Detail**: `x-codex-*` headers (installation ID, turn state, window ID, metadata) are in the forwarding allowlist and reach all downstreams (OpenAI, Anthropic, NIM). These internal attribution headers are already captured in `responses_handler.rs:89–104` for persistence. Forwarding them to third-party providers is a minor privacy concern and risks provider-side validation failures.
- **Fix**: Remove `x-codex-*` from the `collect_forward_headers` allowlist.
- **Decision**: FIXED
