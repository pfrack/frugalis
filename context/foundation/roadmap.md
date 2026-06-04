*** Begin Updated File ***
---
project: cerebrum
version: 1
status: draft
created: 2026-05-26
updated: 2026-06-01
prd_version: 1
main_goal: speed
top_blocker: time
---

# Roadmap: Cerebrum

> Derived from `context/foundation/prd.md` (v1) + auto-researched codebase baseline.
> Edit-in-place; archive when superseded.
> Slices below are listed in dependency order. The "At a glance" table is the index.

## Vision recap

Autonomous agents currently forward prompts to expensive models without intent-aware triage, creating avoidable spend and operational friction. A lightweight intent-aware gateway—combining fast regex/keyword classification, model routing, and a native dashboard—solves this by exposing routing outcomes so the operator can tune efficiency.

## North star

**S-01e: Intent-aware proxy routing** — Smallest end-to-end proof: proxy accepts a request, classifies intent (regex first, cheap-model fallback for ambiguous), routes to an appropriate upstream model, and streams response back via SSE. This validates the core hypothesis: intent-aware triage works and is fast enough for production use.

> The north star is the one slice whose successful delivery proves the product works. Everything else only matters if this works. Here, that's the proxy flow with end-to-end routing.

## At a glance

| ID | Change ID | Outcome (user can …) | Prerequisites | PRD refs | Status |
|---|---|---|---|---|---|
| F-01 | auth-scaffold-access-keys | (foundation) Access key/token validation + operator dashboard auth gates are in place | — | FR-001, Access Control | done |
| F-02 | data-persistence-async-logging | (foundation) Async inference logging pipeline connected to Supabase PostgreSQL | — | FR-005, NFR (non-blocking logs) | impl_reviewed |
| F-03 | dashboard-template-scaffold | (foundation) Askama HTML templating and server-side rendering wired into Axum | — | FR-006, Dashboard | implemented |
| F-04 | critical-logging | (foundation) Add structured logging to all critical paths and make logging level configurable via RUST_LOG | F-01, F-02, F-03 | FR-005, Observability | proposed |
| S-01a | classify-endpoint | classify prompts into intent categories using regex/keyword rules and cheap-model fallback | F-01, F-02 | FR-002 | implemented |
| S-01b | reqwest-upstream-routing | route classified requests to appropriate upstream models via reqwest | S-01a | FR-003 | impl_reviewed |
| S-01c | provider-agnostic-config | generalize routing configuration to support multiple providers with different auth schemes | S-01b | FR-003 | implemented |
| S-01d | sse-streaming-proxy | stream upstream responses back to clients via SSE | S-01c | FR-004 | impl_reviewed |
| S-01e | proxy-intent-routing | end-to-end proxy: receive chat completions, coordinate classification, routing, and streaming | S-01a, S-01b, S-01c, S-01d | US-01, FR-001 | implemented |
| S-02 | inference-log-inspection | view recent inference records in the dashboard with prompt snippet, assigned category, upstream model, and duration | F-02, F-03, S-01e | FR-006 | implemented |
| S-03 | per-intent-latency-summary | view a latency summary grouped by intent category in the dashboard | F-03, S-02 | Secondary Success Criterion | implemented |
| S-04 | cost-savings-metric | view an estimated cost-savings indicator based on logged inferences | S-02 | FR-007 (nice-to-have) | implemented |
| S-05 | dashboard-mvp-rewrite | comprehensive dashboard rewrite: dedicated module, navigation, CSS styling, and integrated UI | F-03, S-02, S-03, S-04 | FR-006, FR-007, Secondary Success Criterion | implemented |
| S-06 | dashboard-logs-page | dedicated logs page showing detailed inference logs and trace information | F-04, F-02, F-03, S-01e | FR-006, Observability | proposed |

## Streams

Navigation aid — groups items that share a Prerequisites chain. Canonical ordering still lives in the dependency graph below; this table is the proposed reading order across parallel tracks.

| Stream | Theme | Chain | Note |
|---|---|---|---|
| A | Proxy core | `F-01` → `F-02` → `S-01a` → `S-01b` → `S-01c` → `S-01d` → `S-01e` | The validating path: gate access, enable logging, classification, routing, provider config, streaming, then end-to-end integration. |
| B | Dashboard | `F-03` → `S-02` → `S-03` → `S-04` → `S-05` | Observability: incremental features (S-02/S-03/S-04) followed by consolidation into polished MVP UI (S-05). S-02 depends on S-01e (proxy must be logging inferences). |
| C | Metrics | — | All metrics features (S-04) integrated into dashboard stream (B). |
| D | Critical Logging | `F-04` → `S-06` | Ensures all critical paths have observability logs and a dedicated UI page. |

## Baseline

What's already in place in the codebase as of 2026-05-26 (auto-researched + confirmed).
Foundations below assume these are present and do NOT re-scaffold them.

- **Backend/API:** Present — Axum router with `/health` endpoint; no additional routes wired.
- **Data:** Absent — No DB drivers or schema tooling; PostgreSQL integration is greenfield.
- **Auth:** Absent — No middleware or token handling; access control is greenfield.
- **Frontend:** Absent — No HTML rendering framework; Askama templates are greenfield.
- **Deploy/infra:** Partial — `render.yaml` + GitHub Actions deployment workflow in place; Dockerfile is absent.
- **Observability:** Partial — `RUST_LOG` env var configured; application metrics / structured logging absent.

## Foundations

### F-01: Auth scaffold — access keys & operator gate

- **Outcome:** (foundation) Access key/token validation middleware + basic HTTP auth for dashboard are in place; proxy routes require a valid key header; dashboard requires operator credentials.
- **Change ID:** `auth-scaffold-access-keys`
- **PRD refs:** FR-001 (client access gated), Access Control section, NFR (private dashboard views)
- **Unlocks:** S-01 (proxy can't emit unprotected responses), S-02 (dashboard must be private)
- **Prerequisites:** —
- **Parallel with:** F-02, F-03 (independent scaffolding work)
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Simplest foundation to ship first; token-validation middleware is table-stakes before any proxy endpoint is exposed. Implementation is bounded (flat single-operator model, no role-based access control).
- **Status:** done

### F-02: Data persistence — async inference logging pipeline

- **Outcome:** (foundation) Supabase PostgreSQL connection, schema for inference records (category, upstream model, duration, timestamp, prompt snippet), and async logging task are in place; proxy can write inference metadata non-blockingly after response streaming completes.
- **Change ID:** `data-persistence-async-logging`
- **PRD refs:** FR-005 (async logging), NFR (non-blocking side paths), guardrail (no full prompt body persisted)
- **Unlocks:** S-01 (proxy can emit inference records), S-02 (dashboard queries inference table), S-03 (latency summaries derive from inference data)
- **Prerequisites:** —
- **Parallel with:** F-01, F-03 (independent)
- **Blockers:** Supabase account setup + free-tier PostgreSQL provisioning (external, but quick; ~15 min).
- **Unknowns:** —
- **Risk:** Async logging is a secondary path; failures here must not stall proxy response streaming (guardrail-level). Implementation uses Tokio spawn or similar to ensure non-blocking semantics. Schema must include prompt-minimization / snippet extraction to meet privacy guardrail.
- **Status:** proposed

### F-03: Dashboard template scaffold — Askama + server-side rendering

- **Outcome:** (foundation) Askama HTML templates wired into Axum routing; `/dashboard` endpoint renders template with static placeholder content; basic HTTP basic-auth gate wraps the endpoint.
- **Change ID:** `dashboard-template-scaffold`
- **PRD refs:** FR-006 (dashboard views), dashboard NFR (private operator access)
- **Unlocks:** S-02 (dashboard queries and displays inference records), S-03 (adds aggregation to the same template)
- **Prerequisites:** —
- **Parallel with:** F-01, F-02 (independent)
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Server-side templating avoids a separate SPA framework (per tech-stack preference). Scaffolding is the setup cost; incremental template work (adding new fields, new sections) happens in S-02 / S-03. No frontend build pipeline, no Node.js, keeps deployment footprint minimal.
- **Status:** proposed

### F-04: Critical logging

- **Outcome:** (foundation) Add structured logging statements to all critical code paths and support configurable logging level via RUST_LOG: authentication middleware, proxy classification, routing, streaming, and error handling. Uses `tracing` crate with appropriate levels (info, error) and includes request identifiers for correlation.
- **Change ID:** `critical-logging`
- **PRD refs:** FR-005, Observability
- **Unlocks:** S-06 (dashboard logs page) and improves debugging of all slices.
- **Prerequisites:** F-01, F-02, F-03
- **Parallel with:** —
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Minimal runtime overhead; logs are emitted asynchronously via `tokio::spawn` to avoid blocking request handling.
- **Status:** proposed

## Slices

### S-01a: Intent classification endpoint

- **Outcome:** API endpoint can classify incoming prompts into intent categories using regex/keyword rules, with a cheap-model fallback for ambiguous cases.
- **Change ID:** `classify-endpoint`
- **PRD refs:** FR-002
- **Prerequisites:** F-01 (access key validation), F-02 (async logging)
- **Parallel with:** — (first in proxy chain)
- **Blockers:** —
- **Unknowns:**
  - How does regex/keyword classification map to intent categories? (Intent categories: COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL per shape-notes.) Owner: you. Block: yes.
  - Which cheap model to use for fallback classification? Owner: you. Block: yes.
- **Risk:** Classification rules (regex + fallback) are the MVP cheapest path; if fallback cost becomes too high in production, that's a post-MVP tuning point. Implementation is self-contained and testable.
- **Status:** implemented

### S-01b: Upstream routing with reqwest

- **Outcome:** Gateway can route classified requests to appropriate upstream models via reqwest, sending the chat completion request and receiving responses.
- **Change ID:** `reqwest-upstream-routing`
- **PRD refs:** FR-003
- **Prerequisites:** S-01a (intent classification)
- **Parallel with:** — (depends on S-01a, precedes S-01c)
- **Blockers:** —
- **Unknowns:**
  - Which upstream models are available on chosen provider (OpenRouter?) and what are their cost/latency profiles? Owner: you. Block: yes.
  - Does reqwest streaming support align with SSE requirements? Owner: implementation research. Block: no.
- **Risk:** Upstream connectivity is a critical path; failures here should have clear error responses. Model choice impacts both cost and routing logic.
- **Status:** impl_reviewed

### S-01c: Provider-agnostic configuration

- **Outcome:** Routing configuration generalized to support multiple providers with different auth schemes; each intent category can route to a different provider with its own API key configuration.
- **Change ID:** `provider-agnostic-config`
- **PRD refs:** FR-003
- **Prerequisites:** S-01b (basic upstream routing)
- **Parallel with:** — (depends on S-01b, precedes S-01d)
- **Blockers:** —
- **Unknowns:**
  - Should the configuration be a single-level routing.toml or two-level (providers + routing)? Owner: implementation. Block: no.
  - How to handle provider-specific body transformations (e.g., Anthropic vs OpenAI format)? Owner: implementation. Block: no.
- **Risk:** Configuration complexity must remain manageable for MVP. Provider abstraction adds indirection but enables flexibility. Non-breaking changes are possible.
- **Status:** implemented

### S-01d: SSE streaming proxy

- **Outcome:** Gateway can stream upstream responses to clients via Server-Sent Events (SSE), maintaining connection and handling backpressure.
- **Change ID:** `sse-streaming-proxy`
- **PRD refs:** FR-004
- **Prerequisites:** S-01c (provider-agnostic routing)
- **Parallel with:** — (depends on S-01c, precedes S-01e)
- **Blockers:** —
- **Unknowns:**
  - Does SSE streaming require application-level keepalive pings, or is HTTP/1.1 transfer-encoding: chunked sufficient? Owner: implementation research. Block: no.
  - How to handle upstream errors during streaming? Owner: implementation. Block: no.
- **Risk:** Streaming edge cases are real but manageable (keepalive pings are a one-liner if needed). SSE is well-supported in Axum and reqwest.
- **Status:** impl_reviewed

### S-01e: End-to-end proxy integration

- **Outcome:** user can send an OpenAI-compatible chat completion request to the gateway, which orchestrates classification, routing, and streaming, returning the full streamed response via SSE.
- **Change ID:** `proxy-intent-routing`
- **PRD refs:** US-01, FR-001
- **Prerequisites:** S-01a (classification), S-01b (routing), S-01c (provider config), S-01d (streaming)
- **Parallel with:** — (north-star integration slice; S-02 / S-03 depend on this)
- **Blockers:** —
- **Unknowns:** — (all unknowns resolved in previous phases)
- **Risk:** The core product slice; all downstream work depends on this shipping. Integration complexity is bounded since components were built to compose. This is the final validation that the pieces work together.
- **Status:** implemented

### S-02: Inference log inspection

- **Outcome:** user can view a table in the dashboard showing recent inference records, each row displaying: prompt snippet (minimized, no full body), assigned intent category, upstream model selected, and request duration.
- **Change ID:** `inference-log-inspection`
- **PRD refs:** FR-006 (dashboard table of inferences)
- **Prerequisites:** F-02 (data in PostgreSQL), F-03 (template rendering), S-01e (inferences are being logged by the end-to-end proxy)
- **Parallel with:** S-03 (both query the same table; S-03 adds aggregation)
- **Blockers:** —
- **Unknowns:**
  - How many recent inferences should the dashboard show by default? (pagination? date range? limit?) Owner: you. Block: no (default: last 100 is reasonable).
  - How should prompt snippets be truncated/minimized for display? Owner: you. Block: no (implementation detail; default: first 200 chars is safe).
- **Risk:** Second slice; depends on S-01e generating data. Template rendering is straightforward (Askama is mature). Query performance should be fine for "recent 100 rows" on a small free-tier PostgreSQL. If this grows to high volume, indexing on timestamp is a future optimization.
- **Status:** implemented

### S-03: Per-intent latency summary

- **Outcome:** user can view a summary (table or chart) in the dashboard showing average and p99 latency grouped by intent category, derived from recent inference records.
- **Change ID:** `per-intent-latency-summary`
- **PRD refs:** Secondary Success Criterion (dashboard shows per-intent latency summary)
- **Prerequisites:** F-03 (dashboard rendering), S-02 (log inspection working)
- **Parallel with:** — (depends on S-02 queries)
- **Blockers:** —
- **Unknowns:**
  - Should the summary be computed in the database (SQL GROUP BY + aggregation) or in Rust (query all rows, compute in-memory)? Owner: implementation. Block: no (SQL is simpler).
  - Time window for the summary? (last hour? last 24h? configurable?) Owner: you. Block: no (default: last 24h is reasonable).
- **Risk:** Third-priority slice after core proxy and basic log view. Aggregation adds minimal complexity. If compute time becomes noticeable, move aggregation to a background job; but that's post-MVP tuning.
- **Status:** implemented

### S-04: Cost-savings metric

- **Outcome:** user can view an estimated cost-savings indicator in the dashboard showing the inferred savings from using routed models vs. sending all prompts to an expensive baseline model.
- **Change ID:** `cost-savings-metric`
- **PRD refs:** FR-007 (nice-to-have)
- **Prerequisites:** S-02 (log inspection), inference cost model (which models cost what)
- **Parallel with:** — (after S-02)
- **Blockers:** —
- **Unknowns:** — (resolved)
- **Risk:** Nice-to-have; not critical for MVP. Baseline model configurable via `BASELINE_MODEL` env var and classification cost model tracked in the inference log, enabling a directional savings estimate without needing per-model cost tables.
- **Status:** implemented

### S-05: Dashboard MVP rewrite

- **Outcome:** The dashboard is transformed from a basic POC scaffold into a full-featured, production-ready observability UI. Includes: dedicated `src/dashboard.rs` module with 4 route handlers, automatic sidebar navigation with icons and active states, modern CSS styling (dark/light theme toggle), and integrated display of all observability data (inference logs, latency summaries, cost savings) on a cohesive homepage.
- **Change ID:** `dashboard-mvp-rewrite`
- **PRD refs:** FR-006 (dashboard views), FR-007 (cost-savings metric), Secondary Success Criterion (latency summary)
- **Prerequisites:** F-03 (template scaffolding), S-02 (inference logs), S-03 (latency summaries), S-04 (cost-savings metrics)
- **Parallel with:** — (consolidation slice that depends on prior dashboard features)
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Low risk polish/consolidation effort that significantly improves operator UX without changing backend semantics. The rewrite is architecturally clean: separates concerns into a dedicated module, uses a macro for template structs, and provides consistent error handling.
- **Status:** implemented

### S-06: Dashboard logs page

- **Outcome:** Dedicated dashboard page presenting detailed structured logs (including request IDs, timestamps, severity) and allowing runtime adjustment of logging level, enabling operators to trace requests end-to-end.
- **Change ID:** `dashboard-logs-page`
- **PRD refs:** FR-006, Observability
- **Prerequisites:** F-04 (critical logging), F-02 (logging persistence), F-03 (template scaffolding), S-01e (proxy operations generate logs)
- **Parallel with:** —
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Provides deep observability; minimal UI complexity as it reuses existing table components.
- **Status:** proposed

## Backlog Handoff

| Roadmap ID | Change ID | Suggested issue title | Ready for `/10x-plan` | Notes |
|---|---|---|---|---|
| F-01 | auth-scaffold-access-keys | Auth: Access key validation middleware + operator dashboard gate | yes | Simplest foundation; no blockers. Plan first to unblock proxy endpoint exposure. |
| F-02 | data-persistence-async-logging | Data: Supabase PostgreSQL + async inference logging | yes | Quick setup (Supabase free tier); enables proxy observability. Plan in parallel with F-01 if team size allows. |
| F-03 | dashboard-template-scaffold | Dashboard: Askama template scaffold + /dashboard route | yes | Pure scaffolding; no external dependencies. Plan in parallel with F-01 / F-02. |
| F-04 | critical-logging | Foundation: Add structured logging to all critical paths | yes | Improves observability and supports logs UI. |
| S-01a | classify-endpoint | Proxy: Intent classification endpoint (regex + cheap-model fallback) | no | Unblock F-01 and F-02 first. Status check: already implemented. 2 blocking unknowns originally (intent classification rules, cheap model choice) - these were resolved during implementation. |
| S-01b | reqwest-upstream-routing | Proxy: Upstream model routing with reqwest | no | Unblock S-01a first. Status: impl_reviewed (implemented but underwent review). 1 blocking unknown originally (upstream model choices) - resolved. |
| S-01c | provider-agnostic-config | Proxy: Provider-agnostic routing configuration for multiple providers | no | Unblock S-01b first. Status: implemented. |
| S-01d | sse-streaming-proxy | Proxy: SSE streaming response handler | no | Unblock S-01c first. Status: impl_reviewed. |
| S-01e | proxy-intent-routing | Proxy: End-to-end intent-aware routing integration | no | Unblock all prior S-01* slices. Status: implemented (north star achieved). |
| S-02 | inference-log-inspection | Dashboard: Recent inferences table (category, model, duration) | no | Already implemented. |
| S-03 | per-intent-latency-summary | Dashboard: Per-intent latency summary | no | Already implemented. |
| S-04 | cost-savings-metric | Dashboard: Estimated cost-savings metric (nice-to-have) | no | Already implemented; baseline model configurable via `BASELINE_MODEL` env var and classification costs accounted. |
| S-05 | dashboard-mvp-rewrite | Dashboard: Comprehensive UI rewrite with navigation, CSS, and consolidated observability views | no | Already implemented; transforms POC scaffold into production-ready dashboard with sidebar, theming, and integrated homepage. |
| S-06 | dashboard-logs-page | Dashboard: Dedicated page for detailed logs and traceability | no | Proposed; depends on critical logging foundation. |

## Open Roadmap Questions

1. **Intent classification categories and regex/keyword rules** — The PRD names four intents (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) but does not provide the actual regex patterns or keyword lists. Define the rules so S-01a planning can proceed. Owner: you. Block: S-01a.
2. **Cheap fallback model for classification** — Which inexpensive model (OpenAI GPT-4o Mini, Anthropic Haiku, etc.) should be used for ambiguous prompts that don't match regex patterns? Owner: you. Block: S-01a.
3. **Upstream model choices and cost/latency profiles** — Which models will the gateway route to (e.g., Claude 3.5 Sonnet for COMPLEX_REASONING, DeepSeek Flash for FILE_READING per shape-notes)? What are the cost and latency tradeoffs? This informs intent-to-model mapping in S-01b. Owner: you. Block: S-01b, S-04.

## Parked

All roadmap items are active or completed; no currently parked items.

## Done

- **F-01: (foundation) Access key/token validation middleware + basic HTTP auth for dashboard are in place; proxy routes require a valid key header; dashboard requires operator credentials.** — Archived 2026-06-01 → `context/archive/2026-05-26-auth-scaffold-access-keys/`. Lesson: —.

---

## Sequencing rationale

**Why this order?**
The 3-week MVP budget under a 6-week hard deadline makes calendar time the #1 blocker. This roadmap sequences must-haves in dependency order and parks nice-to-haves.

1. **Foundations (F-01, F-02, F-03) first, run in parallel** — All three are independent scaffolding tasks (auth, data, template setup). No blockers. Running them in parallel uses available capacity efficiently and unblocks the first proxy slice. Estimated 1 week total wall-clock time if executed in parallel.
2. **Proxy chain (S-01a → S-01b → S-01c → S-01d → S-01e) next** — The core product hypothesis is validated through staged delivery: classification first, then routing, then provider config, then streaming, finally end-to-end integration. Each phase has well-defined outputs and minimal cross-dependencies. S-01a has 2 blocking unknowns (classification rules, cheap fallback model), S-01b has 1 blocking unknown (upstream model choices). Resolve these as they arise. Estimated 2–3 weeks total.
3. **Dashboard observability features (S-02, S-03, S-04) follow** — Depend on S-01e having data to display. They can run in parallel after S-01e lands. Non-blocking slices; estimated 4-5 days combined.
4. **Dashboard consolidation (S-05)** — Polish and UI integration to transform the incremental dashboard features into a cohesive, production-ready experience. Depends on S-02, S-03, S-04. Small effort (2-3 days) but significantly improves operator UX.
5. **Critical logging foundation (F-04) and dedicated logs UI (S-06)** — Ensure end-to-end observability across all slices. Can be done in parallel with later observability features and completed before final polish.

**Parallel tracks:** F-01/F-02/F-03 can run in parallel. S-02, S-03, S-04 can run in parallel after S-01e. S-05 follows them sequentially. The proxy chain itself is strictly sequential due to dependencies.

**Estimated MVP timeline:** Foundations ~1 week → S-01 (+ unknown resolution) ~2 weeks → Dashboard features (S-02/S-03/S-04) ~1 week → Dashboard polish (S-05) ~3 days → Deploy & verify. Fits comfortably in the 3-4 week total budget with buffer.

---

════════════════════════════════════════════════════════════
**ROADMAP GENERATED**
════════════════════════════════════════════════════════════

**Project:** cerebrum
**Path:** context/foundation/roadmap.md
**Main goal:** speed (sequencing bias)
**#1 blocker:** time (6-week hard deadline)
**Baseline present:** Backend/API, Deploy/infra (partial)
**Foundations:** 4
**Slices:** 10 (S-01a through S-01e, S-02, S-03, S-04, S-05, S-06)
**Status breakdown:** ready: 3 (F-01, F-02, F-03) | proposed: 2 (F-04, S-06) | implemented: 9 | blocked: 0
**PRD coverage:** 6 must-have FRs covered | 1 nice-to-have FR (implemented)
**Open Roadmap Q:** 3 (intent classification rules, cheap fallback model, upstream model choices)
**Parked items:** 0

**North star:** S-01e — End-to-end intent-aware proxy routing

════════════════════════════════════════════════════════════

---

## Your next move

**► `/10x-plan classify-endpoint` on S-01a: Intent classification endpoint**

**Why this one first:** It's the first building block in the proxy chain (regex-based classification with fallback). Two blocking unknowns (classification rules, cheap fallback model) must be resolved before planning can proceed, but it's the logical starting point for the north-star sequence.

**Sequential chain:** After S-01a, proceed to S-01b (`reqwest-upstream-routing`), then S-01c (`provider-agnostic-config`), then S-01d (`sse-streaming-proxy`), then S-01e (`proxy-intent-routing`) for end-to-end integration.

**After S-01e lands:** S-02, S-03, S-04 can proceed in parallel. S-05 (dashboard consolidation) follows after those. F-04 (critical logging) and S-06 (logs page) can be planned concurrently to enhance observability.
*** End Updated File ***