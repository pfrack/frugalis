# Proxy Translation Contract Tests — Plan Brief

> Full plan: `context/changes/testing-proxy-translation-contracts/plan.md`
> Frame brief: `context/changes/testing-proxy-translation-contracts/frame.md`
> Research: `context/changes/testing-proxy-translation-contracts/research.md`

## What & Why

The handler-to-protocol integration layer — where `completion_handler`, `messages_handler`, and `responses_handler` compose protocol translation with provider-type dispatch — has no integration tests asserting that translated bodies, headers, and SSE events have the correct structural shape. The protocol unit layer is well-tested (118 tests), but the handler composition layer only verifies "the pipeline didn't crash." This plan adds structural-invariant assertions covering all 5 provider types, all 3 translation directions, and the full `/v1/responses` Codex endpoint — including the 9 pending tests from the `codex-responses-api` review F4.

## Starting Point

`test_app_with_anthropic_http_client` (`src/app/test_helpers.rs:187`) already wires httpmock with Anthropic provider type. 13 existing Anthropic integration tests assert HTTP status + substrings but not body shape. 4 existing responses_handler tests (R1 + R2 non-streaming/streaming) are in the same state. Harnesses for `nvidia_nim` and `ollama` don't exist yet. `handle_responses_anthropic_streaming_response` (two-stage pipeline) is untested at every level. The `codex-responses-api` change has 5 pending F4 tests and 4 existing tests that need structural assertions.

## Desired End State

Every handler-level translation path has at least one integration test asserting structural invariants: field presence, field types, field absence (protocol-specific fields don't leak), mapping correctness (stop_reason ↔ finish_reason, tool_use ↔ tool_calls), and auth header correctness per provider type. All 5 provider types covered with at least one round-trip test. The Responses two-stage streaming pipeline has a dedicated structural-assertions test.

## Key Decisions Made

| Decision | Choice | Why | Source |
|---|---|---|---|
| Test layer | Handler integration, not protocol unit | 118 protocol unit tests exist; gap is at handler composition | Frame |
| Provider scope | All 5 types | User explicitly chose full coverage | Plan |
| Assertion precision | Structural invariants | Catches body-structure regressions without byte-for-byte brittleness | Plan |
| Responses two-stage | Dedicated test sub-phase | Two-stage composition IS the risk; testing stages separately misses stage-boundary bugs | Frame + Plan |
| Assertion style | Field presence/type/absence + mapping correctness | No hardcoded model IDs or ID values; survives upstream model changes | Plan |

## Scope

**In scope:**
- 2 new provider-type harnesses (`nvidia_nim`, `ollama`)
- Anthropic body-contract tests: buffered + streaming for completion_handler, buffered + streaming for messages_handler
- openai_compatible buffered response shape test
- nvidia_nim passthrough + sanitization test (completion_handler + messages_handler)
- ollama passthrough + no-auth-header test (completion_handler + messages_handler)
- Full `/v1/responses` coverage: structural assertions on 4 existing tests, 5 missing F4 tests (R5 passthrough, auth gate, error envelope, cache hit, header forwarding), 2 new tests (two-stage streaming, tool_use mapping)

**Out of scope:**
- Protocol unit tests (already exist)
- Byte-for-byte reference fixtures
- Cache + Anthropic interaction
- Classifier chain routing (test plan Phase 2)
- Persistence + snippet guardrails (test plan Phase 3)
- CI wiring + cookbook (test plan Phase 4)

## Architecture / Approach

Add two new harness functions in `src/app/test_helpers.rs` (following the `test_app_with_anthropic_http_client` template). Add tests inline in `src/proxy/handlers.rs`, `src/proxy/streaming.rs`, and `src/proxy/responses_handler.rs` per AGENTS.md convention. All tests use `test_app()` + `httpmock` + `oneshot` pattern. Structural assertions parse response bodies as JSON via `serde_json::from_str` and check field presence/types/absence rather than exact values.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Provider-type harnesses | `test_app_with_nim_http_client`, `test_app_with_ollama_http_client` | Harnesses don't match existing pattern (field layout mismatch) |
| 2. Anthropic + openai_compatible body-contract | 7 tests: buffered response shape, streaming full sequence, tool_use mapping, request body capture, response shape for both handlers | Structural assertions break on benign upstream model changes (mitigated: no hardcoded IDs) |
| 3. Remaining providers + Full Responses coverage | 16 tests: nvidia_nim/ollama passthrough, structural assertions on 4 existing Responses tests, 5 F4 tests, 2 new Responses streaming/tool_use tests | Two-stage streaming may need slow_tests; cache-hit response_id stability |

**Prerequisites:** None — harness and test code only, no external dependencies.
**Estimated effort:** ~1-2 sessions across 3 phases.

## Open Risks & Assumptions

- httpmock delivers the full SSE body in a single chunk — multi-chunk partial-event delivery is a known gap (flagged in impl-review F1, Jun 22). If the two-stage streaming test requires chunked delivery, it may need `slow_tests` classification.
- `test_app_with_nim_http_client` assertions on stripped fields (`top_k`, `metadata`, `thinking`) require httpmock request-body capture — verify httpmock supports `when.body_does_not_contain()` or capture-then-assert pattern.
- `local` provider type has no endpoint and no auth — no dedicated harness is needed; existing `test_app_with_classifier` (which returns classification-only JSON) covers the no-upstream path.

## Success Criteria (Summary)

- All 5 provider types have at least one integration test exercising their handler path
- Structural assertions catch body-structure regressions (missing fields, wrong types, protocol-field leakage)
- Responses two-stage streaming pipeline has a dedicated test verifying event ordering and type correctness
- Full test suite passes: `cargo test`
