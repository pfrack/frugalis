<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: OpenAI → Anthropic Protocol Translation

- **Plan**: context/changes/translate-openai-to-anthropic/plan.md
- **Scope**: Phase 1–3 of 3
- **Date**: 2026-06-23
- **Verdict**: NEEDS ATTENTION
- **Findings**: 1 critical · 1 warning · 4 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS |

## Findings

### F1 — buffer.clear() discards partial SSE events at TCP chunk boundaries

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:1234-1237
- **Detail**: After calling parse_sse_events(&buffer), if any events are returned, buffer.clear() runs unconditionally. However, parse_sse_events has a trailing-flush heuristic (line 598) that treats incomplete data (no trailing \n\n) as a complete event. When a TCP chunk delivers one complete event + a partial second event, the partial event is either (a) misinterpreted as complete (corrupted JSON → translate returns None → event silently lost), or (b) discarded entirely. This causes dropped content in production SSE streams whenever TCP segmentation splits an event across chunks.
- **Fix**: Replace buffer.clear() with boundary-aware draining — only remove bytes up to the last double-newline boundary, keeping the trailing incomplete fragment for the next chunk.
  - Strength: Matches standard SSE parser semantics (RFC 8895); parse_sse_events' trailing-flush only fires on stream end (None arm at line 1258 already handles this correctly).
  - Tradeoff: Requires modifying parse_sse_events to also return the consumed byte count, OR finding the last \n\n in the buffer before clearing.
  - Confidence: HIGH — the None arm already does the right thing (flushes remainder at stream end). The chunk arm just needs the same discipline.
  - Blind spot: Existing e2e tests pass because httpmock delivers the full SSE body in a single chunk. Multi-chunk delivery is not tested.
- **Decision**: FIXED — boundary-aware drain via `buffer.windows(2).rposition(|w| w == b"\n\n")`

### F2 — Unbounded SSE buffer growth in Anthropic streaming path

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:1233
- **Detail**: buffer.extend_from_slice(&bytes) grows without bound. If an upstream delivers large events or delays sending \n\n separators, memory grows indefinitely. The existing OpenAI streaming path (line 986) does not buffer at all — it forwards chunks directly.
- **Fix**: Add a size cap (e.g. 1 MB) and break with an SSE error event if exceeded.
- **Decision**: FIXED — 1 MB cap with SSE error event on overflow

### F3 — Silent fallback on tool input serialization failure

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality (Lessons violation)
- **Location**: src/protocol_translation.rs:~414
- **Detail**: unwrap_or_else(|_| "{}".to_string()) swallows serialization failure. Per lessons.md "Log operational failures before falling back", a debug!() log should precede the fallback.
- **Fix**: Add tracing::debug!("tool input serialization failed: {e}") before the fallback.
- **Decision**: FIXED — debug log added before fallback

### F4 — No tracing instrumentation in protocol_translation module

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/protocol_translation.rs (whole file)
- **Detail**: Module has zero tracing calls. Contrast with intent_classifier.rs which uses tracing::warn!/debug! at operational boundaries. For a protocol boundary module handling external SSE streams, unknown event types silently return None with no diagnostic trail.
- **Fix**: Add tracing::debug! for the _ => None arm in translate_stream_event and for any fallback paths.
- **Decision**: FIXED — debug logging added for unknown event types, block types, and delta types

### F5 — handle_anthropic_streaming_error near-duplicates handle_streaming_error

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/main.rs:1162-1195
- **Detail**: Nearly identical to handle_streaming_error (1122-1155) with only the translate_error() call as the difference. Small maintenance risk — future changes to one may miss the other.
- **Fix**: Extract shared logic into a helper taking an error-body transform closure.
- **Decision**: FIXED — extracted handle_streaming_error_with_transform with closure parameter

### F6 — Unplanned manual-test/ files (benign scope addition)

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: manual-test/lib.sh, manual-test/run.sh
- **Detail**: Two test infrastructure files not in the plan's "Changes Required". They provide manual integration test harness — useful and non-invasive. Committed in a separate commit (dba87e4) with clear scope separation.
- **Fix**: No action needed. Acknowledge as valid test infrastructure.
- **Decision**: ACKNOWLEDGED — valid test infrastructure, no action needed
