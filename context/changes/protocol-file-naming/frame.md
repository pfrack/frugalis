# Frame Brief: Misleading protocol/ file names

> Framing step before /10x-plan. This document captures what is *actually*
> at issue, separated from what was initially assumed.

## Reported Observation

The names of the Rust files in `src/protocol/` are somehow misleading.
The files are `request.rs`, `response.rs`, `stream.rs`, `responses.rs`,
`responses_stream.rs` — all within a directory originally conceived as a
single `protocol_translation.rs` module (per `context/archive/2026-06-22-translate-anthropic-to-openai/plan.md:40`).

## Initial Framing (preserved)

- **User's stated cause or approach**: Not yet articulated — the user senses the names
  are wrong but hasn't pinned down exactly why.
- **User's proposed direction**: Not yet stated — the user asked for framing analysis
  before deciding what to do.
- **Pre-dispatch narrowing**: Multiple issues felt off; none separated yet. All files
  in the module cause lookup friction.

## Dimension Map

The observation could originate at any of these dimensions:

1. **Direction-ambiguity in `translate_*` function naming** — `request::translate_request`
   goes O→A, but `response::translate_response` goes A→O. The "default" translate
   function direction is inconsistent and implicit across files. Callers must carry
   wrapper-context (comments, variable names) to know which direction a function goes.

2. **File names imply data models, but files contain only translation functions** —
   `request.rs` and `response.rs` export zero structs, zero enums, zero types. Every
   public item is a `pub fn` operating on raw `serde_json::Value`. The project has no
   `Request` or `Response` data model anywhere — yet the file names create that
   expectation. By contrast, `routing/routes.rs` correctly houses actual data models
   (`ProviderEntry`, `RouteEntry`, `ModelCosts`) with a semantically accurate name.

3. **`stream.rs` is named generically but is entirely Anthropic-specific** — every
   `pub` item in `stream.rs` handles Anthropic↔OpenAI SSE protocol translation.
   `parse_sse_events` has zero non-Anthropic callers. Meanwhile `responses_stream.rs`
   properly communicates its domain (OpenAI Responses API streaming).

4. **`response` vs `responses` homograph** — `response.rs` handles Chat
   Completions response translation (both directions). `responses.rs` handles
   the OpenAI Responses API shim — a completely separate API with its own wire
   format. These files share zero types, have non-overlapping callers, and are
   independent subsystems. The singular/plural distinction is invisible in
   many contexts (tab labels, file dialogs, code folding).

5. **The existing noun-prefix convention is appropriate, and function-level
   discoverability is the real problem** — per `context/foundation/lessons.md`
   (Organize src/ into domain subdirectories), files are named after what they
   ARE (nouns). The friction is that callers must remember which function
   handles which direction, not what the files are called.

## Hypothesis Investigation

| Hypothesis | Evidence | Verdict |
| --- | --- | --- |
| H1: Direction-ambiguity — file names don't communicate translation direction | `request::translate_request`=O→A (`request.rs:8`), `response::translate_response`=A→O (`response.rs:10`) — opposite implicit directions. `handlers.rs:433,1244` and `upstream.rs:165,192,227,260` use BOTH directions from the same protocol module. Every implicit call site requires wrapper-function or variable-name disambiguation. | **STRONG** |
| H2: File names promise data models but contain only functions | `request.rs` and `response.rs` export **zero** data types — 100% translation functions on `serde_json::Value`. No `Request` or `Response` struct exists anywhere in the project. Contrast: `routing/routes.rs:19-121` correctly contains `ProviderEntry`, `RouteEntry`, `ModelCosts` with an accurate name. | **STRONG** |
| H3: `stream.rs` too generic — doesn't communicate Anthropic domain | All 8 `pub` items in `stream.rs:8-650` are Anthropic-specific. `parse_sse_events` (`stream.rs:68`) has zero non-Anthropic callers. `responses_stream.rs` communicates domain; `stream.rs` does not. | **STRONG** |
| H4: `responses` vs `responses_stream` naming hides their independence | Files share zero types, have non-overlapping callers (`responses_handler.rs` uses `responses` 26 times; `responses_streaming.rs` uses `responses_stream` 7 times). Only signal differentiating them: the `_stream` suffix. No parent module or shared prelude. | **MODERATE** |
| H5: Noun convention is fine; function-level discoverability is the problem | Callers import from 1-2 submodules each (max 6 symbols per file). Callers already distinguish by module path. But `mod.rs:1-5` has zero documentation explaining the taxonomy. File renaming alone won't fix discoverability of which function goes which direction. | **PARTIAL** |

## Narrowing Signals

Evidence from Step 3 was conclusive enough to skip the narrowing question round:

- **H1, H2, H3 all have STRONG evidence** — the naming is wrong on multiple
  axes simultaneously: direction, content-type expectations, and domain specificity.
- **Independent cross-system check** arrived at the same findings without
  prompting: identified the `response`/`responses` homograph as the single worst
  naming issue, the bidirectional-within-file organization as a discoverability
  barrier, and the absent `mod.rs` documentation as a compounding factor.
- **Original design (archive)** called for a single `protocol_translation.rs`
  — the current file naming was inherited from a fast structural split and
  never revisited, especially after `responses.rs`/`responses_stream.rs` were
  added.

## Cross-System Convention

The project's `lessons.md` rule "Organize src/ into domain subdirectories, not
flat" (`context/foundation/lessons.md:63-66`) establishes that files are named
after domain concepts (nouns). The protocol files broadly follow this (`request`,
`response`, `stream`), but the convention breaks down because:

1. The files are translation engines, not domain models — a verb-ish suffix
   (`request_translation.rs`) would better fit the "named after what they are" rule
   since what they ARE is "request translation code," not "a request."
2. The `responses`/`response` collision violates discoverability expectations
   that a co-located maintainer would have when applying the lessons.md rule.

The archive shows this was originally going to be one file: `protocol_translation.rs`.
That name communicated the module's purpose. The current names lost that signal.

## Reframed (or Confirmed) Problem Statement

> **The actual problem to plan around is**: `src/protocol/` file names don't
> communicate that the module is a *protocol translation layer* — they read like
> data model definitions with a hidden bidirectional translation engine inside
> each file, and the `response`/`responses` homograph creates active confusion
> between two entirely unrelated API subsystems.

Three compounding factors create the naming problem: (1) files named after
protocol artifacts (`request`, `response`) promise data models but deliver only
translation functions on opaque JSON; (2) the implicit translate-function
direction is inconsistent across files (O→A in `request`, A→O in `response`);
(3) the `response`/`responses` singular/plural distinction silently separates
two independent API subsystems with zero shared code. Addressing these would
replace the current "guess what's in each file" experience with a self-documenting
directory listing. The cheapest high-leverage fix is adding `//!` module
documentation to `mod.rs` explaining the taxonomy — this alone would resolve
much of the confusion without any renames.

## Confidence

- **HIGH** — strong multi-dimensional evidence confirmed by an independent
  cross-system investigation, anchored in the project's own archive showing
  the original design intent was a single self-documenting file name
  (`protocol_translation.rs`).

## What Changes for /10x-plan

The plan should evaluate the cost/benefit of: (1) adding `//!` module docs to
`mod.rs` explaining what lives where (minimum viable fix), (2) renaming files
to communicate their translation purpose (e.g., `request_translation.rs`,
`stream_translation.rs`), and (3) possibly renaming `responses.rs` to
`responses_api.rs` to eliminate the `response`/`responses` homograph. Any
rename plan must account for all `crate::protocol::*` call sites in `src/proxy/`.

## References

- Source files: `src/protocol/mod.rs:1-5`, `src/protocol/request.rs:8,365`,
  `src/protocol/response.rs:10,160,209,330`, `src/protocol/stream.rs:8,68,124,385,429`,
  `src/protocol/responses.rs:4,23,185,463,715,750`, `src/protocol/responses_stream.rs:4,24,240,576`
- Related research: `context/archive/2026-06-22-translate-anthropic-to-openai/plan.md:40`
- Related lessons: `context/foundation/lessons.md:63-66` (Organize src/ into domain subdirectories)
- Investigation tasks: `ses_0e54d2ac3ffeauHWmGY70pC8wz`, `ses_0e54c20fdffeQJqrWdmOdn3ema`,
  `ses_0e54c13beffeyMiJ72lR7qD6oz`, `ses_0e54c02aaffeUXkLN2g0kPsqd2`,
  `ses_0e54bf081ffeOutRuAExC9bPZH`, `ses_0e54a9f63ffePsPLgvyr6Uk4aA`
