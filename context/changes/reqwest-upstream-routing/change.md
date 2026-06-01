---
id: reqwest-upstream-routing
status: preparing
created: 2026-06-01
updated: 2026-06-01
user: pfrack
tags: [upstream-routing, reqwest, http-client, proxy, change-2-of-4]
---
# reqwest-upstream-routing

## What
Add upstream HTTP routing to `POST /v1/chat/completions`. Forward the classified request body to the endpoint specified in routing config, collect the buffered response, and return it. Part 2 of 4 in the upstream proxy routing sequence.

## Why
FR-003 (routing) is a must-have requirement. Classification has been delivered; routing is the next step toward the S-01 north star. A single `UPSTREAM_API_KEY` env var secures all upstream calls.

## Related Research
Master research: `context/changes/upstream-proxy-routing/research.md` (Sections 1-3, 6-9, 11, 25-28)
