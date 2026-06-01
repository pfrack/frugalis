---
id: upstream-proxy-routing
status: preparing
created: 2026-06-01
updated: 2026-06-01
user: pfrack
tags: [upstream-routing, sse-streaming, reqwest, provider-agnostic, proxy, s-01]
---
# upstream-proxy-routing

## What
Complete the intent-aware proxy by adding provider-agnostic upstream model routing and SSE streaming, broken into four smaller changes. First, extract classification to a dedicated `POST /v1/classify` endpoint. Then add upstream HTTP routing, generalize provider config, and finally add SSE streaming.

## Why
S-01 (the north star slice) requires a working proxy that classifies intent AND routes to upstream models with streamed responses. Classification-only is a checkpoint, not the destination. The work decomposes into four independently-shippable changes for safety and reviewability.

## Open Questions
- 4 changes or combine classify + routing into fewer steps?
- Should `completion_handler` accept `X-Cerebrum-Category`/`X-Cerebrum-Model` headers to skip re-classification?
- How should upstream API keys be configured (env vars referenced by name in routing.toml)?
- Single-level (flat) routing.toml vs two-level (providers + routing) for MVP?
- Should Anthropic's different body schema be supported or deferred?
