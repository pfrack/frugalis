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

**S-01: Intent-aware proxy routing** — Smallest end-to-end proof: proxy accepts a request, classifies intent (regex first, cheap-model fallback for ambiguous), routes to an appropriate upstream model, and streams response back via SSE. This validates the core hypothesis: intent-aware triage works and is fast enough for production use.

> The north star is the one slice whose successful delivery proves the product works. Everything else only matters if this works. Here, that's the proxy flow with end-to-end routing.

## At a glance

| ID | Change ID | Outcome (user can …) | Prerequisites | PRD refs | Status |
|---|---|---|---|---|---|
| F-01 | auth-scaffold-access-keys | (foundation) Access key/token validation + operator dashboard auth gates are in place | — | FR-001, Access Control | done |
| F-02 | data-persistence-async-logging | (foundation) Async inference logging pipeline connected to Supabase PostgreSQL | — | FR-005, NFR (non-blocking logs) | proposed |
| F-03 | dashboard-template-scaffold | (foundation) Askama HTML templating and server-side rendering wired into Axum | — | FR-006, Dashboard | proposed |
| S-01 | proxy-intent-routing | send a chat-completion request through the gateway, which classifies intent, routes to an upstream model, and receives streamed response | F-01, F-02 | US-01, FR-001..FR-004 | proposed |
| S-02 | inference-log-inspection | view recent inference records in the dashboard with prompt snippet, assigned category, upstream model, and duration | F-02, F-03, S-01 | FR-006 | proposed |
| S-03 | per-intent-latency-summary | view a latency summary grouped by intent category in the dashboard | F-03, S-02 | Secondary Success Criterion | proposed |
| S-04 | cost-savings-metric | view an estimated cost-savings indicator based on logged inferences | S-02 | FR-007 (nice-to-have) | blocked |

## Streams

Navigation aid — groups items that share a Prerequisites chain. Canonical ordering still lives in the dependency graph below; this table is the proposed reading order across parallel tracks.

| Stream | Theme | Chain | Note |
|---|---|---|---|
| A | Proxy core | `F-01` → `F-02` → `S-01` | The validating path: gate access, enable logging, then prove routing works. |
| B | Dashboard | `F-03` → `S-02` → `S-03` | Observability: render templates, surface inferences, add summaries. S-03 joins Stream A at S-01 (both depend on S-01 to have logged data). |
| C | Metrics | `S-04` | Parked until MVP ships; cost precision is a nice-to-have. |

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
- **Unlocks:** S-02 (dashboard queries and displays inference records), S-03 (adds latency summaries to the same template)
- **Prerequisites:** —
- **Parallel with:** F-01, F-02 (independent)
- **Blockers:** —
- **Unknowns:** —
- **Risk:** Server-side templating avoids a separate SPA framework (per tech-stack preference). Scaffolding is the setup cost; incremental template work (adding new fields, new sections) happens in S-02 / S-03. No frontend build pipeline, no Node.js, keeps deployment footprint minimal.
- **Status:** proposed

## Slices

### S-01: Intent-aware proxy routing

- **Outcome:** user can send an OpenAI-compatible chat completion request to the gateway, which classifies intent (regex/keyword rules first, cheap-model fallback for ambiguous prompts), routes to an appropriate upstream model, receives the full streamed response, and the proxy forwards it to the client via SSE.
- **Change ID:** `proxy-intent-routing`
- **PRD refs:** US-01, FR-001 (accept chat-completion requests), FR-002 (intent classification), FR-003 (intent-to-model mapping), FR-004 (stream responses)
- **Prerequisites:** F-01 (access key validation), F-02 (async logging to record inference metadata)
- **Parallel with:** — (north-star blocking slice; S-02 / S-03 depend on this)
- **Blockers:** —
- **Unknowns:**
  - How does regex/keyword classification map to intent categories? (Intent categories: COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL per shape-notes.) Owner: you. Block: yes.
  - Which upstream models are available on chosen provider (OpenRouter?) and what are their cost/latency profiles? Owner: you. Block: yes.
  - Does SSE streaming from the proxy to client require application-level keepalive pings, or is HTTP/1.1 transfer-encoding: chunked sufficient? Owner: implementation research. Block: no (SSE is in prod, fallback to chunked if SSE breaks).
- **Risk:** The core product slice; all downstream work depends on this shipping. Classification rules (regex + fallback) are the MVP cheapest path; if fallback cost becomes too high in production, that's a post-MVP tuning point (tech-stack shape-notes mention "lightweight local inference on CPU" as a future path). Streaming edge cases are real but manageable (keepalive pings are a one-liner if needed).
- **Status:** proposed

### S-02: Inference log inspection

- **Outcome:** user can view a table in the dashboard showing recent inference records, each row displaying: prompt snippet (minimized, no full body), assigned intent category, upstream model selected, and request duration.
- **Change ID:** `inference-log-inspection`
- **PRD refs:** FR-006 (dashboard table of inferences)
- **Prerequisites:** F-02 (data in PostgreSQL), F-03 (template rendering), S-01 (inferences are being logged)
- **Parallel with:** S-03 (both query the same table; S-03 adds aggregation)
- **Blockers:** —
- **Unknowns:**
  - How many recent inferences should the dashboard show by default? (pagination? date range? limit?) Owner: you. Block: no (default: last 100 is reasonable; UI polish is post-MVP).
  - How should prompt snippets be truncated/minimized for display? Owner: you. Block: no (implementation detail; default: first 200 chars is safe).
- **Risk:** Second slice; depends on S-01 generating data. Template rendering is straightforward (Askama is mature). Query performance should be fine for "recent 100 rows" on a small free-tier PostgreSQL. If this grows to high volume, indexing on timestamp is a future optimization.
- **Status:** proposed

### S-03: Per-intent latency summary

- **Outcome:** user can view a summary (table or chart) in the dashboard showing average and p99 latency grouped by intent category, derived from recent inference records.
- **Change ID:** `per-intent-latency-summary`
- **PRD refs:** Secondary Success Criterion (dashboard shows per-intent latency summary)
- **Prerequisites:** F-03 (dashboard rendering), S-02 (log inspection working)
- **Parallel with:** — (depends on S-02 queries)
- **Blockers:** —
- **Unknowns:**
  - Should the summary be computed in the database (SQL GROUP BY + aggregation) or in Rust (query all rows, compute in-memory)? Owner: implementation. Block: no (both are viable; SQL is simpler).
  - Time window for the summary? (last hour? last 24h? configurable?) Owner: you. Block: no (default: last 24h is reasonable).
- **Risk:** Third-priority slice after core proxy and basic log view. Aggregation adds minimal complexity. If compute time becomes noticeable, move aggregation to a background job; but that's post-MVP tuning.
- **Status:** proposed

### S-04: Cost-savings metric

- **Outcome:** user can view an estimated cost-savings indicator in the dashboard showing the inferred savings from using routed models vs. sending all prompts to an expensive baseline model.
- **Change ID:** `cost-savings-metric`
- **PRD refs:** FR-007 (nice-to-have)
- **Prerequisites:** S-02 (log inspection), inference cost model (which models cost what)
- **Parallel with:** — (after S-02)
- **Blockers:** —
- **Unknowns:**
  - What baseline model and which routed models should the savings estimate compare? Owner: you. Block: yes (definition of "savings" is missing).
  - Should the metric account for classification cost (cheap-model fallback calls) or assume classification is free? Owner: you. Block: yes.
- **Risk:** Nice-to-have; explicitly parked in the hard deadline path. PRD says "directional savings are sufficient" (not billing-grade precision). Two unknowns block detailed planning, but those are deliberate deferments. This slice can be planned / shipped in a post-MVP refinement cycle once the core gateway is in production and you've tuned the model choices.
- **Status:** blocked

## Backlog Handoff

| Roadmap ID | Change ID | Suggested issue title | Ready for `/10x-plan` | Notes |
|---|---|---|---|---|
| F-01 | auth-scaffold-access-keys | Auth: Access key validation middleware + operator dashboard gate | yes | Simplest foundation; no blockers. Plan first to unblock proxy endpoint exposure. |
| F-02 | data-persistence-async-logging | Data: Supabase PostgreSQL + async inference logging | yes | Quick setup (Supabase free tier); enables proxy observability. Plan in parallel with F-01 if team size allows. |
| F-03 | dashboard-template-scaffold | Dashboard: Askama template scaffold + /dashboard route | yes | Pure scaffolding; no external dependencies. Plan in parallel with F-01 / F-02. |
| S-01 | proxy-intent-routing | Proxy: Intent classification + upstream routing + SSE streaming | no | Unblock F-01 and F-02 first. 2 blocking unknowns (intent classification rules, upstream model choices) must be resolved before detailed planning. |
| S-02 | inference-log-inspection | Dashboard: Recent inferences table (category, model, duration) | no | Unblock S-01 first. 0 blocking unknowns; 2 non-blocking (pagination, snippet formatting) are implementation details. |
| S-03 | per-intent-latency-summary | Dashboard: Per-intent latency summary | no | Unblock S-02 first. Non-blocking unknowns only. |
| S-04 | cost-savings-metric | Dashboard: Estimated cost-savings metric (parked — nice-to-have) | no | Blocked by 2 unknowns (baseline model choice, classification cost accounting). Park for post-MVP refinement. |

## Open Roadmap Questions

1. **Intent classification categories and regex/keyword rules** — The PRD names four intents (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) but does not provide the actual regex patterns or keyword lists. Define the rules so S-01 planning can proceed. Owner: you. Block: S-01.
2. **Upstream model choices and cost/latency profiles** — Which models will the gateway route to (e.g., Claude 3.5 Sonnet for COMPLEX_REASONING, DeepSeek Flash for FILE_READING per shape-notes)? What are the cost and latency tradeoffs? This informs intent-to-model mapping in S-01 and cost calculation in S-04. Owner: you. Block: S-01, S-04.

## Parked

- **Cost-savings metric (FR-007)** — Marked nice-to-have in PRD. Parked until core proxy and dashboard are verified in production; cost precision requires stable model cost data and classification patterns, both of which emerge post-launch.

## Done

- **F-01: (foundation) Access key/token validation middleware + basic HTTP auth for dashboard are in place; proxy routes require a valid key header; dashboard requires operator credentials.** — Archived 2026-06-01 → `context/archive/2026-05-26-auth-scaffold-access-keys/`. Lesson: —.

---

## Sequencing rationale

**Why this order?**

The 3-week MVP budget under a 6-week hard deadline makes calendar time the #1 blocker. This roadmap sequences must-haves in dependency order and parks nice-to-haves.

1. **Foundations (F-01, F-02, F-03) first, run in parallel** — All three are independent scaffolding tasks (auth, data, template setup). No blockers. Running them in parallel uses available capacity efficiently and unblocks the core proxy slice. Estimated 1 week total wall-clock time if executed in parallel.

2. **North-star slice (S-01) next** — The proxy routing logic is the product's core hypothesis. Unblock it as soon as Foundations land. This slice has 2 blocking unknowns (intent rules, model choices) that must be resolved before planning can proceed; surface and resolve those first.

3. **Dashboard slices (S-02, S-03) follow** — Depend on S-01 having data to display. Non-blocking slices; estimated 3–4 days combined once S-01 is working.

4. **Cost metric (S-04) parked** — Nice-to-have blocked by 2 unknowns that are post-MVP tuning (baseline choice, classification cost accounting). Defer explicitly.

**Parallel tracks:** F-01, F-02, F-03 can run in parallel. S-02 and S-03 can run in parallel *after* S-01 lands.

**Estimated MVP timeline:** Foundations ~1 week → S-01 (+ unknown resolution) ~1.5–2 weeks → S-02 / S-03 ~3–4 days → deploy & verify. Fits comfortably in the 3-week MVP budget with some buffer.

---

═══════════════════════════════════════════════════════════
**ROADMAP GENERATED**
═══════════════════════════════════════════════════════════

**Project:** cerebrum
**Path:** context/foundation/roadmap.md
**Main goal:** speed (sequencing bias)
**#1 blocker:** time (6-week hard deadline)
**Baseline present:** Backend/API, Deploy/infra (partial)
**Foundations:** 3
**Slices:** 4
**Status breakdown:** ready: 3 (F-01, F-02, F-03) | proposed: 4 (S-01 through S-03, S-04) | blocked: 1 (S-04)
**PRD coverage:** 6 must-have FRs covered | 1 nice-to-have FR (parked)
**Open Roadmap Q:** 2 (intent classification rules, upstream model choices)
**Parked items:** 1 (cost-savings metric — nice-to-have)

**North star:** S-01 — Intent-aware proxy routing

═══════════════════════════════════════════════════════════

---

## Your next move

**► `/10x-plan auth-scaffold-access-keys` on F-01: Auth scaffold — access keys & operator gate**

**Why this one first:** It's the simplest foundation (no external blockers, no unknowns) and unblocks everything downstream. You can run it in parallel with F-02 and F-03 to use available weeks efficiently. Auth middleware is table-stakes before the proxy endpoint is exposed.

**Parallel track:** Start F-02 (`data-persistence-async-logging`) and F-03 (`dashboard-template-scaffold`) in the same week if capacity allows. All three are independent.

**After Foundations land:** Resolve the 2 Open Roadmap Questions (intent classification rules + upstream model choices), then plan S-01 (`proxy-intent-routing`). S-01 is the north star — if it works, everything else follows.

**Blocked until unknowns resolve:** S-04 (`cost-savings-metric`) is explicitly parked; it requires post-MVP tuning data.

(Full planning order in `## Backlog Handoff`.)
