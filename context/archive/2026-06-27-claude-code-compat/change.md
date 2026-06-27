---
id: claude-code-compat
status: archived
archived: 2026-06-27
created: 2026-06-25
updated: 2026-06-27
user: pfrack
roadmap_id: (new — derived from competitive-landscape-gaps research, Tier-1 #3/#4)
tags: [claude-code, anthropic, prompt-caching, header-passthrough, protocol-translation, cache-control]
---
# claude-code-compat

## What
Make Cerebrum a true drop-in for Claude Code by (a) passing `anthropic-beta`/`anthropic-version`/`x-claude-code-*` headers through to Anthropic upstreams as an open list, (b) translating `cache_control` prompt-caching blocks across all four protocol crossings (with auto-insert on OpenAI→Anthropic), (c) translating cache tokens in responses and logging them, and (d) serving the Anthropic `/v1/models` shape with `display_name`.

## Why
Claude Code pairs each `anthropic-beta` header with a body field (`context_management`, `thinking`, `output_config`, `cache_control`); Cerebrum currently drops both halves, silently disabling those features. Prompt caching is GA and free to enable, but Cerebrum strips `cache_control` on every translation path. The `/v1/models` endpoint returns the OpenAI shape with no `display_name`, weakening Claude Code's model discovery. Closing these makes Cerebrum's existing 2,763-line bidirectional translator actually deliver its value.

## Related Research
- `context/changes/competitive-landscape-gaps/research.md` — Tier-1 gaps #3, #4 (Claude Code header passthrough + prompt-cache translation).
- `context/changes/competitive-gap-model-routing/research.md` — prior narrower Claude Code proxy research.

## Related Changes
- `context/changes/provider-fallback-cascade/` — implemented; this change builds on its provider-routing plumbing.
