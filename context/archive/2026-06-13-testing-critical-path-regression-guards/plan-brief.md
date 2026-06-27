# Critical-path regression guards — Plan Brief

> Full plan: `context/changes/testing-critical-path-regression-guards/plan.md`
> Research: `context/changes/testing-critical-path-regression-guards/research.md`

## What & Why

Lock Risk #1 (classifier chain `regex → fewshot → LLM` handoff) and Risk #2
(`completion_handler` F1–F4 invariants) of the test rollout by adding the
tests that research proved are missing, aligning one divergent code path
that the tests will expose, and refactoring one test harness so the snippet
path runs in default CI. The change is risk-response-anchored: each phase
maps to a specific test-plan §2 risk row, and the success criteria are
phrased as "a regression in <that risk> is now caught by a test".

## Starting Point

`pfrack/cerebrum` (Rust/Axum, commit `1cc87bfe`, main) has 188 tests
across 6 of 9 modules; `dashboard.rs`, `routing.rs`, `telemetry.rs` have
zero tests. Of 46 tests on `src/main.rs`, only ~10 anchor F1–F4 directly.
The classifier chain has 1 multi-backend test with 2 backends (no LLM);
5 stub-based chain tests prove "first-non-Fallback wins" but not "later
backends not called when earlier matches". The snippet path is unreachable
in default CI because `build_app_with_persistence` requires
`Arc<sqlx::PgPool>` — snippet-path tests skip without `DATABASE_URL`.

## Desired End State

After this plan lands, a future contributor who touches `ClassifierChain`
or `completion_handler` cannot regress the chain-handoff contract, the
F1 snippet path, the F2 SSE error event format, the F3 keepalive, or the
F4 JSON contract shape without a test failure. The snippet path is
exercised in default CI (no `DATABASE_URL` required) via a refactored
harness. The two F2 error paths share a single `format_sse_error_event`
helper with unit tests on the helper and integration tests on the
function. `test-plan.md` §6 (Cookbook) is fully populated with the
patterns this rollout produced, so subsequent rollout phases can build
on them.

## Key Decisions Made

| Decision                                          | Choice                                                            | Why (1 sentence)                                                                                                | Source     |
|---------------------------------------------------|-------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|------------|
| F2 inline mid-stream error branch (`src/main.rs:712-720`) | Align the two paths (extract shared `format_sse_error_event`)     | Single source of truth for the SSE error contract; removes divergence; reduces future regression surface.         | Plan Q1    |
| F1 snippet path harness                           | Refactor `build_app_with_persistence` to accept `DbBackend` (in-memory variant runs in default CI) | "Cheapest layer that gives a real signal" — snippet regression breaks the build, not a scheduled run.            | Plan Q2    |
| Chain-escalation test mechanism                   | New `CountingClassifier` (test-only `IntentClassify` impl with `Arc<AtomicUsize>`) | Required because `LLMClassifier` returns `tier: Regex` — tier inspection cannot distinguish regex-tier from LLM-tier. | Plan Q3    |
| Cold-start → LLM escalation test                  | Synthetic fewshot (CountingClassifier returning Fallback) + assert LLM called once | Mirrors the production data path; deterministic; doesn't require real bootstrap YAML.                           | Plan Q4    |
| `test-plan.md` §6 cookbook scope                  | Fill all 5 subsections now (this change)                          | Future contributors see all 5 patterns at once; each subsection is grounded in a real test this change added.   | Plan Q5    |
| Phase structure                                    | 6 independent phases (Phase 1-5 each cover a distinct risk surface; Phase 6 ships last) | Phases 1-5 are independent, can be implemented in any order; Phase 6 is the closing ritual.                     | Plan §3    |
| Llm tier variant on `ClassificationTier`          | Out of scope (deferred)                                           | Public-type change requires its own change; side-effect observation via CountingClassifier is sufficient.        | Research   |
| Self-describing comments for F2 invariants        | Add at function-level + inline per `lessons.md:26-31` (no `// F1` markers) | Lessons rule: document WHAT + WHY, not review cross-references.                                                 | Research   |
| `FewShotClassifier` 0.6 cold-start threshold test | Out of scope (deferred)                                            | Risk #1's chain-handoff contract is what we lock; threshold mechanics is `FewShotClassifier` internal.           | Research   |

## Scope

**In scope:**
- 3-backend chain integration test (Risk #1)
- 3-backend chain stub-based unit tests (Risk #1)
- New `CountingClassifier` test backend (test-only `IntentClassify` impl)
- HTTP-level snippet-path tests with in-memory backend (F1)
- Refactored `build_app_with_persistence` to accept `DbBackend`
- `format_sse_error_event` helper extraction + self-describing comments
- Inline mid-stream error branch aligned to the helper
- 4+ new F2 integration tests (5 invariants of `handle_streaming_error`)
- 6+ unit tests on the F2 helper
- 3 new keepalive slow tests + 1 tightened existing test (F3)
- 8+ existing F4 tests refactored to JSON parsing
- 4+ new F4 shape tests
- `test-plan.md` §6.1–§6.5 cookbook update

**Out of scope:**
- Llm tier variant on `ClassificationTier` (deferred)
- PII redaction in `extract_snippet` (Risk #6, Phase 2 of test rollout)
- Persistence cross-backend drift tests (Risk #5, Phase 2)
- Async logging failure observability (Risk #3, Phase 2)
- Dashboard rendering tests (Risk #4, Phase 3)
- Constant-time compare grep-based guard (Risk #7, Phase 3)
- CI floor + coverage threshold (test-plan Phase 4)
- `FewShotClassifier` 0.6 cold-start threshold test (deferred)

## Architecture / Approach

**Phase 1 (Risk #1)**: Add a test-only `CountingClassifier` impl in
`src/intent_classifier.rs` mod tests; use it in 3 new stub-based
chain scenarios (regex short-circuits, middle-matches, all-Fallback
→ LLM) and 1 new 3-backend integration test that wires
`[Regex, CountingClassifier(fewshot-stub), LLMClassifier(httpmock)]`
to prove the cold-start escalation fires the LLM tier exactly once.
No production code changes.

**Phase 2 (F1)**: Refactor `build_app_with_persistence`
(`src/main.rs:2206`) to accept `Arc<DbBackend>` so `Memory` can be
injected. The 2 existing `persistence_integration_*` tests (lines
1854, 1923) keep their Postgres + `DATABASE_URL` skip behavior. New
in-memory variant enables 3 new HTTP-level tests: snippet ≤ 200
chars, snippet does not contain full prompt body, log_classification
failure does not block response.

**Phase 3 (F2)**: Extract `format_sse_error_event(error_msg: &str)
-> String` helper (placement: above `handle_streaming_error` at
`src/main.rs:749`, `pub(crate)` or `pub(super)`). The inline
mid-stream error branch in `handle_streaming_response` (line 712)
applies the same escape rule and calls the same helper. Self-
describing comments at the 5 F2 invariants (no `// F1` markers per
`lessons.md:26-31`). Unit tests on the helper cover 2 invariants;
integration tests on the function cover the other 3.

**Phase 4 (F3)**: Add 3 new `mod slow_tests` (real delays, `#[serial]`):
"upstream completes before keepalive" (no keepalive in body),
"upstream chunk during keepalive tick" (chunk + keepalive
interleaved), "multiple consecutive keepalives" (long stall).
Tighten existing assertion from `body.contains(": keepalive\n\n")`
to `body.contains("\n: keepalive\n\n")` (anchored to line start —
distinguishes SSE comment from `data: keepalive`).

**Phase 5 (F4)**: Refactor 8+ existing substring-match assertions
in `src/main.rs` to use `serde_json::from_str::<Value>` and assert
on key count, key types, exact shape. Add 4+ dedicated shape tests
for `classification_only_json` (4 keys), `upstream_error_json`
(3 keys + `status` as integer), `json_response` Content-Type, and
`tier` field across `Regex | FewShot | Fallback` (3 real variants;
`LLM` is NOT a variant and is NOT a valid `tier` value).

**Phase 6**: Update `test-plan.md` §6.1–§6.5 with reference tests
from this rollout. Update `change.md` to final status. Run the full
gate suite (`cargo build --release`, `cargo test`, `cargo test
slow_tests -- --test-threads=1`, `cargo clippy --all-targets -- -D
warnings`, `cargo fmt --check`). Bump `test-plan.md` header to
"Phase 1 → implementation complete".

## Phases at a Glance

| Phase | What it delivers                                                          | Key risk                                                                  |
|-------|---------------------------------------------------------------------------|---------------------------------------------------------------------------|
| 1     | Risk #1: 3-backend chain contract locked via CountingClassifier + 4 tests | Mis-alignment between test backend's counter and the actual call site    |
| 2     | Risk #2 F1: snippet path runs in default CI (3 new tests + harness refactor) | `build_app_with_persistence` signature change ripples to 2 existing tests |
| 3     | Risk #2 F2: 5 invariants of `handle_streaming_error` locked + helper extracted (production refactor) | The only phase with non-trivial production code; refactor + tests must land together |
| 4     | Risk #2 F3: 3 keepalive timing edges + tightened assertion                 | Slow test flakiness if timing edges aren't engineered deterministically  |
| 5     | Risk #2 F4: 8+ refactored JSON-parsing tests + 4+ dedicated shape tests   | "LLM" tier gotcha — `LLMClassifier` returns `tier: Regex` (not a distinct Llm variant) |
| 6     | Documentation + cookbook + verification                                   | Stale references in §6.1-§6.5 if the cookbook update is sloppy            |

**Prerequisites:** Research is complete (status: `preparing` →
`planned` after this plan lands). `Cargo.toml` dev-deps are
sufficient (`httpmock 0.7`, `serial_test 3`). No new dev-deps needed.
**Estimated effort:** 1-2 sessions across 6 phases, ~15-20 new tests,
~1-2 production code changes (Phase 3 only).

## Open Risks & Assumptions

- **`format_sse_error_event` escape rule** — the helper applies the
  same rule as `handle_streaming_error` (replace `\\`, `"`, `\n`, `\r`).
  The inline branch in `handle_streaming_response` (line 712)
  currently uses `serde_json::json!` which produces valid JSON via a
  different escape mechanism. The alignment in Phase 3 is a code-
  organization change with no observable behavior difference for
  well-formed inputs; the test in Phase 3 locks the new behavior.
- **`build_app_with_persistence` ripple** — refactoring the signature
  affects `build_app` (line 2200) and 2 existing
  `persistence_integration_*` tests. The refactor must preserve the
  existing `DATABASE_URL` skip behavior; the in-memory variant is
  new, not a default. The implementer must verify the 2 existing
  tests still skip cleanly after the refactor.
- **The `Llm` tier is a real constraint.** `LLMClassifier` returns
  `tier: ClassificationTier::Regex` on success (`src/intent_classifier.rs:344`),
  not a distinct `Llm` variant. Any test asserting "LLM was called"
  must use `CountingClassifier` side-effect observation. Tier
  inspection cannot distinguish regex-tier from LLM-tier. This is
  not a regression — it is the current code's design — but it
  shapes the test architecture.
- **Slow test timing edges** (Phase 4) depend on real delays and
  TCP listeners. The 3 new tests must engineer deterministic timing
  edges (fast upstream, chunk-then-idle, long stall) using the
  existing `spawn_slow_sse_server` helper. Flaky tests here would
  erode trust in the keepalive coverage.

## Success Criteria (Summary)

- A regression in the chain handoff (e.g., "stop at the first
  backend regardless of result" or "call all backends even after a
  match") is caught by at least one of the new Phase 1 tests.
- A regression in F1 (snippet > 200 chars, snippet contains full
  prompt, log_classification failure blocks response) is caught by
  at least one of the new Phase 2 tests in default CI.
- A regression in F2 (any of the 5 `handle_streaming_error`
  invariants) is caught by at least one of the new Phase 3 tests.
- A regression in F3 (keepalive format or timing) is caught by at
  least one of the new Phase 4 slow tests.
- A regression in F4 (JSON field added, removed, renamed, or
  type-changed) is caught by at least one of the new/refactored
  Phase 5 tests.
- `test-plan.md` §6.1–§6.5 each have a "Reference test" line pointing
  to a real test from this rollout.
