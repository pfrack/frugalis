---
date: 2026-06-24T08:26:00+02:00
researcher: pfrack
git_commit: 1b7e81f
branch: anthropic-to-openapi
repository: cerebrum
topic: "Competitive gap analysis: FCC, ccm, freedius — what's missing in cerebrum"
tags: [research, codebase, competitive-analysis, providers, model-routing]
status: complete
last_updated: 2026-06-24
last_updated_by: pfrack
---

# Research: Competitive Gap Analysis — FCC, ccm, freedius

**Date**: 2026-06-24T08:26:00+02:00
**Researcher**: pfrack
**Git Commit**: 1b7e81f
**Branch**: anthropic-to-openapi
**Repository**: cerebrum

## Research Question

Compare cerebrum against free-claude-code (FCC), claude-code-switch (ccm), and freedius to identify missing features that would improve Claude Code compatibility and operational robustness.

## Summary

Cerebrum's intent-classification routing paradigm is architecturally different from FCC/ccm/freedius (which are model-switching proxies). Most "gaps" are deliberate design divergences, not missing features. After filtering out features that conflict with cerebrum's architecture, **4 genuinely relevant gaps** remain: a `/v1/models` stub endpoint, NIM field sanitization, `/v1/messages/count_tokens` stub, and local request optimizations for trivial probes.

## Competitors Analyzed

### free-claude-code (FCC) — github.com/Alishahryar1/free-claude-code
- **Stars**: 36.7k | **Language**: Python (FastAPI) | **Providers**: 17
- **Architecture**: Full proxy with Admin UI; translates Anthropic Messages API + OpenAI Responses API to any provider
- **Key differentiator**: Per-tier model routing (Opus/Sonnet/Haiku → different providers), native `/model` picker via `/v1/models`, Discord/Telegram bot, Codex support

### claude-code-switch (ccm) — github.com/foreveryh/claude-code-switch
- **Stars**: 633 | **Language**: Shell (100%) | **Providers**: 7 direct + OpenRouter
- **Architecture**: Client-side env var switcher — sets 7 env vars per provider (`ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_MODEL`, `ANTHROPIC_DEFAULT_OPUS_MODEL`, `ANTHROPIC_DEFAULT_SONNET_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL`, `CLAUDE_CODE_SUBAGENT_MODEL`)
- **Key differentiator**: Zero infrastructure, China/Global region variants, Claude Pro account switching, user/project-level settings override via `~/.claude/settings.json`

### freedius — github.com/pfrack/freedius
- **Stars**: 0 (private/personal) | **Language**: Go | **Providers**: 6 (nim, openai, anthropic, go, zen, custom)
- **Architecture**: Single-binary proxy with TUI dashboard; YAML config; family-prefix model matching
- **Key differentiator**: Protocol auto-detection (mix adapter), local BPE token counting, TUI config editing, request-ID tracking, NIM field sanitization

## Detailed Findings

### Features Irrelevant to Cerebrum's Paradigm (NOT gaps)

These are present in competitors but deliberately NOT needed in cerebrum because intent-classification routing overrides the client's model choice:

| Feature | Why irrelevant |
|---|---|
| Per-tier model routing (Opus/Sonnet/Haiku → different providers) | Cerebrum routes by classified intent, not by model name tier |
| Family-prefix model matching (`claude-sonnet-4-6-20250908` → `claude-sonnet-4-6`) | Model field is overridden by routing config |
| `CLAUDE_CODE_SUBAGENT_MODEL` env var | Cerebrum doesn't need to advertise sub-agent models — all requests get classified |
| OpenAI Responses API (`/v1/responses`) | Codex support — out of scope for cerebrum |
| Discord/Telegram bot | Out of scope |
| Voice notes | Out of scope |
| Claude Pro account switching | Client-side concern, irrelevant to proxy |
| China/Global region variants | User already configures endpoint URLs in routing config |

### Genuine Gaps (relevant to cerebrum's operation)

#### Gap 1: `/v1/models` endpoint (static stub)

**What**: Claude Code, when `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` is set, queries `GET /v1/models` to discover available models for the native `/model` picker.

**Impact**: Without it, Claude Code may error on probe or the picker is unavailable. Low severity (feature is opt-in via env var) but trivial to implement.

**Solution**: Return a static JSON response listing model names derived from the routing config's categories or a hardcoded list. The response format follows Anthropic's model list schema:
```json
{"data": [{"id": "claude-sonnet-4-6", "type": "model", ...}], "has_more": false}
```

**Competitors**: FCC has full dynamic model listing from provider catalogs. Freedius doesn't have it.

#### Gap 2: NIM field sanitization

**What**: NVIDIA NIM rejects requests containing unsupported fields like `top_k`, `thinking`, `metadata`, `system` (in some models). FCC and freedius both strip these before forwarding.

**Impact**: Requests routed to NIM may fail with HTTP 400 if Claude Code sends fields NIM doesn't support. This is a real bug users would hit.

**Solution**: When `provider_type == "nvidia_nim"`, strip known-unsupported fields from the translated request body before forwarding. The list: `top_k`, `top_p` (some models), `thinking`, `metadata`, provider-specific extras.

**Competitors**: FCC has dedicated NIM sanitization. Freedius has it (PR #7 "Opencode nim fixes").

#### Gap 3: `/v1/messages/count_tokens` endpoint

**What**: Claude Code may call `POST /v1/messages/count_tokens` to estimate token usage before sending a full request (used for context window management and auto-compaction decisions).

**Impact**: If cerebrum doesn't handle this, Claude Code falls back to internal estimation (not a hard failure). Low severity but would improve UX.

**Solution**: Either (a) proxy to upstream if provider supports it, or (b) return a local approximation (chars/4 heuristic or tiktoken-based count). Freedius uses local BPE counting for this.

**Competitors**: FCC proxies it. Freedius does local BPE. ccm doesn't handle it (client-side switcher).

#### Gap 4: Request optimizations (short-circuit trivial probes)

**What**: FCC answers certain trivial Claude Code requests locally without hitting the upstream provider. These are health-check-like probes, version queries, or empty/minimal requests that waste provider quota.

**Impact**: Saves latency and provider tokens on requests that don't need real LLM processing. Medium value — reduces noise in logs and saves quota.

**Solution**: Detect known trivial request patterns (very short prompts matching health-probe signatures) and return canned responses locally instead of routing upstream. This complements cerebrum's existing `SHORT_PROMPT_LEN` concept in the regex classifier.

**Competitors**: FCC has "request optimizations" module. Freedius and ccm don't.

## Architecture Insights

### Protocol coverage comparison

| Protocol path | cerebrum | FCC | freedius |
|---|---|---|---|
| Client speaks OpenAI → upstream OpenAI | ✅ passthrough | ✅ | ✅ |
| Client speaks OpenAI → upstream Anthropic | ✅ (S-15, done) | N/A | ✅ (mix adapter) |
| Client speaks Anthropic → upstream OpenAI | ✅ (S-16, in progress) | ✅ | ✅ (mix adapter) |
| Client speaks Anthropic → upstream Anthropic | ✅ passthrough | ✅ | ✅ |
| Client speaks OpenAI Responses → upstream any | ❌ (out of scope) | ✅ | ❌ |

### Auth handling comparison

Cerebrum's `auth_headers_for()` already handles all needed cases:
- `openai_compatible` → `Authorization: Bearer {key}`
- `anthropic` → `x-api-key: {key}` + `anthropic-version: 2023-06-01`
- `nvidia_nim` → `Authorization: Bearer {key}` (same as openai_compatible)
- `ollama` → no auth needed

This matches what FCC and freedius do. No gap here.

## Code References

- `src/intent_classifier.rs:424-470` — `auth_headers_for()` handles provider-specific auth headers
- `src/main.rs:1728` — Anthropic provider detection and translation trigger
- `src/config.rs:102-103` — default provider_type is "openai_compatible"
- `src/main.rs:2155` — OpenAI translation trigger (`provider_type != "anthropic"`)
- `routing_examples/routing-nvidia-nim.toml` — NIM routing config pattern

## Historical Context

- `context/changes/translate-anthropic-to-openai/plan-brief.md` — S-16 plan covers Anthropic→OpenAI translation (the main protocol gap is being closed)
- `context/archive/2026-06-22-translate-openai-to-anthropic/` — S-15 already done (OpenAI→Anthropic)
- `context/archive/2026-06-07-provider-agnostic-config/` — Provider routing config was generalized early

## Open Questions

- Should `/v1/models` pull model names from the routing config dynamically, or use a static hardcoded list?
- For NIM sanitization, should the field strip-list be configurable or hardcoded?
- Is `/v1/messages/count_tokens` worth implementing given Claude Code has internal fallback estimation?
