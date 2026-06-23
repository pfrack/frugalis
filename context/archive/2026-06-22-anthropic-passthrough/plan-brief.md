# Anthropic Pass-Through Proxy — Plan Brief

> Full plan: `context/changes/anthropic-passthrough/plan.md`
> Research: `context/changes/anthropic-passthrough/research.md`

## What & Why

Add `POST /v1/messages` endpoint that accepts Anthropic Messages API requests and proxies them to Anthropic-compatible upstreams — no protocol translation. This is the foundation for multi-protocol support: prove the route works before layering translation complexity.

## Starting Point

Cerebrum has a single proxy endpoint (`/v1/chat/completions`) for OpenAI-format traffic. Intent classification + routing + streaming all work. Auth is pluggable via `AuthProviderConfig`. No Anthropic-protocol support exists.

## Desired End State

An Anthropic-speaking client (e.g., Claude Code) can point at `http://cerebrum:10000/v1/messages` and get routed to the optimal Anthropic-native upstream, with intent classification, cost tracking, and full observability — identical to how OpenAI clients use `/v1/chat/completions` today.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Routing mechanism | Same intent classifier | User shouldn't know/care which provider is behind the proxy | Plan |
| Auth for Anthropic upstreams | `provider_type: "anthropic"` in config | Explicit, simple conditional; consistent with other provider types | Plan |
| Streaming approach | Byte-forwarding (existing pattern) | No translation needed for pass-through; proven reliable | Research |
| Error format | Anthropic error JSON | Client speaks Anthropic; proxy errors should match that protocol | Plan |
| Observability | Full parity with existing endpoint | Enterprise-ready means no blind spots | Plan |

## Scope

**In scope:**
- New `POST /v1/messages` route behind existing auth layer
- `extract_last_user_message_anthropic` for classification
- `auth_headers_for` support for `provider_type: "anthropic"` (x-api-key + anthropic-version)
- Streaming byte-forwarding + keepalive
- Model override from classification
- OTel metrics + persistence logging
- OpenAPI spec update

**Out of scope:**
- Protocol translation (Anthropic↔OpenAI)
- New config schema
- Changes to existing `/v1/chat/completions`
- Anthropic-specific rate limiting

## Architecture / Approach

Mirrors `completion_handler` structure: validate → extract prompt → classify → build upstream request → forward → return. Key difference: auth headers use `x-api-key` instead of `Authorization: Bearer`, and proxy errors return Anthropic JSON format. Streaming reuses the existing channel-based byte-forwarding pipeline unchanged.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Prompt Extractor + Auth | `extract_last_user_message_anthropic` + anthropic auth headers | None — pure functions |
| 2. Handler + Route Wiring | Working `/v1/messages` endpoint | Handler mirrors existing pattern; low risk |
| 3. Integration Tests | End-to-end verification with httpmock | Test coverage of streaming path |
| 4. OpenAPI Spec Update | Documentation parity | None |

**Prerequisites:** S-01e (end-to-end proxy) — already done.
**Estimated effort:** ~1 session (4 phases, all straightforward).

## Open Risks & Assumptions

- Assumes Anthropic upstreams return standard SSE format (validated by Claude API docs)
- Body size limit (10MB default) may need increasing for image-heavy Anthropic requests

## Success Criteria (Summary)

- Claude Code can connect to `/v1/messages` and receive responses from an Anthropic upstream
- OTel metrics and dashboard show Anthropic endpoint traffic alongside existing OpenAI traffic
- All existing tests remain green (zero regression)
