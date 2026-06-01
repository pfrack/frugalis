---
id: classify-endpoint
status: planned
created: 2026-06-01
updated: 2026-06-01
user: pfrack
tags: [upstream-routing, classify, api, change-1-of-4]
---
# classify-endpoint

## What
Add a dedicated `POST /v1/classify` endpoint that decouples intent classification from the proxy handler. Part 1 of 4 in the upstream proxy routing sequence.

## Why
Classification should be independently accessible before the proxy handler becomes a full routing proxy. This establishes a clean API boundary and enables pre-classification workflows.

## Related Research
Master research: `context/changes/upstream-proxy-routing/research.md` (Sections 22-25, 28)
