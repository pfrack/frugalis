---
id: competitive-landscape-gaps
status: preparing
created: 2026-06-25
updated: 2026-06-25
user: pfrack
tags: [competitive-analysis, alternatives, llm-gateway, observability, cost, client-integration]
---
# competitive-landscape-gaps

## What
Identify feature gaps in Cerebrum by comparing it against the broader LLM-infrastructure landscape: open-source/commercial LLM gateways & routers (LiteLLM, Portkey, OpenRouter, RouteLLM, Helicone), observability & cost platforms (Langfuse, LangSmith, Braintrust, Phoenix), and the integration requirements of AI coding agents (Claude Code, Codex CLI, Cursor, Cline).

## Why
Cerebrum's regex/fewshot/LLM intent classifier + per-category routing is the *primitive* predecessor of an approach the rest of the industry has commoditized (learned routers, semantic caching, provider failover). A landscape-level gap analysis is needed to decide which gaps are worth closing to keep Cerebrum defensible vs. where to lean into its Rust/single-binary/auditable-routing niche.

## Related Research
`context/changes/competitive-landscape-gaps/research.md`

## Related Changes
- `context/changes/competitive-gap-model-routing/` — narrower prior research on Claude Code switching proxies (FCC, ccm, freedius), now implemented.
