# Provider-Agnostic Routing Configuration — Plan Brief

> Full plan: `context/changes/provider-agnostic-config/plan.md`
> Research: `context/changes/provider-agnostic-config/research.md`

## What & Why

Add `provider_type` and `api_key_env` fields to the routing data model so each intent category can route to a different LLM provider with its own API key and auth scheme. Currently all entries implicitly assume OpenRouter-compatible Bearer auth with a single `UPSTREAM_API_KEY`. This change makes the routing config final before SSE streaming (Change 4), supporting Anthropic (`x-api-key`), OpenAI-compatible (`Bearer`), and Ollama (no auth).

## Starting Point

- `RouteEntry` (`src/intent_classificator.rs:10-14`) has `model`, `endpoint`, `cost_per_1m_input_tokens` — no provider identity
- `ClassificationResult` (`src/intent_classificator.rs:56-61`) has `category`, `model`, `endpoint`, `tier` — no auth info
- `POST /v1/chat/completions` returns `{status, category, model, tier}` — no provider details for downstream use
- `routing.toml.example` hardcodes all endpoints to OpenRouter — last provider-specific assumption in the repo

## Desired End State

- Every `RouteEntry` carries `provider_type` (`"openai_compatible"` | `"anthropic"` | `"ollama"`) and `api_key_env` (env var name for API key)
- `ClassificationResult` propagates both fields so the handler can enrich the response
- `POST /v1/chat/completions` returns `{status, category, model, tier, endpoint, provider_type, api_key}` with lazy key resolution (null if env var missing)
- `POST /v1/classify` stays minimal: `{status, category, model, tier}`
- `auth_headers_for()` lookup function is defined in `intent_classificator.rs` for Change 4 to consume

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Response enrichment | Add endpoint, provider_type, and resolved api_key | Downstream proxy needs all three to forward upstream requests. | Plan |
| Enrichment scope | Only /v1/chat/completions, not /v1/classify | classify endpoint is for lightweight category lookups — injecting keys there is unnecessary. | Plan |
| Missing env var behavior | api_key: null in JSON, no error | Classification succeeds even with broken key config; downstream decides how to handle. | Plan |
| auth_headers_for location | intent_classificator.rs | Keeps provider-related logic in one module — single import for downstream. | Plan |
| Fallback TOML entry | Supports provider_type and api_key_env | Consistent — every entry, including fallback, can independently route to any provider. | Plan |
| API key resolution | In the handler (main.rs) | ClassificationResult stays a pure data struct; env access is at the handler layer. | Plan |
| TOML format | Flat single-level | Two-level `[providers.X]` deferred for MVP to keep parsing simple. | Research |
| Anthropic body translation | Deferred to Change 4 | Anthropic has a different request body schema; provider_type is defined but routing will error until adapter is added. | Research |
| extra_headers | Deferred | OpenRouter-specific headers not configurable in this change. | Research |

## Scope

**In scope:**
- `RouteEntry` and `ClassificationResult` gain `provider_type` + `api_key_env` fields
- `load_routing_from_file` parses both from TOML
- `auth_headers_for` lookup function defined in `intent_classificator.rs`
- `completion_handler` resolves key and builds enriched JSON
- `routing.toml.example` updated to provider-agnostic format
- All constructors and test fixtures updated

**Out of scope:**
- Anthropic body translation
- `extra_headers`, timeout, retry config per provider
- Two-level TOML design
- Changing /v1/classify response format
- Actual upstream proxying (Change 4)

## Architecture / Approach

Bottom-up: data model → TOML parsing → handler enrichment → config/tests. The `auth_headers_for` function is defined but not called yet — it's ready for Change 4's upstream proxy. Key resolution is lazy (per-request `std::env::var` lookup), not preloaded at startup. Both handlers diverge: `completion_handler` builds its own enriched response, while `classify_handler` continues using the existing `classify_and_log` helper for minimal output. A shared `log_classification` helper eliminates code duplication for the fire-and-forget inference record.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Core Data Model | RouteEntry + ClassificationResult with new fields; auth_headers_for defined | All constructors must be updated — missing one causes compile error |
| 2. TOML Parsing | load_routing_from_file reads provider_type + api_key_env | Backward compat: old TOML files without new fields must still parse |
| 3. Handler Enrichment | /v1/chat/completions returns enriched JSON; shared logging helper | classify_and_log refactoring must not break /v1/classify |
| 4. Config & Test Fixtures | routing.toml.example updated; all test constructors updated; 3 new tests | Test discovery must cover all RouteEntry construction sites |

**Prerequisites:** Phase 1-2 (data model + parsing) must complete before Phase 3 (handler needs the parsed fields). Phase 4 can follow any time after Phase 1.

**Estimated effort:** ~2 sessions across 4 phases

## Open Risks & Assumptions

- The `UPSTREAM_API_KEY` env var (Change 2) becomes vestigial after this change — deployments must be updated to set per-provider env vars
- `provider_type = "anthropic"` routes will return enriched responses but fail when Change 4 tries to forward requests (body schema mismatch until the adapter is added)

## Success Criteria (Summary)

- `RouteEntry` and `ClassificationResult` carry provider_type and api_key_env — verified via compile and manual inspection
- `/v1/chat/completions` returns enriched JSON with endpoint, provider_type, and api_key — verified via automated test
- Missing env var produces `api_key: null` — verified via automated test
- `/v1/classify` returns minimal JSON unchanged — verified via automated test
- All existing tests pass — verified via `cargo test`
