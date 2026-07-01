---
date: 2026-07-01T11:51:54Z
researcher: pfrack
git_commit: c12869887d04154c0f7ec10f25dd58651682433c
branch: testing-proxy-translation-contracts
repository: frugalis
topic: "Misleading protocol/ file names in src/protocol/"
tags: [research, codebase, protocol, naming, refactoring, translation]
status: complete
last_updated: 2026-07-01
last_updated_by: pfrack
---

# Research: Misleading protocol/ file names in src/protocol/

**Date**: 2026-07-01T11:51:54Z
**Researcher**: pfrack
**Git Commit**: c12869887d04154c0f7ec10f25dd58651682433c
**Branch**: testing-proxy-translation-contracts
**Repository**: frugalis

## Research Question

What is the full scope and impact of misleading file names in `src/protocol/`? The frame.md identifies five hypotheses about naming problems. This research maps all call sites to quantify the blast radius of potential renames and validates the frame's hypotheses against live code.

## Summary

**All 5 frame hypotheses are confirmed by live code evidence.** The protocol module has 58 external call sites across 5 proxy files. Renaming would require changes in `handlers.rs` (3 sites), `upstream.rs` (4), `streaming.rs` (11), `responses_handler.rs` (27), and `responses_streaming.rs` (13). The heaviest consumer (`responses_handler.rs:27` calls to `protocol::responses`) would be most affected by the `responses` → `responses_api` rename. All callers use fully-qualified `crate::protocol::submodule::function()` paths (no short imports), so renames are grep-simple. The `responses` submodule alone accounts for nearly half (26/58) of all external references.

## Detailed Findings

### File Content Analysis — Validating Frame Hypotheses

#### H1: Direction-Ambiguity (STRONG — confirmed)

- `src/protocol/request.rs:8` — `translate_request` goes **O→A** (OpenAI input → Anthropic Messages format)
- `src/protocol/response.rs:10` — `translate_response` goes **A→O** (Anthropic response → OpenAI Chat Completions)
- `src/protocol/request.rs:365` — `anthropic_to_openai_request` goes **A→O** (Anthropic input → OpenAI Chat Completions)
- `src/protocol/response.rs:209` — `openai_to_anthropic_response` goes **O→A** (OpenAI response → Anthropic Messages)
- Callers confirm bidirectionality: `handlers.rs:433` calls `translate_request` (O→A), `handlers.rs:1244` calls `anthropic_to_openai_request_with_cache_signal` (A→O) from the same `request` module. `upstream.rs` uses both `translate_response` (A→O at :192) and `openai_to_anthropic_response` (O→A at :260) from `response`.
- No implicit default direction — callers must know the mapping for each function.

#### H2: File Names Promise Data Models but Contain Only Functions (STRONG — confirmed)

- `src/protocol/request.rs` (1274+ lines): Zero structs, zero enums, zero type aliases. All `pub` items are functions operating on raw `serde_json::Value`. Module section comments (§5) partition the file into "Request Translation (Anthropic → OpenAI)" and bidirectional helpers.
- `src/protocol/response.rs` (834 lines): Zero data types. Four `pub fn`s: `translate_response` (A→O), `translate_error` (A→O), `openai_to_anthropic_response` (O→A), `openai_to_anthropic_error` (O→A).
- `src/protocol/stream.rs` (918 lines): Contains state structs (`StreamTranslateState` at :8, `AnthropicStreamState` at :385) — but these are translation session state machines, not domain models. No `Request` or `Response` type exists.
- By contrast, `routing/routes.rs:19-121` correctly contains `ProviderEntry`, `RouteEntry`, `ModelCosts` — actual data models with an accurate name (`routes`).

#### H3: `stream.rs` Too Generic (STRONG — confirmed)

- All 8 `pub` items in `stream.rs` are Anthropic↔OpenAI SSE protocol translation:
  - `StreamTranslateState` (A→O streaming state machine, :8)
  - `stream.rs:68` — `parse_sse_events` — generic name, but zero non-Anthropic callers in practice (only called from `streaming.rs` and `responses_streaming.rs`, both in Anthropic SSE contexts)
  - `stream.rs:124` — `translate_stream_event` (A→O SSE event translation)
  - `stream.rs:385` — `AnthropicStreamState` (O→A streaming state machine)
  - `stream.rs:429` — `openai_to_anthropic_stream_event` (O→A SSE)

#### H4: `responses` vs `responses_stream` Collision (MODERATE — confirmed)

- `responses.rs` (1187 lines): OpenAI Responses API shim with `request_to_chat`, `response_from_chat`, `wrap_error_as_responses`, `map_upstream_error_to_responses`. Contains data models: `ResponsesRequestExtras` (:4), `ResponsesRejection` (:23).
- `responses_stream.rs` (740 lines): Responses API SSE streaming with `translate_chat_chunk_to_responses_events`, `finalize_stream`. Contains data models: `SseEvent` (:4), `ResponsesStreamState` (:24).
- Zero shared types, zero overlapping callers: `responses_handler.rs` references `responses` 26 times; `responses_streaming.rs` references `responses_stream` 6 times.
- Only signal differentiating them: the `_stream` suffix.

#### H5: Noun Convention is Fine; Discoverability is the Problem (PARTIAL — confirmed)

- Callers import from 1-2 submodules each (max 6 symbols per file)
- `mod.rs:1-5` has **zero documentation** — no `//!` explaining the taxonomy
- All 58 external call sites use fully-qualified `crate::protocol::submodule::function()` paths with no `use` imports (single exception: `responses_streaming.rs:10` imports `ResponsesStreamState` via `use`)

### Call Site Blast Radius — All External References to `crate::protocol::*`

#### `src/proxy/handlers.rs` (3 call sites)
| Line | Submodule | Function | Direction |
|------|-----------|----------|-----------|
| 433 | `request` | `translate_request` | O→A |
| 1244 | `request` | `anthropic_to_openai_request_with_cache_signal` | A→O |
| 1371 | `response` | `openai_to_anthropic_error` | O→A |

#### `src/proxy/upstream.rs` (4 call sites)
| Line | Submodule | Function | Direction |
|------|-----------|----------|-----------|
| 165 | `response` | `translate_error` | A→O |
| 192 | `response` | `translate_response` | A→O |
| 227 | `response` | `openai_to_anthropic_error` | O→A |
| 260 | `response` | `openai_to_anthropic_response` | O→A |

#### `src/proxy/streaming.rs` (11 call sites)
| Line | Submodule | Function |
|------|-----------|----------|
| 151 | `response` | `translate_error` |
| 238 | `stream` | `StreamTranslateState::default()` |
| 254 | `stream` | `parse_sse_events` |
| 264 | `stream` | `translate_stream_event` |
| 287 | `stream` | `parse_sse_events` |
| 290 | `stream` | `translate_stream_event` |
| 391 | `stream` | `AnthropicStreamState::default()` |
| 407 | `stream` | `parse_sse_events` |
| 416 | `stream` | `openai_to_anthropic_stream_event` |
| 438 | `stream` | `parse_sse_events` |
| 441 | `stream` | `openai_to_anthropic_stream_event` |

#### `src/proxy/responses_handler.rs` (27 call sites)
| Line | Submodule | Function |
|------|-----------|----------|
| 25, 38, 52, 72, 182, 212, 297, 303, 333, 338, 386, 391, 423, 429, 626, 643, 693, 706, 716, 749 | `responses` | `wrap_error_as_responses` |
| 67 | `responses` | `request_to_chat` |
| 131, 635 | `responses` | `response_from_chat` |
| 450, 478, 609 | `responses` | `map_upstream_error_to_responses` |
| 317 | `request` | `translate_request` |

#### `src/proxy/responses_streaming.rs` (13 call sites)
| Line | Submodule | Function |
|------|-----------|----------|
| 10 | `responses_stream` | `ResponsesStreamState` (import) |
| 74, 244, 274 | `responses_stream` | `translate_chat_chunk_to_responses_events` |
| 106, 285 | `responses_stream` | `finalize_stream` |
| 202 | `stream` | `StreamTranslateState::default()` |
| 227, 265 | `stream` | `parse_sse_events` |
| 237, 268 | `stream` | `translate_stream_event` |

### Summary by Submodule
| Submodule | External Call Sites | Files |
|-----------|-------------------|-------|
| `responses` | 26 | `responses_handler.rs` (26) |
| `stream` | 17 | `streaming.rs` (10), `responses_streaming.rs` (7) |
| `response` | 6 | `upstream.rs` (4), `streaming.rs` (1), `handlers.rs` (1) |
| `responses_stream` | 6 | `responses_streaming.rs` (6) |
| `request` | 3 | `handlers.rs` (2), `responses_handler.rs` (1) |
| **Total** | **58** | |

## Code References

- `src/protocol/mod.rs:1-5` — Module declarations with zero documentation
- `src/protocol/request.rs:8` — `translate_request` (O→A), implicit unnamed direction
- `src/protocol/request.rs:365` — `anthropic_to_openai_request` (A→O), opposite direction in same file
- `src/protocol/response.rs:10` — `translate_response` (A→O)
- `src/protocol/response.rs:160` — `translate_error` (A→O)
- `src/protocol/response.rs:209` — `openai_to_anthropic_response` (O→A)
- `src/protocol/response.rs:330` — `openai_to_anthropic_error` (O→A)
- `src/protocol/stream.rs:8` — `StreamTranslateState` (A→O streaming state)
- `src/protocol/stream.rs:68` — `parse_sse_events` (generic name, Anthropic-only callers)
- `src/protocol/stream.rs:124` — `translate_stream_event` (A→O)
- `src/protocol/stream.rs:385` — `AnthropicStreamState` (O→A streaming state)
- `src/protocol/stream.rs:429` — `openai_to_anthropic_stream_event` (O→A)
- `src/protocol/responses.rs:4` — `ResponsesRequestExtras` (data model in responses file)
- `src/protocol/responses.rs:23` — `ResponsesRejection` (data model)
- `src/protocol/responses.rs:185` — `request_to_chat` (Responses API → Chat Completions)
- `src/protocol/responses.rs:463` — `response_from_chat` (Chat Completions → Responses API)
- `src/protocol/responses.rs:715` — `wrap_error_as_responses`
- `src/protocol/responses.rs:750` — `map_upstream_error_to_responses`
- `src/protocol/responses_stream.rs:4` — `SseEvent` (streaming data model)
- `src/protocol/responses_stream.rs:24` — `ResponsesStreamState` (streaming state machine)
- `src/protocol/responses_stream.rs:240` — `translate_chat_chunk_to_responses_events`
- `src/protocol/responses_stream.rs:576` — `finalize_stream`

## Architecture Insights

1. **The protocol files are a hybrid of convention violations.** `request.rs` and `response.rs` violate H2 (no data models, pure functions) but follow the lessons.md noun convention. `responses.rs` and `responses_stream.rs` actually contain data models (`ResponsesRequestExtras`, `SseEvent`) alongside functions, making them partial conformists. `stream.rs` is the worst offender — generic name + Anthropic-specific content.

2. **Rename blast radius is grep-trivial.** All 58 external call sites use fully-qualified `crate::protocol::submodule::function()` paths with no `use` imports (single exception: `responses_streaming.rs:10`). A file rename that changes the submodule name (e.g., `responses` → `responses_api`) requires only the module declaration in `mod.rs` + the fully-qualified path updates in callers — a straightforward sed/replace operation.

3. **Rust's module system provides no friction for renames.** Files follow the convention `mod foo;` ↔ `foo.rs`, so renaming `responses.rs` to `responses_api.rs` only requires updating `mod.rs` and updating `crate::protocol::responses::` → `crate::protocol::responses_api::` in callers. No trait re-exports, no `pub use` aliases in `mod.rs` exist today.

4. **The `responses` submodule is the highest-value rename target.** At 26 external call sites in a single file (`responses_handler.rs`), renaming `responses` → `responses_api` would affect the most code but also resolve the worst naming collision (the `response`/`responses` homograph). The `_api` suffix would align with the H4 finding that these are independent API subsystems.

5. **Streaming files create a dependency chain.** `responses_streaming.rs` depends on both `protocol::stream` (for Anthropic SSE → Chat SSE) and `protocol::responses_stream` (for Chat SSE → Responses SSE), forming a layered translation pipeline: Anthropic SSE → Chat SSE → Responses SSE.

## Historical Context (from prior changes)

- `context/archive/2026-06-22-translate-openai-to-anthropic/plan.md:42` — Original design created a single `src/protocol_translation.rs` file. The name explicitly communicated the module's purpose as a translation layer.
- `context/archive/2026-06-28-code-structure-reorg/plan.md:101-113` — Phase 2 of the code structure reorg split `protocol_translation.rs` (3,165 lines) into `protocol/{mod.rs, request.rs, response.rs, stream.rs}`. The names inherited from the monolithic file's internal section headings (§1 Request Translation, §2 Response Translation, §5 Streaming Translation) rather than being thoughtfully chosen for discoverability as separate files.
- `context/archive/2026-06-30-codex-responses-api/plan.md:159` — Added `responses.rs` and `responses_stream.rs` after the split, compounding the naming problem. These files were added to the already-misnamed directory without revisiting the naming convention.
- `context/foundation/lessons.md:63-66` — "Organize src/ into domain subdirectories" rule establishes noun-based naming. The protocol files follow this letter but break its spirit: they're named after protocol artifacts (nouns like "request") but contain no artifact definitions — only translation machinery.
- The archive shows the directory has grown from 3 files (`request`, `response`, `stream`) to 5 files with the addition of `responses` and `responses_stream`. The naming was never revisited after the initial split, and the new files inherited the same misleading convention.

## Related Research

None — this is the first research artifact for this change folder.

## Open Questions

1. **Should `stream.rs` be split into `anthropic_stream.rs` + `openai_stream.rs`?** The file contains bidirectional translation in both directions (A→O at §4, O→A at §8). Splitting by direction would create more files but make the direction explicit in the file name. Counter-argument: the streaming state machines are tightly coupled (shared `parse_sse_events`), and splitting would fragment closely related streaming logic.

2. **Should function names be prefixed with direction?** The frame (H1) identifies that `translate_request` (O→A) and `translate_response` (A→O) have opposite implicit directions. Would `openai_to_anthropic_request` / `anthropic_to_openai_response` better self-document? However, several functions already use this pattern (`anthropic_to_openai_request`, `openai_to_anthropic_response`), suggesting the codebase is already converging toward explicit direction naming.

3. **Is `parse_sse_events` truly generic or Anthropic-tied?** It parses standard SSE format (`event: <type>\ndata: <json>\n\n`) which is protocol-agnostic. But every caller is in an Anthropic SSE translation context. Moving it to a shared utility could be premature abstraction, but keeping it in an Anthropic-named file would be misleading.
