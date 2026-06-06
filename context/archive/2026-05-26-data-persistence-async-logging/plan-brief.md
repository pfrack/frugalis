# Data Persistence Async Logging Pipeline — Plan Brief

> Full plan: context/changes/data-persistence-async-logging/plan.md

## What & Why

We are implementing roadmap foundation F-02: persistent inference logging to PostgreSQL as a strict non-blocking side path. The goal is to capture routing metadata required by upcoming slices without compromising proxy response behavior. This creates the data substrate for dashboard visibility and future latency/cost analysis.

## Starting Point

The service already has auth-gated proxy and dashboard routes plus CI gates for auth and build quality, but no persistence layer or migration system. Current code has a placeholder completion handler and no database runtime contract, so F-02 is greenfield at the data layer.

## Desired End State

Each completed proxy request emits one asynchronous inference record containing category, upstream model, duration, timestamp, prompt snippet, request_id, and status. Logging failures never block client responses: they retry once in background, then drop with structured error logging. The repository includes versioned SQL migrations and selective DB integration checks.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Schema breadth | Minimal fields + request_id + status | Keeps MVP small while preserving operability/debug context. | Plan |
| Prompt storage | Fixed-length trimmed snippet | Meets privacy guardrail and keeps dashboard usefulness. | Plan |
| Failure semantics | Drop on final failure + structured error log | Preserves non-blocking guarantee while keeping observability. | Plan |
| Retry policy | One short retry in background | Handles transient failures without queue complexity. | Plan |
| Migration approach | Versioned SQL files in repo | Reproducible schema setup across local/CI/deploy environments. | Plan |
| Trigger timing | Log after request handling completes | Avoids response-path blocking and inconsistent partial records. | Plan |
| Test strategy | Unit + selective DB integration | Balanced confidence and MVP speed. | Plan |
| Performance target | Near-zero synchronous overhead | Protects primary product experience under load. | Plan |

## Scope

**In scope:**
- PostgreSQL inference schema + migration files.
- Runtime DB config and persistence module.
- Non-blocking background logging with one retry.
- Snippet minimization and structured failure logs.
- CI/test updates for persistence guardrails.

**Out of scope:**
- Full prompt persistence.
- Analytics/event pipeline.
- Multi-tenant data model.
- Billing-grade cost engine.

## Architecture / Approach

App startup initializes persistence state from environment and injects it into route handling. The completion route finalizes metadata, then schedules asynchronous persistence work after request handling completes. Database write errors are isolated from the response path and surfaced via logs. Migration files in repo define the single source of truth for schema evolution.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Data Contract and Runtime Configuration | Schema migration + dependency/env contracts | Drift between environments if migration contract is not enforced early |
| 2. Persistence Layer and Async Logger | Core write path, snippet policy, retry/drop semantics | Hidden failure modes if logging errors are not clearly surfaced |
| 3. App Integration, Verification, and Deploy Gate | Lifecycle wiring, selective DB integration tests, CI guardrails | Regressions if persistence checks are not enforced before deploy |

**Prerequisites:** Supabase PostgreSQL instance and DATABASE_URL configured in deployment/testing environments.
**Estimated effort:** ~2-3 focused sessions across 3 phases.

## Open Risks & Assumptions

- Assumes one-retry policy is sufficient for MVP traffic/reliability profile.
- Assumes snippet truncation limit remains acceptable for dashboard debugging needs.
- Assumes selective DB integration tests can run in CI without excessive pipeline slowdown.

## Success Criteria (Summary)

- Completed proxy requests create correct inference records in PostgreSQL.
- Logging failures never block or degrade client response behavior.
- Schema and persistence checks are enforced by automated verification before deployment.
