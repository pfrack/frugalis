# Test Plan

> Phased test rollout for this project. Strategy is frozen at the top
> (§1–§5); cookbook patterns at the bottom (§6) fill in as phases ship.
> Read before writing any new test.
>
> Refresh: re-run `/10x-test-plan --refresh` when stale (see §8).
>
> Last updated: 2026-06-13 (Phase 1 → change opened)

## 1. Strategy

Tests follow three non-negotiable principles for this project:

1. **Cost × signal.** The cheapest test that gives a real signal for the
   risk wins. Do not promote to e2e because e2e "feels safer." Do not put a
   vision model on top of a deterministic visual diff that already catches
   the regression.
2. **User concerns are first-class evidence.** Risks anchored in "<the
   team is worried about X, and the failure would surface somewhere in
   <area>>" carry the same weight as PRD lines or hot-spot data.
3. **Risks are scenarios, not code locations.** This plan documents *what
   could fail* and *why we believe it's likely* — drawn from documents,
   interview, and codebase *signal* (churn, structure, test base). It does
   NOT claim to know which line owns the failure. That knowledge is
   produced by `/10x-research` during each rollout phase. If the plan and
   research disagree about where the failure lives, research is the
   ground truth.

Hot-spot scope used for likelihood weighting: `src/`.

## 2. Risk Map

The top failure scenarios this project must protect against, ordered by
risk = impact × likelihood. Risks are failure scenarios in user / business
terms, not test names. The Source column cites the *evidence that surfaced
this risk* — never a specific file as "where the failure lives" (that is
research's job, see §1 principle #3).

| # | Risk (failure scenario)                                                                                                                                              | Impact | Likelihood | Source (evidence — not anchor)                                                                                                  |
|---|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------|------------|--------------------------------------------------------------------------------------------------------------------------------|
| 1 | Classifier chain (regex→fewshot→LLM) mis-hands-off: ambiguous prompt stays on regex tier, low confidence gets routed to a weak model, user gets garbage output       | High   | High       | Interview Q1, Q3, Q4; PRD FR-002; hot-spot dir `src/intent_classifier.rs` (12 commits/30d) + new `src/fewshot_classifier.rs`    |
| 2 | `completion_handler` regression loses review fixes F1–F4 (snippet extraction, SSE error path, keepalive, JSON contract) when a follow-up change rewrites the handler | High   | High       | Interview Q2; `lessons.md:12-17` (F1–F4 lost across dashboard rewrite + SSE proxy commits); hot-spot dir `src/main.rs` (47/30d)   |
| 3 | `log_inference` failure (DB down / schema drift / pool exhausted) blinds operator silently: proxy keeps responding, dashboard shows empty data, nobody notices           | High   | Medium     | Interview Q4; PRD NFR ("failures in async logging … do not block primary response delivery"); hot-spot `src/persistence.rs` (21/30d) |
| 4 | Dashboard rendering breaks silently: 4 routes + `dashboard_page!` macro have 0 tests; a template rename or nav change breaks the operator UI                         | Medium | High       | Interview Q4; PRD FR-006; hot-spot dir `src/dashboard.rs` (8 commits/30d, 0 tests)                                              |
| 5 | Persistence cross-backend drift: `memory` vs `sqlite` vs `postgres` produce different `InferenceRecord` rows or different snippet extraction on edge cases           | High   | Medium     | Interview Q4; PRD FR-005, NFR (testing); roadmap S-12 (in-memory DB fallback, proposed); dev-deps `testcontainers = 0.27`        |
| 6 | Prompt body leaks to DB / error logs: snippet extraction regresses, full prompt body (PII: email, name, SSN) lands in `inferences` table or `tracing` span          | High   | Low–Medium | PRD NFR ("excludes full prompt bodies by default"); PRD Guardrail; hot-spot `src/persistence.rs`; abuse lens: PII leakage        |
| 7 | Auth boundary regression: proxy bearer-token or dashboard basic-auth drops constant-time compare, goes back to `==`; or a new endpoint forgets to gate              | High   | Low        | PRD FR-001; `AGENTS.md` mandates `constant_time_eq_str`; lessons.md S-10 phase 7; hot-spot `src/auth.rs` (6/30d); abuse lens    |

### Risk Response Guidance

| Risk | What would prove protection | Must challenge | Context `/10x-research` must ground | Likely cheapest layer | Anti-pattern to avoid |
|------|-----------------------------|----------------|--------------------------------------|-----------------------|-----------------------|
| #1   | Given an ambiguous prompt, the chain escalates regex→fewshot→LLM and the final category drives routing to the right model | "Each backend works" ≠ "chain hands off" — the 28 regex unit tests don't prove escalation | Where the chain is constructed; the confidence threshold that triggers handoff; whether `fallback()` invokes the next backend or short-circuits | Integration test with three mock backends returning known confidence scores; assert routing decision matches | Testing each backend in isolation only; asserting "some category came back" without checking which tier fired |
| #2   | Given a future change rewrites `completion_handler`, the F1–F4 invariants persist (snippet extraction, streaming error path, keepalive, JSON contract) | 46 tests on `main.rs` may not all anchor F1–F4 — a passing test suite can still lose a guard | Which tests actually exercise `completion_handler`; which asserts correspond to F1–F4; whether keepalive uses real delays or fast mocks | Invariant assertions on log output (snippet does not contain full prompt), response body shape, SSE chunk shape; slow_tests for keepalive timing | Snapshotting `main.rs` at review time as "ongoing protection"; asserting "test passed once" |
| #3   | When `log_inference` fails (DB unreachable, schema drift, pool exhausted), the proxy response still completes; the failure is logged at warn level; the bounded semaphore doesn't deadlock | "Non-blocking" can mean "non-blocking + silent" — silent failure is worse than blocking loud failure | Where `log_inference` is spawned; whether spawn failures are observed; pool exhaustion behavior; the persistence semaphore on saturation | Integration test that points `log_inference` at an unreachable backend and asserts (a) response completes, (b) warn log emitted, (c) semaphore releases | Asserting only that the call returns `Ok`; treating "non-blocking" as "no error path needed" |
| #4   | Given a dashboard page route with valid basic auth, it returns 200 with the expected template fragments (page title, nav context, data fields); given unauthenticated, it returns 401 | 0 tests today means any macro change, template rename, or nav change can break rendering silently | The 4 route handlers' actual response shapes; the `dashboard_page!` macro's emitted struct; `nav_for("name")` behavior on unknown page | HTTP-level integration via `test_app()` harness; assert status, content-type, presence of key template fragments | Snapshotting HTML (brittle on CSS — explicitly Q5 out of scope); asserting only on template path string |
| #5   | When `log_inference` is called with the same input on `memory` / `sqlite` / `postgres`, the persisted `InferenceRecord` is identical; snippet extraction is the same | `memory` is the default backend and what most tests use — `sqlite` / `postgres` can drift invisibly until prod | The 3-tier backend config wiring; which tests actually exercise all three; how `testcontainers` is invoked; snippet extraction edge cases | `testcontainers`-backed integration for `postgres` + `sqlite`; unit tests on `memory` + snippet extraction | Asserting "log_inference works" once on `memory`; letting the other two drift silently |
| #6   | When `log_inference` is called with a prompt containing adversarial PII (email, name, SSN, phone), the persisted snippet contains none of the PII; error / log variants do not contain the full prompt | "snippet length < full prompt length" is satisfied by truncation alone — doesn't prove PII is actually redacted | The snippet extraction function's exact rules; where error messages are formatted; whether `tracing` spans include the full prompt body | Property tests on snippet extraction with a corpus of PII inputs; assert no PII in output and no full prompt in error/log variants | Asserting only on length; checking only one PII pattern; tautological oracle (assertion lifted from implementation) |
| #7   | Given a proxy request without the bearer token → 401; with wrong token → 401 (constant-time); with right token → passes; dashboard basic-auth behaves the same; **all** auth comparisons use `constant_time_eq_str` | The constant-time compare lesson was added by hand — a future "convenience" revert to `==` is the realistic regression | All call sites of `constant_time_eq_str`; whether the dashboard basic-auth path uses it; whether any new endpoint added since F-01 uses it | Direct unit test importing the function and asserting behavior on equal/unequal/prefix/different-length strings; grep-based check that no auth path uses `==` on secret strings | Single token in test (proves one comparison, not all); testing only the happy path |

## 3. Phased Rollout

Each row is a discrete rollout phase that will open its own change folder
via `/10x-new`. Status moves left-to-right through the values below; the
orchestrator updates Status as artifacts appear on disk.

| # | Phase name | Goal (one line) | Risks covered | Test types | Status | Change folder |
|---|------------|------------------|----------------|------------|--------|----------------|
| 1 | Critical-path regression guards | Defend Risk #1 + #2 at the cheapest layer; lock the chain-handoff contract and the F1–F4 invariants | #1, #2 | integration (chain escalation with mock backends), regression (invariant assertions on `completion_handler`) | change opened | `testing-critical-path-regression-guards` |
| 2 | Persistence + snippet guardrails | Make the NFR ("async logging failure does not block response") observable, and prove snippet extraction holds across all three backends + adversarial PII inputs | #3, #5, #6 | integration (`log_inference` against unreachable backend), testcontainers cross-backend, property tests on snippet extraction | not started | — |
| 3 | Dashboard + auth coverage | Close the 0-test gap on `src/dashboard.rs` (4 routes + macro) and prove the constant-time compare invariant holds at every call site | #4, #7 | HTTP integration (dashboard routes via `test_app()`), unit (constant-time compare), grep-based guard | not started | — |
| 4 | CI floor + cookbook | Wire `slow_tests` into a scheduled CI job, add a coverage-fail threshold, and update §6 cookbook with the patterns the rollout just shipped | cross-cutting | gates + cookbook (no new test code) | not started | — |

## 4. Stack

The classic test base for this project. AI-native tools (if any) carry a
`checked:` date so future readers can see which lines need re-verification.
Recommendations in this section must be grounded in local manifests/configs
plus the MCP/tools actually exposed in the current session. If a useful docs
or search MCP such as Context7 or Exa.ai is not available, say that instead
of assuming access.

| Layer                | Tool                | Version | Notes                                                                                            |
|----------------------|---------------------|---------|--------------------------------------------------------------------------------------------------|
| unit + integration   | built-in `#[test]` / `#[tokio::test]` | n/a     | Standard Rust test harness. Tests organized in `mod tests` and `mod slow_tests` per `AGENTS.md`. |
| HTTP mocking         | `httpmock`          | 0.7     | For mocking upstream LLM endpoints. Listed under `[dev-dependencies]`.                            |
| serial env tests     | `serial_test`       | 3       | For tests that touch process-wide env vars (e.g. `PROXY_API_BEARER_TOKEN`).                       |
| integration containers | `testcontainers` | 0.27    | For spinning up real `postgres` / `sqlite` backends in cross-backend tests (Phase 2).            |
| e2e                  | none yet            | n/a     | No e2e layer wired; integration via `test_app()` + axum `Request` covers proxy hot path.         |
| accessibility        | not applicable      | n/a     | Operator-only dashboard; no end-user UI surface.                                                |
| (optional) AI-native | not applicable      | n/a     | Deterministic gateway; no place for vision models. Cost × signal fails for an AI-native layer.   |

**Stack grounding tools (current session):**
- Docs: **Context7 MCP exposed** (resolve-library-id + query-docs) — available for stack-sensitive test setup verification; checked: 2026-06-13
- Search: not available in current session
- Runtime/browser: not available in current session
- Provider/platform: not available in current session

Use docs MCPs for current framework/library APIs and setup details. Use
search MCPs to discovery or current status only, then prefer official docs
as the evidence. Do not use MCP docs/search to infer code failure anchors;
those belong in per-phase `/10x-research`.

## 5. Quality Gates

The full set of gates that must pass before a change reaches production.
"Required for §3 Phase <N>" means the gate is enforced once that rollout
phase lands; before that, the gate is `planned`.

| Gate                          | Where             | Required?                       | Catches                                       |
|-------------------------------|-------------------|----------------------------------|-----------------------------------------------|
| lint + typecheck              | local + CI        | required                         | syntactic / type drift                        |
| unit + integration            | local + CI        | required (existing)              | logic regressions                             |
| `slow_tests` group            | local only        | required after §3 Phase 4        | keepalive timing, real-delay behaviors        |
| coverage threshold            | CI on PR          | required after §3 Phase 4        | silent loss of test coverage on critical paths |
| dashboard route integration   | local + CI        | required after §3 Phase 3        | silent dashboard rendering breakage           |
| constant-time compare guard   | local + CI (grep) | required after §3 Phase 3        | reversion of `constant_time_eq_str` to `==`   |
| post-edit hook                | not applicable    | n/a                              | n/a — deterministic gateway                   |
| visual diff (deterministic)   | not applicable    | n/a                              | n/a — explicitly out of scope (§7)            |
| multimodal visual review      | not applicable    | n/a                              | n/a — no end-user visual surface              |
| pre-prod smoke                | between merge + prod | required (existing)           | environment-specific failures                 |

## 6. Cookbook Patterns

How to add new tests in this project. Each sub-section is filled in once
the relevant rollout phase ships; before that, the sub-section reads
"TBD — see §3 Phase <N>."

### 6.1 Adding a unit test

- **Location**: same file as the unit under test, inside a `#[cfg(test)] mod tests` block (per `AGENTS.md` rule).
- **Naming**: `test_<unit>_<case>`.
- **Reference test**: TBD — see §3 Phase 1.
- **Run locally**: `cargo test <test_name>` (fast) or `cargo test slow_tests` (real-delay).

### 6.2 Adding an integration test

- **Location**: same file as the unit under test, inside `#[cfg(test)] mod tests`; use the shared `test_app()` harness (per `AGENTS.md`).
- **Mocking policy**: mock only at the HTTP edge via `httpmock`. Never mock internal modules. For cross-backend DB work, use `testcontainers` (Phase 2).
- **Reference test**: TBD — see §3 Phase 1 (chain escalation) and §3 Phase 2 (cross-backend log_inference).
- **Run locally**: `cargo test <test_name>`.

### 6.3 Adding an e2e test

- TBD — see §3 Phase 1. (No e2e layer exists today; `test_app()` + axum `Request` is the cheapest layer that exercises the full proxy stack including middleware.)

### 6.4 Adding a test for a new API endpoint

- TBD — see §3 Phase 1 (chain handoff on `/v1/chat/completions`) and §3 Phase 3 (dashboard routes).

### 6.5 Adding a test for a new classifier backend

- TBD — see §3 Phase 1. The `IntentClassify` trait has three live implementations (`RegexClassifier`, `FewshotClassifier`, `LLMClassifier`); the chain-handoff test in Phase 1 is the canonical example.

### 6.6 Per-rollout-phase notes

(Filled in as phases land.)

## 7. What We Deliberately Don't Test

Exclusions agreed during the rollout (Phase 2 interview, Q5). Future
contributors should respect these unless the underlying assumption changes.

- **Visual diff / snapshot on dashboard CSS** (572 lines, Q5) — CSS class names change frequently and snapshots would fail for cosmetic reasons. Use HTTP integration that asserts on response status, content-type, and presence of key template fragments, not on rendered pixels or full HTML strings. Re-evaluate if the dashboard gains a real end-user surface or a visual-regression budget is explicitly approved. (Source: Phase 2 interview Q5.)

## 8. Freshness Ledger

- Strategy (§1–§5) last reviewed: 2026-06-13
- Stack versions last verified: 2026-06-13
- AI-native tool references last verified: 2026-06-13 (none — see §4)

Refresh (`/10x-test-plan --refresh`) when:

- a new top-3 risk surfaces from the roadmap or archive,
- a recommended tool's `checked:` date is older than three months,
- the project's tech stack changes (new framework, new test runner),
- §7 negative-space no longer matches what the team believes.
