<!-- PLAN-REVIEW-REPORT -->
# Plan Review: Intent Classification Implementation

- **Plan**: context/changes/proxy-intent-routing/plan.md
- **Mode**: Deep
- **Date**: 2026-06-01
- **Verdict**: SOUND
- **Findings**: 0 critical | 4 warnings | 0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| End-State Alignment | PASS |
| Lean Execution | PASS |
| Architectural Fitness | PASS |
| Blind Spots | PASS |
| Plan Completeness | PASS |

## Grounding
Grounding: 5/5 paths ✓, 3/3 symbols ✓, brief↔plan ✓

## Findings

### F1 — Redundant JSON Parsing and Logic Duplication

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Lean Execution
- **Location**: Phase 1.3 / Phase 2.3
- **Detail**: The plan introduces `extract_prompt_text` in `intent_classificator.rs` while `persistence::extract_snippet` already exists. Both functions parse the same JSON body and search for the last user message. This results in redundant CPU work and duplicate logic for finding the last user message (including the 1000-message DoS guard).
- **Fix**: Move the "extract last user message" logic to a utility function in `persistence.rs`. Refactor `extract_snippet` to use this utility and truncate. Let the classifier also use this utility for full-text extraction.
- **Decision**: FIXED (refactored extract_snippet to use shared utility)

### F2 — Breaking Change without OpenAPI Update

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: End-State Alignment
- **Location**: "What We're NOT Doing"
- **Detail**: The plan modifies the public `/v1/chat/completions` endpoint's response from a static string to a JSON object. This is a breaking change that violates the project's OpenAPI rule in `lessons.md`.
- **Fix**: Update the plan to include a basic OpenAPI specification update for the modified endpoint.
- **Decision**: FIXED (added OpenAPI spec file to Phase 2)

### F3 — Memory/CPU DoS Risk with Full Prompt Extraction

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Blind Spots
- **Location**: Phase 1.3
- **Detail**: `extract_prompt_text` extracts the untruncated last user message. Oversized requests (e.g., 50MB of text) could cause excessive memory allocation and CPU usage during regex matching across 45 patterns.
- **Fix A ⭐ Recommended**: Cap the extraction at 10,000 characters.
  - Strength: Protects memory/CPU while remaining highly effective for intent classification.
  - Tradeoff: Extremely rare edge cases with keywords >10k chars might miss classification.
  - Confidence: HIGH — intent is almost always clear in the first 10k.
  - Blind spot: None significant.
- **Fix B**: Use a streaming JSON parser.
  - Strength: Minimal memory footprint even for massive requests.
  - Tradeoff: Significantly higher implementation complexity.
  - Confidence: LOW — `serde_json` is already established in the project.
- **Decision**: FIXED (cap at 10,000 chars, Fix A)

### F4 — Improper Response Construction Order

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Completeness
- **Location**: Phase 2.3
- **Detail**: The plan instructs to replace the response at line 94 with one built from classification fields, but classification isn't performed until line 185 (inserted after line 99). The response cannot be constructed before the data is available.
- **Fix**: Update Phase 2.3d to construct the response *after* the classification logic block.
- **Decision**: FIXED (response constructed after classification)
