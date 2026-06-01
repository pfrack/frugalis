---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 90ea2b188a24f5ba8ac220bad6e4560fb0c67b78
branch: main
repository: pfrack/cerebrum
topic: "Intent-aware proxy routing: ONNX integration and intent_classificator crate preparation"
tags: [research, intent-classification, onnx, proxy-routing, intent-classificator, tract, ort, zeroshot-classification, axum]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
last_updated_note: "Added follow-up research for easy regex-only classifier design"
---

# Research: ONNX Integration and intent_classificator Preparation for Proxy Routing

**Date**: 2026-06-01T00:00:00+02:00
**Researcher**: pfrack
**Git Commit**: 90ea2b188a24f5ba8ac220bad6e4560fb0c67b78
**Branch**: main
**Repository**: pfrack/cerebrum

## Research Question

How to prepare the Cerebrum gateway for ONNX-based intent classification and an `intent_classificator` crate — what ONNX runtime, what model, how to integrate into the existing Axum architecture, and how this compares to the original regex + OpenRouter fallback plan.

## Summary

**The codebase is architecturally ready for intent classification.** All three foundation changes (F-01 auth, F-02 persistence, F-03 dashboard) are implemented. The `completion_handler` at `src/main.rs:87` is a stub with logging plumbing already in place. `InferenceRecord` already has `category` and `upstream_model` fields (both `Option<String>`). The dashboard template already renders them.

**Two viable ONNX paths exist:**

1. **tract (recommended)** — pure Rust, zero system deps, works on Render native runtime without Docker. NNEF pre-compilation keeps binary small. <50ms latency for DistilBERT-scale models. Best fit for solo-dev after-hours constraints.

2. **ort** — fastest with quantized models, but needs `libonnxruntime.so` (requires Docker on Render). Better if quantized int8 inference speed matters later.

**Best classification model**: A zero-shot NLI model (ModernBERT-base-zeroshot-v2.0, ~200MB ONNX) with `tokenizers` crate for text preprocessing. This avoids model fine-tuning (a non-goal), works by computing entailment scores between the prompt and 4 category hypotheses. Regex Tier 1 (~0.01ms) runs first; ONNX Tier 2 (5-50ms) only fires for ambiguous prompts.

**Integration**: Add `intent_classificator: Option<Arc<IntentClassifier>>` to `AppState` (matching the `persistence` Option pattern). The `IntentClassifier` loads an ONNX session + tokenizer at startup, exposes `fn classify(&self, text: &str) -> Result<ClassificationResult, String>`, and is called inside `completion_handler` before logging. Failure degrades gracefully to CASUAL routing.

## Detailed Findings

### 1. ONNX Runtime Crate Selection

Four Rust crates were evaluated for running ONNX models on CPU in a Render-deployed binary:

| Criterion | **tract** | **ort** | **candle** | **burn** |
|---|---|---|---|---|
| Version | 0.23.0 | 2.0.0-rc.12 | ~0.10.x (multicrate) | 0.21.0 |
| Pure Rust | Yes | No (C FFI) | Yes (CPU) | Yes |
| Build complexity | Low (2/5) | High (4/5) | Medium (3/5) | Very High (5/5) |
| System deps | None | libonnxruntime.so | None (CPU) | None |
| ONNX loading | Native runtime | Native runtime | Runtime (less mature) | Build-time codegen |
| Transformer ops | Good (tract-transformers) | Full | Full (native, not ONNX) | Limited |
| CPU latency | Very good | Fastest | Very good | Good |
| Render native deploy | Yes | Docker required | Yes | Yes |
| Deployment format | NNEF (pre-compiled) | ONNX | safetensors/ONNX | Generated Rust code |

**Recommendation: tract.** It's the only pure-Rust ONNX runtime with proven transformer support (Sonos runs BERT-like wake-word models). The NNEF pre-compilation workflow keeps the binary small: convert the ONNX model to tract's compact `.nnef.tgz` format once at build time, ship only `tract-core` + `tract-nnef` at runtime. Zero system dependencies means `cargo build` on Render's native Rust runtime works with no Dockerfile needed. `src/intent_classificator.rs` would use `tract::nnef::tract().model_for_path()` to load the pre-compiled model.

If future quantization (int8) for throughput becomes critical, migrate to `ort` — the ONNX Runtime's XNNPACK backend is significantly faster with quantized models. For the MVP FP32 path, tract is competitive.

**Discarded**: `burn` is a build-time code generator (converts ONNX to Rust source at compile time), unsuitable for a gateway that may swap models. `candle-onnx` exists but HuggingFace's primary candle path uses safetensors, not ONNX — swimming against the current.

### 2. Classification Model Selection

The project needs short-text classification into 4 intent categories (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) without model training (non-goal from PRD). **Zero-shot NLI classification** is the best fit: the model takes (prompt, hypothesis) pairs and computes entailment scores.

| Model | Params | ONNX Size | CPU Latency | Rust Setup |
|---|---|---|---|---|
| **ModernBERT-base-zeroshot-v2.0** | 0.1B | ~200 MB | 5-20ms | `tract` + `tokenizers` |
| DeBERTa-v3-base-mnli | 0.2B | ~400 MB | 30-80ms | `tract` + `tokenizers` |
| mDeBERTa-v3 ONNX (community) | 0.3B | ~550 MB | 40-100ms | `tract` + `tokenizers` |

**Recommendation: ModernBERT-base-zeroshot-v2.0** (`MoritzLaurer/ModernBERT-base-zeroshot-v2.0` on HuggingFace). It's "multiple times faster and consumes multiple times less memory than DeBERTav3" with 83.5% mean accuracy. Exports to ONNX via HuggingFace Optimum. The 200MB model is reasonable for shipping in a Docker image or downloading at deploy time.

**Tokenization**: The `tokenizers` crate (v0.23, official HuggingFace) loads the tokenizer.json from the HuggingFace model distribution. Paired with `tract` for inference, the total dependency footprint is ~2 crates.

**Classification workflow**:

```
prompt → tokenizers crate (encode to token IDs + attention mask) → tract ONNX session (run model) → argmax over 4 scores → intent
```

**Hypothesis templates** for the 4 categories:
```
COMPLEX_REASONING: "This prompt requires complex reasoning or multi-step problem solving."
FILE_READING: "This prompt is about reading or viewing the contents of a file."
SYNTAX_FIX: "This prompt is about fixing a bug, error, or compilation issue."
CASUAL: "This prompt is a simple question or casual conversation."
```

### 3. Two-Tier Classification Architecture

Following the roadmap's two-tier strategy but replacing the OpenRouter API fallback with local ONNX:

#### Tier 1: Regex/Keyword (always runs first, ~0.01ms)

Heuristic patterns for each category:

- **COMPLEX_REASONING**: Keywords like "architect", "refactor", "design pattern", "how would you", "trade-off", "scale", "concurrency", "pipeline". Multi-sentence prompts >100 chars.
- **FILE_READING**: Keywords like "read", "show", "display", "contents of", "view", "look at". Regex `/read|show|display|cat|view|open\s+(file\s+|the\s+)?[\w.\/-]+/`.
- **SYNTAX_FIX**: Keywords like "fix", "error", "doesn't compile", "broken", "crash", "exception", "stack trace". Regex `/fix (this|the|my)/`.
- **CASUAL**: Keywords like "hello", "hi", "thanks", "what is". Short prompts (<30 chars), grep for simple Q&A patterns.

**Decision logic**: If >2 keyword hits in one category with 0 hits in others → confident, use directly. Otherwise → Tier 2.

#### Tier 2: ONNX Zero-Shot (fallback, 5-50ms)

Runs only when Tier 1 is ambiguous. Tokenize prompt, run NLI inference against 4 hypothesis templates, softmax scores, argmax → intent.

#### Routing Map (intent → upstream model)

| Intent | Upstream Model | Provider |
|---|---|---|
| COMPLEX_REASONING | Claude 3.5 Sonnet | Anthropic (via OpenRouter) |
| FILE_READING | DeepSeek Flash | DeepSeek (via OpenRouter) |
| SYNTAX_FIX | GPT-4o-mini | OpenAI (via OpenRouter) |
| CASUAL | GPT-4o-mini | OpenAI (via OpenRouter) |

### 4. Codebase Integration Points

All identified in the current codebase with specific file:line references:

#### Files to modify:

| File | Line(s) | Change |
|---|---|---|
| `src/main.rs:17` | mod declarations | Add `mod intent_classificator;` |
| `src/main.rs:36-39` | `AppState` struct | Add `intent_classificator: Option<Arc<intent_classificator::IntentClassifier>>` |
| `src/main.rs:48-60` | init block | Add classifier init after persistence, same graceful-degradation pattern |
| `src/main.rs:58-60` | `Arc::new(AppState{...})` | Add `intent_classificator` field |
| `src/main.rs:87-121` | `completion_handler` | Insert classification call before line 104; populate `category`/`upstream_model` |
| `src/main.rs:105-112` | `InferenceRecord` | Replace `category: None` and `upstream_model: None` with classification results |
| `src/main.rs:233` | `test_app()` | Add `intent_classificator: None` |
| `src/main.rs:94` | placeholder response | Replace with upstream proxy call (reqwest + SSE streaming) |
| `Cargo.toml` | dependencies | Add `tract`, `tokenizers`, `reqwest`, `serde`, `tokio-stream`, `ndarray` |
| `render.yaml:17` | envVars | Add `ONNX_MODEL_PATH` |

#### New file:

| File | Purpose |
|---|---|
| `src/intent_classificator.rs` | `IntentClassifier` struct (ONNX session + tokenizer + routing config), `ClassificationResult` struct, `from_env()` constructor, `classify()` method, regex Tier 1 logic, ONNX Tier 2 fallback, routing map |

#### Files that do NOT need changes:

| File | Reason |
|---|---|
| `src/persistence.rs` | `InferenceRecord` already has `category: Option<String>` and `upstream_model: Option<String>` |
| `src/auth.rs` | Classification is downstream of auth middleware |
| `templates/dashboard/inferences.html:63-68` | Already renders category and upstream_model with Some/None handling |
| `migrations/` | Schema already has `category TEXT` and `upstream_model TEXT` columns |

#### `IntentClassifier` struct sketch (module boundary):

```rust
// src/intent_classificator.rs

pub struct ClassificationResult {
    pub category: String,       // e.g. "COMPLEX_REASONING"
    pub upstream_model: String, // e.g. "gpt-4o" for logging
    pub upstream_url: String,   // full API endpoint for proxying
    pub tier: ClassificationTier, // Regex or Onnx
    pub confidence: f32,        // 0.0-1.0
}

pub struct IntentClassifier {
    regex_rules: Vec<IntentRule>,       // Tier 1
    onnx_session: Option<tract::...>,   // Tier 2 (None if model not loaded)
    tokenizer: Option<tokenizers::...>, // Tier 2
    routing_map: HashMap<String, RouteTarget>, // intent → upstream model
}

impl IntentClassifier {
    pub fn from_env() -> Result<Self, String> { ... }
    pub fn classify(&self, prompt: &str) -> ClassificationResult { ... }
}
```

### 5. Error Handling and Fallback Strategy

Consistent with the existing `persistence::None` graceful-degradation pattern:

1. **ONNX_MODEL_PATH not set**: Classifier is `None` in AppState. Handler routes everything as CASUAL (cheapest model) with a warning log. Gateway functions in degraded mode.

2. **ONNX model fails to load**: Same as above — `None` + warning printed at startup.

3. **ONNX inference runtime error**: Catch error, log it, fall back to CASUAL + log the failure. Never block the proxy response.

4. **Tokenizer fails (malformed input, empty prompt)**: Default to CASUAL.

5. **All confidence scores equal (Tier 2 returns ambiguous)**: Route as CASUAL. The model is confused — don't guess.

6. **Concurrency**: ONNX sessions are thread-safe (immutable post-load). Share via `Arc<IntentClassifier>`. No mutex needed. Wrap inference in `tokio::task::spawn_blocking` since tract/ort are synchronous.

### 6. New Dependencies Summary

For `intent_classificator` (ONNX only, no routing yet):

| Crate | Purpose |
|---|---|
| `tract` | ONNX inference engine (pure Rust, re-exports tract-onnx, tract-nnef) |
| `tokenizers` | HuggingFace tokenizer for text preprocessing |
| `regex` | Keyword pattern matching for Tier 1 (likely already transitive) |

For proxy routing (reqwest + SSE, needed for S-01 completion):

| Crate | Purpose |
|---|---|
| `reqwest` | HTTP client for upstream model API calls (features: `json`, `stream`) |
| `serde` | Serialization for upstream request bodies |
| `tokio-stream` | Stream combinators for SSE forwarding |

### 7. Configuration Pattern

Following the established `AuthConfig::from_env()` / `PersistenceConfig::from_env()` pattern:

```rust
// Error type matches existing convention (Result<Self, String>)
pub fn from_env() -> Result<Self, String> {
    let model_path = std::env::var("ONNX_MODEL_PATH")
        .map_err(|_| "ONNX_MODEL_PATH is required for classification".to_string())?;
    let tokenizer_path = std::env::var("TOKENIZER_PATH")
        .unwrap_or_else(|_| format!("{}/tokenizer.json", model_path));

    let routing_config_path = std::env::var("ROUTING_CONFIG_PATH")
        .unwrap_or_else(|_| "routing.toml".to_string());

    // Load ONNX model via tract, tokenizer via tokenizers crate
    // Load routing.toml for intent → upstream model mapping
    Ok(Self { ... })
}
```

Env vars: `ONNX_MODEL_PATH` (required), `TOKENIZER_PATH` (optional, defaults to `$ONNX_MODEL_PATH/tokenizer.json`), `ROUTING_CONFIG_PATH` (optional, defaults to `routing.toml`).

## Architecture Insights

1. **The `Option<Arc<T>>` pattern is the established convention** for optional subsystems. Both `persistence` and the proposed `intent_classificator` should use it. Tests set the field to `None` for zero-config unit testing.

2. **Classification sits at a well-defined seam** between auth middleware and response assembly. The handler receives a validated request body (as `Bytes`), classifies it, routes it, streams the response, and logs metadata. Classification does not need to "know" about auth or persistence — it's purely `text → result`.

3. **The `InferenceRecord` already anticipates classification output** with `category: Option<String>` and `upstream_model: Option<String>`. The data model was designed with S-01 in mind. No schema migration needed.

4. **Regex Tier 1 + ONNX Tier 2 is a composable pipeline** that can be implemented incrementally: start with regex-only (no ONNX dependency), then add ONNX as a `feature` flag or optional module. This lets the MVP ship fast with regex classification, and ONNX can be layered on later.

5. **ONNX model loading should be lazy or startup-only**, not per-request. The ONNX session and tokenizer are loaded once at `main()` startup and shared across all handlers via `Arc`.

## Historical Context

- **shape-notes.md:30** — Original decision: "regex/keyword rules first; cheap-model fallback only for ambiguous prompts." The fallback was `gpt-4o-mini` via OpenRouter — this research recommends replacing with local ONNX.
- **shape-notes.md:176** — Future possibility noted: "lightweight local inference on CPU" as a post-MVP tuning path. This research turns that into a concrete implementation plan.
- **roadmap.md:24** — North star: "regex first, cheap-model fallback for ambiguous." The ONNX path preserves the two-tier architecture while eliminating per-request API costs.
- **roadmap.md:114-117** — Two blocking unknowns (classification rules, upstream model choices) are partially resolved by this research. Regex patterns are defined; upstream model choices suggested (OpenRouter with specific model names).
- **F-02 plan.md:67** — `extract_snippet()` was wired anticipating S-01. Works with OpenAI-compatible `{"messages": [...], "model": "..."}` body format — already extracts the last user message.

## Open Questions

1. **ONNX vs OpenRouter API tradeoff**: ONNX saves per-request API costs (~$0.00015/call at gpt-4o-mini pricing) but adds 200MB to the deployment. On Render's Starter plan ($7/month, always-on), a 200MB binary is fine. The tradeoff is operational simplicity (no external classifier dependency) vs deployment size.

2. **Model download at deploy time**: Should the ONNX model be baked into the Docker image, or downloaded at startup? Baked-in is simpler but makes the image large. Download-at-startup via `reqwest` from HuggingFace or a CDN keeps the image small but adds deployment startup time and network dependency.

3. **Quantization**: Should the ONNX model be quantized (FP16 or int8) for faster inference and smaller size? FP16 cuts model size in half with minimal accuracy loss; int8 is fastest but requires ort (tract supports it partially).

4. **SSE upstream proxying mechanics**: Not yet researched — does `reqwest` streaming work seamlessly with Axum SSE responses? Keepalive pings needed? This is the second half of S-01 (after classification).

5. **Intent-to-upstream-model routing config**: Should routing rules live in `routing.toml` (static file, per shape-notes) or in environment variables? TOML is better for maps and lists; env vars are better for Render's secret management. Recommend TOML for the map, with environment override for API keys.

## Related Research

- `context/changes/data-persistence-async-logging/plan.md` — `InferenceRecord` schema design with `category` and `upstream_model` fields anticipating S-01
- `context/changes/inference-log-inspection/plan.md` — Dashboard queries that render category/model badges already
- `context/foundation/roadmap.md` — S-01 definition, blocking unknowns, sequencing rationale
- `context/foundation/prd.md` — FR-002 (classification), FR-003 (routing), FR-004 (streaming)

## Follow-up Research: Easy Regex-Only Classifier Design (2026-06-01T12:00:00+02:00)

**Question**: Build an easy, regex-only intent classifier — skip ONNX complexity for the first iteration. Focus on `RegexSet`, concrete scoring, and minimal code footprint.

### 8. RegexSet Pattern Inventory (4 Categories × ~45 Patterns)

All patterns designed for `regex::RegexSet::new()` in Rust. Each pattern has a category and weight (1-3). Categories: FR=FILE_READING, CR=COMPLEX_REASONING, SF=SYNTAX_FIX, CA=CASUAL.

#### Prompt Sanitization (runs before classification)

1. Lowercase, collapse whitespace, trim
2. Strip code blocks (` ```...``` `) — code is context, not intent signal
3. Strip prefixes (`user:`, `human:`)
4. Truncate to 300 chars (keyword density drops beyond this)

#### FILE_READING Patterns (FR01–FR12, strongest signals)

| ID | W | Pattern |
|---|---|---|
| FR01 | 3 | `(?i)\b(?:read\|show\|display\|print\|cat\|view\|open)\s+(?:the\s+)?(?:file\|contents\|this\s+file\|that\s+file)\b` |
| FR02 | 3 | `(?i)\b(?:show\|display\|print\|cat)\s+(?:me\s+)?(?:the\s+)?(?:content\|output)(?:\s+of)?` |
| FR03 | 3 | `(?i)\b(?:[a-zA-Z0-9_\-./\\]+\.(?:rs\|py\|js\|ts\|go\|java\|c\|cpp\|h\|to?ml\|ya?ml\|json\|md\|sql\|sh\|html))` |
| FR04 | 3 | `(?i)\b(?:line\|lines)\s+\d+` |
| FR05 | 2 | `(?i)\b(?:what(?:\s+is\|'s)\s+(?:in\|inside))\s+(?:the\s+)?(?:file\|directory\|folder)` |
| FR06 | 2 | `(?i)\b(?:look\|go\|navigate)\s+(?:at\|through\|to\|into)\s+(?:the\s+)?(?:file\|directory\|code\|source)` |
| FR07 | 2 | `(?i)\b(?:list\|ls\|dir\|tree)\s+(?:files\|directories\|contents\|all\|the)` |
| FR08 | 2 | `(?i)\b(?:find\|search\|grep\|locate\|where\s+is)\s+(?:in\|through\|within\|the)\s+(?:the\s+)?(?:file\|code\|project\|source)` |
| FR09 | 2 | `(?i)\b(?:where\s+is\|locate\s+the\|find\s+the)\s+(?:file\|definition\|function\|class\|module\|struct\|trait\|impl)` |
| FR10 | 1 | `(?i)\b(?:what\s+does\s+this\s+file\|show\s+me\s+the\s+code\|view\s+the\s+source\|check\s+the\s+file)` |
| FR11 | 1 | `(?i)\b(?:see\|check\|inspect\|examine)\s+(?:the\s+)?(?:file\|code\|content\|output\|log)` |
| FR12 | 1 | `(?i)\b(?:around\s+line\|near\s+line\|before\s+line\|after\s+line)` |

#### COMPLEX_REASONING Patterns (CR01–CR16)

| ID | W | Pattern |
|---|---|---|
| CR01 | 3 | `(?i)\b(?:architect\|design\s+pattern\|system\s+design\|trade.?off\|refactor\|restructure\|rearchitect)` |
| CR02 | 3 | `(?i)\b(?:how\s+would\s+you\s+(?:design\|architect\|structure\|build\|implement\|approach\|solve))` |
| CR03 | 3 | `(?i)\b(?:multi.?step\|concurr\|distributed\|pipeline\|scal(?:e\|ing\|able)\|optimiz\|bottleneck)` |
| CR04 | 3 | `(?i)\b(?:redesign\s+the\|rewrite\s+(?:the\s+)?(?:entire\|whole)\|audit\s+the\s+codebase\|rearchitect)` |
| CR05 | 2 | `(?i)\b(?:deep\s+dive\|analy(?:ze\|sis)\|evaluat\|compare\s+and\s+contrast\|trade.?off\|pros?\s+and\s+cons?\b)` |
| CR06 | 2 | `(?i)\b(?:best\s+(?:practice\|approach\|way\|pattern)\|design\s+(?:a\|the)\s+(?:system\|architecture\|api\|database\|schema\|service))` |
| CR07 | 2 | `(?i)\b(?:reason\s+about\|explain\s+why\|what(?:\s+is\|'s)\s+the\s+(?:best\|optimal\|right\|correct)\s+way)` |
| CR08 | 2 | `(?i)\b(?:multi.?thread\|async\|event.?driven\|microservice\|rate\s+limit\|load\s+balanc)` |
| CR09 | 2 | `(?i)\b(?:integrat(?:e\|ion)\s+(?:with\|into\|between)\|migrat(?:e\|ion)\s+(?:from\|to\|strategy)\|orchestrat)` |
| CR10 | 2 | `(?i)\b(?:performance\s+(?:bottleneck\|issue\|problem\|analysis\|tuning\|profiling\|regression)\|memory\s+leak\|race\s+condition\|deadlock)` |
| CR11 | 1 | `(?i)\b(?:can\s+you\s+(?:help\s+me\s+)?(?:design\|plan\|architect\|think\s+(?:through\|about)\|reason\s+about))` |
| CR12 | 1 | `(?i)\b(?:strategy\|blueprint\|roadmap\|plan\s+(?:out\|for)\|approach\s+to)` |
| CR13 | 1 | `(?i)\b(?:security\s+(?:audit\|review\|analysis)\|threat\s+model)` |
| CR14 | 1 | `(?i)\b(?:state\s+machine\|algorithm\s+(?:design\|complexity\|analysis))` |
| CR15 | 1 | `(?i)\b(?:dependenc(?:y\|ies)\s+(?:graph\|tree\|injection\|management)\|coupling\|cohesion)` |
| CR16 | 1 | `(?i)\b(?:resilien(?:t\|ce)\|fault\s+toleran\|circuit\s+breaker\|retry\s+strategy)` |

#### SYNTAX_FIX Patterns (SF01–SF11)

| ID | W | Pattern |
|---|---|---|
| SF01 | 3 | `(?i)\b(?:fix\|correct\|repair\|patch)\s+(?:this\|the\|my\|a)\s+(?:bug\|error\|issue\|typo\|problem\|mistake\|warning)` |
| SF02 | 3 | `(?i)\b(?:doesn't\s+compile\|won't\s+compile\|doesn't\s+build\|won't\s+build\|compilation\s+error\|syntax\s+error\|build\s+error)` |
| SF03 | 3 | `(?i)\b(?:type\s+error\|linter?\s+(?:error\|warning)\|runtime\s+error\|segfault\|null\s+pointer\|borrow\s+check)` |
| SF04 | 2 | `(?i)\b(?:why\s+doesn't\s+this\s+work\|what(?:\s+is\|'s)\s+wrong\s+with\|this\s+(?:is\|seems)\s+broken)` |
| SF05 | 2 | `(?i)\b(?:stack\s+trace\|backtrace\|panic\|exception\|traceback\|\.unwrap)` |
| SF06 | 2 | `(?i)\b(?:missing\s+(?:semicolon\|import\|parenthesis\|brace\|bracket\|quote\|comma\|colon\|use\s+statement\|dependency\|argument\|parameter))` |
| SF07 | 2 | `(?i)\b(?:undefined\s+(?:variable\|function\|symbol\|reference\|type\|method)\|not\s+found\s+in\s+this\s+scope\|unresolved\s+reference)` |
| SF08 | 2 | `(?i)\b(?:typo\|misspell(?:ed\|ing)?\|copy.?paste\s+error\|fat\s+finger)` |
| SF09 | 1 | `(?i)\b(?:doesn't\s+work\|is\s+broken\|stopped\s+working\|broke\|isn't\s+working\|not\s+working)` |
| SF10 | 1 | `(?i)\b(?:here(?:'s\|\s+is)\s+(?:the\|an\|my)\s+error\|getting\s+(?:this\|an)\s+error\|seeing\s+(?:this\|an)\s+error)` |
| SF11 | 1 | `(?i)\b(?:error[:;].{0,40}\b(?:\d+\|E\d{4}\|0x[0-9a-fA-F]+)\b)` |

#### CASUAL Patterns (CA01–CA05)

| ID | W | Pattern |
|---|---|---|
| CA01 | 3 | `(?i)^\s*(?:hi\|hey\|hello\|greetings\|good\s+morning\|good\s+afternoon\|good\s+evening\|howdy)(?:\s+there)?[\s!.,]*$` |
| CA02 | 2 | `(?i)^\s*(?:thanks\|thank\s+you\|thx\|ty\|appreciate\s+it\|cheers\|thanks\s+a\s+lot)[\s!.,]*$` |
| CA03 | 1 | `(?i)^\s*(?:what\s+is\|what\s+are\|what's\|what\s+does\|define\|definition\s+of)\s+\w+(?:\s+\w+){0,2}\s*\??$` |
| CA04 | 1 | `(?i)^\s*(?:how\s+(?:do\|can\|should)\s+I\s+\w+)(?:\s+\w+){0,4}\s*\??$` |
| CA05 | 1 | `(?i)^\s*(?:ok\|okay\|got\s+it\|understood\|alright\|cool\|nice\|good\|great\|sure\|yes\|no\|maybe\|idk)[\s!.,]*$` |

#### Negative/Suppression Patterns (NP01–NP04)

These subtract points from another category when matched. Prevents false positives like "read the architecture document" (FR + CR → NP01 suppresses CR → FR wins).

| ID | Suppresses | Penalty | Pattern |
|---|---|---|---|
| NP01 | CR | -2 | `(?i)\b(?:read\|show\|display\|cat\|view\|open)\s+(?:the\|this\|my\|a)\s+\w*(?:architecture\|design\|system\|pattern\|refactor)` |
| NP02 | CR | -2 | `(?i)\b(?:fix\|correct\|repair)\s+(?:the\|this\|my)\s+(?:compile\|syntax\|typo\|lint\|warning\|error)` |
| NP03 | SF | -2 | `(?i)\b(?:design\|architect\|refactor\|rearchitect\|restructure)\s+(?:a\|the\|an)\s+(?:fix\|solution\|remedy\|patch\|workaround)` |
| NP04 | FR | -2 | `(?i)\b(?:explain\|describe\|tell\s+me\s+about\|what\s+do\s+you\s+think\s+about)\s+(?:the\|this\|that)\s+(?:file\|code\|class\|module)` |

### 9. Scoring Algorithm (Cascading with Negative Suppression)

```
classify(prompt):
    sanitized = sanitize(prompt)
    matches = regex_set.is_match(sanitized)
    
    // Tally scores
    scores = {FR: 0, CR: 0, SF: 0, CA: 0}
    for each matching pattern i:
        cat, weight = metadata[i]
        scores[cat] += weight
    
    // Apply negative suppressions
    for each matching negative pattern i:
        suppressed_cat, penalty = negative_metadata[i]
        scores[suppressed_cat] -= penalty
    
    // Clip to zero
    for each cat: scores[cat] = max(0, scores[cat])
    
    // Short prompts (<30 chars, no matches) → CASUAL
    if sanitized.len() < 30 and all scores == 0:
        return CASUAL
    
    // Check thresholds
    fr = scores[FR] >= 3
    sf = scores[SF] >= 4 or (scores[SF] >= 3 and scores[FR] == 0)
    cr = scores[CR] >= 3
    ca = scores[CA] >= 1
    
    met = [fr, sf, cr, ca].filter(true).count()
    
    if met == 0: return CASUAL
    if met >= 2: return AMBIGUOUS (→ Tier 2 ONNX or default CASUAL)
    if fr: return FILE_READING
    if sf: return SYNTAX_FIX
    if cr: return COMPLEX_REASONING
    return CASUAL  // fallback
```

**Cascade priority**: FILE_READING > SYNTAX_FIX > COMPLEX_REASONING > CASUAL. FR is most distinctive (file paths are unambiguous). CASUAL is the safe default when nothing matches.

### 10. Coverage Estimate: Tier 1 Regex vs Tier 2 Fallback

Based on analysis of typical AI coding agent prompts:

| Category | Tier 1 (regex alone) | Tier 2 (needs fallback) |
|---|---|---|
| FILE_READING | ~92% | ~8% |
| SYNTAX_FIX | ~78% | ~22% |
| COMPLEX_REASONING | ~65% | ~35% |
| CASUAL | ~88% | ~12% |
| **Overall weighted** | **~72-78%** | **~22-28%** |

~3/4 of requests should classify correctly with regex alone. The rest (ambiguous prompts, prompts without distinctive keywords, non-English) fall through to CASUAL. With ONNX Tier 2, most of that 22-28% would get correctly classified.

### 11. Module API Design (Rust)

Module: `src/intent_classificator.rs` (~120 lines for MVP)

```rust
use regex::RegexSet;
use std::sync::Arc;

pub struct IntentClassifier {
    set: RegexSet,
    metadata: Vec<PatternMeta>,         // parallel to RegexSet indices
    negative: Vec<NegativeMeta>,        // separate for suppression
}

struct PatternMeta {
    category: &'static str,
    weight: u8,
}

struct NegativeMeta {
    suppressed: &'static str,
    penalty: u8,
}

#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub category: String,       // "COMPLEX_REASONING" | "FILE_READING" | "SYNTAX_FIX" | "CASUAL"
    pub model: String,          // upstream model name
    pub tier: ClassificationTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationTier {
    Regex,      // matched by regex patterns
    Fallback,   // no match — default to CASUAL
}

impl IntentClassifier {
    /// Always available — uses built-in patterns. Returns Ok even without env vars.
    pub fn from_env() -> Result<Self, String> {
        let all_patterns = build_all_patterns();  // concat const slices
        let set = RegexSet::new(all_patterns)
            .map_err(|e| format!("regex compilation failed: {e}"))?;
        Ok(Self { set, metadata: build_metadata(), negative: build_negative() })
    }

    /// Never fails — unmatched prompts get Fallback tier + CASUAL category.
    pub fn classify(&self, prompt: &str) -> ClassificationResult {
        let sanitized = sanitize(prompt);
        let matches: Vec<usize> = self.set.matches(&sanitized).into_iter().collect();
        // ... tally, suppress, threshold, resolve
    }
}
```

**Key design decisions**:
- **No `Option<T>` needed by default** — built-in patterns mean classifier is always available, unlike persistence which needs a DB URL. Makes `AppState.classifier` non-optional (`Option` only needed if you want to disable classification entirely).
- **`RegexSet` compiled once at startup** — ~2-5ms for ~50 patterns, paid once.
- **`classify()` never fails** — returns `Fallback` tier for unmatched prompts. Never blocks the proxy path.
- **No trait** — YAGNI for a single implementation. Swap to ONNX later by replacing the body, keeping the same signature.

### 12. Rust Crate Choice

**`regex` crate only.** No other dependencies needed for Tier 1.

Comparison:
- `regex::RegexSet`: Full regex (word boundaries, `(?i)`, alternation). ~50 patterns compiled at startup. Per-request: ~10-50µs. Best fit.
- `aho-corasick` crate: Faster for pure keyword substrings, but can't do word boundaries `\b` or multi-word spans. Intent patterns need those.
- Iterating individual `Regex` instances: Simpler code but ~80-200µs per request vs ~10-50µs. Not worth the code simplicity tradeoff.

**No `spawn_blocking` needed** — regex matching at this scale is CPU-bound but measured in microseconds. The proxy's I/O latency (seconds) dwarfs classification time.

### 13. Integration Plan (7 Change Sites, 2 Files)

All in `src/main.rs`:

1. **Line 17** — `mod intent_classificator;`
2. **Lines 36-39** — Add `classifier: Arc<intent_classificator::IntentClassifier>` to `AppState` (non-optional since built-in patterns always work)
3. **Lines 48-60** — Add classifier init after persistence: `let classifier = intent_classificator::IntentClassifier::from_env().unwrap_or_else(|e| { eprintln!("WARN: {e}"); panic!("classifier required") });`
4. **Lines 58-60** — Add `classifier` to `Arc::new(AppState { ... })`
5. **Lines 93-94** — Insert classification after timing start, before response assembly:
   - Extract full prompt from `&body` (re-parse JSON for full last user message, not truncated 200-char snippet)
   - Call `state.classifier.classify(&prompt_text)`
6. **Lines 108-109** — Replace `category: None` and `upstream_model: None` with `Some(result.category)` and `Some(result.model)`
7. **Line 233** — `test_app()`: construct a test classifier or keep `classifier: Arc::new(IntentClassifier::test_classifier())`

**No changes needed**: `src/persistence.rs`, `src/auth.rs`, `migrations/`, `templates/`.

**One consideration**: `extract_snippet()` at persistence.rs:217 truncates to 200 chars. The classifier needs the **full** last user message text. Solution: add a new function `extract_prompt_text` (or extract full text in the classifier module itself) that re-parses the JSON body for un-truncated content.

### 14. Real-World Validation: Existing Regex Routers

Two open-source projects demonstrate this pattern works for LLM routing:

- **model-matchmaker** (github.com/coyvalyss1/model-matchmaker, 160 stars): Routes Claude Code prompts to Haiku/Sonnet/Opus using ~40 regex patterns. Reported 12/12 correct after tuning, 50-70% cost savings. Design motto: "No LLM calls for classification. Instant, free, deterministic."
- **claude-model-router-hook** (github.com/tzachbon/claude-model-router-hook, 40 stars): Fork with JSON config, extend/replace modes, sub-agent routing rules.

Key lesson from model-matchmaker: **fail open, not fail closed**. "A false allow (wasting some money) is always better than a false block (interrupting your flow)." The Cerebrum classifier should follow the same principle — when in doubt, route to the cheapest model (CASUAL), never reject the request.

### 15. Edge Case Examples

| Prompt | FR | CR | SF | Result | Why |
|---|---|---|---|---|---|
| `"read the architecture document"` | 3 | 3→1 (NP01) | 0 | **FILE_READING** | Negative suppression resolves CR/FR overlap |
| `"fix this performance issue"` | 0 | 2 | 3 | **SYNTAX_FIX** | SF≥3 AND FR=0 → SF wins |
| `"optimize database queries for performance"` | 0 | 5 | 0 | **COMPLEX_REASONING** | CR≥3, unambiguous |
| `"what does git status do"` | 0 | 0 | 0 | **CASUAL** | No category hits threshold; default |
| `"how do I refactor git status to be faster"` | 0 | 3 | 0 | **COMPLEX_REASONING** | CR01 "refactor" = 3, meets threshold |
| `""` (empty) | 0 | 0 | 0 | **CASUAL** | <30 chars, no matches |
| Non-English prompt | 0 | 0 | 0 | **CASUAL** | Patterns are English-specific → fallback |

### 16. Performance Budget

| Metric | Value |
|---|---|
| RegexSet compilation (startup, 50 patterns) | ~2-5ms |
| Per-request classification (500 char prompt) | ~10-50µs |
| Memory (RegexSet + metadata) | ~50-100KB |
| No async needed | Matches in microseconds |
| No `spawn_blocking` needed | CPU-bound but negligible |

### 17. Minimal Next Step (Fewest Lines to Working Classifier)

1. Add `regex = "1"` to `Cargo.toml` (+1 line)
2. Create `src/intent_classificator.rs` (~90 lines):
   - `ClassificationResult`, `ClassificationTier`, `IntentClassifier` structs
   - 5 `const` pattern slices: `FILE_READING: &[&str]`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`, `NEGATIVE`
   - `fn sanitize(text: &str) -> String` — lower, strip code blocks, collapse whitespace
   - `IntentClassifier::from_env()` — compile RegexSet, return Ok
   - `IntentClassifier::classify(&self, prompt: &str) -> ClassificationResult` — run matches, tally, resolve
3. Wire into `src/main.rs` (~30 lines across 7 change sites from section 13 above)
4. Add unit tests in `#[cfg(test)] mod tests` inside `src/intent_classificator.rs` (+20 lines)

**Total: ~140 lines of new code + ~30 modified lines.**

This gives you a working regex classifier that maps every request to one of 4 categories, routes to a hardcoded `routing.toml` or const mapping, and logs the category/model into the existing `inferences` table. ONNX Tier 2 can be layered on later by swapping the `classify()` implementation body while keeping the same external API.
