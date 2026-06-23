<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Anthropic Pass-Through Proxy

- **Plan**: context/changes/anthropic-passthrough/plan.md
- **Scope**: All 4 phases (full plan)
- **Date**: 2026-06-23
- **Verdict**: APPROVED
- **Findings**: 0 critical  1 warning  2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Drift Check

All 10 planned items match implementation. Unplanned changes: `manual-test/lib.sh`, `manual-test/run.sh` (test infrastructure only). Plan's `build_upstream_request_anthropic` was skipped — reuses `build_upstream_request` since `auth_headers_for` handles anthropic headers. Cleaner, same contract.

## Success Criteria

| Phase | Check | Result |
|-------|-------|--------|
| 1 | `cargo test` | ✅ 255 passed |
| 1 | `cargo test extract_last_user_message_anthropic` | ✅ 7 tests |
| 1 | `cargo test auth_headers_for` | ✅ 2 tests |
| 1 | `cargo clippy -- -D warnings` | ✅ clean |
| 2 | `cargo build` | ✅ |
| 2 | `cargo clippy -- -D warnings` | ✅ clean |
| 2 | `cargo test` | ✅ 255 passed |
| 3 | `cargo test messages_handler` | ✅ 6 tests |
| 3 | `cargo test` (full suite) | ✅ 255 passed |
| 4 | YAML valid | ✅ |
| 4 | `cargo test` | ✅ 255 passed |

## Findings

### F1 — Dead code: truncate_snippet with #[allow(dead_code)]

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/persistence.rs:1188
- **Detail**: `truncate_snippet` was annotated with `#[allow(dead_code)]` and had zero production callers. Violated lessons.md rule: "Delete dead code rather than suppressing warnings."
- **Fix**: Deleted `truncate_snippet` entirely. One-liner trivially recreatable if needed.
- **Decision**: FIXED

### F2 — Upstream status code not logged in buffered error path

- **Severity**: 👁 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:1609-1616
- **Detail**: When the Anthropic handler got a non-2xx upstream response in buffered mode, `log_classification` recorded status "upstream_error" but did not include the upstream HTTP status code.
- **Fix**: Added `warn!(upstream_status = status.as_u16(), "upstream returned non-2xx");` to the error path.
- **Decision**: FIXED

### F3 — Dead code: classify_messages with fragile model-name heuristics

- **Severity**: 👁 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Reliability
- **Location**: manual-test/lib.sh:164-219
- **Detail**: `classify_messages` was defined but never called anywhere. Used fragile substring matching on model names as fallback. Dead code per lessons.md rule.
- **Fix**: Deleted `classify_messages` entirely.
- **Decision**: FIXED
