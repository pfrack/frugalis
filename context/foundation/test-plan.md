# Test Plan

> Phased test rollout for this project. Strategy is frozen at the top
> (§1–§5); cookbook patterns at the bottom (§6) fill in as phases ship.
> Read before writing any new test.
>
> Refresh: re-run `/10x-test-plan --refresh` when stale (see §8).
>
> Last updated: 2026-06-30

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

| # | Risk (failure scenario) | Impact | Likelihood | Source (evidence — not anchor) |
|---|---|---|---|---|
| 1 | Protocol translation corrupts message bodies across OpenAI/Anthropic/Codex boundaries — proxy returns 200 with valid-looking but wrong content, broken headers, or dropped cache_control | High | High | Interview Q1, Q3; hot-spot `src/proxy/` (7 file-touches/30d); roadmap S-15, S-16, S-18, S-21 |
| 2 | Classifier chain (regex→fewshot→LLM) silently degrades — threshold/config change routes all prompts to wrong tier, output quality craters, no alarm fires | High | Medium-High | Interview Q1; PRD FR-002; config.toml `classifiers.order`; hot-spot `src/classification/` |
| 3 | Chain-to-translation interaction gap — classifier picks a `RouteEntry` whose provider type has an untested or broken translation path; output silently garbage | High | Medium-High | Interview Q4; config.toml: 5 provider types across routing entries; hot-spot `src/proxy/handlers.rs` |
| 4 | Streaming emitter state-machine edge cases — malformed upstream SSE, mid-stream errors, empty deltas, broken tool_use JSON produce garbled output or hung connections | High | Medium | Interview Q1, Q3; roadmap S-16 risk note ("medium-high"); hot-spot `src/proxy/streaming.rs`, `src/proxy/responses_streaming.rs` |
| 5 | `log_inference` fails silently (DB unreachable / schema drift / pool exhausted) — proxy stays up, dashboard shows empty data, operator never knows | High | Medium | PRD NFR ("failures in async logging … do not block primary response delivery"); config.toml `persistence.backend = "memory"`; hot-spot `src/persistence/` (8 touches/30d) |
| 6 | Snippet extraction regresses — full prompt bodies containing PII (email, name, SSN, phone) leak into persisted records or tracing spans | High | Low–Medium | PRD NFR ("excludes full prompt bodies by default"); abuse lens: PII leakage; hot-spot `src/persistence/` |
| 7 | Auth constant-time compare regresses — cleanup change reverts `constant_time_eq_str` to `==` on any auth path (proxy bearer, dashboard basic, new endpoint) | High | Low | AGENTS.md mandate; `lessons.md` S-10 phase 7; abuse lens: timing side-channel |

### Risk Response Guidance

| Risk | What would prove protection | Must challenge | Context `/10x-research` must ground | Likely cheapest layer | Anti-pattern to avoid |
|---|---|---|---|---|---|
| #1 | Given a real-looking chat/messages/responses request routed through the proxy with a specific provider type, the translated body, headers, and SSE events match a known-good reference output | "Returns 200" ≠ "translation correct" — body can be intact while header passthrough is broken and cache_control is silently dropped | Each translation direction (OpenAI→Anthropic, Anthropic→OpenAI, Responses→Chat); which headers must pass through per direction; the reference output shape per provider type; where `httpmock` fixtures simulate upstream behavior | Integration/contract test with known input→output pairs via `test_app()` + `httpmock` | Asserting only HTTP status; testing only one translation direction; hardcoding expected output from the implementation |
| #2 | Given a prompt corpus with known expected routing categories, the full chain (regex→fewshot→LLM) routes each prompt to the correct model tier; changing chain order or threshold produces a detectable difference in routing decisions | "Category X returned" ≠ "routed to the correct model" — must assert on the final routing decision, not just the category label | Where chain construction happens; confidence thresholds per tier; how routing table maps categories to models; what `CountingClassifier` reveals about which tier fired | Integration test with mock backends (CountingClassifier pattern) + known-category prompts; assert routing decision + tier that fired | Testing each classifier in isolation; asserting "some category" without validating routing |
| #3 | Given the chain picks a routing decision for each of the 5 provider types (nvidia_nim, openai_compatible, anthropic, ollama, local), the full proxy handler translates and forwards correctly for all 5 | "One provider works" ≠ "all providers work" — translation paths differ per `provider_type`; auth header injection varies | Which provider types have distinct translation code paths; whether ollama/local bypass translation; auth header injection per provider type per `config.toml [[auth_provider]]` rules | Integration test exercising all 5 provider types through `test_app()` + `httpmock`; assert on translated body + header shape per type | Testing only the most common provider; hardcoding auth expectations |
| #4 | Given malformed upstream SSE (empty delta, broken JSON tool_use, mid-stream HTTP error), the streaming emitter produces clean error termination — not garbled output, not a hung connection, not an unterminated SSE stream | "Stream completes" ≠ "stream was correct" — must assert on SSE event sequence and error handling, not just TCP close | Each streaming emitter's state machine (anthropic→openai, openai→anthropic, responses→chat); the error-injection surface via `httpmock`; keepalive interval behavior | Streaming integration test feeding crafted malformed SSE chunks via `httpmock`; assert error handling + event sequence | Only happy-path streaming; asserting "stream ended" without checking event sequence correctness |
| #5 | When `log_inference` fails (unreachable backend, schema drift, pool exhausted), proxy response still completes, warn-level log is emitted, bounded semaphore releases; cross-backend: same input → identical record on memory/sqlite/postgres | "Non-blocking" can mean "non-blocking + silent" — silent failure is strictly worse than blocking loud failure | Where `log_inference` is spawned; whether spawn failures are observed; pool/semaphore saturation behavior; `testcontainers` wiring for postgres; snippet extraction exact rules | Integration + testcontainers: unreachable-backend test + cross-backend identity test | Asserting only "call returns Ok"; testing only the memory backend; treating "non-blocking" as "no error path needed" |
| #6 | When snippet extraction processes prompts containing adversarial PII (email, name, SSN, phone), output contains zero PII patterns; error/log/tracing variants never include the full prompt body | "snippet length < full prompt length" is satisfied by truncation alone — doesn't prove PII is actually redacted | The snippet extraction function's exact rules; where error messages format; whether tracing spans include the full prompt body; the existing snippet-path test locations | Property tests with PII corpus; assert zero PII in output; assert no full prompt in error/log | Asserting only on length; checking only one PII pattern; tautological oracle (assertion lifted from implementation) |
| #7 | All auth comparisons (proxy bearer token, dashboard basic auth, any new endpoint) use `constant_time_eq_str`; no auth path uses `==` on secret-derived strings | The constant-time rule was added by hand after a review — a future "convenience" revert to `==` is the realistic regression, not a new attack | All call sites of `constant_time_eq_str`; whether dashboard basic-auth uses it; whether any endpoint added since the last audit is gated | Unit test importing the function directly + grep-based CI guard that forbids `==` in auth comparison context | Testing only one token path (proves one comparison, not all); testing only the happy path |

## 3. Phased Rollout

Each row is a discrete rollout phase that will open its own change folder
via `/10x-new`. Status vocabulary (parser literals): `not started` → `change opened` → `researched` → `planned` → `implementing` → `complete`.

| # | Phase name | Goal (one line) | Risks covered | Test types | Status | Change folder |
|---|---|---|---|---|---|---|
| 1 | Proxy translation contract tests | Lock translation correctness for all 3 protocol crossings (OpenAI↔Anthropic bidirectional + Responses→Chat) with known-good reference outputs + streaming edge-case resilience | #1, #4 | integration (translation contract), streaming edge-case | planned | `testing-proxy-translation-contracts` |
| 2 | Classifier chain routing integrity | Prove the full chain→routing→translation path works across all 5 provider types; chain degradation is detectable without inspecting production traffic | #2, #3 | integration (chain-to-translation e2e with CountingClassifier + httpmock across provider types) | implementing | `classifier-chain-routing-integrity` |
| 3 | Persistence + snippet guardrails | Make async logging failure observable (not silent) + prove snippet extraction holds across all 3 backends and against adversarial PII inputs | #5, #6 | integration (unreachable backend), testcontainers cross-backend, property tests | not started | — |
| 4 | Auth + CI floor + cookbook | Lock constant-time compare invariant at every call site + wire CI gates + update §6 cookbook with patterns shipped in Phases 1–3 | #7 | unit (constant-time compare), grep-based CI guard, CI workflow wiring | not started | — |

## 4. Stack

The classic test base for this project. AI-native tools (if any) carry a
`checked:` date so future readers can see which lines need re-verification.
Recommendations in this section must be grounded in local manifests/configs
plus the MCP/tools actually exposed in the current session. If a useful docs
or search MCP such as Context7 or Exa.ai is not available, say that instead
of assuming access.

| Layer | Tool | Version | Notes |
|---|---|---|---|
| unit + integration | built-in `#[test]` / `#[tokio::test]` | n/a | Standard Rust test harness. Tests organized inline in `mod tests` and `mod slow_tests` per `AGENTS.md`. |
| HTTP mocking | `httpmock` | 0.7 | For mocking upstream LLM/provider endpoints. Listed under `[dev-dependencies]`. |
| serial env tests | `serial_test` | 3 | For tests that touch process-wide env vars (e.g. `PROXY_API_BEARER_TOKEN`). |
| integration containers | `testcontainers` | 0.27 | For spinning up real postgres backends in cross-backend tests (Phase 3). |
| e2e | none yet | n/a | No e2e layer wired. Integration via `test_app()` + axum `Request::oneshot` covers the proxy hot path. |
| Local dev / OTel | Docker Compose v2 | 2.20+ | `docker-compose.yml` with postgres + OTel collector profiles. |

**Stack grounding tools (current session):**
- Docs: Context7 MCP exposed (resolve-library-id + query-docs) — available for stack-sensitive test setup; checked: 2026-06-30
- Search: not available in current session
- Runtime/browser: not available in current session
- Provider/platform: not available in current session

Use docs MCPs for current framework/library APIs and setup details. Use
search MCPs to discover current status only, then prefer official docs
as the evidence. Do not use MCP docs/search to infer code failure anchors;
those belong in per-phase `/10x-research`.

## 5. Quality Gates

The full set of gates that must pass before a change reaches production.
"Required for §3 Phase <N>" means the gate is enforced once that rollout
phase lands; before that, the gate is `planned`.

| Gate | Where | Required? | Catches |
|---|---|---|---|
| lint + typecheck | local + CI | required (existing) | syntactic / type drift |
| unit + integration | local + CI | required (existing) | logic regressions |
| translation contract | local + CI | required after §3 Phase 1 | silent protocol translation corruption |
| chain-to-translation integrity | local + CI | required after §3 Phase 2 | chain degradation + provider-type routing gaps |
| `slow_tests` group | local only | required after §3 Phase 4 | keepalive timing, real-delay behaviors |
| persistence integration (`testcontainers`) | CI (compose-backed) | required after §3 Phase 3 | cross-backend drift, silent logging failure |
| snippet PII property tests | local + CI | required after §3 Phase 3 | PII leakage into persisted records |
| constant-time compare guard | local + CI (grep) | required after §3 Phase 4 | reversion of `constant_time_eq_str` to `==` |
| coverage threshold | CI on PR | required after §3 Phase 4 | silent loss of test coverage on critical paths |
| post-edit hook | not applicable | n/a | n/a — deterministic gateway |
| visual diff (deterministic) | not applicable | n/a | n/a — explicitly out of scope (§7) |
| multimodal visual review | not applicable | n/a | n/a — no end-user visual surface |
| pre-prod smoke | between merge + prod | required (existing) | environment-specific failures |
| PR CI workflow | CI on PR | required | catches lint+typecheck+test+slow+build+compose-services-up |

## 6. Cookbook Patterns

How to add new tests in this project. Each sub-section is filled in once
the relevant rollout phase ships; before that, the sub-section reads
"TBD — see §3 Phase <N>."

### 6.1 Adding a unit test

TBD — existing pattern (inline `mod tests`, `test_<unit>_<case>`) documented in AGENTS.md. This section will be updated with any new patterns shipped in Phases 1–4.

### 6.2 Adding an integration test

TBD — see §3 Phase 1 for translation contract test patterns; Phase 2 for chain-to-translation patterns; Phase 3 for persistence patterns.

### 6.3 Adding a streaming / SSE test

TBD — see §3 Phase 1 for malformed-SSE edge-case patterns.

### 6.4 Adding a property test

TBD — see §3 Phase 3 for PII-snippet extraction property-test patterns.

### 6.5 Adding a grep-based CI guard

TBD — see §3 Phase 4 for constant-time-compare guard pattern.

### 6.6 Per-rollout-phase notes

(Filled in as phases land.)

## 7. What We Deliberately Don't Test

Exclusions agreed during the rollout (Phase 2 interview, Q5). Future
contributors should respect these unless the underlying assumption changes.

- **Visual diff / snapshot on dashboard CSS and UI** — CSS and template structure change frequently; visual snapshots would break for cosmetic reasons. Use HTTP integration that asserts on response status, content-type, and presence of key template fragments, not on rendered pixels or full HTML strings. Re-evaluate if the dashboard gains a real end-user surface. (Source: Phase 2 interview Q5.)

## 8. Freshness Ledger

- Strategy (§1–§5) last reviewed: 2026-06-30
- Stack versions last verified: 2026-06-30
- AI-native tool references last verified: 2026-06-30 (none — see §4)

Refresh (`/10x-test-plan --refresh`) when:

- a new top-3 risk surfaces from the roadmap or archive,
- a recommended tool's `checked:` date is older than three months,
- the project's tech stack changes (new framework, new test runner),
- §7 negative-space no longer matches what the team believes.
