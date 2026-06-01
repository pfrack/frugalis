---
id: provider-agnostic-config
status: implementing
created: 2026-06-01
updated: 2026-06-01
user: pfrack
tags: [upstream-routing, provider-agnostic, routing-config, toml, change-3-of-4]
---
# provider-agnostic-config

## What
Generalize the routing configuration so each intent category can route to a different provider with its own API key and auth scheme. Add `provider_type` and `api_key_env` fields to `routing.toml` and the `RouteEntry`/`ClassificationResult` structs. Part 3 of 4 in the upstream proxy routing sequence.

## Why
A single `UPSTREAM_API_KEY` (Change 2) is too restrictive. Different providers have different auth schemes (Bearer, x-api-key, no auth) and require different configuration. This change makes the routing config final before SSE streaming (Change 4).

## Related Research
Master research: `context/changes/upstream-proxy-routing/research.md` (Sections 2, 13-20, 25, 27-29)
