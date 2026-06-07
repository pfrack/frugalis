---
date: 2026-06-07T14:12:22+02:00
researcher: pfrack
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "LLM Classifier Backend (S-09) — research for implementing LLMClassifier implementing IntentClassify"
tags: [research, llm-classifier, intent-classify, classifier-chain, s-09, roadmap]
status: complete
last_updated: 2026-06-07
last_updated_by: pfrack
last_updated_note: "Added follow-up research on shared category configuration between RegexClassifier and LLMClassifier"
---

# Research: LLM Classifier Backend (S-09)

**Date**: 2026-06-07T14:12:22+02:00
**Researcher**: pfrack
**Git Commit**: 792b2618299caf26c5adabf28293e2eb1bafc836
**Branch**: provider-url-derivation
**Repository**: cerebrum

## Research Question

What is the current state of the classifier architecture, and what is needed to implement the `LLMClassifier` backend (roadmap S-09) — an `IntentClassify` implementation that sends prompts to a small/cheap LLM for intent classification?

## Summary

**S-09's prerequisite S-07 (IntentClassify trait) is already fully implemented in the live codebase.** The `IntentClassify` trait, `ClassifierChain`, and all integration points in `AppState`/`completion_handler` are in place. The codebase is architecturally ready for an `LLMClassifier` backend. Key findings:

1. **The trait exists** at `src/intent_classifier.rs:79` with `classify(&self, &str) -> ClassificationResult` and `get_routing()` returning `None` by default — exactly what an LLM classifier needs.
2. **`ClassifierChain` exists** at `src/intent_classifier.rs:113`, already iterating backends and falling through on `ClassificationTier::Fallback`.
3. **`AppState` already holds `classifier: Option<Arc<ClassifierChain>>`** (line 31 of `src/main.rs`) and merges routing from all backends.
4. **`reqwest::Client` is already shared** via `AppState.http_client` — the LLM classifier can receive it at construction time.
5. **The road from code to S-09 is short**: define `LLMClassifier` struct with config, implement `IntentClassify`, add a `from_env()` constructor, and wire it as a second backend in `main()`.

## Detailed Findings

### 1. Current Classifier Architecture (S-07 — Already Implemented)

The roadmap shows S-07 (intent-classifier-trait) as "proposed" but it was already delivered and archived to `context/archive/2026-06-06-intent-classifier-trait/`. The live code reflects this.

**`IntentClassify` trait** (`src/intent_classifier.rs:78-87`):
```rust
pub trait IntentClassify {
    fn classify(&self, prompt: &str) -> ClassificationResult;
    fn get_routing(&self) -> Option<&HashMap<String, RouteEntry>> { None }
}
```
- `classify()` is **synchronous** — the plan explicitly notes that `LLMClassifier` will handle async internally (e.g., via `tokio::task::block_in_place` or a blocking runtime).
- `get_routing()` default returns `None` — exactly right for `LLMClassifier` which doesn't own routing tables.

**`ClassifierChain`** (`src/intent_classifier.rs:113-145`):
```rust
pub struct ClassifierChain {
    backends: Vec<Arc<dyn IntentClassify + Send + Sync>>,
}
```
Fallback logic (lines 129-144): iterates backends in order; the first non-`Fallback` result wins. If all backends return `Fallback`, returns the last one. Empty chain returns `ClassificationResult::fallback()` (CASUAL).

**`ClassificationResult`** (`src/intent_classifier.rs:62-70`): carries `category`, `model`, `endpoint`, `tier`, `provider_type`, `api_key_env` — all fields an `LLMClassifier` needs to populate.

**`ClassificationTier`** (`src/intent_classifier.rs:72-76`): two variants — `Regex` and `Fallback`. An `LLMClassifier` would return `Regex` for confident results and `Fallback` when it can't classify confidently (or when the LLM call fails).

**`RegexClassifier`** (`src/intent_classifier.rs:99-107`): the only current backend implementing `IntentClassify`. Contains `routing`, `fallback_entry`, `model_costs`, `baseline_model`. The type alias `IntentClassifier = RegexClassifier` exists at line 110 for backward compatibility.

**`RouteEntry`** (`src/intent_classifier.rs:11-18`): `model`, `endpoint`, `cost_per_1m_input_tokens`, `provider_type`, `api_key_env`.

### 2. AppState Integration Points

**`AppState`** (`src/main.rs:28-37`) already holds the chain:
```rust
pub struct AppState {
    classifier: Option<Arc<intent_classifier::ClassifierChain>>,  // line 31
    routing: Arc<HashMap<String, intent_classifier::RouteEntry>>,  // line 32 — merged from all backends
    http_client: Option<reqwest::Client>,                          // line 36
    // ...
}
```

**Construction** (`src/main.rs:71-100`):
1. `RegexClassifier::from_env()` builds the regex backend (line 72)
2. `ClassifierChain::new(vec![Arc::new(regex_classifier)])` wraps it (lines 78-79)
3. Routing is merged from all backends via `backend.get_routing()` (lines 82-88)
4. Falls back to `None` + empty routing if regex fails (lines 91-99)

**To add `LLMClassifier`**, the construction would change from a single-element vec to a two-element vec:
```rust
// Current (line 78-79):
ClassifierChain::new(vec![Arc::new(regex_classifier)])
// Future:
ClassifierChain::new(vec![
    Arc::new(regex_classifier),
    Arc::new(llm_classifier),
])
```

**Integration point in `completion_handler`** (`src/main.rs:307-312`):
```rust
state.classifier.as_ref().map(|c| c.classify(&prompt))
    .unwrap_or_else(ClassificationResult::fallback)
```
No changes needed here — the trait object dispatch handles it transparently.

### 3. Upstream HTTP Call Patterns (Model for LLMClassifier)

The `completion_handler` (`src/main.rs:236-617`) provides the pattern for upstream HTTP requests:

**API key resolution** (`src/main.rs:330-358`): reads env var named by `api_key_env` field, degrades gracefully (returns classification-only JSON) if missing.

**Auth header construction** (`src/main.rs:401-402`): calls `auth_headers_for(&classification.provider_type, &api_key)` from `src/intent_classifier.rs:358-365`.

**Request building** (`src/main.rs:404-410`):
```rust
client.post(&endpoint)
    .header(header::CONTENT_TYPE, "application/json")
    .body(serde_json::to_vec(&body).unwrap())
```
Then attach auth headers.

**Response buffering** (`src/main.rs:577-606`): reads chunks into `Vec<u8>`, capped at `MAX_UPSTREAM_BODY = 10 MB`.

**Shared `reqwest::Client`** (`src/main.rs:105-108`): built with 300s timeout, `json` + `rustls-tls` + `stream` features enabled (Cargo.toml:20). The `reqwest::Client` is `Clone` (cheap Arc internally), so the `LLMClassifier` can receive it at construction time.

### 4. LLMClassifier Configuration Requirements

Based on the roadmap (S-09, lines 283-294) and the provider-agnostic-config research (`context/archive/2026-06-07-provider-agnostic-config/research.md`), an `LLMClassifier` needs:

| Config Field | Source | Purpose |
|---|---|---|
| `model` | Env var (e.g., `LLM_CLASSIFIER_MODEL`) or TOML | Which cheap model to call (e.g., `gpt-4o-mini`) |
| `endpoint` | Env var (e.g., `LLM_CLASSIFIER_ENDPOINT`) | Where to send the classification request |
| `api_key` | Env var (e.g., `LLM_CLASSIFIER_API_KEY`) | Auth key for the classification provider |
| `provider_type` | Config (default: `"openai_compatible"`) | Determines auth header format via `auth_headers_for()` |
| `prompt_template` | Hardcoded or config | System prompt instructing the model to classify |
| `categories` | Hardcoded | The four known intent categories: COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL |

**Suggested env vars** (following existing naming patterns):
- `LLM_CLASSIFIER_ENABLED` — boolean, enables LLM classifier backend (default: false)
- `LLM_CLASSIFIER_MODEL` — model name (default: `"gpt-4o-mini"`)
- `LLM_CLASSIFIER_ENDPOINT` — endpoint URL
- `LLM_CLASSIFIER_API_KEY` — API key for the classification provider
- `LLM_CLASSIFIER_PROVIDER_TYPE` — defaults to `"openai_compatible"`

### 5. Classification Prompt Template Design

From the roadmap (lines 292-293) and prior research (`context/archive/2026-06-07-proxy-intent-routing/research.md:85-91`):

The original research proposed **NLI hypothesis templates** for local ONNX inference. For an actual LLM-based classifier, the roadmap specifies: **"few-shot examples in the system prompt, constrained output to known category names."**

A prompt template would look like:

```
System: You are an intent classifier. Classify user prompts into one of:
- COMPLEX_REASONING: requires multi-step reasoning, architecture design, or deep analysis
- FILE_READING: about reading, viewing, or inspecting files or code
- SYNTAX_FIX: about fixing bugs, errors, typos, or compilation issues
- CASUAL: simple questions, greetings, or general conversation

Respond with only the category name, nothing else.

Examples:
User: "read the file src/main.rs" → FILE_READING
User: "fix this compilation error" → SYNTAX_FIX
User: "architect a distributed rate limiter" → COMPLEX_REASONING
User: "hello" → CASUAL

User: {prompt}
```

The `LLMClassifier` would:
1. Format the prompt with the user's text
2. Send as a chat completion request to the cheap model
3. Parse the response, extracting the category name
4. Map the category to a route entry (from the merged routing in `AppState`)
5. Return `ClassificationResult` with `ClassificationTier::Regex` on success, `Fallback` on failure

### 6. Current Env Var Landscape

All env vars consumed in `src/intent_classifier.rs` (for reference when naming new LLM classifier vars):

| Env Var | Default | Purpose |
|---|---|---|
| `NVIDIA_ENDPOINT` | `https://integrate.api.nvidia.com/v1/chat/completions` | Hardcoded routing fallback endpoint |
| `DEFAULT_MODEL` | `meta/llama-3.1-8b-instruct` | Default model for SYNTAX_FIX, CASUAL |
| `DEFAULT_MODEL_COMPLEX` | `meta/llama-3.3-70b-instruct` | Default model for COMPLEX_REASONING |
| `DEFAULT_MODEL_READING` | `meta/llama-3.1-70b-instruct` | Default model for FILE_READING |
| `BASELINE_MODEL` | `DEFAULT_MODEL_COMPLEX` = `meta/llama-3.3-70b-instruct` | Baseline for cost-savings calculation |
| `ROUTING_CONFIG_PATH` | `routing.toml` | Path to TOML routing file |

Additional env vars in `main()`:
- `CLASSIFY_DB_LOG` — controls whether `/v1/classify` logs to DB
- `STREAMING_CHANNEL_CAPACITY` — MPSC channel capacity (default: 32)
- `KEEPALIVE_INTERVAL_SECS` — SSE keepalive interval (default: 15)
- `ALLOWED_ORIGINS` — CORS origins (comma-separated)

### 7. Implementation Approach and Risks

**Architecture fit:**
- `LLMClassifier` struct with fields: `client: reqwest::Client`, `model: String`, `endpoint: String`, `api_key: String`, `provider_type: String`, `prompt_template: String`
- Implements `IntentClassify` — `classify()` makes a synchronous (blocking) HTTP call, `get_routing()` returns `None`
- Constructor: `LLMClassifier::from_env(client: reqwest::Client) -> Result<Self, String>`
- The `classify()` method is synchronous on the trait but internally uses `tokio::runtime::Handle::current().block_on(...)` or receives a `reqwest::Client` that works with `block_on`

**Risk: synchronous `classify()` with async HTTP.** The trait's `classify()` is synchronous. Solutions:
1. **`block_on` approach**: Use `tokio::runtime::Handle::current().block_on(async { client.post(...).send().await })` — simplest, works within an existing Tokio runtime. This is the standard pattern for bridging sync trait methods to async I/O.
2. **Blocking reqwest**: Use `reqwest::blocking::Client` — requires a separate `Client` instance, but avoids `block_on`. However, the `blocking` feature is not currently in `Cargo.toml`.

**Risk: latency.** Each classification call adds ~200-500ms. Acceptable as a Tier 2 fallback (only fires when regex is ambiguous). If used as primary classifier, latency becomes noticeable.

**Risk: cost.** ~$0.15/1M tokens for `gpt-4o-mini`. With typical classification prompts (~200 tokens), cost is ~$0.00003 per classification — negligible at any reasonable scale.

**Risk: reliability.** If the LLM endpoint is down, the classifier must degrade gracefully. The `ClassifierChain` already handles this — if `LLMClassifier::classify()` returns `Fallback` on any error, the chain falls through to the `RegexClassifier`'s CASUAL fallback.

## Code References

- `src/intent_classifier.rs:78-87` — `IntentClassify` trait definition (already exists)
- `src/intent_classifier.rs:89-97` — `RegexClassifier` trait impl (pattern to follow)
- `src/intent_classifier.rs:113-145` — `ClassifierChain` struct and trait impl (fallback logic)
- `src/intent_classifier.rs:62-70` — `ClassificationResult` struct (output type)
- `src/intent_classifier.rs:72-76` — `ClassificationTier` enum (Regex vs Fallback)
- `src/intent_classifier.rs:358-365` — `auth_headers_for()` (reusable for LLM classifier auth)
- `src/intent_classifier.rs:531-558` — `RegexClassifier::from_env()` (constructor pattern)
- `src/main.rs:28-37` — `AppState` struct (integration point for classifier chain)
- `src/main.rs:71-100` — Classifier chain construction in `main()`
- `src/main.rs:236-617` — `completion_handler` (upstream HTTP call pattern)
- `src/main.rs:105-108` — `reqwest::Client` construction (shared client)
- `src/main.rs:330-358` — API key resolution from env var (pattern for LLM classifier)
- `src/main.rs:401-410` — Auth header attachment and upstream request building
- `src/main.rs:180-219` — `classify_and_log` shared helper (classification entry point)
- `Cargo.toml:20` — reqwest dependency with `json`, `rustls-tls`, `stream` features

## Architecture Insights

1. **The `IntentClassify` trait was explicitly designed for S-09.** The archived plan (`context/archive/2026-06-06-intent-classifier-trait/plan.md:36-38`) states: "Adding the LLMClassifier backend (roadmap S-09) — that's a separate change." The trait was made narrow enough (single method) to accommodate future backends without modification.

2. **`get_routing()` returns `None` by default** — this was a deliberate design choice so non-routing backends (like `LLMClassifier`) don't need to implement it. The chain's routing merge iterates all backends and collects from those that return `Some`.

3. **The synchronous `classify()` method is intentional.** The archived plan (`context/archive/2026-06-06-intent-classifier-trait/plan.md:38`) notes: "Introducing async into the trait — the classify method is synchronous. Future LLMClassifier will handle async inside its own implementation." The standard approach is `Handle::block_on()`.

4. **Two-tier architecture is the design.** Regex (Tier 1, ~0.01ms) runs first. LLM (Tier 2, 200-500ms) only fires when regex is ambiguous. This minimizes both latency and cost. The `ClassifierChain` already implements this pattern: first non-Fallback result wins.

5. **Graceful degradation at every level.** If regex fails to load → chain is empty → CASUAL fallback. If LLM classifier fails to load → chain only has regex → works as before. If LLM call fails at runtime → return Fallback → chain falls through → CASUAL.

6. **The `reqwest::Client` is shared.** Building a separate HTTP client would be redundant. Pass the existing client from `AppState` to `LLMClassifier::new()`.

7. **No changes to `completion_handler` or routing needed.** The existing trait object dispatch (`classifier.classify(&prompt)`) already works with any `IntentClassify` backend. The routing merge already skips backends that return `None` from `get_routing()`.

## Historical Context

### From the intent-classifier-trait change (S-07)
- **Archived at**: `context/archive/2026-06-06-intent-classifier-trait/`
- **Plan** (`plan.md:14`): "The original classification research rejected a trait as YAGNI. The roadmap (S-07) reverses this — the codebase is mature enough for pluggable backends (S-09: LLM-based classifier)."
- **Design decisions**: Synchronous `classify()`, `get_routing()` defaulting to `None`, `ClassifierChain` fallback-first iteration — all architected with S-09 in mind.

### From the proxy-intent-routing research (S-01e)
- **Archived at**: `context/archive/2026-06-07-proxy-intent-routing/research.md`
- **Lines 85-91**: Original NLI hypothesis templates for classification:
  ```
  COMPLEX_REASONING: "This prompt requires complex reasoning or multi-step problem solving."
  FILE_READING: "This prompt is about reading or viewing the contents of a file."
  SYNTAX_FIX: "This prompt is about fixing a bug, error, or compilation issue."
  CASUAL: "This prompt is a simple question or casual conversation."
  ```
- **Lines 93-106**: Two-tier architecture originally designed for regex + ONNX, but the same architecture applies to regex + LLM.

### From the provider-agnostic-config research (S-01c)
- **Archived at**: `context/archive/2026-06-07-provider-agnostic-config/research.md`
- **Provider auth matrix** (lines 20-41): 90% of providers use `Bearer` auth, Anthropic uses `x-api-key`, Ollama/vLLM have no auth.
- **Lazy key resolution pattern** (lines 129-132): read API keys on first use rather than at startup, to avoid requiring env vars for unused providers.

### From the roadmap (S-09 definition)
- `context/foundation/roadmap.md:283-294` — formal definition of `LLMClassifier`
- `context/foundation/roadmap.md:48-49` — at-a-glance: S-09 depends on S-07, status: proposed
- `context/foundation/roadmap.md:262-273` — S-07 definition (already implemented despite "proposed" status)

## Related Research

- `context/archive/2026-06-06-intent-classifier-trait/plan.md` — S-07 plan that designed the trait for S-09
- `context/archive/2026-06-06-intent-classifier-trait/plan-brief.md` — S-07 brief, diagrams the chain architecture with future LLM backend
- `context/archive/2026-06-07-proxy-intent-routing/research.md` — Original classification research with prompt templates
- `context/archive/2026-06-07-provider-agnostic-config/research.md` — Provider auth matrix and config patterns
- `context/archive/2026-06-07-provider-url-derivation/research.md` — Descoped; not relevant to S-09

## Open Questions

1. **Should `LLMClassifier` have its own `routing.toml` or use the merged routing from the chain?** The current design merges routing from `RegexClassifier` only. If `LLMClassifier` returns `None` from `get_routing()`, it relies on the merged routing in `AppState` — which is the correct design (the LLM only classifies; it doesn't own routing).

2. **What format should the classification prompt template be?** The roadmap suggests "few-shot examples in the system prompt, constrained output to known category names." A concrete template needs to be designed — see Section 5 above.

3. **Should the LLM classifier be enabled by default or opt-in?** An `LLM_CLASSIFIER_ENABLED` env var (default: `false`) would allow existing deployments to continue working unchanged.

4. **How should `LLMClassifier::classify()` handle the async HTTP call?** The trait is synchronous. Options: `Handle::block_on()` (simplest) or a separate `reqwest::blocking::Client` (requires new Cargo feature).

5. **What model should be the default?** The roadmap mentions `gpt-4o-mini` as the example. This is cheap ($0.15/1M tokens) and fast enough for a fallback tier.

## Follow-up Research 2026-06-07T14:30+02:00 — Shared Category Configuration

### Problem

The four intent categories (`FILE_READING`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`) are currently **hardcoded in at least four separate places** within `src/intent_classifier.rs`, all inside the `RegexClassifier` implementation:

1. **Category name constants** (`src/intent_classifier.rs:168-172`) — `CAT_FILE_READING`, `CAT_COMPLEX_REASONING`, `CAT_SYNTAX_FIX`, `CAT_CASUAL`
2. **Regex pattern arrays** (`src/intent_classifier.rs:205-259`) — `FILE_READING` (12 patterns), `COMPLEX_REASONING` (16), `SYNTAX_FIX` (11), `CASUAL` (5), plus `NEGATIVE` (4)
3. **Weight arrays** (`src/intent_classifier.rs:184-187`) — `FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS` — each indexed in parallel with the pattern arrays
4. **Threshold constants** (`src/intent_classifier.rs:191-196`) — `FR_THRESHOLD=3`, `CR_THRESHOLD=3`, `SF_THRESHOLD_HIGH=4`, `SF_THRESHOLD_LOW=3`, `CA_THRESHOLD=1`
5. **Hardcoded routing** (`src/intent_classifier.rs:297-351`) — maps each category constant to a `RouteEntry` with model, endpoint, provider_type
6. **`build_all_patterns()`** (`src/intent_classifier.rs:385-430`) — assembles patterns with category metadata, hardcoded per-category iteration order
7. **`classify()` threshold logic** (`src/intent_classifier.rs:611-643`) — priority order FR → SF → CR → CA, and the "2+ thresholds met → CASUAL" rule

An `LLMClassifier` needs the **same four categories** but with different data: category **descriptions** (human-readable explanations for the prompt template) instead of regex patterns and weights.

Currently there is **no shared source of truth** for which categories exist. The list of four categories is implicitly defined by the union of the constants, pattern arrays, weight arrays, and routing entries — all tightly coupled inside the regex implementation.

### Impact on S-09

If an `LLMClassifier` separately defines its own list of categories, the codebase has two independent copies of the same category list that must be kept in sync. If a category is added, removed, or renamed, both classifiers (and the routing table, and the prompt template) must be updated consistently.

**Example divergence risk:** `RegexClassifier` knows about 4 categories. `LLMClassifier` prompt template hardcodes 4 categories. The operator adds a 5th category to `routing.toml`. The regex classifier ignores it (no patterns), but the LLM classifier doesn't know about it either (hardcoded prompt). The routing table has a dead entry.

### What a Shared CategoryConfig Would Look Like

A shared `CategoryConfig` would be defined in `src/intent_classifier.rs`, consumed by both `RegexClassifier` and `LLMClassifier` at construction time:

```rust
/// Shared category definition — single source of truth for both classifiers.
pub struct CategoryConfig {
    pub name: &'static str,          // "FILE_READING", etc.
    pub description: &'static str,   // Human-readable for LLM prompt template
    pub regex_threshold: Option<u32>, // Only used by RegexClassifier; None for categories it can't match
    pub priority: u8,                // Ordering for tie-breaking (lower = higher priority)
}
```

**Category definitions** (from `context/archive/2026-06-07-proxy-intent-routing/research.md:85-91` NLI hypothesis templates + regex pattern themes at `src/intent_classifier.rs:205-267`):

| Category | Description | Regex Patterns? | Priority |
|---|---|---|---|
| `FILE_READING` | Reading, viewing, inspecting, searching, or navigating files or code | Yes (12 patterns) | 1 (highest) |
| `COMPLEX_REASONING` | Multi-step reasoning, architecture design, refactoring, deep analysis, performance optimization | Yes (16 patterns) | 3 |
| `SYNTAX_FIX` | Fixing bugs, errors, typos, compilation issues, or broken code | Yes (11 patterns) | 2 |
| `CASUAL` | Simple questions, greetings, general conversation, or short prompts | Yes (5 patterns) | 4 (lowest — catch-all) |

### What Changes in intent_classifier.rs

| Area | Current State | Change |
|---|---|---|
| `CAT_*` constants (lines 168-172) | Private `&str` constants | Replace with references into `CategoryConfig` entries |
| Pattern arrays `FILE_READING`, `COMPLEX_REASONING`, etc. (lines 205-259) | Standalone `&[&str]` arrays | Grouped under `CategoryConfig` or associated via parallel indexing |
| Weight arrays `FR_WEIGHTS`, etc. (lines 184-187) | Standalone `&[u8]` arrays | Moved into `RegexClassifier` internals, keyed by category name |
| Threshold constants (lines 191-196) | Standalone `const` values | Moved into `CategoryConfig.regex_threshold` |
| `build_all_patterns()` (lines 385-430) | Iterates arrays in hardcoded order | Iterates over `CategoryConfig` entries, building patterns from associated data |
| `hardcoded_routing()` (lines 297-351) | 4 separate `routing.insert()` calls | Loop over `CategoryConfig` entries |
| `classify()` priority (lines 634-643) | Hardcoded `if fr { } if sf { }` chain | Driven by `CategoryConfig.priority` ordering |
| `NegativeMeta.suppressed` (lines 270-287) | References `CAT_*` constants | References `CategoryConfig.name` strings |

### What Changes in LLMClassifier (New)

The `LLMClassifier` constructor would receive `&[CategoryConfig]` and build its prompt template from it:

```rust
fn build_classification_prompt(categories: &[CategoryConfig], prompt: &str) -> String {
    let mut system = String::from(
        "You are an intent classifier. Classify user prompts into one of:\n"
    );
    for cat in categories {
        system.push_str(&format!("- {}: {}\n", cat.name, cat.description));
    }
    system.push_str("\nRespond with only the category name, nothing else.\n\n");
    // ... few-shot examples ...
    system.push_str(&format!("User: {}\n", prompt));
    system
}
```

This means:
- The `LLMClassifier` **never hardcodes category names** — it reads them from `CategoryConfig`
- Adding a new category requires: add one `CategoryConfig` entry, add regex patterns (optional — can be `None`), add routing.toml entry, update prompt examples
- The prompt template is **generated** from the config, not hardcoded as a static string

### What Does NOT Change

- **`IntentClassify` trait** (`src/intent_classifier.rs:78-87`) — no change. `classify()` still returns `ClassificationResult { category: String, ... }`.
- **`ClassificationResult`** — no change. Category is still a `String`.
- **`ClassifierChain`** — no change. Fallback logic is category-agnostic.
- **`AppState`** — no change. Routing merge is category-agnostic.
- **`completion_handler`** / `classify_handler` — no change. Trait dispatch is transparent.
- **`src/dashboard.rs`** — no change. Treats categories as opaque strings.
- **`src/persistence.rs`** — no change. Category is `Option<String>` in `InferenceRecord`.
- **`routing.toml`** files — no change. Table keys already match category names.
- **Test code** in `src/main.rs` — 17 occurrences of raw category strings (`"SYNTAX_FIX"`, `"CASUAL"`). Could be updated to reference config, but not required for correctness.

### Recommendation

**Extract a shared `CategoryConfig` as a prerequisite step before implementing `LLMClassifier`.** This is a pure refactor that:
1. Defines the four categories once with names, descriptions, thresholds, and priorities
2. Makes `RegexClassifier` consume `CategoryConfig` at construction time
3. Provides the same `CategoryConfig` to `LLMClassifier` for prompt generation
4. Eliminates the risk of category drift between classifiers
5. Is a contained change (~80-100 lines touched in `src/intent_classifier.rs`)

This could be done as part of S-09 (llm-classifier) since it's small and directly enables the LLM backend, or as a separate preparatory refactor. The archived plan for S-07 already established that config is "bundled at construction time" (plan.md line 41) — `CategoryConfig` fits this pattern: passed to each backend's `new()` or `from_env()`.

### Code References for Category Flow

- `src/intent_classifier.rs:168-172` — category name constants
- `src/intent_classifier.rs:184-187` — weight arrays (reused in `build_all_patterns()` at line 385)
- `src/intent_classifier.rs:191-196` — threshold constants (used in `classify()` at line 611)
- `src/intent_classifier.rs:205-267` — pattern arrays with semantic themes
- `src/intent_classifier.rs:297-351` — hardcoded routing (maps categories to route entries)
- `src/intent_classifier.rs:385-430` — `build_all_patterns()` (assembles patterns by category)
- `src/intent_classifier.rs:585-644` — `classify()` (scoring, threshold checks, routing by category)
- `src/intent_classifier.rs:149-152` — `PatternMeta` (carries `category: &'static str`, no description)
- `context/archive/2026-06-07-proxy-intent-routing/research.md:85-91` — NLI hypothesis templates (only existing category descriptions)
- `src/main.rs:744,754,800,834,861,871,918,1302,1346,1356,1393,1403,1576,2107` — all category references are in test code
