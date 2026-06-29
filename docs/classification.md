# `src/classification/` — Intent Classification Pipeline

Frugalis routes every proxied request to a provider based on the **intent** of the prompt. The `classification` module implements a three-tier pipeline that progressively escalates from cheap/fast to expensive/accurate:

```
Prompt
  │
  ▼
┌──────────────────┐   non-Fallback
│  RegexClassifier │ ─────────────────▶ ClassificationResult { tier: Regex }
└──────────────────┘
  │ Fallback
  ▼
┌──────────────────┐   non-Fallback
│ FewShotClassifier│ ─────────────────▶ ClassificationResult { tier: FewShot }
└──────────────────┘
  │ Fallback
  ▼
┌──────────────────┐   non-Fallback
│  LLMClassifier   │ ─────────────────▶ ClassificationResult { tier: Regex }
└──────────────────┘
  │ Fallback
  ▼
ClassificationResult { tier: Fallback, category: fallback_category }
```

The chain is assembled at startup by `ClassifierChain` and stored in `AppState`.

---

## Modules

| File | Responsibility |
|---|---|
| `mod.rs` | Re-exports submodules; owns `code_block_re()` — the shared prompt sanitisation regex. |
| `types.rs` | Shared data types: `CategoryConfig`, `PatternEntry`, `DualThreshold`, `NegativePatternConfig`, `ClassificationResult`, `ClassificationTier`, `PatternMeta`, `FewShotExample`. Category names are a **public API contract** (used by routing config, OpenAPI schema, and dashboard templates). |
| `chain.rs` | `IntentClassify` trait + `ClassifierChain`. Iterates backends in order, returns the first non-`Fallback` result. |
| `regex.rs` | `RegexClassifier` (aliased as `IntentClassifier`). Compiles all category patterns into a `RegexSet`. Supports per-pattern weights, negative suppression patterns, dual-threshold overrides, and short-prompt fast-path routing. |
| `fewshot.rs` | `FewShotClassifier`. TF-weighted cosine similarity over a vocabulary built from labelled training examples. Bootstraps from `data/fewshot_bootstrap.yaml`; merges persisted feedback from disk; retrains in-place when the feedback threshold is reached. |
| `llm.rs` | `LLMClassifier`. Last resort: sends the prompt to a configurable LLM endpoint (OpenAI-compatible or Anthropic) with a generated system prompt. Refreshes its API key from the environment every 60 s. Also owns `auth_headers_for` — the shared auth-header builder used by the proxy layer. |

---

## Key types

- **`ClassificationResult`** — output of every classifier: `category` (string matching a `CategoryConfig` name), `model`, `tier` (`Regex | FewShot | Fallback`), and `providers` (resolved routing list).
- **`ClassificationTier`** — indicates which pipeline stage produced the result; used by `ClassifierChain` to decide whether to fall through.
- **`CategoryConfig`** — single source of truth for a category: name, description (used in LLM prompts), regex patterns, threshold, priority, and optional dual-threshold rule. Category names are a breaking-change surface — see the warning block in `types.rs`.
- **`FewShotExample`** — a labelled training example (`text`, `category`, `confidence`). Confidence ≥ 0.99 marks bootstrap examples; lower values are user-feedback entries.

---

## Classifier details

### `RegexClassifier`

Compiles all positive patterns (from `CategoryConfig.patterns`) and negative suppression patterns into a single `RegexSet`. On each call to `classify_internal`:

1. Sanitise: lowercase, strip code blocks, collapse whitespace.
2. Run the `RegexSet` and tally weighted scores per category.
3. Apply negative pattern penalties via `saturating_sub`.
4. Short-prompt fast-path: if the prompt is under `short_prompt_len` chars and all scores are zero, route to the fallback category.
5. Apply `dual_threshold` overrides per config.
6. If exactly one category meets its threshold, route to it. Otherwise fall back.

### `FewShotClassifier`

Maintains a TF-weighted vocabulary and per-category feature matrices in `RwLock`-protected `DashMap`s. On each `classify`:

1. Preprocess the prompt (same sanitisation as regex).
2. Exact-match check against the training set — returns immediately on hit.
3. Extract a TF feature vector over the vocabulary.
4. Score each category with max cosine similarity over its stored pattern vectors.
5. Compare against `effective_threshold_for` (cold-start vs. normal threshold).
6. Return `FewShot` tier on success, `Fallback` otherwise.

Feedback is ingested via `add_feedback`. When the training set reaches `retraining_threshold` examples, `retrain_internal` rebuilds the vocabulary and pattern matrices synchronously and persists the new set to disk.

### `LLMClassifier`

Builds a system prompt from `CategoryConfig.description` fields (or loads one from `prompt_template_path`). Sends a two-message chat completion request to the configured endpoint with `max_tokens: 20, temperature: 0.0`. Parses the response by scanning for a known category name. Falls back on any network error, non-2xx status, or unknown category string.

A background `tokio::spawn` task polls the API key environment variable every 60 s and updates the shared `RwLock<Arc<str>>` on change, enabling zero-downtime key rotation.

---

## Adding a new classifier backend

1. Implement `IntentClassify` for your type (one async `classify` method; optionally `get_routing` if it owns a routing table).
2. Wrap it in `Arc` and insert it at the desired position in the `ClassifierChain::new(backends)` vec in `app.rs`.
3. If it owns a routing table, return it from `get_routing` so `AppState` merges it into the global routing map.
