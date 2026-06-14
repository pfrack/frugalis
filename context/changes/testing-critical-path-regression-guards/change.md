---
change_id: testing-critical-path-regression-guards
title: Critical-path regression guards (test rollout phase 1)
status: implementing
created: 2026-06-13
updated: 2026-06-14
archived_at: null
---

## Notes

Open a change folder for rollout Phase 1 of context/foundation/test-plan.md: "Critical-path regression guards".
Risks covered: #1 (classifier chain regex→fewshot→LLM handoff) and #2 (completion_handler regression losing F1–F4 review fixes).
Test types planned: integration (chain escalation with mock backends), regression (invariant assertions on completion_handler).
Risk response intent:
- Risk #1: prove the chain escalates from regex to fewshot to LLM when regex confidence is low, and that the final category drives routing to the right model; challenge the assumption that "each backend works in isolation" implies the chain hands off correctly; do not assert "some category came back" without checking which tier fired.
- Risk #2: prove the F1–F4 review fixes (snippet extraction, streaming error path, keepalive, JSON contract) survive any future rewrite of completion_handler; challenge the assumption that 46 tests on main.rs anchor all four invariants; do not rely on a one-time "test passed" snapshot as ongoing protection.
Hot-spot scope (likelihood evidence, not anchors): src/intent_classifier.rs (12 commits/30d), src/main.rs (47 commits/30d).
Stack: Rust/Axum, dev-deps include httpmock 0.7 / serial_test 3 / testcontainers 0.27; existing mod tests + mod slow_tests layout per AGENTS.md.
After creating the folder, follow the downstream continuation rule.

## Progress (per-phase commit SHAs)

- **Phase 1** (chain handoff contract, 3 stub-based scenarios + 3-backend integration test): 35906ce (Tests #12 — squashed merge of 9c2626d, fd971f7, a6f1eca, cf9076a, 4445c9d per the plan's speculative SHAs; the actual per-phase commits existed only on the squashed feature branch and were dropped on merge).
- **Phase 2** (snippet path coverage, harness refactor + 3 HTTP-level F1 tests): 35906ce (Tests #12).
- **Phase 3** (SSE error path invariants, format_sse_error_event helper + 5 F2 invariants): 35906ce (Tests #12).
- **Phase 4** (keepalive coverage, 3 new slow tests + tightened existing): 35906ce (Tests #12).
- **Phase 5** (JSON contract parsing, parse_json_body helper + 7 refactored + 4 new shape tests): 35906ce (Tests #12).
- **Phase 6** (cookbook + verification, 5 pre-existing clippy fixes for the gate): see epilogue commit (this change's `implemented` SHA is the epilogue's closing commit).

Gates verified on 2026-06-14: `cargo build --release` ✓, `cargo test` 215 passed ✓, `cargo test slow_tests -- --test-threads=1` 5 passed ✓, `cargo clippy --all-targets -- -D warnings` ✓ (5 pre-existing clippy issues fixed in Phase 6: let-and-return at src/intent_classifier.rs:267, manual_unwrap_or_default at src/main.rs:242, manual_clamp at src/main.rs:1210, len_zero at src/fewshot_classifier.rs:539, items_after_test_module at src/persistence.rs — moved test_pool() above mod tests), `cargo fmt --check` ✓.
