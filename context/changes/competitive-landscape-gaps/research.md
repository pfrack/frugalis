---
date: 2026-06-25T23:21:10+02:00
researcher: opencode (glm-5.2)
git_commit: d5878aa5ecfab07a14793e13a673f860dddc0382
branch: provider-failover-chain
repository: cerebrum
topic: "Alternatives & gap analysis: what Cerebrum lacks vs. the LLM-gateway/observability/client-integration landscape"
tags: [research, codebase, competitive-analysis, llm-gateway, observability, cost, client-integration, routing]
status: complete
last_updated: 2026-06-25
last_updated_by: opencode (glm-5.2)
---

# Research: Alternatives & Gap Analysis — what Cerebrum lacks

**Date**: 2026-06-25T23:21:10+02:00
**Researcher**: opencode (glm-5.2)
**Git Commit**: d5878aa5ecfab07a14793e13a673f860dddc0382
**Branch**: provider-failover-chain
**Repository**: cerebrum

## Research Question

Research the broader landscape of LLM-infrastructure alternatives (gateways/routers, observability/cost platforms, and AI-coding-agent integration requirements) to identify what Cerebrum is lacking, so the team can prioritize which gaps to close vs. which to accept as deliberate niche divergence.

## Methodology

Four parallel research tracks:
1. **Codebase baseline** — precise catalog of Cerebrum's current capabilities (file:line grounded) to define the "current state" for gap analysis.
2. **Gateway/router landscape** — official docs for LiteLLM, Portkey, OpenRouter, RouteLLM, Helicone (fetched Jun 2026).
3. **Observability/cost platforms** — official docs for Langfuse, LangSmith, Helicone, Braintrust, Arize Phoenix, Lunary.
4. **Client/tool integration** — official docs for Claude Code (gateway protocol), Codex CLI, Cursor, Cline/Roo/Aider/Continue, MCP.

## Summary

Cerebrum occupies a defensible but narrow niche: a **Rust/Axum single-binary** with **explicit, auditable intent classification** (regex→fewshot→LLM chain) and **real bidirectional OpenAI↔Anthropic protocol translation** (incl. streaming + tools + reasoning). That combination is unique. However, against the mature gateway/router market it is missing nearly every feature that buyers now consider **table-stakes**:

- **No caching at all** (esp. semantic caching) — universal across all 5 gateways surveyed.
- **No per-provider retries/backoff/cooldowns/circuit-breakers/health-checks** — only static failover-to-next-provider exists.
- **No multi-tenant surface**: single global bearer token, no per-user API keys, no RBAC, no budgets/quotas, no audit logs, no per-client rate limiting.
- **No guardrails** (PII redaction, prompt-injection detection, JSON-schema validation, deny semantics).
- **Cerebrum's core differentiator (intent classification) is the *primitive* version** of an approach the industry has commoditized: RouteLLM (BERT/matrix-factorization/similarity-weighted-Elo routers trained on human-preference data, 85% cost reduction at 95% quality), OpenRouter's NotDiamond-powered Auto Router (with a 0–10 cost-quality dial), and Portkey's composable JSON-DSL conditional routing.
- **Observability is 1-dimensional**: a single "savings vs baseline" number, where competitors slice cost by user/session/feature/model/prompt-version, with multi-step agent tracing, automated LLM-as-a-judge evals, datasets/experiments, prompt management, and alerting.
- **Client integration has two hard blockers**: modern **Codex CLI cannot use Cerebrum at all** (it now speaks only the Responses API `/v1/responses`, not Chat Completions), and **Claude Code drop-in is incomplete** because Cerebrum strips `anthropic-beta` headers and `cache_control` blocks, silently disabling context management, interleaved thinking, extended context, and prompt caching.

**The strategic question** is not "build all of this" but "which subset preserves Cerebrum's niche while removing the deal-breakers." The highest-leverage gaps (routing credibility, Claude Code/Codex drop-in, semantic caching, per-provider reliability) are recommended in the prioritized backlog below.

---

## Detailed Findings

### A. Cerebrum's current capability baseline (the "before" picture)

Grounded in the codebase at `d5878aa`:

**Routing intelligence** — 3 backends in a `ClassifierChain` (`intent_classifier.rs:147-180`): `RegexClassifier` (weighted patterns + negative suppression + `dual_threshold`, `intent_classifier.rs:132`, `:604-700`), `FewShotClassifier` (bag-of-words cosine similarity against per-category centroids with cold-start logic, `fewshot_classifier.rs:15`, `:99-114`), and `LLMClassifier` (separate completion call, `max_tokens=20`/`temperature=0.0`, `intent_classifier.rs:187`). First non-`Fallback` result wins. `/v1/feedback` (`main.rs:2538-2587`) genuinely retrains the few-shot backend (`fewshot_classifier.rs:197-269`). Manual override via `X-Cerebrum-Category`/`X-Cerebrum-Model` headers (`main.rs:1817-1876`).

**Reliability** — static provider cascade: each category carries a `providers: Vec<ProviderEntry>`; both handlers iterate in order and skip to the next provider on retryable errors (connect/timeout/429/5xx, `is_retryable_error` at `main.rs:1029-1037`). No same-provider retries, no backoff, no cooldowns, no circuit breaker, no health checks.

**Protocol** — `POST /v1/chat/completions`, `POST /v1/messages`, `POST /v1/messages/count_tokens` (chars/4 heuristic, `main.rs:762`), `GET /v1/models` (hardcoded 3 Claude IDs, `main.rs:864`), `POST /v1/classify`, `POST /v1/feedback`, `GET /health`. Full bidirectional translation in `protocol_translation.rs` (2763 lines): text, images via data URIs, tool_calls↔tool_use, reasoning_content↔thinking.

**Observability** — 4 dashboard pages (`dashboard.rs:349-356`): overview, inferences (200-char snippet + char count), per-intent latency avg+p99, savings vs baseline. 3 persistence backends (memory/sqlite/postgres, `persistence.rs:219-222`). OTel feature-gated (`telemetry.rs`): 4 instruments (request counter, request/upstream duration histograms, classification counter) over OTLP/HTTP. No alerting, no per-tenant views, no token/cost counters in OTel.

**Auth/enterprise** — single global bearer token + single basic-auth credential (`auth.rs`), constant-time compared. No multi-tenancy, RBAC, per-user keys, budgets, audit logs, or per-client rate limiting.

**Client integration** — OpenAI drop-in + Anthropic Messages drop-in. But `build_upstream_request` **unconditionally rewrites the `"model"` field** to the routed provider model (`main.rs:1054-1058`); the client's requested model is ignored for routing. No MCP support, no SDKs.

### B. Gateway/router landscape gaps

#### LiteLLM (BerriAI) — open-source incumbent
- **6 load-balancing routing strategies**: simple-shuffle (weighted), usage-based-routing-v2 (TPM/RPM-aware), latency-based, least-busy, cost-based, plus custom strategies. **Routing Groups** bind a different strategy per model alias.
- **Typed fallbacks**: `fallbacks`, `context_window_fallbacks`, `content_policy_fallbacks`, `default_fallbacks`; per-deployment **cooldowns** (`allowed_fails` + `cooldown_time`, `AllowedFailsPolicy`); retries w/ exponential backoff; weighted failover; `max_parallel_requests`; pre-call context-window checks. Redis-backed for multi-pod consistency.
- **Caching**: in-memory, disk, Redis, S3, GCS + **semantic caches** (Qdrant, Redis-semantic, Valkey-semantic with cosine `similarity_threshold`). Per-request `ttl`/`no-cache`/`namespace` controls.
- **Cost**: budgets at global/team/team-member/virtual-key/customer/agent levels; multiple concurrent budget windows per key; model-specific budgets; per-key TPM/RPM; virtual keys; SSO/SAML/audit logs (enterprise).
- **Notable**: 100+ providers, guardrails, prompt-injection detection, MCP/Agent gateway, traffic mirroring/A-B testing, Admin UI.
- **Cerebrum gap**: no load-balancing strategies (only intent→category map); no typed fallbacks/cooldowns/retry policies; no caching at all; no budgets/keys/spend tracking; no pre-call checks; no Admin UI.

#### Portkey — richest routing DSL
- **Three composable, nestable strategies**: `conditional`, `loadbalance`, `fallback` — any target can itself be another strategy (5 documented composition patterns). **Conditional routing uses a JSON-path query DSL** over request `params` and `metadata` (`{"params.model": {"$eq": "..."}}`) — the industrial-strength counterpart to Cerebrum's single-shot regex classifier.
- **Guardrails** (major differentiator): 20+ deterministic checks (regex, JSON-schema, PII redaction) + LLM checks (gibberish, prompt-injection); 7 action types incl. **Deny (446)** / **log-only (246)**; guardrail verdicts can trigger fallback/retry.
- **Caching** (simple + semantic), retry w/ per-status-code control, prompt management/versioning, virtual keys, analytics API.
- **Cerebrum gap**: no composable routing strategies or JSON-DSL conditional routing; no guardrails; no semantic/simple caching; no prompt versioning; no observability analytics.

#### OpenRouter — routing-as-a-service
- **Two routing layers**: provider-level (price-inverse-square load balancing, `:nitro`/`:floor`/`:thinking` variants, percentile SLA cutoffs `preferred_min_throughput`/`preferred_max_latency` over rolling 5-min windows, `max_price` cap) and model-level routers.
- **Model-level routers (the direct competitor to Cerebrum)**: **Auto Router** (`openrouter/auto`, powered by **NotDiamond**, analyzes prompt complexity with a `cost_quality_tradeoff` 0–10 knob), **Pareto Router** (coding-score threshold), **Fusion Router** (panel + judge), Free/Latest/Exacto routers.
- Automatic multi-provider failover; **zero-completion insurance**; response + prompt caching; workspace budgets; regex prompt-injection/PII guardrails; custom classifiers; public pricing leaderboard.
- **Cerebrum gap**: no prompt-complexity ML routing (NotDiamond/Pareto/Fusion); no price/latency/throughput provider selection or percentile SLAs; no caching; no workspace budgets/guardrails; no published benchmarks/leaderboard.

#### RouteLLM (lm-sys) — research-grade router
- Open-source framework; "reduce costs up to 85% while maintaining 95% of GPT-4 performance." Four trained routers: `mf` (matrix factorization), `sw_ranking` (similarity-weighted Elo — **semantic/embedding routing**), `bert` (BERT classifier), `causal_llm` (fine-tuned LLM). **Threshold calibration** CLI against Chatbot Arena data for a target `% strong-model calls`.
- **Cerebrum gap**: Cerebrum's regex/fewshot classifier is the *naive predecessor*. The SOTA has moved to MF/BERT/LLM-judge/similarity-Elo trained on human-preference data, with empirical cost-quality benchmarks. Cerebrum has no learned router, no calibration, no eval framework, no published benchmarks.

#### Helicone — observability-first with a managed gateway
- **Automatic provider routing**: cheapest-first, equal-cost load-balance, instant failover on 429/401/400/408/500+. Manual chains via model string (`m1/azure,m1/openai,m1`).
- **Gateway features Cerebrum lacks**: caching + prompt caching + context-editing (middle-out truncation for long agent sessions), retry headers, billing-method fallback.
- **Observability moat**: request logs, sessions, user metrics, custom properties, **alerts** (graduated 50/80/95% budget alerts), HQL SQL queries, webhooks, datasets, eval scores, fine-tuning export, prompt management. MCP server.
- **Cerebrum gap**: no provider-level routing/failover; no caching/context-editing; observability is a basic dashboard + inference logging only; no alerts; no prompt management.

#### State of the art for "LLM routing" (2024–2025)
Two distinct meanings of "routing" now coexist:
- **Provider/load-balancing routing** (LiteLLM, Portkey loadbalance, OpenRouter, Helicone) — pick *which deployment* of a *fixed model*. Mature, table-stakes.
- **Prompt-level/model routing** (RouteLLM, OpenRouter Auto/Pareto/Fusion) — pick *which model* by prompt content/complexity. **This is Cerebrum's category**, and intent classification (regex/rules) is the *baseline/naive* approach. SOTA = learned classifiers on preference data, embedding/semantic similarity, LLM-judge/panel, with **cost/quality as a tunable dial** (not a static map) and published benchmarks.

### C. Observability & cost platform gaps

Cross-cutting matrix (✅ = has it):

| Capability | Cerebrum | Langfuse | LangSmith | Helicone | Braintrust | Phoenix |
|---|---|---|---|---|---|---|
| Per-intent latency avg/p99 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Cost: savings-vs-baseline only | ✅ (1 #) | full slice | ✅ | ✅ +session econ | ✅ | ✅ |
| Multi-step / agent tracing | ❌ | ✅ sessions+graphs | ✅ | ✅ session trees | ✅ | ✅ tool/retrieval spans |
| Evals (LLM-judge/code/human) | ❌ | ✅ | ✅ | partial | ✅ first-class | ✅ |
| Datasets + experiments / A-B | ❌ | ✅ | ✅ | ❌ | ✅ | ✅ |
| Prompt management (versioned) | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Alerting (cost/latency/error) | ❌ | ✅ | ✅ | ✅ graduated | ✅ | partial |
| Budget/spend forecasting | ❌ | partial | partial | ✅ | partial | ❌ |
| OTel export | ✅ | ✅ (native basis) | ❌ | ❌ | ❌ | ✅ (OTLP native) |

**OTel-native vs. purpose-built**: Phoenix and Langfuse are *themselves built on OTel/OpenInference* — so OTel export is the right **transport substrate**, not a competitor. Cerebrum exporting OTel means it can already feed Phoenix/Langfuse/Jaeger for free. What OTel alone does NOT give: LLM-specific span semantics (token counts, cost, prompt I/O as structured fields), session grouping across multi-turn agents, eval pipelines, prompt versioning, datasets/experiments, slice-by-user cost analytics. To match competitors, Cerebrum would need to emit richer LLM-span semantics (OpenInference/OpenLLMetry attribute conventions) and either build the app-layer features or position as a thin OTel-emitting gateway that feeds Langfuse/Phoenix.

### D. Client/tool integration gaps

This is the area with the most concrete, high-impact blockers.

#### Claude Code (gold-standard contract — `docs.claude.com/.../llm-gateway-protocol`)
- Points via `ANTHROPIC_BASE_URL`; speaks Anthropic Messages API at `/v1/messages` (must stream SSE). Cerebrum's `/v1/messages` is the right shape.
- **Critical rule**: forward `anthropic-version` and `anthropic-beta` **unchanged as an open list** (never allowlist); never rewrite request bodies for inspection. Capabilities ship as beta-header + body-field *pairs* — splitting them yields `400`s or silent feature disablement.
- **Confirmed Cerebrum gaps** (0 matches in `src/`):
  - `anthropic-beta` header pass-through — **missing**. Breaks/disables context management, interleaved thinking, extended context, beta tool fields, effort/structured-outputs. **#1 Claude Code gap.**
  - `cache_control` prompt-caching blocks — **missing**. Caching silently ineffective across the translation boundary.
  - `context_management`, `output_config` body fields — **missing**. Forwarding to a non-Anthropic upstream yields `400 Extra inputs are not permitted`.
  - `x-claude-code-session-id`/`-agent-id` attribution capture — **missing**. Loses free per-developer/per-agent cost attribution Cerebrum's logging is built to capture.
  - `display_name` + `claude`/`anthropic`-prefixed IDs in `/v1/models` — discovery filter requires `claude*`/`anthropic*` IDs.
  - Error-body verbatim forwarding — **partial**; Cerebrum emits `upstream_error` envelopes that may break Claude Code's retry/recovery matching.
- **Architectural risk**: Claude Code runs a **5-minute idle watchdog** on non-first-party hosts; a slow Cerebrum-routed provider can trip it.

#### OpenAI Codex CLI — the Responses-API curveball
- `wire_api` now accepts **only `responses`** — the OpenAI **Responses API** (`POST /v1/responses`). *"`responses` is the only supported value."* Codex CLI **no longer speaks Chat Completions.**
- **Cerebrum gap**: no `/v1/responses` endpoint. **Codex CLI cannot use Cerebrum at all today.** This is the single biggest OpenAI-side gap — implementable as a translation layer on top of the existing `/v1/chat/completions` core (reasoning items ↔ `reasoning_content`, tool-call items ↔ `tool_calls`, SSE event translation).

#### Cursor / Cline / Roo Code / Aider / Continue — easy wins
- All comfortably served by `/v1/chat/completions` (OpenAI-compatible) and/or `/v1/messages` (Anthropic) with streaming + tool calls. Cline's "OpenAI Compatible" provider is 3 fields (Base URL + API Key + Model ID); its Anthropic provider has an explicit "Use custom base URL" checkbox. Aider uses `OPENAI_API_BASE`. **No code change needed** for these, assuming streaming/tool-calls work end-to-end.

#### MCP — orthogonal
- A gateway does **not** need to be an MCP server/client to route `/v1/messages` or `/v1/chat/completions`. MCP is a separate tool-exposition channel between agent and tool servers. **No relevance for drop-in compatibility.** (One niche detail: non-first-party `ANTHROPIC_BASE_URL` disables Claude Code's "MCP tool search" unless `ENABLE_TOOL_SEARCH=true`.)

---

## Prioritized Gap Backlog

Ranked by leverage (competitive frequency × user impact × strategic fit with Cerebrum's Rust/auditable-routing niche):

### Tier 1 — Deal-breakers / table-stakes (do these first)

| # | Gap | Who has it | Why it matters | Suggested change-id |
|---|---|---|---|---|
| 1 | **Semantic + exact-match caching** | LiteLLM, Portkey, OpenRouter, Helicone (universal) | Universal across all gateways; biggest cost lever for repeatable agent prompts; Cerebrum is the *only* one with zero caching. | `add-response-cache` |
| 2 | **Per-provider retries + backoff + cooldowns** | LiteLLM, Portkey, OpenRouter, Helicone | Currently a failed provider is tried exactly once. Same-provider retry w/ exponential backoff + cooldown is reliability table-stakes. (Note: `provider-fallback-cascade` is in-progress — verify it adds retries, not just failover.) | extend `provider-fallback-cascade` |
| 3 | **`anthropic-beta` + header open-list pass-through** (Claude Code) | Anthropic gateway contract | #1 Claude Code gap; closes context-management, interleaved-thinking, extended-context, beta-tool-field blockers with one policy change. | `claude-code-header-passthrough` |
| 4 | **`cache_control` prompt-caching translation** (Claude Code) | Anthropic API | Caching is silently ineffective across the translation boundary; a real cost/latency win for Claude Code traffic. | `prompt-cache-translation` |
| 5 | **`/v1/responses` shim** (Codex CLI) | OpenAI Responses API | Without it, modern Codex CLI cannot use Cerebrum at all. Implementable atop existing translator. | `codex-responses-api` |

### Tier 2 — Credibility & differentiation

| # | Gap | Who has it | Why it matters |
|---|---|---|---|
| 6 | **Learned/embedding prompt router** (vs regex) | RouteLLM, OpenRouter (NotDiamond) | Cerebrum's core value prop is the *primitive* version. An embedding-similarity or calibrated-threshold router (à la RouteLLM `sw_ranking`/`mf`) + published benchmark would restore credibility. |
| 7 | **Cost/quality dial** | OpenRouter, RouteLLM | A single calibrated threshold knob (`cost_quality_tradeoff` 0–10) replaces the static category→model map and is the modern expectation. |
| 8 | **Multi-step agent tracing** (OTel span semantics) | Langfuse, Phoenix, Helicone | Single biggest structural observability gap. Cerebrum already exports OTel — enrich spans with OpenInference attributes (token counts, cost, prompt I/O) to feed Phoenix/Langfuse. |
| 9 | **Slice-by-user/feature cost analytics** | Helicone, Langfuse | Replace the single "savings vs baseline" number with per-developer/per-agent cost (enabled by capturing `x-claude-code-*` attribution headers — gap #3's logging side). |
| 10 | **Guardrails** (PII, prompt-injection, JSON-schema) | Portkey, OpenRouter, LiteLLM | Especially relevant since Cerebrum already inspects prompt text for classification — the regex engine could double as a guardrail layer. |

### Tier 3 — Enterprise / smaller leverage

| # | Gap | Who has it | Why it matters |
|---|---|---|---|
| 11 | Per-user API keys + budgets/quotas | LiteLLM, Portkey, OpenRouter | Unlocks multi-developer/team use; pairs with attribution capture. |
| 12 | Alerting (cost/latency/error) | Helicone, LangSmith, Langfuse | Cerebrum has the data; just needs thresholds + notification sink. |
| 13 | Datasets/experiments + automated evals (LLM-as-judge) | Langfuse, Braintrust, Phoenix | Validate routing decisions offline; regression-test routing quality. |
| 14 | Prompt management/versioning | Langfuse, Portkey, Helicone | Centrally-stored, deploy-without-code-change prompts. |
| 15 | `/v1/models` driven by routing + `display_name` | OpenRouter, Anthropic discovery contract | Replace hardcoded 3 Claude IDs; satisfy Claude Code discovery filter. |
| 16 | Circuit breaker / upstream health checks | LiteLLM | Proactive provider health, not just reactive failover. |
| 17 | Real tokenizer for `count_tokens` | all | Replace chars/4 heuristic. |

---

## Code References

- `src/intent_classifier.rs:132` — `RegexClassifier`; `:147-180` `ClassifierChain`; `:187` `LLMClassifier`; `:604-700` dual_threshold.
- `src/fewshot_classifier.rs:15` — `FewShotClassifier`; `:99-114` cosine scoring; `:197-269` `add_feedback` retrain path.
- `src/main.rs:1029-1037` — `is_retryable_error` (429/5xx/timeout/connect); `:1054-1058` mandatory model-field rewrite; `:1817-1876` `X-Cerebrum-Category` override; `:2538-2587` feedback handler; `:2589-2636` router assembly.
- `src/protocol_translation.rs` — 2763-line bidirectional translator (request/response/error/stream for both directions).
- `src/persistence.rs:219-222` — 3 backends; `:1107-1118` `InferenceRecord` (200-char snippet).
- `src/dashboard.rs:349-356` — 4 routes; `:44-65` `PAGES`.
- `src/auth.rs:11,38-45` — single bearer + single basic-auth; `:169-195` constant-time HMAC.
- `src/telemetry.rs:29-34,42-49` — 4 OTel instruments, feature-gated.
- `config.toml:64-185` — categories/patterns; `:205-224` auth providers.

## Architecture Insights

1. **Cerebrum's defensible niche** (if pursued): the only product combining (a) Rust/Axum performance/footprint, (b) explicit, *auditable* intent classification (regex/fewshot — appealing for compliance/debuggability where ML routers are black boxes), and (c) self-hostable single-binary. The risk: every Tier-1 gap is a feature buyers currently get elsewhere by default, and the core "route by prompt" value is being commoditized by learned routers with published benchmarks.
2. **Protocol translation is Cerebrum's strongest asset** — 2763 lines of bidirectional OpenAI↔Anthropic translation including streaming/tool-use/reasoning is rare in the OSS gateway space (LiteLLM translates but Portkey's edge workers and Helicone do not do per-prompt classification). This asset is currently *undermined* by header/body stripping that breaks Claude Code feature pairs — closing gaps #3/#4 unlocks the full value of translation work already done.
3. **Two strategic forks**: (A) lean into the auditable-routing niche — add caching + reliability + Claude Code/Codex drop-in and position as the "debuggable, self-hostable cost router"; (B) compete head-on by adding a learned router + cost/quality dial + benchmarks. Fork A is far lower effort and plays to existing strengths.
4. **OTel is an asset, not a liability** — Cerebrum's OTLP export lets it feed Phoenix/Langfuse for free. Rather than rebuilding observability app-layers, the high-ROI move is enriching span semantics (OpenInference attrs) and partnering with OTel-native backends.

## Historical Context (from prior changes)

- `context/changes/competitive-gap-model-routing/research.md` — narrower prior research comparing Cerebrum to Claude Code *switching* proxies (FCC, ccm, freedius). Concluded most "gaps" were deliberate architectural divergences; 4 genuine gaps (`/v1/models` stub, NIM sanitization, count_tokens stub, trivial-probe optimization) were implemented. This broader research complements it by covering the enterprise gateway/observability landscape the prior research did not.
- `context/changes/provider-fallback-cascade/` (in-progress on the current branch `provider-failover-chain`) — directly addresses Tier-1 gap #2 (partial). Verify it adds same-provider retries/backoff/cooldowns, not just next-provider failover.
- `context/foundation/roadmap.md` S-17 (`provider-fallback-cascade`, status: planning) — the planned scope matches Tier-1 #2.
- `context/foundation/lessons.md` — "Handle upstream error bodies without full buffering" and "Log operational failures before falling back" are directly relevant to implementing retries/cooldowns and Claude Code's verbatim-error-forwarding requirement.

## Related Research

- `context/changes/competitive-gap-model-routing/research.md` — Claude Code switching-proxy competitors.
- `context/changes/provider-fallback-cascade/` — in-progress reliability work (Tier-1 #2).
- `context/changes/translate-anthropic-to-openai/research.md` and `context/changes/translate-openai-to-anthropic/research.md` — protocol translation foundations that Tier-1 #3/#4/#5 build on.

## Open Questions

1. **Strategic fork**: lean into the auditable-routing niche (Fork A) or build a learned router to compete head-on (Fork B)? This determines whether Tier-2 #6/#7 are in-scope.
2. **Caching backend choice**: in-memory (simple, ephemeral) vs Redis (matches LiteLLM, multi-pod) vs SQLite? Affects effort for Tier-1 #1.
3. **`/v1/responses` scope**: full Responses API fidelity, or a Chat-Completions-backed shim sufficient for Codex CLI's streaming/tool-call paths? Affects Tier-1 #5 effort.
4. **Observability build-vs-feed**: build slice-by-user cost analytics in the dashboard, or emit OpenInference spans and direct operators to Phoenix/Langfuse? Affects Tier-2 #8/#9.
5. Does `provider-fallback-cascade` (in-progress) already cover same-provider retries/cooldowns, or only next-provider failover? If only failover, Tier-1 #2 remains open.
