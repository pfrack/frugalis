---
id: per-intent-latency-summary
status: archived
created: 2026-06-01
updated: 2026-06-07
archived_at: 2026-06-07T11:28:45Z
user: pfrack
tags: [dashboard, latency, intent, observability]
---

# per-intent-latency-summary

## What
Add a per-intent latency summary view to the Cerebrum dashboard: a summary card on the index page plus a dedicated `/dashboard/latency` page with configurable time window, AVG + P99 per category, and unclassified-count footnote.

## Why
S-03 observability slice — the PRD's secondary success criterion requires "a per-intent latency summary for recent traffic." After S-02 (log inspection), the operator needs aggregated latency data grouped by intent category to evaluate routing performance.

## Roadmap slot
S-03 — third observability slice after S-02. Depends on F-03 (templates), S-02 (log data being queryable). S-01 provides the data.
