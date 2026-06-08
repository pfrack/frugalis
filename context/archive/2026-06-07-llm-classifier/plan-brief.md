# LLM Classifier Backend (S-09) — Plan Brief

> Full plan: `context/changes/llm-classifier/plan.md`
> Research: `context/changes/llm-classifier/research.md`

## What & Why

Implement a second `IntentClassify` backend that sends prompts to a cheap LLM (e.g., `gpt-4o-mini`) for classification when the regex classifier is uncertain. This eliminates the "everything ambiguous → CASUAL" gap, improving routing accuracy for prompts that don't match clear regex patterns.

## Starting Point

The `IntentClassify` trait, `ClassifierChain` (fallback iteration), `CategoryConfig` (shared category definitions), and shared `reqwest::Client` are all in place. The chain currently holds only `RegexClassifier`. Adding a second backend is a matter of implementing the trait and pushing it into the vec.

## Desired End State

When `[llm_classifier]` is present and enabled in `config.toml`, ambiguous prompts that regex can't confidently classify get sent to a cheap LLM. The LLM returns one of the 4 known category names, and the chain routes accordingly. On any failure, the system silently falls back to CASUAL — zero user impact.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Async bridging | `Handle::current().block_on()` | Simplest approach, no new deps, acceptable since it only fires on fallback path | Plan |
| Error handling | Return `Fallback` → CASUAL | Matches chain semantics; silent degradation with warn-level logging | Plan |
| Timeout | 3 seconds | Fast enough to not block UX, generous enough for most providers | Plan |
| Prompt template | Hardcoded + optional file override | Good default from CategoryConfig; escape hatch for operators | Plan |
| API format | Chat completions (system + user) | Matches OpenAI-compatible format already used by the proxy | Plan |
| Few-shot examples | 4 (one per category) | Minimal tokens, clear signal; edge cases handled by regex primary | Plan |
| Configuration | `[llm_classifier]` TOML section | Grouped, discoverable, consistent with config.toml structure | Plan |

## Scope

**In scope:**
- `LlmClassifierConfig` struct + TOML parsing
- `LLMClassifier` struct implementing `IntentClassify`
- Prompt generation from `CategoryConfig` + few-shot examples
- Chain wiring in `main.rs` (conditional second backend)
- Unit + integration tests with `httpmock`

**Out of scope:**
- Response caching
- New `ClassificationTier::Llm` variant
- Retry logic
- Streaming classification response
- S-09a config boundary formalization

## Architecture / Approach

```
Request → ClassifierChain
           ├─ RegexClassifier (Tier 1, ~0ms)
           │   └─ Returns Regex tier on match, Fallback on ambiguous
           └─ LLMClassifier (Tier 2, ~200-500ms, only fires on Fallback)
               ├─ Builds prompt from CategoryConfig descriptions
               ├─ POST to LLM endpoint (chat completions format)
               ├─ Parses category name from response
               └─ Returns Regex tier on success, Fallback on any error
```

Config lives in `config.toml`:
```toml
[llm_classifier]
enabled = true
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
provider_type = "openai_compatible"
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Config | `[llm_classifier]` TOML parsing + struct | None — follows existing pattern |
| 2. Implementation | `LLMClassifier` struct + `IntentClassify` impl | Prompt/response parsing edge cases |
| 3. Chain Wiring | Conditional second backend in chain | Ordering correctness (regex must be first) |
| 4. Testing | Integration tests with mock HTTP | Mock fidelity to real provider responses |

**Prerequisites:** S-07 (IntentClassify trait) ✓, S-07b (CategoryConfig) ✓, `httpmock` in dev-dependencies ✓
**Estimated effort:** ~2 sessions across 4 phases

## Open Risks & Assumptions

- `Handle::current().block_on()` blocks a Tokio worker thread — acceptable for fallback path but could be a concern under very high concurrency
- Assumes LLM providers consistently return category names in `choices[0].message.content` — may need normalization (trim, uppercase)
- 3s timeout may be tight for cold-start serverless providers (will gracefully fallback)

## Success Criteria (Summary)

- Ambiguous prompts that regex can't classify now get routed to the correct category via LLM
- Zero regression: existing regex classification and all tests continue to pass
- Graceful degradation: LLM failure/timeout silently falls back to CASUAL with no user-visible error
