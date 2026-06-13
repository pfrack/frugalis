# Critical-path regression guards — Implementation Plan

## Overview

Lock Risk #1 (classifier chain `regex → fewshot → LLM` handoff) and Risk #2
(`completion_handler` F1–F4 invariants) by adding the tests that research
proved are missing, aligning one divergent code path that the tests will
expose, and refactoring one test harness so the snippet path runs in default
CI. The change ships six independent phases that touch two production files
(`src/main.rs`, `src/intent_classifier.rs`) and one test surface
(`mod tests` + `mod slow_tests`).

## Current State Analysis

Research (`context/changes/testing-critical-path-regression-guards/research.md`,
613 lines, complete) settled the baseline. Headlines:

- **Risk #1 is half-anchored.** The 3-backend chain has 1 multi-backend test
  (`test_chain_with_regex_and_fewshot` at `src/main.rs:1333`) with 2 backends
  (no LLM), plus 5 stub-based chain tests at `src/intent_classifier.rs:854-998`
  that prove "first-non-Fallback wins" but not "later backends not called
  when earlier matches". A real architectural constraint: `LLMClassifier`
  returns `tier: ClassificationTier::Regex` on success
  (`src/intent_classifier.rs:344`), not a distinct `Llm` variant, so the
  chain's `tier != Fallback` check cannot distinguish regex-tier from
  LLM-tier via tier alone — it requires side-effect observation.
- **Risk #2 is largely un-anchored.** Of 46 tests on `src/main.rs`, only ~10
  anchor F1–F4 directly. F1 (snippet) is unreachable in default CI because
  `test_app` and `test_app_with_classifier` set `persistence: None` (lines
  1222, 1268); the only snippet-path tests skip without `DATABASE_URL`.
  F2 (`handle_streaming_error` at `src/main.rs:749`) has 5 invariants, 1
  test covers 2 of 5. F3 (keepalive) has 1 test in `mod slow_tests`. F4
  (JSON contract) has 6+ substring-match assertions, zero `serde_json::from_str`.
- **A real code-path divergence** in F2: the inline mid-stream error branch
  in `handle_streaming_response` (`src/main.rs:712-720`) uses un-escaped
  `serde_json::json!({"error": error_msg})` while `handle_streaming_error`
  (line 749+) escapes `\\`, `"`, `\n`, `\r` before serializing. Both produce
  valid JSON today (the inline branch escapes via `serde_json` itself), but
  the two paths use different escape mechanisms and have no shared helper.
- **`lessons.md:26-31`** mandates self-describing comments, not F-markers,
  for guard points — the F1–F4 markers were removed in a prior cleanup and
  must not be re-introduced as `// F1`, `// F2` annotations.
- **Test infrastructure gap**: `build_app_with_persistence`
  (`src/main.rs:2206`) takes `Arc<sqlx::PgPool>` and a `Semaphore`; tests
  using it always skip without `DATABASE_URL`. Snippet path is not exercised
  in default CI today.

## Desired End State

After this plan lands:

- A future contributor who touches `ClassifierChain` cannot accidentally
  regress the first-non-Fallback contract without a test failure.
- A future contributor who touches `completion_handler` cannot lose any of
  the F1–F4 invariants (snippet ≤ 200 chars, SSE error event format, SSE
  keepalive as a valid comment, JSON contract shape) without a test failure.
- The snippet path is exercised in default CI (no `DATABASE_URL` required)
  via an in-memory `DbBackend` wired through a refactored harness.
- The two F2 error paths share a single `format_sse_error_event` helper
  with unit tests on the helper and integration tests on the function.
- `test-plan.md` §6 (Cookbook) is fully populated with the patterns this
  rollout produced, so subsequent rollout phases can build on them.

### Key Discoveries:

- `LLMClassifier` returns `tier: Regex` on success — chain observability
  must come from a test-only `CountingClassifier` impl that records call
  counts, not from `tier` inspection.
- `DbBackend` enum (`src/persistence.rs:176`) has 3 variants
  (`Memory`, `Sqlite`, `Postgres`); `MemoryBackend::new()` is a sync
  constructor at line 45. The refactor in Phase 2 is to thread
  `Arc<DbBackend>` (or a `DbBackend` value) through the harness instead of
  hard-coding `Postgres`.
- `FewShotClassifier` has no `for_test` constructor (`src/fewshot_classifier.rs:25`).
  Phase 1 uses a `CountingClassifier` for the fewshot tier instead of
  loading real bootstrap YAML. The 0.6 cold-start threshold
  (`src/fewshot_classifier.rs:128-134`) is `FewShotClassifier` internal
  and is NOT exercised by Phase 1's chain test — it requires a separate
  test on `FewShotClassifier` itself, which is out of scope here.
- The keepalive payload is `Bytes::from_static(b": keepalive\n\n")` —
  a valid SSE comment. The current test's
  `body.contains(": keepalive\n\n")` is *not* precise (a regression to
  `data: keepalive\n\n` would still pass); the fix is
  `body.contains("\n: keepalive\n\n")`, anchored to line start.
- `mod slow_tests` (`src/main.rs:3003-3185`) already uses `#[serial]` and
  real TCP listeners via `tokio::io`. No new infrastructure is needed for
  Phase 4 keepalive tests.

## What We're NOT Doing

- **Adding an `Llm` tier variant to `ClassificationTier`.** Out of scope:
  this is a public-type change that would require its own change. Phase 1
  uses `CountingClassifier` side-effect observation instead.
- **PII redaction in `extract_snippet`** (Risk #6, Phase 2 work). The
  current function only truncates. PII corpus tests are Phase 2.
- **Persistence cross-backend drift tests** (Risk #5, Phase 2 work).
  `testcontainers`-backed tests for `postgres` + `sqlite` are Phase 2.
- **Async logging failure observability** (Risk #3, Phase 2 work). The
  bounded semaphore + spawn-failure detection is Phase 2.
- **Dashboard rendering tests** (Risk #4, Phase 3). Routes + `dashboard_page!`
  macro coverage is Phase 3.
- **Constant-time compare grep-based guard** (Risk #7, Phase 3). The
  CI grep gate is Phase 3.
- **CI floor + coverage threshold** (test-plan Phase 4). Out of scope for
  this change.
- **The `FewShotClassifier` 0.6 cold-start threshold test.** A future test
  on `FewShotClassifier::effective_threshold_for` boundary is desirable but
  is not part of Risk #1's chain-handoff contract.
- **F2 helper as public API.** The `format_sse_error_event` helper stays
  `pub(crate)` or `pub(super)`.

## Implementation Approach

Six independent phases. Phases 1–5 are independent and can be implemented
in any order; Phase 6 ships last. Each phase is small enough to land in
one or two commits. The whole change is tractable in 1–2 sessions.

The pattern: for each phase, write the test first, watch it fail (proving
the gap), then add the minimum production code (if any) to make it pass.
Phase 3 is the only phase with a non-trivial production-code refactor
(extracting `format_sse_error_event` and aligning the inline branch); the
helper extraction and the test that locks the new behavior should land
in the same commit to keep the diff small and reviewable.

## Critical Implementation Details

Three constraints the implementer needs to know before touching the code:

- **Chain observability requires side-effect observation, not tier inspection.**
  `LLMClassifier` returns `tier: ClassificationTier::Regex` on success
  (`src/intent_classifier.rs:344`); the `ClassificationTier` enum has
  only `Regex | FewShot | Fallback` (lines 89-94). Any test that needs to
  prove "the LLM was called" or "the regex short-circuited the chain"
  must use `CountingClassifier` (test-only `IntentClassify` impl that
  increments `AtomicUsize::fetch_add(1)` per call) and assert on the
  counter. Tier inspection cannot distinguish "regex matched" from
  "LLM matched".

- **`format_sse_error_event` helper contract.** The helper takes a
  pre-truncated, pre-escaped error message string and returns the SSE
  event body `event: error\ndata: {"error":"<msg>"}\n\n`. The 2 KB cap
  and 512-char truncate (upstream) and the status passthrough,
  `Content-Type`, `Cache-Control` (downstream on the response) are NOT
  the helper's concern. Both call sites — `handle_streaming_error` and
  the inline mid-stream branch in `handle_streaming_response` — must
  apply the same escape rule (`\\` → `\\\\`, `"` → `\\"`, `\n` → ` `,
  `\r` → ` `) on `error_msg` before calling the helper. The helper's
  own unit tests cover the 2 invariants that are its concern: JSON
  escape correctness and the SSE event format.

- **`build_app_with_persistence` refactor ripple.** Changing the
  signature to accept `Arc<DbBackend>` (so `Memory` can be injected in
  default CI) affects 3 call sites: `build_app` at `src/main.rs:2200`,
  the 2 `persistence_integration_*` tests at lines 1854 and 1923. The
  refactor must preserve their current behavior (real `Postgres`
  backend) — the in-memory variant is a new parameter, not a new
  default. The 2 existing tests keep their `DATABASE_URL` skip
  behavior; new in-memory tests always run.

## Phase 1: Risk #1 — Chain handoff contract

### Overview

Add the missing 3-backend chain coverage: a test-only `CountingClassifier`
impl that records call counts, two new stub-based scenarios
(regex short-circuits; middle-backend matches), and a 3-backend
integration test that proves the cold-start `regex → fewshot → LLM`
escalation path fires the LLM tier exactly once. Risk #1 is locked
when the chain cannot regress to "stop at the first backend regardless
of result" or "call all backends even after a match" without a test
failure.

### Changes Required:

#### 1. `CountingClassifier` test backend

**File**: `src/intent_classifier.rs` (in `#[cfg(test)] mod tests` block,
sibling to the existing `StubClassifier` at line 856)

**Intent**: A new test-only `IntentClassify` impl that records how many
times `classify()` is invoked and returns a configurable
`ClassificationResult`. This is the only mechanism the chain tests can
use to prove which backend fired, because `LLMClassifier` returns
`tier: Regex` on success and the tier enum has no Llm variant.

**Contract**: A struct holding an `Arc<AtomicUsize>` counter and a
`ClassificationResult` field; impls `IntentClassify` with `async fn
classify()` that increments the counter then returns the configured
result. Must be `Send + Sync + 'static` to fit in `ClassifierChain`'s
`Vec<Arc<dyn IntentClassify + Send + Sync>>`. Public to the test
module only.

#### 2. Stub-based 3-backend chain scenarios

**File**: `src/intent_classifier.rs` (in `#[cfg(test)] mod tests` block,
alongside the 5 existing stub-chain tests at lines 854-998)

**Intent**: Three new unit tests using `CountingClassifier` to prove
the chain's "first-non-Fallback wins, later backends not called" and
"last-Fallback returned when all fail" contracts with 3 backends.
This is the unit-level floor of Risk #1; the integration test (item
3) is the chain-vs-real-classifier floor.

**Contract**: Three new test functions in the same block as
`chain_returns_first_regex_match` (line 867), each constructs a
`ClassifierChain` of 3 `CountingClassifier`s with specific
configurations, invokes `classify()`, and asserts (a) the returned
`ClassificationResult` matches the expected backend's result and
(b) the counters on the other backends are still 0 (or the expected
counts). Test names follow the existing `chain_<scenario>` pattern.

The 3 scenarios:
- 3-backend chain where only the first matches: regex match, fewshot +
  LLM never called. Asserts `counter[0] == 1`, `counter[1] == 0`,
  `counter[2] == 0`.
- 3-backend chain where only the middle matches: regex + LLM never
  called. Asserts `counter[1] == 1`, `counter[0] == 0`, `counter[2] == 0`.
- 3-backend chain where all return Fallback: LLM's Fallback returned
  (chain returns the last backend's Fallback). Asserts
  `counter[2] == 1` and the returned result matches the LLM stub.

#### 3. 3-backend integration test

**File**: `src/main.rs` (in `#[cfg(test)] mod tests` block, sibling to
`test_chain_with_regex_and_fewshot` at line 1333)

**Intent**: An HTTP-level test that constructs a real chain with
`[Regex, CountingClassifier(fewshot-stub), LLMClassifier(httpmock)]`
and proves that an ambiguous prompt (regex no match, fewshot stub
returns Fallback, LLM returns a category) escalates the request to
the LLM tier exactly once. This is the production-data-path floor of
Risk #1's cold-start escalation contract.

**Contract**: A new test function in the same block as
`test_chain_with_regex_and_fewshot` (line 1333). Uses
`test_app_with_classifier()` (line 1243) as the harness, builds a
3-element chain, sets up an `httpmock::MockServer` as the LLM
endpoint, sends a request with a prompt designed to bypass regex
(`"can you explain what a hash map is"` matches fewshot's bootstrap;
the test sends a new prompt that matches neither regex nor fewshot's
bootstrap but matches the LLM's mock), and asserts:
(a) response status is 200 (chain matched the LLM's category),
(b) response body indicates the LLM's category,
(c) the LLM `httpmock` mock was called exactly once (call-count
assertion via `httpmock`'s `MockServer::hits()` API),
(d) the fewshot `CountingClassifier` counter is 1.

The test does not exercise the 0.6 cold-start threshold
(`src/fewshot_classifier.rs:128-134`); that is `FewShotClassifier`
internal and is out of scope for Risk #1. A future test on
`FewShotClassifier::effective_threshold_for` boundary is desirable
but deferred.

#### 4. Extend the existing 2-backend test (optional, low priority)

**File**: `src/main.rs:1333` (`test_chain_with_regex_and_fewshot`)

**Intent**: Add a `CountingClassifier` assertion to the existing test
to prove "regex matched, so fewshot was not called". This is a
defense-in-depth change, not a new test.

**Contract**: Wrap the fewshot classifier (currently a real
`FewShotClassifier`) in a `CountingClassifier` (or replace it) such
that the prompt `"fix this bug"` (which regex matches) leaves the
fewshot counter at 0. If the test currently uses a real
`FewShotClassifier`, the refactor is to swap it for a stub
returning a known non-Fallback result; if the existing test is
already using a stub, add a counter to it.

### Success Criteria:

#### Automated Verification:

- `cargo test chain_` (the 3 new chain scenarios + 1 extended test pass).
- `cargo test test_chain_3_backend` (the new integration test passes).
- `cargo build --release` succeeds.
- `cargo clippy --all-targets` reports no new warnings.
- `cargo fmt --check` passes.

#### Manual Verification:

- Open `src/intent_classifier.rs` mod tests block and confirm the
  3 new stub-chain tests are present and the assertions are on
  counter values, not just on the returned result.
- Open `src/main.rs` mod tests block and confirm the new
  integration test is wired to `httpmock` and asserts on call
  count, not just on the response body.
- Skim the production code in `src/main.rs:790-976`
  (`completion_handler`) — confirm no production code was changed
  in Phase 1. Phase 1 is test-only.

**Implementation Note**: After completing this phase and all automated
verification passes, pause here for manual confirmation before
proceeding to Phase 2. The Phase 1 work is test-only; production code
in `src/main.rs::completion_handler` is unchanged.

---

## Phase 2: Risk #2 F1 — Snippet path coverage

### Overview

Refactor `build_app_with_persistence` to accept a `DbBackend` (or
`Arc<DbBackend>`) so an in-memory backend can be injected, then add
HTTP-level tests that exercise the snippet path in default CI. The
tests lock 3 F1 invariants: snippet is ≤ 200 chars, snippet does not
contain the full prompt body, and `log_classification` failure does
not block the proxy response.

### Changes Required:

#### 1. Refactor `build_app_with_persistence` signature

**File**: `src/main.rs:2206`

**Intent**: Change the harness to accept a `DbBackend` value (or
`Arc<DbBackend>`) so callers can pass `DbBackend::Memory(...)`
without a Postgres pool. The current signature requires
`Arc<sqlx::PgPool>` and a `Semaphore`; both are still needed for
Postgres callers but can be made optional or moved to a Postgres-only
constructor.

**Contract**: A new `pub(crate) fn build_app_with_persistence_backend(
backend: Arc<DbBackend>, ...)` that takes the backend by value. The
existing `build_app_with_persistence` (Postgres variant) becomes a
thin wrapper that constructs a `PostgresBackend` from the pool and
calls the new function. Or: change the existing function's signature
to take `Arc<DbBackend>` and have the 2 existing
`persistence_integration_*` tests (lines 1854, 1923) construct the
Postgres variant themselves. Either way, the 2 existing tests must
keep their `DATABASE_URL` skip behavior; their semantics are
unchanged. The new in-memory variant always runs in default CI.

#### 2. New HTTP-level snippet-path tests

**File**: `src/main.rs` (in `#[cfg(test)] mod tests` block, sibling
to the existing `persistence_integration_*` tests at lines 1854, 1923)

**Intent**: Three new test functions that use the refactored harness
with `DbBackend::Memory(MemoryBackend::new())` and exercise the
snippet path end-to-end. The tests are HTTP-level (full axum stack
including middleware) but use the in-memory backend so they always
run in default CI.

**Contract**: Three new test functions:

- `test_snippet_path_truncates_to_200_chars` — send a request with a
  user message > 200 chars; assert the persisted
  `InferenceRecord::prompt_snippet` is ≤ 200 chars. Reads from the
  `MemoryBackend::records` `Vec` directly (it has
  `pub records: Arc<RwLock<Vec<InferenceRecord>>>` per
  `src/persistence.rs:41`).
- `test_snippet_path_does_not_contain_full_prompt` — send a request
  with a unique marker substring; assert the persisted snippet
  contains a 200-char-prefix slice of the user message and not the
  full prompt body. Use a marker like `UNIQUE_MARKER_XYZ` to make
  the assertion precise.
- `test_log_classification_failure_does_not_block_response` —
  point the `DbBackend` at a backend whose `log_inference` returns
  an error (or use a wrapper that returns Err); send a request;
  assert (a) response status is 200 (proxy succeeds), (b) warn log
  is emitted at the configured `tracing` level, (c) the bounded
  semaphore on the `PersistenceConfig` is released (no deadlock).
  This may require a custom test-only `DbBackend` variant or a
  test-only flag on `MemoryBackend` to simulate failure; the
  implementer picks the cleanest path.

The 2 existing `persistence_integration_*` tests at lines 1854, 1923
are updated to use the new signature (Postgres variant, same
`DATABASE_URL` skip behavior).

### Success Criteria:

#### Automated Verification:

- `cargo test snippet_path_` (the 3 new tests pass in default CI,
  no `DATABASE_URL` required).
- `cargo test persistence_integration_` (the 2 existing tests pass
  with their existing `DATABASE_URL` skip behavior unchanged).
- `cargo build --release` succeeds.
- `cargo clippy --all-targets` reports no new warnings.

#### Manual Verification:

- Open the new tests and confirm they assert on the persisted
  `InferenceRecord` fields, not on a mocked call to
  `log_classification`. The tests must read from
  `MemoryBackend::records` directly to prove the data flowed
  through `log_classification` end-to-end.
- Confirm the 2 existing `persistence_integration_*` tests still
  skip without `DATABASE_URL` (their `cfg!(feature)` or env-var
  guard is preserved).
- Skim the production code in `src/main.rs:458-490`
  (`log_classification`) and `src/persistence.rs:1080-1088`
  (`extract_snippet`) — confirm no production code was changed in
  Phase 2. Phase 2 is harness + test changes only.

**Implementation Note**: After completing this phase and all automated
verification passes, pause here for manual confirmation before
proceeding to Phase 3. The Phase 2 work is harness + test changes;
production code in `log_classification` and `extract_snippet` is
unchanged.

---

## Phase 3: Risk #2 F2 — SSE error path invariants (production refactor + tests)

### Overview

Extract a `format_sse_error_event(error_msg: &str) -> String` helper
used by both `handle_streaming_error` and the inline mid-stream
error branch in `handle_streaming_response`. Add self-describing
comments for the 5 F2 invariants (per `lessons.md:26-31`, no F-marker
re-introduction). Add unit tests on the helper and integration
tests on the function that lock all 5 invariants.

### Changes Required:

#### 1. Extract `format_sse_error_event` helper

**File**: `src/main.rs` (placement: above `handle_streaming_error`
at line 749, scoped as `pub(crate)` or `pub(super)` so it can be
called from the inline mid-stream branch in `handle_streaming_response`
at line 712)

**Intent**: A single source of truth for the SSE error event format.
The helper takes a pre-truncated, pre-escaped error message string
and returns the formatted SSE event body
`event: error\ndata: {"error":"<msg>"}\n\n`. The 2 KB body cap and
512-char truncate are upstream (only `handle_streaming_error` needs
them); the status passthrough, `Content-Type`, and `Cache-Control`
are downstream on the response (only `handle_streaming_error` sets
them). The helper's own invariants are: (a) JSON-escape correctness
of the embedded message and (b) the SSE event format.

**Contract**: A function `fn format_sse_error_event(error_msg: &str)
-> String` that returns exactly the SSE event body. The escape rule
replaces `\\` with `\\\\`, `"` with `\\"`, `\n` and `\r` with a
single space. The 5 F2 invariants of the overall
`handle_streaming_error` function are split: 1 upstream (2 KB cap),
2 in the helper (JSON escape + SSE format), 2 downstream (status +
headers).

#### 2. Add self-describing comments for the 5 F2 invariants

**File**: `src/main.rs` (in `handle_streaming_error` body, lines
749-783)

**Intent**: Document the 5 F2 invariants as self-describing comments
inside the function — per `lessons.md:26-31`, these comments explain
WHAT invariant is protected and WHY, not review cross-references.
The F-markers (`// F1`, `// F2`) must NOT be re-introduced; this
phase reintroduces the documentation that the F-markers used to
provide, in the form the team agreed on.

**Contract**: A function-level docstring at the top of
`handle_streaming_error` describing the 5 invariants in plain
language (2 KB cap, JSON escape, SSE format, status passthrough,
content-type + cache-control). Inline comments at the key lines
explaining why each invariant matters (e.g., "Truncate early to
bound latency and memory on large upstream error bodies" at the
2 KB cap line). No `// F1`-style markers.

#### 3. Use the helper in both call sites

**File**: `src/main.rs:712-720` (inline mid-stream error branch)
and `src/main.rs:749-783` (`handle_streaming_error`)

**Intent**: Replace both SSE-event-formatting sites with calls to
the new helper. The inline branch currently uses
`serde_json::json!({"error": error_msg}).to_string()` followed by
`format!("event: error\ndata: {}\n\n", json_payload)`. After the
refactor, both call sites do:
1. Apply the escape rule to `error_msg` (replace `\\`, `"`, `\n`, `\r`).
2. Call `format_sse_error_event(&error_text)`.
3. Use the returned string as the body.

**Contract**: The inline branch (line 712) escapes the error
message using the same rule as `handle_streaming_error` (line 768-770)
before calling the helper. The output of the inline branch is now
`event: error\ndata: {"error":"<escaped>"}\n\n`, identical in
contract to `handle_streaming_error`'s output body. The
`serde_json::json!` call is removed from the inline branch.

#### 4. Unit tests on the helper

**File**: `src/main.rs` (in `#[cfg(test)] mod tests` block, sibling
to `test_streaming_handler_non_2xx_returns_sse_error_event` at line
2639)

**Intent**: Unit tests on `format_sse_error_event` covering the 2
helper-owned invariants. These are pure-string tests, no HTTP
infrastructure needed.

**Contract**: New test function `test_format_sse_error_event_<case>`
covering:
- plain text input → `event: error\ndata: {"error":"hello"}\n\n`.
- input with `\\` → escape becomes `\\\\`.
- input with `"` → escape becomes `\\"`.
- input with `\n` → escape becomes ` `.
- input with `\r` → escape becomes ` `.
- combined injection attempt
  `";\n}\nattack\n\r{"` → produces valid JSON inside `data:` payload
  (parseable by `serde_json::from_str` after splitting off the
  `data: ` prefix).

#### 5. Integration tests on `handle_streaming_error`'s 5 invariants

**File**: `src/main.rs` (in `#[cfg(test)] mod tests` block, sibling
to `test_streaming_handler_non_2xx_returns_sse_error_event` at line
2639)

**Intent**: HTTP-level tests that lock all 5 F2 invariants of
`handle_streaming_error` end-to-end. The existing test at line 2639
covers 2 of 5 (status passthrough + body starts with `event: error`).
The new tests cover the remaining 3 + tighten the existing 2.

**Contract**: New test functions:

- `test_streaming_handler_error_truncates_oversized_body` — upstream
  returns 503 with a body > 2 KB. Assert the SSE event body is
  bounded (assert body byte length ≤ 2 KB + format overhead).
- `test_streaming_handler_error_escapes_json_injection` — upstream
  returns 503 with body `{"error":"a\"b\\c\nd"}`. Assert the SSE
  event's `data: <payload>` is parseable JSON when extracted (split
  on `data: ` and `\n`).
- `test_streaming_handler_error_content_type_and_cache_control` —
  upstream returns 503 with a small body. Assert
  `Content-Type: text/event-stream` and `Cache-Control: no-cache`
  on the response.
- `test_streaming_handler_error_status_passthrough_multiple_codes` —
  upstream returns 429, 500, 502, 503 in separate test cases. Assert
  the response status matches in each case.
- (optional) `test_inline_mid_stream_error_uses_same_format` —
  arrange for the upstream chunk stream to error mid-stream. Assert
  the inline branch emits `event: error\ndata: {"error":"..."}\n\n`
  matching the helper's output. Requires a way to trigger the
  inline branch deterministically; defer if the arrange is hard.

### Success Criteria:

#### Automated Verification:

- `cargo test format_sse_error_event_` (helper unit tests pass).
- `cargo test streaming_handler_error_` (integration tests pass;
  covers all 5 invariants).
- `cargo test streaming_handler_non_2xx_returns_sse_error_event`
  (the existing test still passes — its 2 invariants are still
  covered).
- `cargo build --release` succeeds.
- `cargo clippy --all-targets` reports no new warnings.

#### Manual Verification:

- Open `src/main.rs:749-783` and confirm the function-level
  docstring + inline comments describe the 5 invariants in plain
  language, with no `// F1`/`// F2` markers.
- Open `src/main.rs:712-720` and confirm the inline branch now
  calls the same helper (or applies the same escape rule) as
  `handle_streaming_error`.
- Skim a few upstream test bodies that previously asserted
  `body.contains("event: error")` — confirm none of them broke
  from the refactor (the SSE event format is unchanged at the
  byte level for inputs without injection attempts).

**Implementation Note**: This phase is the only one with
non-trivial production-code changes. The helper extraction,
inline branch alignment, and self-describing comments all ship
together in one commit. After automated verification passes,
pause for manual confirmation that the docstring is well-written
(per the team's review preference in `lessons.md:26-31`) before
proceeding to Phase 4.

---

## Phase 4: Risk #2 F3 — Keepalive coverage

### Overview

Add 3 new `slow_tests` that exercise keepalive timing edges
("upstream completes before keepalive", "upstream chunk during
keepalive tick", "multiple consecutive keepalives"), and tighten
the existing assertion from `body.contains(": keepalive\n\n")` to
`body.contains("\n: keepalive\n\n")` (anchored to line start —
distinguishes SSE comment from `data: keepalive`).

### Changes Required:

#### 1. New keepalive slow tests

**File**: `src/main.rs:3003-3185` (in `mod slow_tests` block, sibling
to `test_streaming_keepalive_injected` at line 3047)

**Intent**: Lock 3 keepalive timing edges that the single existing
test does not cover. The existing test engineers a 1500ms stall so
at least one keepalive fires; the new tests cover the cases the
existing test explicitly avoids.

**Contract**: Three new test functions, each `#[serial]`, each
using `spawn_slow_sse_server` (line 3025) or a similar helper
to control the upstream's chunk timing:

- `test_streaming_keepalive_not_injected_when_upstream_fast` —
  upstream sends `data: hello\n\n` within 500ms (shorter than the
  1s keepalive interval). Assert the response body does NOT
  contain `: keepalive`. Proves the chain does not pre-empt
  upstream data.
- `test_streaming_keepalive_injected_alongside_chunk` — upstream
  sends a chunk, then idles past the keepalive interval, then
  sends another chunk. Assert the response body contains both
  the upstream chunks AND at least one `: keepalive` between
  them. Proves the `tokio::select!` race is handled correctly.
- `test_streaming_keepalive_multiple_consecutive` — upstream stalls
  for 3500ms (≥ 3 keepalive intervals at 1s). Assert the response
  body contains ≥ 3 `: keepalive` payloads. Proves the keepalive
  loop is sustained.

#### 2. Tighten existing assertion

**File**: `src/main.rs:3148` (in `test_streaming_keepalive_injected`)

**Intent**: Change `body.contains(": keepalive\n\n")` to
`body.contains("\n: keepalive\n\n")` to anchor the match to a line
start. The current assertion is not precise (a regression to
`data: keepalive\n\n` would still pass because the substring
`: keepalive\n\n` is contained in `ata: keepalive\n\n`); the new
assertion requires the line to BEGIN with `:`, distinguishing
SSE comment from `data:` event.

**Contract**: The existing test's `assert!(body.contains("\n: keepalive\n\n"))`
(line 3148 area) is updated. The test still passes; a regression
to `data: keepalive\n\n` would now fail.

### Success Criteria:

#### Automated Verification:

- `cargo test slow_tests -- --test-threads=1` (the 3 new keepalive
  tests + the tightened existing test pass; the `serial` macro
  enforces sequential execution).
- `cargo test test_streaming_keepalive_injected` (the tightened
  test passes in isolation).
- `cargo build --release` succeeds.
- `cargo clippy --all-targets` reports no new warnings.

#### Manual Verification:

- Open the 3 new tests and confirm each one engineers a different
  timing edge (fast upstream, chunk-then-idle, long stall). The
  tests must be deterministic, not flaky.
- Confirm the new tests are in `mod slow_tests`, not `mod tests`
  — they use real delays and must not run in fast CI.

**Implementation Note**: After completing this phase and all
automated verification passes, pause here for manual confirmation
before proceeding to Phase 5. The Phase 4 work is slow_tests only;
no production code in `handle_streaming_response` is changed.

---

## Phase 5: Risk #2 F4 — JSON contract parsing

### Overview

Refactor 6+ existing substring-match assertions in `src/main.rs`
to use `serde_json::from_str::<Value>` and assert on key count,
key types, and exact shape. Add dedicated shape tests for
`classification_only_json`, `upstream_error_json`, the `tier`
field, and `Content-Type: application/json` on the classification
path. F4 is locked when a regression that adds, removes, renames,
or type-changes any JSON field is caught by a test.

### Changes Required:

#### 1. Refactor existing substring assertions to JSON parsing

**File**: `src/main.rs` (multiple test bodies: 1412, 1446, 1576,
1619, 1648, 2459, 1485, 2800)

**Intent**: Replace `body.contains("...")` substring matches with
`serde_json::from_str::<serde_json::Value>(&body)` followed by
assertions on the parsed `Value` (key count, key types, exact
shape). This catches regressions that the substring match misses
— e.g., a regression that adds an extra JSON field, changes a
field type, or renames a key.

**Contract**: For each of the 8 tests listed, replace the
substring-match assertion with:
- `let v: serde_json::Value = serde_json::from_str(&body).expect("body is valid JSON");`
- Assert `v.is_object()`, `v.as_object().unwrap().len() == <expected count>`.
- Assert each key is present with the expected type
  (`v.get("status").and_then(|x| x.as_str()) == Some("classified")`,
  `v.get("status").and_then(|x| x.as_u64()).is_some()` for integers,
  etc.).
- For negative-contract tests (e.g., "no `api_key` field"), assert
  `v.get("api_key").is_none()`.

The refactor is mechanical per test; the helper functions
`classification_only_json` (`src/main.rs:569`),
`upstream_error_json` (`src/main.rs:560`), `json_response`
(`src/main.rs:550`), and `classify_and_log` (`src/main.rs:531-537`)
define the expected shapes.

#### 2. New dedicated JSON-shape tests

**File**: `src/main.rs` (in `#[cfg(test)] mod tests` block)

**Intent**: Add new tests that lock the JSON contract shape
independent of the production code path. These are the
shape-equivalence tests for the helpers.

**Contract**: New test functions:

- `test_classification_only_json_shape` — call
  `classification_only_json(...)` with a sample result. Parse
  the output. Assert exactly 4 keys: `status`, `category`,
  `model`, `tier`. Assert each key's value is a string.
  Assert `status == "classified"`. Assert `tier` is one of
  `"Regex"`, `"FewShot"`, `"Fallback"` (the 3
  `ClassificationTier` variants — `LLM` is NOT a variant and
  is NOT a valid `tier` value, even though
  `LLMClassifier` returns `tier: Regex`; the test asserts on
  the JSON shape, not on the implementation detail).
- `test_upstream_error_json_shape` — call
  `upstream_error_json(...)` with sample inputs. Parse the
  output. Assert exactly 3 keys: `error`, `status`,
  `message`. Assert `error` and `message` are strings.
  Assert `status` is an integer (NOT a string). Assert
  `error == "upstream_error"`.
- `test_json_response_content_type` — call `json_response(...)`.
  Assert the response has `Content-Type: application/json`.
- `test_tier_field_values` — exercise all 3
  `ClassificationTier` variants (Regex, FewShot, Fallback) via
  the chain and assert the resulting JSON's `tier` field
  matches `"Regex"`, `"FewShot"`, `"Fallback"`. This is the
  one test that ties the contract to the enum.
- `test_classification_only_early_return_sites_reached` —
  trigger each of the 4 early-return sites in
  `completion_handler` (`src/main.rs:854, 875, 887, 892`) and
  assert all 4 paths return the same JSON shape. This is a
  higher-effort test (4 different arrange blocks); defer if
  the arrange is hard and the unit-level shape test
  (`test_classification_only_json_shape`) is considered
  sufficient.

### Success Criteria:

#### Automated Verification:

- `cargo test completion_handler_returns_classification_json`
  (and all 7 other refactored tests pass — substring match
  replaced with JSON parsing).
- `cargo test classification_only_json_shape`,
  `cargo test upstream_error_json_shape`,
  `cargo test json_response_content_type`,
  `cargo test tier_field_values` (new dedicated tests pass).
- `cargo build --release` succeeds.
- `cargo clippy --all-targets` reports no new warnings.

#### Manual Verification:

- Open one of the refactored tests (e.g., the one at line 1412)
  and confirm the new assertion is on the parsed `Value` shape,
  not on a substring. The assertion should fail loudly if a
  field is added/removed/renamed.
- Confirm the `tier` field test covers the 3 real variants
  (`Regex | FewShot | Fallback`) and does not claim
  `"LLM"` as a valid value (it is not a `ClassificationTier`
  variant).

**Implementation Note**: After completing this phase and all
automated verification passes, pause here for manual confirmation
before proceeding to Phase 6. The Phase 5 work is test-only; the
helper functions in `src/main.rs:550-577` are unchanged.

---

## Phase 6: Documentation + cookbook + verification

### Overview

Update `test-plan.md` §6.1–§6.5 (Cookbook) with the reference
patterns this rollout produced; update `change.md` with the final
status and per-phase commit SHAs; run the full gate suite; bump
`test-plan.md` header to "Phase 1 → implementation complete".

### Changes Required:

#### 1. Update `test-plan.md` §6.1 — Adding a unit test

**File**: `context/foundation/test-plan.md:130-135`

**Intent**: Replace the `TBD — see §3 Phase 1` reference test
placeholder with the actual reference test from this rollout.

**Contract**: §6.1 lists `CountingClassifier` (in
`src/intent_classifier.rs` mod tests) as the canonical example of
a test-only `IntentClassify` impl for asserting on backend call
counts. Naming: `test_<unit>_<case>` (already in place). Run
locally: `cargo test <test_name>` (already in place).

#### 2. Update `test-plan.md` §6.2 — Adding an integration test

**File**: `context/foundation/test-plan.md:137-142`

**Intent**: Replace the `TBD — see §3 Phase 1` reference test
placeholder with the 3-backend integration test from Phase 1 and
the snippet-path test from Phase 2.

**Contract**: §6.2 lists the new 3-backend chain integration test
(in `src/main.rs` mod tests) and the `snippet_path_*` tests (also
in `src/main.rs` mod tests) as the canonical examples. Mocking
policy already in place (mock only at HTTP edge via `httpmock`,
never mock internal modules).

#### 3. Update `test-plan.md` §6.3 — Adding an e2e test

**File**: `context/foundation/test-plan.md:144-146`

**Intent**: The "no e2e layer exists today" line is still true
after Phase 1; replace the `TBD — see §3 Phase 1` with a
concrete reference to the closest thing the project has:
`test_app()` + axum `Request` integration tests (e.g., the
3-backend chain test, the snippet-path tests). These exercise the
full proxy stack including middleware, which is the closest
analog to e2e the project has.

**Contract**: §6.3 names the `test_app()` / `test_app_with_classifier()`
harness family (in `src/main.rs` mod tests) as the de facto
integration-test harness. Run locally: `cargo test <test_name>`.

#### 4. Update `test-plan.md` §6.4 — Adding a test for a new API endpoint

**File**: `context/foundation/test-plan.md:148-150`

**Intent**: Replace the `TBD — see §3 Phase 1` with a concrete
reference: the chain-handoff tests in this rollout hit the
`/v1/chat/completions` endpoint and exercise the chain's
escalation logic.

**Contract**: §6.4 names the `test_chain_*` and
`test_snippet_path_*` test families (in `src/main.rs` mod tests)
as the canonical examples. The pattern: build a test app via
`test_app_with_classifier()`, send a request, assert on the
response body and on the backend's call count.

#### 5. Update `test-plan.md` §6.5 — Adding a test for a new classifier backend

**File**: `context/foundation/test-plan.md:152-154`

**Intent**: Replace the `TBD — see §3 Phase 1` with the
`CountingClassifier` test backend (introduced in Phase 1) as the
canonical example of a new `IntentClassify` impl, and the
`ClassifierChain` constructor + backends vector as the wiring
pattern.

**Contract**: §6.5 names `CountingClassifier` (in
`src/intent_classifier.rs` mod tests) as the canonical test
backend. The pattern: a struct holding
`Arc<AtomicUsize>` + `ClassificationResult`, impls
`IntentClassify`. Tests pass it to `ClassifierChain::new(...)` to
assert on call counts.

#### 6. Update `change.md` final status

**File**: `context/changes/testing-critical-path-regression-guards/change.md`

**Intent**: Set `status: implemented` (or `complete` per the
test-plan.md §3 status vocabulary), set `updated: <today>`, list
the commit SHAs for each phase in the Notes section.

**Contract**: The change.md frontmatter is updated. The Notes
section appends a "Progress" block listing the 6 phases with
their closing commit SHAs.

#### 7. Run all gates

**File**: n/a (command execution)

**Intent**: Verify the full rollout passes all gates that
test-plan.md §5 mandates as "required for §3 Phase 1".

**Contract**: Run in sequence:
- `cargo build --release` — compiles cleanly.
- `cargo test` — all fast tests pass in default CI (no
  `DATABASE_URL`).
- `cargo test slow_tests -- --test-threads=1` — all slow tests
  pass.
- `cargo clippy --all-targets -- -D warnings` — no lints.
- `cargo fmt --check` — formatting is clean.

#### 8. Bump `test-plan.md` header

**File**: `context/foundation/test-plan.md:9`

**Intent**: Update the "Last updated" line from
`2026-06-13 (Phase 1 → research complete)` to
`2026-06-13 (Phase 1 → implementation complete)`.

**Contract**: The header line is updated. The §3 Phase 1 row's
Status is updated from `researched` to `complete` (or whatever
the test-plan vocabulary settles on after Phase 6 lands —
`implementing` is the right intermediate value; `complete` is
the right final value).

### Success Criteria:

#### Automated Verification:

- `test-plan.md` §6.1–§6.5 all read as concrete (no remaining
  `TBD — see §3 Phase 1` placeholders).
- `change.md` `status` field is `complete` (or final value per
  vocabulary).
- `cargo build --release` succeeds.
- `cargo test` passes (all fast tests).
- `cargo test slow_tests -- --test-threads=1` passes.
- `cargo clippy --all-targets -- -D warnings` reports no issues.
- `cargo fmt --check` reports no issues.

#### Manual Verification:

- Read `test-plan.md` §6.1–§6.5 end-to-end and confirm each
  subsection's "Reference test" line points to a real test
  this rollout produced, with a real file:line reference.
- Skim the diff of each phase's commit (or PR if reviewing
  pre-merge) and confirm no production code outside the
  explicitly-changed lines in Phase 3
  (`handle_streaming_error`, `handle_streaming_response` inline
  branch) was modified.

**Implementation Note**: Phase 6 is the closing ritual for the
rollout. After it lands, the next action is Handoff D: invoke
`/10x-status testing-critical-path-regression-guards` to confirm
Progress is `complete`, then archive the change via `/10x-archive`
when the team is ready. Phase 2 of the test-plan (Persistence +
snippet guardrails) is the next rollout phase and is out of scope
for this change.

---

## Testing Strategy

### Unit Tests:

- Phase 1: 3 new stub-based chain tests in
  `src/intent_classifier.rs` (Risk #1 contract at the unit level).
- Phase 3: 6+ new `format_sse_error_event` unit tests in
  `src/main.rs` mod tests (F2 helper invariants).
- Phase 5: 4+ new shape tests for `classification_only_json`,
  `upstream_error_json`, `json_response`, and the `tier` field.

### Integration Tests:

- Phase 1: 1 new 3-backend integration test in `src/main.rs` mod
  tests (Risk #1 escalation via real axum stack + `httpmock`).
- Phase 2: 3 new snippet-path tests in `src/main.rs` mod tests
  (F1 invariants via real axum stack + in-memory backend).
- Phase 3: 4+ new F2 integration tests in `src/main.rs` mod tests
  (5 invariants of `handle_streaming_error`).
- Phase 4: 3 new keepalive slow tests in `src/main.rs` mod
  slow_tests (F3 timing edges). Plus 1 tightened existing test.
- Phase 5: 6+ refactored existing tests now use JSON parsing
  instead of substring match.

### Manual Testing Steps:

1. After Phase 1 lands: run `cargo test chain_` and confirm
   counter assertions are on the expected values (not just `> 0`).
2. After Phase 2 lands: run `cargo test snippet_path_` with no
   `DATABASE_URL` set. The 3 new tests must pass; the 2 existing
   `persistence_integration_*` tests must skip cleanly.
3. After Phase 3 lands: read the new docstring on
   `handle_streaming_error` and confirm it describes the 5
   invariants in plain language (no `// F1` markers).
4. After Phase 4 lands: run `cargo test slow_tests` and confirm
   the 4 keepalive tests (3 new + 1 tightened) pass deterministically.
5. After Phase 5 lands: temporarily add a `junk_field` to
   `classification_only_json`'s output, run the shape test,
   confirm it fails. Revert.
6. After Phase 6 lands: read `test-plan.md` §6 end-to-end and
   confirm each subsection's "Reference test" line points to a
   real test from this rollout.

## Performance Considerations

None. The change adds tests and one small helper extraction.
Production code in `handle_streaming_error` and
`handle_streaming_response` runs the same number of operations
per request; the helper extraction is a code-organization change
with no runtime impact. The `build_app_with_persistence` refactor
in Phase 2 is a test-harness change; production app construction
is untouched.

The keepalive slow tests (Phase 4) add ~5 seconds to the slow
test suite (3 new tests × ~1.5s each + setup overhead). This is
acceptable per the test plan's `slow_tests` policy
(`test-plan.md:115`).

## Migration Notes

None. This change adds tests, refactors a test harness, extracts
one helper, and aligns one error path. There are no schema
changes, no public API changes (the `Llm` tier variant is
explicitly out of scope), no env-var changes, no config-file
changes. Existing tests that depend on the current
`build_app_with_persistence` signature (the 2
`persistence_integration_*` tests at lines 1854, 1923) are
updated as part of Phase 2 but their semantics are preserved.

The `format_sse_error_event` helper is `pub(crate)` or
`pub(super)` — not public API. External consumers of the project
(if any) are unaffected.

## References

- Research: `context/changes/testing-critical-path-regression-guards/research.md`
- Change: `context/changes/testing-critical-path-regression-guards/change.md`
- Test plan: `context/foundation/test-plan.md` (Phase 1 brief, §2 risk
  map, §5 quality gates, §6 cookbook)
- Lessons: `context/foundation/lessons.md:12-17` (F1–F4 regression
  history), `:26-31` (self-describing comments, not F-markers)
- Key code anchors (commit `1cc87bfe`):
  - `src/main.rs:712-720` — inline mid-stream error branch (refactored in
    Phase 3)
  - `src/main.rs:749-783` — `handle_streaming_error` (5 invariants,
    refactored in Phase 3)
  - `src/main.rs:458-490` — `log_classification` (F1 path, exercised
    in Phase 2)
  - `src/main.rs:569-577` — `classification_only_json` (F4, shape
    tested in Phase 5)
  - `src/main.rs:560-567` — `upstream_error_json` (F4, shape tested in
    Phase 5)
  - `src/main.rs:2206` — `build_app_with_persistence` (refactored in
    Phase 2)
  - `src/intent_classifier.rs:89-94` — `ClassificationTier` (3 variants,
    no `Llm` — used in Phase 1 + Phase 5)
  - `src/intent_classifier.rs:134-167` — `ClassifierChain` (3-backend
    contract, locked in Phase 1)
  - `src/intent_classifier.rs:854-998` — 5 existing stub-chain tests
    (extended in Phase 1)
  - `src/persistence.rs:40-50` — `MemoryBackend::new()` (used in
    Phase 2)
  - `src/persistence.rs:1080-1088` — `extract_snippet` (F1, exercised
    in Phase 2)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Risk #1 — Chain handoff contract

#### Automated

- [x] 1.1 `cargo test chain_` (3 new stub-based scenarios + 1 extended test pass) — 9c2626d
- [x] 1.2 `cargo test test_chain_3_backend` (3-backend integration test passes) — 9c2626d
- [x] 1.3 `cargo build --release` succeeds — 9c2626d
- [x] 1.4 `cargo clippy --all-targets` reports no new warnings — 9c2626d
- [x] 1.5 `cargo fmt --check` passes — 9c2626d

#### Manual

- [x] 1.6 Production code in `completion_handler` (`src/main.rs:790-976`) is unchanged — 9c2626d

### Phase 2: Risk #2 F1 — Snippet path coverage

#### Automated

- [x] 2.1 `cargo test snippet_path_` (3 new tests pass in default CI) — fd971f7
- [x] 2.2 `cargo test persistence_integration_` (2 existing tests pass with their `DATABASE_URL` skip behavior preserved) — fd971f7
- [x] 2.3 `cargo build --release` succeeds — fd971f7
- [x] 2.4 `cargo clippy --all-targets` reports no new warnings — fd971f7

#### Manual

- [x] 2.5 New tests assert on the persisted `InferenceRecord` fields (not on a mocked `log_classification` call) — fd971f7
- [x] 2.6 2 existing `persistence_integration_*` tests still skip without `DATABASE_URL` — fd971f7
- [x] 2.7 Production code in `log_classification` (`src/main.rs:458-490`) and `extract_snippet` (`src/persistence.rs:1080-1088`) is unchanged — fd971f7

### Phase 3: Risk #2 F2 — SSE error path invariants (production refactor + tests)

#### Automated

- [x] 3.1 `cargo test format_sse_error_event_` (helper unit tests pass) — pending
- [x] 3.2 `cargo test streaming_handler_error_` (integration tests pass; all 5 invariants covered) — pending
- [x] 3.3 `cargo test streaming_handler_non_2xx_returns_sse_error_event` (existing test still passes) — pending
- [x] 3.4 `cargo build --release` succeeds — pending
- [x] 3.5 `cargo clippy --all-targets` reports no new warnings — pending

#### Manual

- [ ] 3.6 Function-level docstring on `handle_streaming_error` describes the 5 invariants in plain language (no `// F1`/`// F2` markers)
- [ ] 3.7 Inline mid-stream error branch (`src/main.rs:712-720`) calls the same helper (or applies the same escape rule) as `handle_streaming_error`

### Phase 4: Risk #2 F3 — Keepalive coverage

#### Automated

- [ ] 4.1 `cargo test slow_tests -- --test-threads=1` (3 new keepalive tests + 1 tightened existing test pass)
- [ ] 4.2 `cargo test test_streaming_keepalive_injected` (tightened test passes in isolation)
- [ ] 4.3 `cargo build --release` succeeds
- [ ] 4.4 `cargo clippy --all-targets` reports no new warnings

#### Manual

- [ ] 4.5 3 new keepalive tests each engineer a different timing edge (fast upstream, chunk-then-idle, long stall)
- [ ] 4.6 3 new tests are in `mod slow_tests` (not `mod tests`)
- [ ] 4.7 Production code in `handle_streaming_response` (`src/main.rs:688-747`) is unchanged

### Phase 5: Risk #2 F4 — JSON contract parsing

#### Automated

- [ ] 5.1 `cargo test completion_handler_returns_classification_json` (and 7 other refactored tests pass with JSON parsing)
- [ ] 5.2 `cargo test classification_only_json_shape`
- [ ] 5.3 `cargo test upstream_error_json_shape`
- [ ] 5.4 `cargo test json_response_content_type`
- [ ] 5.5 `cargo test tier_field_values`
- [ ] 5.6 `cargo build --release` succeeds
- [ ] 5.7 `cargo clippy --all-targets` reports no new warnings

#### Manual

- [ ] 5.8 Refactored tests assert on parsed `Value` shape (not on a substring)
- [ ] 5.9 `tier` field test covers the 3 real variants (`Regex | FewShot | Fallback`) and does not claim `"LLM"` as a valid value

### Phase 6: Documentation + cookbook + verification

#### Automated

- [ ] 6.1 `test-plan.md` §6.1–§6.5 all read as concrete (no remaining `TBD — see §3 Phase 1` placeholders)
- [ ] 6.2 `change.md` `status` field is the final value per the test-plan vocabulary
- [ ] 6.3 `cargo build --release` succeeds
- [ ] 6.4 `cargo test` passes (all fast tests)
- [ ] 6.5 `cargo test slow_tests -- --test-threads=1` passes
- [ ] 6.6 `cargo clippy --all-targets -- -D warnings` reports no issues
- [ ] 6.7 `cargo fmt --check` reports no issues

#### Manual

- [ ] 6.8 `test-plan.md` §6.1–§6.5 each have a "Reference test" line pointing to a real test from this rollout
- [ ] 6.9 Phase 3's production code changes are limited to `handle_streaming_error` and `handle_streaming_response` inline branch
