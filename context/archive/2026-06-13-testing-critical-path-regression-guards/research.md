---
date: 2026-06-13T17:15:50+02:00
researcher: opencode (M3)
git_commit: 1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a
branch: main
repository: cerebrum
topic: "Critical-path regression guards (test rollout phase 1) — Risks #1 and #2"
tags: [research, test-rollout, intent-classification, completion-handler, regression-guards, risk-1, risk-2, phase-1]
status: complete
last_updated: 2026-06-13
last_updated_by: opencode (M3)
---

# Research: Critical-path regression guards (test rollout phase 1)

**Date**: 2026-06-13T17:15:50+02:00
**Researcher**: opencode (M3)
**Git Commit**: `1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a`
**Branch**: `main`
**Repository**: pfrack/cerebrum

## Research Question

Open a change folder for rollout Phase 1 of `context/foundation/test-plan.md`:
"Critical-path regression guards". Risks covered: #1 (classifier chain
regex→fewshot→LLM handoff) and #2 (completion_handler regression losing
F1–F4 review fixes).

Risk response intent (verbatim from change.md):

- **Risk #1**: prove the chain escalates from regex to fewshot to LLM when
  regex confidence is low, and that the final category drives routing to
  the right model; challenge the assumption that "each backend works in
  isolation" implies the chain hands off correctly; do not assert "some
  category came back" without checking which tier fired.
- **Risk #2**: prove the F1–F4 review fixes (snippet extraction, streaming
  error path, keepalive, JSON contract) survive any future rewrite of
  completion_handler; challenge the assumption that 46 tests on main.rs
  anchor all four invariants; do not rely on a one-time "test passed"
  snapshot as ongoing protection.

## Summary

This is a brownfield Rust/Axum gateway (`pfrack/cerebrum`, commit
`1cc87bfe`, main). The two risks land in adjacent surfaces: **Risk #1** in
`src/intent_classifier.rs` + `src/fewshot_classifier.rs` + the chain
construction in `src/main.rs`; **Risk #2** across `src/main.rs` (handler
code) and `src/persistence.rs` (snippet helper).

**Risk #1 is half-anchored.** The chain construction
(`src/intent_classifier.rs:149-167`) and per-backend classify methods are
clean, but **no test exercises the full 3-backend escalation path**. There
is exactly one multi-backend test (`src/main.rs:1333`), and it uses
`[Regex, FewShot]` only — no LLM. The 5 chain tests in
`src/intent_classifier.rs:854-998` use stub backends and prove
"first-non-Fallback wins" but not "later backends are not called when an
earlier one matches". A subtle implementation detail makes this gap worse:
`LLMClassifier` returns `tier: ClassificationTier::Regex` on success
(`src/intent_classifier.rs:344`), not a distinct `LLM` variant. The chain's
check `if result.tier != ClassificationTier::Fallback` therefore cannot
distinguish "regex matched" from "LLM matched" — the tier enum has only
`Regex | FewShot | Fallback`. This is a real concern for any test asserting
"the LLM was/wasn't called" via tier alone.

**Risk #2 is largely un-anchored.** The 46-test count on `src/main.rs` is
real (`mod tests`: 44, `mod slow_tests`: 2 — confirmed by `grep -cE
"#\[(tokio::)?test\]" src/main.rs`), but only **20 of 46** tests directly
exercise `completion_handler`, and only **~10 of those 46** anchor any
F1–F4 invariant. The single test that covers the SSE error path
(`src/main.rs:2639`) checks 2 of 5 invariants; the single test for
keepalive (`src/main.rs:3047`, in `mod slow_tests`) checks content
presence, not timing or format-as-SSE-comment. Snippet extraction has
8 unit tests on the helper, but **no HTTP-level test** exercises the
snippet path because all in-process `test_app` variants set
`persistence: None` (`src/main.rs:1222`, `:1268`). The JSON-contract
assertions across the test file all use `body.contains(...)` substring
matching — **no test parses the JSON to assert on shape**. A regression
that added, removed, or renamed a JSON field would not be caught by any
existing test. The `Some(Err(_e))` branch inside `handle_streaming_response`
(`src/main.rs:712-720`) — a mid-stream error path distinct from
`handle_streaming_error` — has **zero tests** and emits un-escaped JSON,
a divergence from the `handle_streaming_error` contract.

**Lessons.md context shapes the plan.** `lessons.md:12-17` warns that
F1–F4 were lost across two follow-up commits; `lessons.md:26-31`
explains why the `// F1` markers are gone (replaced with self-describing
comments). The current code DOES contain the F1–F4 functionality but
lacks the F-markers. Risk #2's "challenge the assumption that 46 tests
anchor all four invariants" is confirmed: tests exist, but they are
spread thin and rely on substring matching.

## Detailed Findings

### Risk #1: Classifier chain mis-handoff

#### The chain (current code)

**`ClassifierChain`** at [`src/intent_classifier.rs:134-167`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L134):

```rust
pub struct ClassifierChain {
    backends: Vec<Arc<dyn IntentClassify + Send + Sync>>,
}

impl IntentClassify for ClassifierChain {
    async fn classify(&self, prompt: &str) -> ClassificationResult {
        if self.backends.is_empty() {
            return ClassificationResult::fallback();
        }
        let mut last_result = None;
        for backend in &self.backends {
            let result = backend.classify(prompt).await;
            if result.tier != ClassificationTier::Fallback {
                return result;
            }
            last_result = Some(result);
        }
        // All backends returned Fallback; return the last one.
        last_result.unwrap_or_else(ClassificationResult::fallback)
    }
}
```

The chain iterates backends in order and returns the **first non-Fallback**.
If all return Fallback, it returns the **last** backend's Fallback result.
There is no confidence-based short-circuit and no tier-priority check
beyond `!= Fallback`.

#### The three live backends

| Backend | File:line | Returns on match | Returns on no-match |
|---|---|---|---|
| `RegexClassifier` | [`src/intent_classifier.rs:557-626`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L557) | `tier: Regex` (via `route_match` at `:628-641`) | `tier: Fallback` (via `route_fallback` at `:643-652`) when 0 or ≥2 categories match threshold |
| `FewShotClassifier` | [`src/fewshot_classifier.rs:312-365`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/fewshot_classifier.rs#L312) | `tier: FewShot` on exact match or cosine score ≥ threshold (0.4 normal / 0.6 cold-start) | `tier: Fallback` (category="unknown", model=DEFAULT_MODEL, endpoint=String::new()) |
| `LLMClassifier` | [`src/intent_classifier.rs:258-374`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L258) | **`tier: Regex` (!)** on successful category match (line 344) | `tier: Fallback` on timeout, bad JSON, empty API key, network error, unknown category (lines 305-321, 351-360) |

**Critical implementation detail**: `LLMClassifier` returns
`tier: ClassificationTier::Regex` (not a distinct `Llm` variant) on a
successful match. The `ClassificationTier` enum
([`src/intent_classifier.rs:89-94`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L89))
has only `Regex | FewShot | Fallback`. The chain's
`tier != ClassificationTier::Fallback` check therefore **cannot
distinguish** a regex-tier match from an LLM-tier match — they look
identical. Any test asserting "the LLM was/wasn't called" must use
side-effect observation (e.g., mock the `reqwest::Client`), not tier
inspection.

`LLMClassifier::get_routing()` returns `None`
([`src/intent_classifier.rs:371-373`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L371)),
so the LLM's category routing is **not** added to the merged routing
table. The LLM's response category must already be in the regex
classifier's routing table to drive upstream selection.

#### Chain construction (production)

[`src/main.rs:246-323`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L246)
builds the chain by iterating `classifiers_config.order` (config-driven;
default `vec!["regex", "fewshot", "llm"]` per
[`src/config.rs:914`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/config.rs#L914)).
Each backend is pushed only if its `enabled` flag is true. The chain is
`None` (not an empty chain) when no backends are enabled. When the chain
is `Some`, it is always used — there is no short-circuit at the
`completion_handler` level for single-backend cases.

The merged routing table
([`src/main.rs:315-320`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L315))
calls `backend.get_routing()` on each chain backend. Regex and FewShot
contribute their routing tables; LLM contributes `None`. So the routing
table seen by `completion_handler` is **regex + fewshot** even when LLM
is in the chain. If the LLM escalates a prompt to a category that
exists in the regex routing table, the request is forwarded; if the LLM
returns a category NOT in the regex routing table, the request falls
through to the fallback model — a subtle but real behavior.

#### Routing decision flow (downstream of chain)

[`src/main.rs:790-976`](https://github.com/pfrack/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L790)
is the `completion_handler`. The chain result flows as:

1. `let prompt = persistence::extract_last_user_message(&body_str);` (line 860)
2. `let classification = c.classify(&prompt).await;` (line 862) — `c` is `Option<Arc<dyn IntentClassify>>`; at runtime it is the `ClassifierChain` or `None`.
3. `let (client_wants_stream, upstream_req) = build_upstream_request(client, &classification, &body, &api_key, &state.auth_providers);` (line 907)
4. Inside `build_upstream_request` ([`src/main.rs:579-617`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L579)):
   - `let mut map = serde_json::Map::new();` — builds the upstream JSON body.
   - `map.insert("model".to_string(), serde_json::Value::String(classification.model.clone()));` (line 594-598) — the chain's `model` field flows to the upstream body.
   - `client.post(&classification.endpoint)` (line 607) — chain's `endpoint` is used.

No code path overrides `classification.model` between the chain return
and `build_upstream_request`. The only other `model` reference is the
`X-Cerebrum-Model` header at [`src/main.rs:842`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L842)
which is a header-based bypass, not a chain-internal override.

#### Real-backend chain test (the only one)

[`src/main.rs:1333-1409`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1333)
`test_chain_with_regex_and_fewshot`:

- Chain built at lines 1395-1398: `vec![Arc::new(regex_classifier), Arc::new(fewshot)]`. **LLM is absent.**
- Assertion 1 (lines 1401-1403): prompt `"fix this bug"` → category `SYNTAX_FIX`, tier `Regex`. Proves regex can match.
- Assertion 2 (lines 1406-1408): prompt `"can you explain what a hash map is"` → category `CASUAL`, tier `FewShot`. Proves fall-through from regex to fewshot via exact-match on bootstrap text.
- **What it does NOT verify**:
  - It does not instrument or mock the fewshot classifier to prove it was **not** called when regex matched.
  - The LLM is not in the chain, so nothing is proven about LLM escalation.
  - The "fall-through" assertion uses `exact_match_in`
    ([`src/fewshot_classifier.rs:114-122`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/fewshot_classifier.rs#L114))
    — it bypasses the cosine-similarity threshold entirely. The cold-start
    threshold (0.6) and the post-cold-start threshold (0.4) are never
    exercised by this test.

#### Stub-based chain tests (5)

[`src/intent_classifier.rs:854-998`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L854)
all use `StubClassifier` (line 856-865, returns a hardcoded
`ClassificationResult` regardless of input):

| Test | Lines | Covers | Gaps |
|---|---|---|---|
| `chain_returns_first_regex_match` | 867-893 | 2 stubs, both `tier: Regex`; asserts first returned. | Does not verify 2nd stub was bypassed. |
| `chain_falls_through_to_next` | 895-921 | 2 stubs, 1st Fallback, 2nd Regex; asserts 2nd returned. | Does not test middle-backend scenarios (chain of 3+). |
| `chain_returns_last_on_all_fallback` | 923-942 | 2 stubs, both Fallback; asserts last returned. | Does not test 3+ backends, all-Fallback. |
| `chain_handles_empty_backends` | 944-950 | 0 backends; asserts `tier: Fallback, category: "unknown"`. | Edge case covered. |
| `trait_boundary_compilation` | 952-973 | Single stub; compile-time check. | Trivial; not behavioral. |

**Missing stub-based scenarios** relevant to Risk #1:
- 3-backend chain where **only the last** returns non-Fallback (escalation to LLM).
- 3-backend chain where **only the middle** returns non-Fallback (the actual `regex → fewshot → llm` case where fewshot matches but regex and LLM don't).
- 3-backend chain where the **first** returns non-Fallback and the 2nd/3rd are Fallback (regex short-circuits, no escalation) — this would prove the chain stops at the first non-Fallback when later backends would return Fallback.
- **Call-count verification**: no test asserts "backend 2 was not called when backend 1 returned non-Fallback".

#### The 28 unit tests in `src/intent_classifier.rs`

Enumerated by reading `#[tokio::test]` and `#[test]` attributes:

| Category | Count | Lines | Anchors chain behavior? |
|---|---|---|---|
| Regex intent (`intent_classify_*`) | 7 | 786-838 | No — all use `RegexClassifier` directly, not in a chain |
| Routing sanity | 1 | 841-852 | No |
| Chain (with stubs) | 5 | 854-998 | Partially — see above |
| Auth headers (`auth_headers_for_*`) | 6 | 986-1037 | No |
| LLM (`llm_classifier_*`) | 4 | 1041-1175 | No — LLMClassifier alone, not in chain |
| LLM prompt builder | 1 | 1178-1187 | No |
| Engine-generality (`test_engine_works_*`) | 4 | 1192-1365 | No — RegexClassifier alone |
| **Total** | **28** | — | — |

The 11 regex-classifier-specific tests (7 + 4) all assert `tier ==
Regex` or `tier == Fallback` on the standalone `RegexClassifier`. They
prove the regex backend in isolation but **do not** prove that a chain
wrapping it short-circuits before the next backend runs. The test
plan's "28 regex unit tests don't prove escalation" intuition is
correct.

#### Cold-start → LLM escalation path (untested)

[`src/fewshot_classifier.rs:128-134`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/fewshot_classifier.rs#L128)
`effective_threshold_for`: returns `cold_start_threshold` (0.6) when
`feedback_count < cold_start_feedback_count` (5); otherwise
`confidence_threshold` (0.4).

For a prompt with cosine score 0.5 against a cold-start fewshot
classifier:
1. Regex returns Fallback (no match) → chain continues.
2. Fewshot score 0.5 < cold_start 0.6 → returns Fallback → chain continues.
3. **LLM is called** ([`src/intent_classifier.rs:158`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L158)).
4. If LLM also returns Fallback, chain returns LLM's Fallback
   ([`src/intent_classifier.rs:165`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L165)).

The LLM's Fallback has `category: "unknown", model: DEFAULT_MODEL,
endpoint: String::new()` — meaning the request would be forwarded to
`DEFAULT_MODEL` (from `src/routing.rs:50`) at an empty endpoint, which
hits the "no endpoint configured" path at [`src/main.rs:898-904`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L898)
and returns a `BAD_GATEWAY`. This is an undocumented contract that the
test plan should consider.

No test covers the cold-start → LLM escalation. The single multi-backend
test (`src/main.rs:1333`) doesn't include LLM.

### Risk #2: completion_handler F1–F4 regression

#### F1: Snippet extraction

**Where it lives** (delegation chain):

| Layer | Location | Behavior |
|---|---|---|
| Handler | [`completion_handler`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L790) | Calls `log_classification` (synchronous, returns a `JoinHandle`). |
| Helper | [`log_classification` at `src/main.rs:458-490`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L458) | Builds `InferenceRecord` with `snippet = persistence::extract_snippet(body_str)`; spawns `log_inference` task. |
| Snippet fn | [`persistence::extract_snippet` at `src/persistence.rs:1085-1088`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1085) | Calls `extract_last_user_message` (line 1051-1078) and `chars().take(200)`. |
| Last-user fn | [`persistence::extract_last_user_message` at `src/persistence.rs:1051-1078`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1051) | Parses `body.messages`, finds last `role == "user"`, returns content truncated to 10,000 chars. 1000-message DoS guard at line 1056. |

**Critical observations**:
1. **No PII redaction.** Docstring at
   [`src/persistence.rs:1080`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1080)
   says "Extract a 200-char privacy-safe snippet", but the function
   only truncates. `grep -nE "pii|PII|redact" src/*.rs` returns zero
   matches. **Risk #6 (test plan §2: PII leakage) is unaddressed in
   code.** A plan for Risk #6 will need a redaction layer first
   (out of scope for Phase 1, but worth flagging here).
2. **The 200-char cap is enforced.** The helper does
   `chars().take(200)` on the result of `extract_last_user_message`
   (which is itself capped at 10,000 chars at
   [`src/persistence.rs:1063`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1063)).
3. **`log_classification` is fire-and-forget.** It spawns the DB
   write via `tokio::spawn` and returns immediately. DB failure is
   logged with `error!` at [`src/persistence.rs:1124`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1124)
   but not propagated to the client. The proxy response is unaffected.
   This is the desired non-blocking behavior, but no test currently
   asserts "DB failure does not affect response".
4. **HTTP test harnesses have `persistence: None`.**
   [`test_app()` at `src/main.rs:1213-1241`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1213)
   (line 1222) and `test_app_with_classifier()` (line 1243-1290) both
   set `persistence: None`. So `log_classification` early-returns at
   [`src/main.rs:465-489`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L465)
   and the snippet path is **never exercised via the HTTP-level tests
   in `mod tests`**. Only the `persistence_integration_*` tests
   (which call `build_app_with_persistence` at line 2206) exercise
   the snippet path, and those skip gracefully when `DATABASE_URL`
   is unset (lines 1766, 1791, 1846, 1915).

**Tests that anchor F1**:

- 8 direct unit tests on `extract_snippet` at
  [`src/persistence.rs:1235-1283`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1235)
  (function names: `persistence_snippet_returns_last_user_content`,
  `persistence_snippet_truncates_at_200_chars`,
  `persistence_snippet_picks_last_user_message`,
  `persistence_snippet_returns_empty_on_invalid_json`,
  `persistence_snippet_returns_empty_when_no_user_message`,
  `persistence_snippet_returns_empty_on_empty_body`,
  `persistence_snippet_returns_empty_on_missing_messages_field`,
  `persistence_snippet_returns_empty_on_oversized_array`).
- 6 direct unit tests on `extract_last_user_message` at
  [`src/persistence.rs:1287-1324`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1287).
- 2 indirect integration tests
  ([`src/main.rs:1842`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1842)
  `persistence_integration_sse_streaming_success`,
  [`src/main.rs:1911`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1911)
  `persistence_integration_sse_streaming_error`) — both skip without
  `DATABASE_URL`. The first asserts `prompt_snippet LIKE '%{unique_id}%'`
  (proving the snippet contains the test's unique ID), but does not
  assert the snippet is ≤ 200 chars or that it does not contain PII.

**Test gaps for F1**:
- **No test verifies `prompt_snippet` is ≤ 200 chars** via the HTTP path.
- **No test asserts `prompt_char_count` matches the full text length** in HTTP integration.
- **No PII corpus tests** (Risk #6 is Phase 2 work, but the docstring claim of "privacy-safe" is currently false).
- **No test for `log_classification` failure handling** (no assertion that "if `log_inference` returns an error, the response is still 2xx").
- **No HTTP test exercises the snippet path** because all in-process test apps have `persistence: None`.

#### F2: SSE error path

**Where it lives**: [`handle_streaming_error` at `src/main.rs:749-783`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L749).
Called from `completion_handler` at line 939 when
`client_wants_stream && !upstream_response.status().is_success()`.

**Invariants (5)**:

1. **2 KB body cap** (`MAX_ERROR_BODY_BYTES = 2 * 1024`, line 750;
   enforced at line 755 with `break`).
2. **JSON escaping of `\\`, `"`, `\n`, `\r`** (lines 768-770), applied
   to `error_text` after truncation to 512 chars.
3. **SSE event format** — exactly `format!("event: error\ndata:
   {{\"error\":\"{}\"}}\n\n", error_text)` (line 771). The double
   `{{` `}}` are `format!` escape syntax, producing literal `{` `}` in
   the output.
4. **Status passthrough** — `*resp.status_mut() = upstream_response.status();` (line 773).
5. **Content-Type + Cache-Control** — `text/event-stream` (line 776)
   and `no-cache` (line 780).

**Tests that anchor F2**: **ONE test** —
[`test_streaming_handler_non_2xx_returns_sse_error_event` at `src/main.rs:2639`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L2639)
(in `mod tests`, not `slow_tests`). It asserts:
- Response status is `StatusCode::SERVICE_UNAVAILABLE` (line 2663) — anchors invariant 4.
- Body starts with `"event: error"` (line 2669) — partially anchors invariant 3.

The upstream mock returns status 503 + body `{"error":"overloaded"}`
(lines 2645-2647).

**Test gaps for F2** (the single test covers 2 of 5 invariants):
- **Invariant 1 (2 KB cap)**: no test for body > 2 KB. The cap is silently enforced by `break` at line 756, but no test asserts "truncated" or "body ≤ 2 KB".
- **Invariant 2 (JSON escaping)**: no test for `\\`, `"`, `\n`, `\r` in the upstream body. A test that returns an upstream body containing literal `"` or `\` would prove the escape works.
- **Invariant 5 (content-type + cache-control)**: not asserted in this test.
- **Different upstream status codes** (429, 500, 502): only 503 is tested.
- **Non-UTF-8 upstream body**: the code uses `String::from_utf8_lossy` (line 764) which silently replaces invalid bytes. No test for this.
- **Empty upstream body**: no test for status 503 + empty body.

**Separate (uncovered) error path**: [`handle_streaming_response` at `src/main.rs:688-747`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L688)
has its OWN inline `Some(Err(_e))` branch at lines 712-720 that emits
an SSE error event when the upstream chunk stream errors mid-stream.
This is a different error path from `handle_streaming_error` (it fires
mid-stream on a `reqwest` chunk error, not on a non-2xx response) and
has **zero tests**. The path uses
`serde_json::json!({"error": error_msg}).to_string()` (line 715)
**without** any `\\` or `"` escaping, which is a divergence from the
`handle_streaming_error` contract. A regression that removes the
escaping in `handle_streaming_error` would not be caught by the single
test, and the inline branch is a separate risk surface.

#### F3: Keepalive

**Where it lives**: [`handle_streaming_response` at `src/main.rs:688-747`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L688).
The keepalive machinery:
- `tokio::spawn` at line 701 starts the streaming task.
- `tokio::time::interval(Duration::from_secs(keepalive_secs))` at line 703.
- `interval.tick().await` at line 706 (initial tick) and line 724 (in the select loop).
- `tokio::select!` at line 708 with two arms: `chunk = stream.next()` (line 709) and `_ = interval.tick()` (line 724).
- The keepalive payload is the literal `Bytes::from_static(b": keepalive\n\n")` at line 725 — a proper SSE comment (colon-prefixed line) that event parsers ignore but keeps the connection warm.

**Tests that anchor F3**: **ONE test** —
[`test_streaming_keepalive_injected` at `src/main.rs:3047-3156`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L3047)
(in `mod slow_tests`, declared at line 3003). The test:
- Uses `#[serial]` annotation (line 3046).
- Calls helper `spawn_slow_sse_server` (line 3025-3043) which creates a
  **real TCP listener** bound to `127.0.0.1:0`. The server:
  1. Accepts the request.
  2. Sends `HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n` immediately (line 3033-3034).
  3. `tokio::time::sleep(1500ms)` (line 3036) — **real delay**.
  4. Sends `data: hello\n\n` (line 3037-3038).
  5. `tokio::time::sleep(100ms)` (line 3040).
- Sets `keepalive_interval_secs = 1` (line 3108).
- Asserts:
  - Response status is `StatusCode::OK` (line 3133).
  - `Content-Type` is `text/event-stream` (lines 3139-3142).
  - Body contains `": keepalive\n\n"` (line 3148) — anchors the payload format.
  - Body contains `"data: hello"` (line 3152) — anchors that upstream data is forwarded after the keepalive.

**Module placement — confirmed slow_tests**: The test is inside
`mod slow_tests` (line 3003). The test plan's recommendation "slow_tests
for keepalive timing" (test-plan.md:55) is correctly followed. **Real
delays**, not a fake clock — `grep -nE "tokio::time::pause|advance|MockClock"`
returns zero matches.

**Test gaps for F3**:
- **No test for "upstream completes before keepalive fires"** (loop exits cleanly via `Some(None) => break` at line 721). The current test specifically engineers a 1500ms stall so that at least one keepalive is forced.
- **No test for "upstream chunk arrives during keepalive tick"** (the `tokio::select!` race). The current test only has a single chunk after a single keepalive.
- **No test for multiple consecutive keepalives** (long stall producing 2+ `": keepalive\n\n"`).
- **No test for keepalive format being a valid SSE comment** (the leading `:` is what makes it a comment per the SSE spec; a regression that changed it to `data: keepalive` would still pass `body.contains(": keepalive\n\n")` because both strings contain the substring — there is no assertion that distinguishes the two).
- **No test for the `interval.tick().await` "consumed-tick" semantics**: the initial `interval.tick().await` at line 706 fires immediately on creation.

#### F4: JSON contract

**Where it lives**:

- [`classification_only_json(result)` at `src/main.rs:569-577`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L569) — emits `{"status":"classified","category":"...","model":"...","tier":"..."}`. `tier` is `format!("{:?}", classification.tier)` (line 574), producing strings like `"Regex"`, `"FewShot"`, `"LLM"`, `"Fallback"`.
- [`upstream_error_json(status, message)` at `src/main.rs:560-567`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L560) — emits `{"error":"upstream_error","status":<u16>,"message":"<str>"}`.
- [`json_response(status, body)` at `src/main.rs:550-558`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L550) — sets `Content-Type: application/json`.
- 4 classification-only early returns in `completion_handler` at [`src/main.rs:854, 875, 887, 892`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L854).
- Error returns at [`src/main.rs:809, 819, 900, 910, 925`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L809).
- `classify_and_log` builds its own `serde_json::json!` at [`src/main.rs:531-537`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L531) (slightly different ordering, same keys).

**Tests that anchor F4**: **No test parses the JSON.** All assertions
use `body.contains(...)` or `body.starts_with(...)`. Confirmed by
`awk 'NR>=1100 && NR<=3200' src/main.rs | grep -E "from_str|Value::|parse"`
returning zero matches.

Tests that touch the JSON contract via substring match:

| Test | File:line | What it asserts | Anchors |
|---|---|---|---|
| `test_completion_handler_returns_classification_json` | `src/main.rs:1412` | contains `"category":"SYNTAX_FIX"`, `"status":"classified"`, `"tier":"Regex"` | `classification_only_json` keys |
| `test_classify_handler_returns_classification_json` | `src/main.rs:1446` | contains `"category":"SYNTAX_FIX"`, `"model":"sf-model"`, `"status":"classified"`, `"tier":"Regex"` | `classify_and_log` JSON |
| `test_completion_does_not_include_enriched_fields` | `src/main.rs:1576` | contains `"category":"SYNTAX_FIX"`; does NOT contain `"provider_type"`, `"endpoint"`, `"api_key"` | Negative contract |
| `test_completion_no_enriched_fields_with_missing_env` | `src/main.rs:1619` | does NOT contain `"api_key"` | Negative contract |
| `test_classify_no_enriched_fields` | `src/main.rs:1648` | does NOT contain `"provider_type"`, `"api_key"` | Negative contract |
| `test_upstream_unreachable_returns_502` | `src/main.rs:2459` | contains `"error":"upstream_error"` | `upstream_error_json` error key |
| `test_max_upstream_body_bytes_truncation` | `src/main.rs:1485` | contains `"upstream response too large"` | `upstream_error_json` message |
| `test_streaming_degradation_no_client` | `src/main.rs:2800` | contains `"status":"classified"` | Classification JSON in degradation path |

**Test gaps for F4**:
- **No test parses JSON and asserts on key count, key types, or exact shape.** A `serde_json::from_str::<Value>` and `Value::Object` length check would catch a regression that added or removed a field.
- **No test asserts `status` is an integer in `upstream_error_json`**. The substring `body.contains(r#""error":"upstream_error""#)` does not distinguish `status:502` from `status:"502"`.
- **No test for the `message` field of `upstream_error_json`**.
- **No test for the `tier` field values** across all tiers (`Regex`, `FewShot`, `LLM`, `Fallback`). Only `Regex` is exercised.
- **No test for `Content-Type: application/json` on the classification JSON path** (only on the streaming SSE path via `test_streaming_handler_returns_sse_content_type`).
- **No test for `json_response` Content-Type on the error path**.
- **No test that the 4 distinct classification-only early-return sites** (lines 854, 875, 887, 892) are all reached — the existing test exercises only the line 854-856 path (X-Cerebrum-Category not found).

### The 46-test count — verified and categorized

`grep -nE "#\[(tokio::)?test\]" src/main.rs | wc -l` = **46**. The
test plan's claim is accurate (44 in `mod tests` at line 1156-3000,
2 in `mod slow_tests` at line 3003-3185).

**Test counts per handler** (per `grep -nE "fn (test|completion|classify|feedback|dashboard|..." src/main.rs`):

| Handler / surface | Count | Anchors F1-F4? |
|---|---|---|
| `completion_handler` (chat/completions, including SSE) | **20** | Yes — 10 of these anchor F1-F4 directly |
| `classify_handler` (classify) | 2 | Yes — `classification_only_json` keys via substring |
| `feedback_handler` | 2 | No |
| `dashboard` (index + inferences + latency + savings pages) | 17 | No — Risk #4 (Phase 3) |
| `health` | 1 | No |
| `routes_auth` (proxy bearer-token) | 1 | No — Risk #7 (Phase 3) |
| `ClassifierChain` (unit, no HTTP) | 1 | Yes — `test_chain_with_regex_and_fewshot` (Risk #1) |
| Persistence (DB schema) | 2 | Yes — `persistence_integration_sse_streaming_*` (F1 indirectly) |
| `graceful_shutdown` (axum generic) | 1 | No |

**Bottom line**: 20/46 tests (43%) directly exercise
`completion_handler`. Of those, only ~10 anchor F1-F4 directly
(`1412`, `1446`, `1576`, `2459`, `1485`, `2639`, `2800`, `3047`,
`1842`, `1911`). The 46 count is misleading because:
- **17 are about dashboard pages** (Risk #4, Phase 3).
- **2 are about the feedback handler** (unrelated).
- **1 is the `health` route**.
- **4 are persistence integration** (2 of which — 1842, 1911 — touch
  SSE/completion; 2 are pure DB schema at 1762, 1787).
- **1 is `test_graceful_shutdown`** (generic axum).
- **1 is `test_chain_with_regex_and_fewshot`** (chain handoff, not a
  specific HTTP handler).

If we subtract the 4 persistence-integration tests that require
`DATABASE_URL` and skip otherwise, only **16 of the 46 tests run in
default CI without `DATABASE_URL` set** and exercise
`completion_handler`.

### Risk Response Guidance cross-check (test plan §2)

| Test plan recommendation | Status in current code | Gap |
|---|---|---|
| **Risk #1, cheapest layer**: "Integration test with three mock backends returning known confidence scores; assert routing decision matches" | **MISSING.** No 3-backend test exists. `test_chain_with_regex_and_fewshot` has 2 backends, no LLM. | Need 3-backend test with mock `reqwest::Client` to prove LLM is/isn't called. |
| **Risk #1, must challenge**: "Each backend works" ≠ "chain hands off" — 28 regex unit tests don't prove escalation | **CONFIRMED.** The 28 tests are 7+4 regex-classifier standalone + 5 stub-chain + others. None prove 3-backend escalation. | Plan must include 3-backend test. |
| **Risk #2, cheapest layer**: "Invariant assertions on log output (snippet does not contain full prompt)" | **MISSING.** No test asserts "snippet does not contain full prompt" or "snippet is ≤ 200 chars" via the HTTP path. | Plan must add HTTP-level snippet test (requires `persistence` not None in test_app). |
| **Risk #2, response body shape** | **WEAKLY COVERED.** All assertions are `body.contains(...)`, not `serde_json::from_str(...).get(...)`. | Plan must add JSON-parse assertions on F4. |
| **Risk #2, SSE chunk shape** | **WEAKLY COVERED.** Single test asserts 2 of 5 invariants of `handle_streaming_error`. Inline mid-stream error path at line 712-720 has zero tests. | Plan must add tests for 2KB cap, JSON escaping, content-type, multiple upstream status codes. |
| **Risk #2, slow_tests for keepalive timing** | **CONFIRMED.** `test_streaming_keepalive_injected` is in `mod slow_tests`, uses real delays (1500ms upstream stall, 1s keepalive), no fake clock. | Test plan correctly followed. May want to add "upstream completes before keepalive" and "upstream chunk during keepalive tick" tests. |

## Code References

### Risk #1 — chain handoff

- [`src/intent_classifier.rs:89-94`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L89) — `ClassificationTier` enum (3 variants: Regex | FewShot | Fallback)
- [`src/intent_classifier.rs:98-106`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L98) — `IntentClassify` trait
- [`src/intent_classifier.rs:134-167`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L134) — `ClassifierChain` (returns first non-Fallback; last result on all-Fallback)
- [`src/intent_classifier.rs:258-374`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L258) — `LLMClassifier` (returns `tier: Regex` on success at line 344)
- [`src/intent_classifier.rs:344`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L344) — **the LLM tier-equals-Regex gotcha**
- [`src/intent_classifier.rs:557-652`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L557) — `RegexClassifier::classify_internal` + `route_match` + `route_fallback`
- [`src/intent_classifier.rs:854-998`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/intent_classifier.rs#L854) — 5 stub-based chain tests
- [`src/fewshot_classifier.rs:312-365`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/fewshot_classifier.rs#L312) — `FewShotClassifier::classify`
- [`src/fewshot_classifier.rs:128-134`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/fewshot_classifier.rs#L128) — `effective_threshold_for` (cold-start 0.6, normal 0.4)
- [`src/main.rs:246-323`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L246) — chain construction (config-driven order)
- [`src/main.rs:860-864`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L860) — `completion_handler` chain invocation
- [`src/main.rs:1333-1409`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1333) — `test_chain_with_regex_and_fewshot` (only multi-backend test; no LLM)
- [`src/main.rs:594-610`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L594) — `build_upstream_request` uses `classification.model` and `classification.endpoint`

### Risk #2 — completion_handler F1-F4

- [`src/main.rs:458-490`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L458) — `log_classification` (calls `persistence::extract_snippet`)
- [`src/main.rs:495-559`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L495) — `classify_and_log`
- [`src/main.rs:550-558`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L550) — `json_response`
- [`src/main.rs:560-567`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L560) — `upstream_error_json`
- [`src/main.rs:569-577`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L569) — `classification_only_json`
- [`src/main.rs:688-747`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L688) — `handle_streaming_response` (keepalive at lines 701-732; mid-stream error at lines 712-720)
- [`src/main.rs:749-783`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L749) — `handle_streaming_error` (F2 — 5 invariants)
- [`src/main.rs:790-976`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L790) — `completion_handler`
- [`src/persistence.rs:1051-1078`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1051) — `extract_last_user_message`
- [`src/persistence.rs:1080-1088`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1080) — `extract_snippet` (F1; 200-char truncation; no PII redaction)
- [`src/persistence.rs:1235-1324`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/persistence.rs#L1235) — 8 snippet unit tests + 6 last-user-message unit tests
- [`src/main.rs:1213-1290`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1213) — `test_app` (line 1222) and `test_app_with_classifier` (line 1243) — both with `persistence: None`
- [`src/main.rs:1412-1443`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L1412) — `test_completion_handler_returns_classification_json`
- [`src/main.rs:2536-2572`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L2536) — `test_streaming_handler_returns_sse_content_type`
- [`src/main.rs:2639-2674`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L2639) — `test_streaming_handler_non_2xx_returns_sse_error_event` (the F2 test)
- [`src/main.rs:3003-3156`](https://github.com/pfrack/cerebrum/blob/1cc87bfe5e3ec96858e821a5dbdc07fc37a4cc4a/src/main.rs#L3003) — `mod slow_tests` block; `test_streaming_keepalive_injected` at 3047

## Architecture Insights

1. **The `ClassificationTier` enum is too coarse for chain observability.** With only `Regex | FewShot | Fallback`, the chain cannot distinguish "regex matched" from "LLM matched" via tier alone. This is a real architectural concern: a test asserting "the LLM was called" must use side-effect observation (mock `reqwest::Client`, count calls) — not tier inspection. The Phase 1 plan should either (a) accept this constraint and use side-effect tests, or (b) suggest adding a new `Llm` tier variant (out of scope for Phase 1 but worth flagging).

2. **The `last_result.unwrap_or_else(ClassificationResult::fallback)` contract is undocumented.** When all backends return Fallback, the chain returns the **last** backend's Fallback. If the chain is `[regex, fewshot, llm]` and all fail, the LLM's Fallback (with `category: "unknown", model: DEFAULT_MODEL, endpoint: String::new()`) becomes the final result — which then hits `completion_handler:898-904` (no endpoint configured → 502). This is a non-obvious behavior that the Phase 1 plan should document as part of the chain-handoff contract.

3. **The F1-F4 review markers were deliberately removed** (lessons.md:26-31). Current code has the functionality but lacks the F-markers. The Phase 1 plan cannot grep for F1/F2/F3/F4 in the source; it must verify invariants by reading the code (5 invariants for F2 alone). This is consistent with the lessons rule but makes the risk harder to anchor — any future contributor who touches `handle_streaming_error` has no in-code breadcrumb pointing to the 5 invariants.

4. **`handle_streaming_response` has two SSE error paths with different contracts.** `handle_streaming_error` (line 749) escapes `\\`, `"`, `\n`, `\r`. The inline `Some(Err(_e))` branch in `handle_streaming_response` (line 712-720) does **not** escape. A regression that removes escaping in `handle_streaming_error` would not be caught by the single existing F2 test, and the un-escaped branch is a separate risk surface that is currently invisible to tests.

5. **HTTP test harnesses intentionally omit persistence.** `test_app` and `test_app_with_classifier` set `persistence: None`, which means `log_classification` early-returns. This is a sensible design for fast unit-style HTTP tests, but it creates a structural blind spot: the snippet path is only exercised when `DATABASE_URL` is set, which is the CI skip case. The Phase 1 plan must add a new test harness variant (or use `build_app_with_persistence` at line 2206) to exercise the snippet path via HTTP.

6. **The chain backend order is config-driven, not hardcoded.** With default config and all three enabled, the order is `[Regex, FewShot, LLM]`. Misconfiguration (e.g., `order = ["llm", "regex", "fewshot"]`) would silently change escalation behavior. No test verifies that the order is enforced as `[regex, fewshot, llm]` or that the chain is built correctly from config. The Phase 1 plan could include a config-driven order test, but this is low-priority compared to the chain-escalation contract itself.

## Historical Context

- `context/foundation/lessons.md:12-17` — "Re-run review after a follow-up change touches the same handler." The rule documents that `completion_handler` review fixes (F1-F4) were lost across `f19fc07` (Dashboard rewrite) and `9fb9ce3` (SSE streaming proxy). The current code restores the F1-F4 functionality but without the F-markers.
- `context/foundation/lessons.md:19-23` — "Handle upstream error bodies without full buffering where possible." Points to `src/main.rs:347-361` (streaming error path) and `src/main.rs:436-449` (non-streaming path). The 2 KB cap in `handle_streaming_error` at line 750 is the implementation of this rule. The 10 MB cap in `handle_buffered_response` is the non-streaming equivalent. Risk #2's F2 invariant 1 (2 KB cap) is a direct descendant of this lesson.
- `context/foundation/lessons.md:26-31` — "Document guard points with self-describing comments, not review cross-references." This is why the F1-F4 markers are gone from the source. The Phase 1 plan should add the 5 F2 invariants as self-describing comments at the top of `handle_streaming_error` (currently the only docstring is at line 749).
- `context/foundation/lessons.md:33-38` — "Favor dynamic WHERE clause building over duplicated SQL branches." Points to `src/persistence.rs:135-224` (`fetch_inferences` method). This is a Phase 2 concern (Risk #3, #5) and not directly relevant to Phase 1.
- `context/foundation/lessons.md:40-45` — "Log operational failures before falling back." Relevant to `LLMClassifier` at `src/intent_classifier.rs:278-283` (empty API key) and `:305-321` (network errors). The LLM never panics; it logs and returns Fallback. This is good — but no test asserts "LLM failure does not block the response" or "LLM failure is logged at warn level".
- `context/foundation/lessons.md:47-52` — "Delete dead code rather than suppressing warnings." Not directly relevant to Phase 1.

No prior research documents exist for this change. This is the first
research artifact under `context/changes/testing-critical-path-regression-guards/`.

## Related Research

- None yet. This is the first research document in the project.
- Future research will be created as the rollout progresses (Phase 2 will research Risks #3, #5, #6; Phase 3 will research Risks #4, #7; Phase 4 will research cross-cutting CI concerns).

## Open Questions

These should be resolved by `/10x-plan` before implementation:

1. **Should the Phase 1 plan add a new `Llm` tier variant** to `ClassificationTier` so the chain can distinguish LLM-tier matches from regex-tier matches? (Out of scope: this changes the public type and would require a follow-up change; but if not added, the Phase 1 tests must use side-effect observation.)

2. **What is the recommended approach for proving "regex match short-circuits the chain"?** Options:
   - (a) Stub the `reqwest::Client` and count LLM calls (recommended — least invasive).
   - (b) Add a `call_count` field to `IntentClassify` (intrusive — changes the trait).
   - (c) Use the `last_result` return value of the chain (doesn't help — it only triggers on all-Fallback).

3. **What is the recommended approach for proving the cold-start → LLM escalation path?** The fewshot bootstrap data is loaded from `data/fewshot_bootstrap.yaml` (line 31 of `fewshot_classifier.rs`). The Phase 1 plan should decide whether to (a) use a corpus of real prompts with known cosine scores, (b) construct a synthetic fewshot classifier with a known score, or (c) skip the cold-start path and focus on the cold-start → no-escalation case.

4. **Should the Phase 1 plan add a new HTTP test harness variant** with `persistence: Some(...)` to exercise the snippet path via HTTP, or should it use the existing `build_app_with_persistence` at line 2206? The existing helper is used by the `persistence_integration_*` tests but they skip without `DATABASE_URL`. A new harness variant would let the snippet path run in default CI.

5. **For F2 (SSE error), should the inline `Some(Err(_e))` branch at `src/main.rs:712-720` be brought into alignment with the `handle_streaming_error` contract** (add `\\`, `"`, `\n`, `\r` escaping)? Or is the divergence intentional (since the inline branch fires mid-stream on a `reqwest` chunk error, not on a non-2xx response)? The Phase 1 plan should decide whether to add a test that locks in the current un-escaped behavior or refactor to share the escaping logic.

6. **For F3 (keepalive), should the plan add a test that asserts the keepalive payload is a valid SSE comment** (i.e., starts with `:`) vs a `data:` line? The current `body.contains(": keepalive\n\n")` assertion does not distinguish the two.

7. **For F4 (JSON contract), should the plan add `serde_json::from_str::<Value>` assertions to the existing substring-match tests** (a refactor, no new test infrastructure) or add new dedicated JSON-shape tests? The former is cheaper; the latter is more explicit.
