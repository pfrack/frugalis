<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Competitive Gap — Model Routing

- **Plan**: context/changes/competitive-gap-model-routing/plan.md
- **Scope**: All 4 phases (full plan)
- **Date**: 2026-06-26
- **Verdict**: APPROVED
- **Findings**: 0 critical · 3 warnings · 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | WARNING |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Findings

### F1 — Scope creep in try_optimize_request

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: src/main.rs:772,823
- **Detail**: Plan specified `fn try_optimize_request(body: &[u8])`. Implementation adds `is_anthropic: bool` param, extra probe `"hey"`, "Hello!" greeting (plan said "Hi!"), streaming guard, and routing-header bypass. All defensible but unplanned.
- **Fix**: Update plan.md Phase 4 to document the actual contract — `fn try_optimize_request(body: &[u8], is_anthropic: bool)` with patterns ["hello","hi","test","hey"] and streaming guard.
- **Decision**: FIXED — plan.md updated to match implementation

### F2 — Double JSON parse on every proxied request

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:773
- **Detail**: `try_optimize_request` called `serde_json::from_slice(body)` on every request before the body-size guard. Large bodies were fully parsed then discarded.
- **Fix**: Added `if body.len() >= 512 { return None; }` as the first line of `try_optimize_request`, before the serde_json parse. Removed redundant `body.len() < 512` guard from pattern matching.
- **Decision**: FIXED — early return added, redundant guard removed

### F3 — models_handler bypasses json_response() helper

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/main.rs:903-909
- **Detail**: `models_handler` manually constructed a `Response` with explicit status and Content-Type header, while every other handler uses the shared `json_response()` helper.
- **Fix**: Replaced manual Response construction with `json_response(StatusCode::OK, body.to_string())`.
- **Decision**: FIXED — now uses shared helper

### F4 — NIM sanitization double-parse

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:2329-2339,2660-2670
- **Detail**: NIM call sites parse body into Value, sanitize, re-serialize. `build_upstream_request` likely parses again. Two parse+serialize cycles per NIM request.
- **Fix**: Defer to future refactor — not blocking.
- **Decision**: SKIPPED

### F5 — count_tokens_handler silent on malformed input

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:760
- **Detail**: On serde_json parse failure, returns `{"input_tokens": 0}` silently. Other handlers return explicit error codes on bad input.
- **Fix**: Defer — best-effort heuristic makes this acceptable.
- **Decision**: SKIPPED
