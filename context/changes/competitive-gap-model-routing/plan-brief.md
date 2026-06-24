# Competitive Gap: Model Routing Compatibility — Plan Brief

> Full plan: `context/changes/competitive-gap-model-routing/plan.md`
> Research: `context/changes/competitive-gap-model-routing/research.md`

## What & Why

Close 4 practical gaps identified in competitive analysis against free-claude-code (36.7k stars), claude-code-switch, and freedius. These gaps cause real friction when using cerebrum as a Claude Code proxy: model discovery fails, NIM requests error on unsupported fields, and trivial probes waste upstream quota. None of the fixes conflict with cerebrum's intent-routing paradigm.

## Starting Point

Cerebrum has a working proxy with `/v1/chat/completions` and `/v1/messages` endpoints, bidirectional protocol translation (S-15 done, S-16 in progress), intent classification, and per-category routing. It lacks 4 small auxiliary features that competitors have.

## Desired End State

Claude Code connects through cerebrum with zero errors on model discovery, NIM-routed requests never fail on unsupported fields, token estimation works locally, and trivial probes complete in <5ms without upstream calls.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) |
| --- | --- | --- |
| `/v1/models` data | Static hardcoded Claude model list | Simpler than dynamic; cerebrum ignores model choice anyway |
| `/v1/models` auth | Unauthenticated (like `/health`) | Claude Code probes before authenticating |
| NIM sanitization | Hardcoded strip-list, `nvidia_nim` only | Only 3 fields to strip; configurable system is overkill |
| Token counting | chars/4 heuristic | Avoids tokenizer dependency; Claude Code has internal fallback |
| Request optimizations | Empty messages + tiny known probes | Minimal pattern set; can expand later |

## Scope

**In scope:**
- `GET /v1/models` static endpoint
- NIM field sanitization (`top_k`, `metadata`, `thinking`)
- `POST /v1/messages/count_tokens` with chars/4 approximation
- Trivial probe short-circuit in handlers

**Out of scope:**
- Per-tier model routing (Opus/Sonnet/Haiku mapping)
- Dynamic model discovery from upstream providers
- Full BPE tokenizer
- Rate limiting
- OpenAI Responses API / Codex support

## Architecture / Approach

Four independent additions to `src/main.rs` — no new modules, no shared state. Each is a small handler or guard function wired into the existing router. NIM sanitization plugs into the existing provider_type branching. All can be implemented in parallel.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. `/v1/models` | Static model list endpoint | None — trivial handler |
| 2. NIM sanitization | Strip unsupported fields before NIM forwarding | Incomplete strip-list (can expand later) |
| 3. `count_tokens` | Local token approximation | Inaccurate estimate (acceptable — fallback exists) |
| 4. Request optimizations | Short-circuit trivial probes | False positive on legitimate short requests |

**Prerequisites:** None — all phases are independent of each other and of S-16.
**Estimated effort:** ~1 session total across all 4 phases (each is 15-30 min).

## Open Risks & Assumptions

- `/v1/models` hardcoded list needs manual update when Anthropic releases new model names
- NIM strip-list may be incomplete — new unsupported fields could appear; expand as discovered
- chars/4 token estimate is ~20% off from real BPE; Claude Code uses it as a hint, not a hard decision

## Success Criteria (Summary)

- Claude Code with `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` connects without errors
- NIM-routed requests succeed even when client sends `top_k` or `thinking` fields
- Token count endpoint returns reasonable approximation without upstream calls
