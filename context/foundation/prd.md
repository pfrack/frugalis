---
project: frugalis
version: 1
status: draft
created: 2026-05-25
context_type: greenfield
product_type: api
target_scale:
  users: small
  qps: low
  data_volume: small
timeline_budget:
  mvp_weeks: 3
  hard_deadline: 2026-07-01
  after_hours_only: true
---

## Vision & Problem Statement

Autonomous agents currently route prompts without intent-aware triage, which creates avoidable spend and operational friction for the person running the workflow.

A lightweight intent-aware gateway can apply a low-cost first-pass decision, choose an appropriate processing path, and expose routing outcomes so the operator can continuously tune efficiency.

## User & Persona

### Primary persona

Solo developer/operator running autonomous agent workflows who needs lower inference cost and direct visibility into routing behavior with a single integrated product surface.

## Success Criteria

### Primary

- End-to-end request flow works with intent-aware routing: request enters the gateway, intent is categorized, an appropriate processing path is selected, and output is returned to the client without interruption.
- Operator can inspect recent inference records and core efficiency indicators in the integrated dashboard.

### Secondary

- Dashboard includes a per-intent latency summary for recent traffic.

### Guardrails

- Long responses remain continuously readable by the client during delivery.
- Persisted inference data excludes full prompt bodies and keeps only minimized snippets needed for observability.

## User Stories

### US-01: Intent-aware routing for an agent request

- **Given** a valid chat-completion request sent to the gateway
- **When** the gateway categorizes intent with a low-cost first pass and applies the intent-to-path mapping
- **Then** the request is handled by the selected path, output is returned continuously to the client, and inference metadata is recorded asynchronously

#### Acceptance Criteria

- Clients receive continuous output chunks for successful responses
- Ambiguous prompts trigger fallback classification before final routing
- A completed inference produces one record with category, selected path, and duration

## Functional Requirements

### Proxy Flow

- FR-001: Client can send standard chat-completion requests through the gateway. Priority: must-have
  > Socrates: Counter-argument considered: compatibility breadth can lock the gateway to assumptions that are hard to change later.
  > Resolution: kept; MVP can support a constrained compatibility subset first.
- FR-002: Gateway can classify prompt intent using low-cost heuristic rules with fallback classification for ambiguous prompts. Priority: must-have
  > Socrates: Counter-argument considered: classification overhead can erase savings on short or simple prompts.
  > Resolution: kept; low-cost first-pass classification minimizes fallback calls.
- FR-003: Gateway can route each request using an intent-to-path mapping policy. Priority: must-have
  > Socrates: Counter-argument considered: routing policy complexity can introduce avoidable early failures.
  > Resolution: kept; explicit routing is core product value and fallback behavior is bounded.
- FR-004: Gateway can return incremental response chunks to clients during long-running completions. Priority: must-have
  > Socrates: Counter-argument considered: continuous-output edge cases can increase MVP complexity.
  > Resolution: kept; uninterrupted client output is a guardrail-level requirement.
- FR-005: System can persist inference metadata asynchronously for completed requests. Priority: must-have
  > Socrates: Counter-argument considered: asynchronous logging can hide reliability issues in side paths.
  > Resolution: kept; logging remains non-blocking relative to primary response delivery.

### Dashboard

- FR-006: Operator can view recent inferences in a dashboard table including prompt snippet, assigned category, selected path, and duration. Priority: must-have
  > Socrates: Counter-argument considered: dashboard work can be deferred for CLI-first usage.
  > Resolution: kept; integrated visibility is part of the product promise.
- FR-007: Operator can view an estimated cost-savings metric derived from inference records. Priority: nice-to-have
  > Socrates: Counter-argument considered: savings estimates can mislead without robust baseline assumptions.
  > Resolution: retained as nice-to-have so MVP can ship without metric precision blocking core flow.

## Non-Functional Requirements

- For valid requests, user-perceived acknowledgement starts quickly and output remains continuously visible during longer operations.
- Dashboard views are restricted to an authorized operator identity.
- Inference records retain minimized snippets only and exclude full prompt bodies by default.
- Failures in asynchronous logging or secondary metric calculation do not block primary response delivery.

## Business Logic

For each incoming prompt, the gateway chooses the cheapest acceptable processing path by intent while preserving usable response quality.

The rule consumes request context from each completion call and applies a low-cost first-pass categorization, escalating only when intent is ambiguous.

Its output is a concrete routing decision that determines which processing path handles the prompt and the expected cost/latency profile.

Operators observe the rule through request outcomes and through inference traces and metrics in the dashboard.

## Access Control

- Request routing access is gated by an access key/token model for client requests.
- Dashboard access is private to the operator and protected by an authentication gate.
- MVP uses a flat single-operator permission model without role separation.

## Non-Goals

- No proprietary intent-model training in MVP.
- No multi-tenant accounts, team workspaces, or collaboration role matrix.
- No billing-grade accounting precision in MVP; directional savings are sufficient.
- No advanced real-time analytics pipeline beyond recent-view observability.
- No separate end-user application surface beyond the integrated operator dashboard.

## Open Questions

1. None at this stage.
