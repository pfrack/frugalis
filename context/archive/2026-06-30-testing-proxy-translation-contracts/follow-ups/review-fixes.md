# Review Follow-Ups — Proxy Translation Contract Tests

Tracked follow-ups from `/10x-impl-review` on commit d3a347e. Date: 2026-06-30.

## Pending — Phase 3 commit

Phase 3 source code (12 tests in `src/proxy/responses_handler.rs`, 4 tests in `src/proxy/handlers.rs` for nvidia_nim/ollama) exists as uncommitted working-tree changes. The plan's Progress section items 3.1–3.16 are `[ ]` after the F1 fix (`894681a`). Phase 3 should land in a separate commit, then items 3.1–3.16 should be checked off with that commit's SHA appended per repo convention.

### Phase 3 sub-items to land (per plan §"Phase 3: Remaining Provider Types + Full Responses Coverage")

| Item | Test | File |
|------|------|------|
| 3.1 | R1 non-streaming structural assertions | `src/proxy/responses_handler.rs` (around line 797) |
| 3.2 | R1 streaming structural assertions | `src/proxy/responses_handler.rs` (around line 853) |
| 3.3 | R2 non-streaming structural assertions | `src/proxy/responses_handler.rs` (around line 903) |
| 3.4 | R2 streaming structural assertions | `src/proxy/responses_handler.rs` (existing test extended) |
| 3.5 | `test_completion_handler_nvidia_nim_passthrough` | `src/proxy/handlers.rs` (around line 3443) |
| 3.6 | `test_completion_handler_ollama_passthrough` | `src/proxy/handlers.rs` (uncommitted) |
| 3.7 | `test_messages_handler_nvidia_nim_translation` | `src/proxy/handlers.rs` (uncommitted) |
| 3.8 | `test_messages_handler_ollama_translation` | `src/proxy/handlers.rs` (uncommitted) |
| 3.9–3.15 | Responses F4 tests + two-stage streaming + tool_use | `src/proxy/responses_handler.rs` (uncommitted) |
| 3.16 | Full test suite passes: `cargo test` | n/a — verification command |

### Commit expectation

- Single Phase 3 commit with message prefix `test(testing-proxy-translation-contracts):` (matches `d3a347e` convention).
- After commit lands, edit plan.md Progress section 3.1–3.16 to `- [x] ... — <new-sha>`.
- Verify with `cargo test --no-fail-fast` → 439 + Phase 3 tests passing.
- Re-run `/10x-impl-review` on the Phase 3 commit before considering the change fully shipped.
