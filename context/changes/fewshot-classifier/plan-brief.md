# Few-Shot Intent Classifier — Plan Brief

> Full plan: `context/changes/fewshot-classifier/plan.md`
> Research: `context/changes/fewshot-classifier/research.md`

## What & Why

Build a `FewShotClassifier` that fills the gap between cerebrum's cheap-but-rigid regex classifier and its expensive LLM fallback. It uses bag-of-words + cosine similarity to classify intents from a small set of training examples, and learns over time from user feedback submitted via a new `/v1/feedback` endpoint. When confidence is low, it defers to the LLM — so it never makes the chain worse.

## Starting Point

Cerebrum already has a trait-based `ClassifierChain` (`src/intent_classifier.rs:136`) that tries backends in order: regex first, then LLM. The regex classifier returns `Fallback` for ~30-40% of real-world prompts according to the research gap analysis, sending every one to the LLM. The infrastructure for adding a third backend (trait impl, config, chain assembly loop) is already in place.

## Desired End State

A three-tier chain: `regex → fewshot → llm`. The few-shot classifier catches prompts the regex can't match, returning a `FewShot` tier classification without an API call. Users correct misclassifications via the feedback endpoint; the model retrains when enough feedback accumulates. Training data persists as YAML across restarts. Operators get warned when the vocabulary grows large enough to consider a reset.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Chain position | After regex, before LLM | Fills the gap between cheap regex and expensive LLM without adding cost to regex-handled requests. | Research |
| Classification algorithm | Bag-of-words + cosine similarity | Proven approach from ciresnave/intent-classifier; ~200 lines of Rust, no ML crates needed. | Research |
| Feedback auth | Bearer token (proxy_auth_layer) | Matches existing `/v1/classify` and `/v1/chat/completions` auth pattern. | Plan |
| Cold start | Higher threshold (0.6), relax to 0.4 after N feedback | Immediate value from bootstrap without risking false positives from poorly-matched bootstrap examples. | Plan |
| Training data format | YAML | Project already has serde_yaml as a dependency; readable, diffable, consistent with config patterns. | Plan |
| Persistence | Gitignored by default, path configurable | Prevents accidental commits; operators can override path for testing/shared datasets. | Plan |
| Vocabulary growth | `warn!` when exceeds 5000 words, no hard cap | Vocabulary grows slowly; alert is sufficient. Adds zero complexity vs eviction policies. | Plan |
| Concurrency model | DashMap for vocab/patterns, Arc<RwLock> for training data | Lock-free reads on the hot classify path; write-heavy feedback path uses RwLock (rare writes). | Research |

## Scope

**In scope:**
- `FewShotClassifier` implementing `IntentClassify` trait
- `FewShot` variant on `ClassificationTier`
- Bootstrap YAML with ~60-80 examples (15-20 per category)
- Config: `[fewshot_classifier]` section with thresholds and paths
- Chain integration: `"fewshot"` recognized backend name
- `POST /v1/feedback` with bearer auth
- YAML persistence for training data
- Vocabulary size operator warning

**Out of scope:**
- Per-user training data (global model only)
- Dashboard page for training data
- Database-backed training data store
- Modifications to regex or LLM classifiers
- Automatic bootstrap example generation

## Architecture / Approach

```
Request → Chain: [RegexClassifier] → [FewShotClassifier] → [LLMClassifier]
                │                      │                      │
                │ returns Fallback     │ returns Fallback     │ returns result
                │ (no pattern match)   │ (low confidence)     │
                ▼                      ▼                      ▼
           FewShot runs           LLM runs               Response sent

Feedback:
POST /v1/feedback { text, actual_category } → FewShotClassifier.add_feedback()
  → training_data updated
  → If feedback_count ≥ retraining_threshold → retrain() → save to YAML
```

The classifier preprocesses text (lowercase, strip code blocks, collapse whitespace), tries exact match against training data, extracts a TF-normalized bag-of-words feature vector (1000 dims), computes cosine similarity against stored pattern vectors per category, and returns the highest-scoring category if it meets the threshold.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Foundation | Tier variant, config structs, bootstrap YAML, dependencies | Scope creep on config fields |
| 2. Classifier Core | FewShotClassifier with full classification logic and tests | Cosine similarity implementation bugs — validated by test suite |
| 3. Chain Integration | Wired into production chain, new default order | Breaking existing configs — mitigated: unknown backend names are safely ignored |
| 4. Feedback Endpoint | `/v1/feedback` route with auth, learning, retraining | Auth bypass — validated by `cargo test auth` |
| 5. Persistence | YAML save/load, vocabulary alert, .gitignore | I/O errors crashing startup — handled: missing file = bootstrap only |

**Prerequisites:** `dashmap` crate available (no special setup needed beyond `cargo build`)
**Estimated effort:** ~2-3 sessions across 5 phases

## Open Risks & Assumptions

- The 0.6/0.4 threshold values and `cold_start_feedback_count: 5` are initial guesses. Tuning may be needed after observing real-world classification quality.
- Bootstrap examples are hand-written and may not generalize well to all user prompt styles. The cold-start higher threshold mitigates this.
- If the training data file grows very large (thousands of examples), retraining latency could become noticeable. The plan caps this via the retraining threshold (default 5) — retraining only rebuilds, it doesn't grow unboundedly between retrains.

## Success Criteria (Summary)

- Few-shot classifier catches prompts regex misses, returning `FewShot` tier with correct category
- Feedback endpoint accepts corrections and triggers retraining after threshold
- Training data survives server restarts (YAML persistence)
- Chain gracefully degrades: removing few-shot from config order keeps the system working
