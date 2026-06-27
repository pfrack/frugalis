---
project: frugalis
context_type: greenfield
created: 2026-05-25
updated: 2026-05-25
product_type: api
target_scale:
  users: small
  qps: low
  data_volume: small
timeline_budget:
  mvp_weeks: 3
  hard_deadline: 2026-07-01
  after_hours_only: true
checkpoint:
  current_phase: 8
  phases_completed: [1, 2, 3, 4, 5, 6, 7]
  gray_areas_resolved:
    - topic: pain category
      decision: workflow friction, decision paralysis, missing capability, and coordination overhead
    - topic: insight
      decision: cheap intent classification can gate expensive models while preserving useful quality
    - topic: primary persona scope
      decision: single named user (solo developer/operator)
    - topic: access entry model
      decision: access key/token model for proxy clients; no account creation
    - topic: role separation
      decision: flat single-operator model for MVP
    - topic: intent classification strategy
      decision: regex/keyword rules first; cheap-model fallback only for ambiguous prompts
    - topic: dashboard scope
      decision: minimal dashboard stays in MVP
    - topic: FR prioritization
      decision: estimated cost-savings metric marked nice-to-have; remaining selected capabilities are must-have
    - topic: Socrates challenge round
      decision: keep FR-001..FR-006 as-is; keep FR-007 as nice-to-have
    - topic: business logic rule
      decision: gateway selects the cheapest acceptable upstream model per prompt intent while preserving usable response quality
    - topic: non-functional priorities
      decision: fast streaming start, private dashboard access, prompt-minimizing logs, and non-blocking side-path failures
    - topic: product framing
      decision: API/backend service, small initial scale, after-hours build, hard deadline 2026-07-01
    - topic: non-goals
      decision: no proprietary model training, no multi-tenant/team features, no billing-grade accounting, no advanced real-time analytics pipeline, no separate end-user app beyond integrated dashboard
  frs_drafted: 7
  quality_check_status: accepted
---

## Seed Idea

I want to build a monolithic AI Intent Gateway with an integrated HTML Dashboard using Rust and Axum. My problem is that autonomous AI agents blindly forward all prompts to expensive LLMs, wasting resources. I need a lightweight proxy to intelligently route these queries while displaying metrics natively.

The application will be a single Rust binary that does two things:

Proxy Endpoint (/v1/chat/completions): Intercepts OpenAI-compatible requests and performs a fast intent classification (using a cheap model like gpt-4o-mini via OpenRouter) to categorize the prompt into predefined intents: "COMPLEX_REASONING", "FILE_READING", "SYNTAX_FIX", or "CASUAL". Based on the category, it dynamically routes the prompt to the most appropriate upstream model (e.g., Claude 3.5 Sonnet for complex reasoning, DeepSeek Flash for basic file reading) and streams the response via SSE back to the CLI client. Asynchronously, it saves inference logs to a free Supabase PostgreSQL database.

Dashboard Endpoint (/dashboard): Instead of a separate SPA frontend, Axum will use the askama crate to render server-side HTML templates. The dashboard reads directly from the Supabase PostgreSQL database to display a table of recent inferences (showing the prompt snippet, the assigned category, upstream model, and duration) and calculates metrics like estimated cost savings. It includes basic HTTP Basic Auth middleware so only I can see the logs.

The goal is a unified architecture with NO separate frontend framework like Next.js. The deployment target is the free tier on Render.com using a standard Dockerfile. Configuration (intent-to-model mapping) is managed via a static YAML file.

## Vision & Problem Statement

Autonomous AI agents are currently forwarding OpenAI-compatible prompts to expensive models without intent-aware triage, which creates avoidable spend and operational friction for the person running the workflow.

The core insight is that a lightweight gateway can classify intent with a cheap model first, then route each prompt to a fit-for-purpose upstream model while exposing routing decisions and outcomes through a native dashboard.

Scale note: at 100x usage, the same rule must keep intent classification fast through lightweight CPU-friendly inference paths.

## User & Persona

### Primary persona

Solo developer/operator (you) running autonomous agent workflows who needs lower inference cost and direct visibility into routing behavior without adding a separate frontend stack.

## Access Control

- Proxy access is gated by an access key/token model compatible with OpenAI-style client usage.
- Dashboard access is private to the operator and protected by a basic authentication gate.
- MVP uses a flat permission model with a single operator persona and no multi-role matrix.

## Success Criteria

### Primary

- End-to-end gateway flow works with intent-aware routing: request enters proxy, is classified (regex first, cheap-model fallback when ambiguous), routed to an upstream model, and streamed back via SSE.
- Operator can inspect recent inference rows and core savings/latency indicators in a native dashboard view.

### Secondary

- Dashboard shows a per-intent latency summary for recent traffic.

### Guardrails

- SSE streaming remains compatible with CLI clients using OpenAI-style streaming expectations.
- Raw prompt data is not persisted beyond an explicit snippet/redaction policy.

## User Stories

### US-01: Intent-aware proxy routing for an agent request

- **Given** a valid OpenAI-compatible chat completion request sent to the gateway
- **When** the gateway classifies intent (regex/keyword first, cheap-model fallback when ambiguous) and applies intent-to-model mapping
- **Then** the request is routed to the selected upstream model, the response is streamed via SSE, and inference metadata is logged asynchronously

#### Acceptance Criteria

- Streaming clients receive response chunks in a continuous SSE stream for successful upstream calls
- Ambiguous prompts trigger the cheap-model fallback classifier before final routing
- A completed inference writes one log record with category, upstream model, and duration

## Functional Requirements

### Proxy Flow

- FR-001: Client can send OpenAI-compatible chat completion requests through the gateway. Priority: must-have
  > Socrates: Counter-argument considered: payload compatibility can lock the proxy to upstream-specific assumptions.
  > Resolution: kept; MVP treats OpenAI compatibility as a constrained boundary and can start with a strict subset.
- FR-002: Gateway can classify prompt intent using regex/keyword rules with cheap-model fallback for ambiguous prompts. Priority: must-have
  > Socrates: Counter-argument considered: classifier overhead may erase savings on short/simple prompts.
  > Resolution: kept; regex-first path minimizes fallback calls and preserves the cost-control thesis.
- FR-003: Gateway can route each request to an upstream model using intent-to-model mapping. Priority: must-have
  > Socrates: Counter-argument considered: routing policy complexity may create avoidable failure modes early.
  > Resolution: kept; explicit routing is core product value and failures are bounded with deterministic fallback behavior.
- FR-004: Gateway can stream routed upstream responses to clients via SSE. Priority: must-have
  > Socrates: Counter-argument considered: streaming edge cases increase MVP protocol complexity.
  > Resolution: kept; CLI compatibility depends on streaming behavior and is a guardrail-level requirement.
- FR-005: System can persist inference metadata asynchronously for completed requests. Priority: must-have
  > Socrates: Counter-argument considered: async logging can introduce hidden backpressure or reliability gaps.
  > Resolution: kept; logging remains async and non-blocking to protect proxy response flow.

### Dashboard

- FR-006: Operator can view recent inferences in a server-rendered dashboard table including prompt snippet, assigned category, upstream model, and duration. Priority: must-have
  > Socrates: Counter-argument considered: dashboard can be deferred if CLI-only usage dominates.
  > Resolution: kept; native visibility is part of the product promise and enables routing validation.
- FR-007: Operator can view an estimated cost-savings metric derived from logged inferences. Priority: nice-to-have
  > Socrates: Counter-argument considered: savings estimates may mislead without robust baseline assumptions.
  > Resolution: retained as nice-to-have so MVP can ship without metric precision blocking core flow.

## Business Logic

For each incoming prompt, the gateway decides the cheapest acceptable upstream model by intent while preserving usable response quality.

The rule consumes user-facing request context from each completion call and classifies intent through a low-cost path first, escalating only when intent is ambiguous.

Its output is a concrete routing decision (intent class plus mapped upstream model) that determines which model receives the prompt and what cost/latency profile is expected.

The operator encounters this rule in two places: directly through routed streaming responses, and indirectly through logged inference traces and metrics in the dashboard.

## Non-Functional Requirements

- For valid requests, the proxy starts a visible response stream quickly enough to feel immediate to a CLI operator.
- Dashboard views are available only to an authorized operator identity.
- Persisted inference records exclude full raw prompt bodies by default and retain only minimized/redacted snippets needed for observability.
- Failures in asynchronous logging or secondary metric paths do not block or stall primary response streaming to the client.

## Non-Goals

- Do not build or train proprietary intent models in MVP; use external classifier capability.
- Do not add multi-tenant accounts, team workspaces, or role-heavy collaboration flows.
- Do not target billing-grade cost accounting precision in MVP; directional savings are sufficient.
- Do not build advanced real-time analytics pipelines beyond recent-view observability.
- Do not create a separate end-user application surface beyond the integrated operator dashboard.

## Forward: tech-stack

- Preferred implementation language/runtime: Rust monolithic binary.
- Preferred web framework and rendering approach: Axum with server-side HTML templates.
- Preferred dashboard templating style: server-rendered templates (no standalone SPA framework).
- Preferred persistence target for logs: hosted PostgreSQL service.
- Preferred deployment shape: containerized deployment using Dockerfile on a free-tier host.
- Preferred configuration style: static YAML file for intent-to-model mapping.

## Forward: technical-roadmap

- Consider a future classification performance path for higher scale (for example, lightweight local inference on CPU) if external classifier latency/cost becomes limiting.

## Quality cross-check

- Access Control: present.
- Business Logic: present.
- Project artifacts: present.
- Timeline-cost acknowledgment: present (mvp_weeks = 3).
- Non-Goals: present.
- Preserved behavior: n/a for greenfield.
