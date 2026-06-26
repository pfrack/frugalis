# Provider Fallback / Cascade — Plan Brief

> Full plan: `context/changes/provider-fallback-cascade/plan.md`

## What & Why

When an upstream provider fails (5xx, timeout, 429), cerebrum currently returns a 502 to the caller with no retry. This adds automatic cascade to a fallback provider — if the primary is down or rate-limited, the proxy transparently retries on the next configured provider. Improves resilience for Claude Code users who depend on always-available upstream routing.

## Starting Point

`RouteEntry` holds a single provider (model, endpoint, provider_type, api_key_env). The forwarding path in `completion_handler` and `messages_handler` makes one attempt and returns immediately on failure. Three separate forwarding paths exist (OpenAI, Anthropic, translation).

## Desired End State

Each routing category specifies an ordered list of providers. On retryable failure, the proxy walks the list until one succeeds. Operators see fallback events in the dashboard (attempt count, final provider). Streaming requests cascade only before the first byte — no mid-stream corruption.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) |
|---|---|---|
| Config schema | Inline ordered provider array per category | Simple priority ordering; backward-compatible via serde `from` conversion. |
| Trigger conditions | 5xx + timeout + 429 | Covers all common transient failure modes. |
| Streaming behavior | Retry only before first byte | Prevents partial-response corruption; transparent to client. |
| Max retries | Try all configured providers once | Predictable, bounded latency. |
| Cross-protocol fallback | Yes, re-translate per provider | Maximum flexibility (Claude primary → NIM fallback). |
| Timeout | Per-provider with global default | Fast providers get short timeouts; minimizes cascade latency. |
| Observability | warn! log + persist attempts | Aligns with lessons.md; enables dashboard visibility. |

## Scope

**In scope:**
- Ordered provider list in routing config
- Retry loop in all 3 forwarding paths
- Cross-protocol re-translation on fallback
- Per-provider timeout
- Inference log: attempt count + final provider
- Dashboard display of fallback events

**Out of scope:**
- Same-provider retry with backoff
- Mid-stream fallback
- Circuit breaker / health checks
- Weighted load balancing
- Retry-After header parsing

## Architecture / Approach

`RouteEntry` gains a `Vec<ProviderEntry>` replacing the single flat fields. The forwarding path wraps its send logic in a loop: for each provider in the list, build request (translating body per provider_type), send, check if retryable. On failure, warn! + advance. On success, break. Existing single-provider configs deserialize to a one-element vec — zero behavioral change for current users.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Config & Data Model | Multi-provider RouteEntry + backward-compatible parsing | Serde `from` conversion must handle both flat and array formats cleanly |
| 2. Retry Loop | Automatic cascade in all forwarding paths | Cross-protocol re-translation logic in 3 code paths; must not regress existing behavior |
| 3. Observability | Attempt count + final provider in logs and dashboard | Schema migration on live database |

**Prerequisites:** None beyond current codebase (S-01e, S-01c already implemented)
**Estimated effort:** ~2-3 sessions across 3 phases

## Open Risks & Assumptions

- If all providers are exhausted, the last error is returned (not aggregated) — acceptable for MVP
- Per-provider timeout via `RequestBuilder::timeout()` works alongside client-level timeout (needs verification)
- Schema migration must be additive (new columns with defaults) to avoid downtime

## Success Criteria (Summary)

- Requests succeed transparently when primary provider is down and a fallback is configured
- Zero behavioral change for single-provider configs (backward compatible)
- Operators can identify fallback events in the dashboard
