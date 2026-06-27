# Claude Code Compatibility — Plan Brief

> Full plan: `context/changes/claude-code-compat/plan.md`
> Research: `context/changes/competitive-landscape-gaps/research.md` (Tier-1 #3, #4)

## What & Why

Claude Code pairs each `anthropic-beta` header with a body field (`context_management`, `thinking`, `cache_control`); Cerebrum drops both halves today, silently disabling those features. Prompt caching is GA and free, yet Cerebrum strips `cache_control` on every translation path, and `/v1/models` returns the OpenAI shape with no `display_name`. This change makes Cerebrum a true drop-in for Claude Code so its existing 2,763-line bidirectional translator actually delivers value.

## Starting Point

Cerebrum already has full bidirectional OpenAI↔Anthropic translation (`src/protocol_translation.rs`) and both `/v1/chat/completions` + `/v1/messages` endpoints. But headers are captured and dropped (`src/main.rs:1783`, `:2189` never reach `build_upstream_request`), translation uses explicit allowlist insertion so unknown fields vanish, cache tokens aren't parsed, and `InferenceRecord` has no token/attribution columns.

## Desired End State

Point Claude Code at Cerebrum and everything works: prompt caching activates and reports real cache-hit tokens, beta features reach the upstream intact, model discovery shows friendly names, and the inference log reflects cache savings + per-session attribution.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Scope | Full (headers + cache_control + usage logging + /v1/models) | "Translate + log" usage choice re-expanded scope to include the observability migration | Plan (user) |
| Header forwarding | Open-list forward `anthropic-*`/`x-claude-code-*` to Anthropic upstreams only | Matches Anthropic's gateway guidance; future-proof against new betas | Research + Plan |
| `anthropic-beta` on cross-protocol | Do NOT forward to OpenAI upstreams; Cerebrum injects betas for features it adds | Betas are meaningless to OpenAI providers; avoids noise | Plan (user) |
| `anthropic-version` | Prefer client value, fall back to `2023-06-01` | Lets Claude Code pin newer versions without a Cerebrum change | Plan (user) |
| `cache_control` coverage | All four protocol crossings | Uniform behavior, no reliance on byte-passthrough luck | Plan (user) |
| Insertion strategy | Top-level automatic caching (`cache_control:{"type":"ephemeral"}`) on OpenAI→Anthropic | GA + simplest; no beta header, no block surgery | Research (verified GA) + Plan |
| Prompt-caching beta header | Not injected (caching is GA) | Verified against Anthropic docs | Research |
| Cache tokens | Translate for client visibility AND log to `InferenceRecord` | Accurate client usage + observability of cache savings | Plan (user) |
| `src/translate/` | Leave as dead stub; all work in `src/protocol_translation.rs` | Stub's submodule files don't exist; avoid resurrecting | Research |

## Scope

**In scope:** `/v1/models` Anthropic shape + `display_name`; open-list header forwarding; `anthropic-version` preference; `cache_control` translation/insertion across all crossings; cache-token usage translation (streaming + non-streaming); `InferenceRecord` token + session-id fields with DB migration.

**Out of scope:** Codex `/v1/responses` (separate change); response/semantic caching (separate change); learned router (enterprise); RBAC/budgets (enterprise); full error-envelope verbatim-forwarding audit.

## Architecture / Approach

Four phases, lowest-risk-first. Phase 1 is a trivial independent `/v1/models` win. Phase 2 threads inbound headers through `build_upstream_request` + `auth_headers_for` (signature change across 3 call sites). Phase 3 adds request-side `cache_control` handling. Phase 4 adds response-side cache-token translation and the `InferenceRecord` migration — the riskiest phase because streaming logs emit at stream-open before usage is known and must be restructured to finalize at stream-close.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. `/v1/models` shape | Anthropic entries with `display_name` | Breaking the unauthenticated discovery probe |
| 2. Header plumbing | Open-list forwarding + client `anthropic-version` | Auth-header override if forwarding order is wrong |
| 3. `cache_control` translation | Caching works on all 4 crossings | Same-protocol regression dropping unknown fields |
| 4. Usage tokens + logging | Client-visible cache tokens + logged attribution | Streaming-log finalization restructure (~20 call sites) |

**Prerequisites:** existing `protocol_translation.rs` (present); `provider-fallback-cascade` provider plumbing (implemented).
**Estimated effort:** ~4-6 sessions across 4 phases; Phase 4 is the bulk.

## Open Risks & Assumptions

- **Streaming-log restructure (Phase 4)** is the highest-risk change; `log_classification` is called from ~20 sites and the lessons.md history shows handlers get rewritten and regress. Mitigation: comprehensive httpmock matrix + the manual Claude Code E2E.
- **Error-body forwarding** is only lightly touched; a full audit of whether Cerebrum's `upstream_error` envelopes break Claude Code retry matching is deferred (flagged for follow-up).
- **Per-route cache-insert toggle** deferred — automatic insertion is unconditional this change.

## Success Criteria (Summary)

- Real Claude Code through Cerebrum shows `cache_read_input_tokens > 0` on repeated turns and correct client `usage`.
- `anthropic-beta` headers reach Anthropic upstreams unchanged; `/v1/models` shows `display_name`.
- Inference-log rows carry token counts + Claude Code session id; the 4-way httpmock matrix is green.
