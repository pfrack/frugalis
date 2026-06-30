---
change_id: testing-proxy-translation-contracts
title: Proxy translation contract tests
status: implementing
created: 2026-06-30
updated: 2026-06-30
last_updated_note: "Implementing Phase 1: provider-type test harnesses"
archived_at: null
---

## Notes

Open a change folder for rollout Phase 1 of context/foundation/test-plan.md: "Proxy translation contract tests".
Risks covered: #1 (protocol translation corruption), #4 (streaming emitter edge cases). Test types planned: integration (translation contract), streaming edge-case.
Risk response intent:
- #1: Prove the translated body, headers, and SSE events match known-good reference output for each translation direction (OpenAI→Anthropic, Anthropic→OpenAI, Responses→Chat). Challenge: "returns 200" ≠ "translation correct" — headers and cache_control can silently drop.
- #4: Prove streaming emitters handle malformed SSE (empty deltas, broken tool_use JSON, mid-stream errors) with clean error termination, not garbled output or hung connections. Challenge: "stream completes" ≠ "stream was correct" — must assert on event sequence.
After creating the folder, follow the downstream continuation rule.
