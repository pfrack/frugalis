---
change_id: testing-critical-path-regression-guards
title: Critical-path regression guards (test rollout phase 1)
status: implementing
created: 2026-06-13
updated: 2026-06-13
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
