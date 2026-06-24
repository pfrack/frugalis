---
id: competitive-gap-model-routing
status: implementing
created: 2026-06-24
updated: 2026-06-24
user: pfrack
tags: [competitive-analysis, providers, model-routing, /v1/models, claude-code]
---
# competitive-gap-model-routing

## What
Close the competitive gap with free-claude-code, claude-code-switch (ccm), and freedius by adding model-tier routing, `/v1/models` endpoint, and provider-specific quirk handling.

## Why
Cerebrum currently routes by intent classification but lacks model-name-based routing that Claude Code sends (opus/sonnet/haiku tiers). Without `/v1/models`, Claude Code's native model picker doesn't work. Provider-specific field sanitization (NIM) and request optimizations are missing vs competitors.

## Related Research
`context/changes/competitive-gap-model-routing/research.md`
