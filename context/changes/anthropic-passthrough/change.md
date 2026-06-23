---
id: anthropic-passthrough
status: impl_reviewed
created: 2026-06-22
updated: 2026-06-23
user: pfrack
tags: [anthropic, proxy, streaming, pass-through, endpoint]
---
# anthropic-passthrough

## What
Add a `POST /v1/messages` endpoint that accepts Anthropic Messages API requests, classifies intent via the existing classifier, and forwards verbatim to an Anthropic-compatible upstream (pass-through — no protocol translation). Includes `provider_type: "anthropic"` support in auth and SSE byte-forwarding.

## Why
Foundation for multi-protocol support. Before adding translation logic, prove the `/v1/messages` route works end-to-end with Anthropic upstreams. This gives Claude Code (and other Anthropic-speaking clients) a working proxy path through cerebrum to native Anthropic providers.

## Dependencies
None — builds on existing infrastructure.

## Related Research
`context/changes/anthropic-passthrough/research.md`
