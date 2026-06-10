---
date: 2026-06-09T12:00:00+02:00
researcher: pfrack
git_commit: 9a25d9d3cc60b3f6412eb39506a3f5037232b04d
branch: code-review-cleanup
repository: cerebrum
topic: "In-memory config filesystem — eliminate hardcoded fallback values, embed default configs, enable dashboard-driven config reload"
tags: [research, config, in-memory-filesystem, dashboard-reload, hardcoded-values, architecture]
status: complete
last_updated: 2026-06-09
last_updated_by: pfrack
last_updated_note: "Added follow-up research for single-source-of-truth design — env vars for secrets only, config.toml for everything else, embedded with include_str!()"
---

# Research: In-Memory Config Filesystem & Dashboard Config Reload

**Date**: 2026-06-09T12:00:00+02:00
**Researcher**: pfrack
**Git Commit**: 9a25d9d3cc60b3f6412eb39506a3f5037232b04d
**Branch**: code-review-cleanup
**Repository**: cerebrum

## Research Question

> I was thinking if I shouldnt change how config is loaded. I would like to avoid having hardcoded values and have either a temporary filesystem in memory with default config, or create a file at the start with a different config. A temporary filesystem with config gives me the possibility to have few default configs available and the ability to reload/switch from the dashboard.

## Summary

The Cerebrum codebase has **~65 distinct hardcoded values** across 7 source files, serving as fallback defaults when `config.toml` is absent or incomplete. The config is loaded once at startup into `Arc<AppState>` and never mutated. There is already a proven runtime-updatable pattern (`LLMClassifier` uses `Arc<RwLock<String>>` for API key rotation), but it's scoped to a single field. Extending this pattern to all config-derived `AppState` fields, combined with `include_str!()` for embedded default TOML configs, would enable both a config-file-free deployment and dashboard-driven config reloading without restart. The hardest part is the `classifier` field, which requires rebuilding the `ClassifierChain` (regex patterns + LLM backend) atomically.

## Detailed Findings

### 1. Hardcoded Values Catalog (65+ distinct values)

The config subsystem relies on hardcoded fallbacks at every level. Here is the complete inventory grouped by category:

#### Model Name Constants (`src/routing.rs:48-50`)
- `DEFAULT_MODEL` = `"meta/llama-3.1-8b-instruct"` — env override: `DEFAULT_MODEL`
- `DEFAULT_MODEL_COMPLEX` = `"meta/llama-3.3-70b-instruct"` — env override: `DEFAULT_MODEL_COMPLEX`
- `DEFAULT_MODEL_READING` = `"meta/llama-3.1-70b-instruct"` — env override: `DEFAULT_MODEL_READING`
- Used by `hardcoded_routing()`, `ClassificationResult::fallback()`, `hardcoded_model_default()`

#### Hardcoded Model Costs (`src/intent_classifier.rs:16-23`)
| Model | Cost per 1M tokens |
|---|---|
| `claude-3.5-sonnet` | 3.00 |
| `gpt-4o` | 2.50 |
| `gpt-4o-mini` | 0.15 |
| `deepseek-chat` | 0.14 |

Can be overridden per-route via `cost_per_1m_input_tokens` in routing config. Absent these, the savings dashboard produces `unknown_cost_count` entries.

#### Category Definitions (`src/intent_classifier.rs:49-80`)
4 hardcoded `CategoryConfig` entries consumed by `hardcoded_categories()`:
- `FILE_READING` (threshold=3, priority=1, model_env_var="DEFAULT_MODEL_READING")
- `SYNTAX_FIX` (threshold=3, priority=2, model_env_var="DEFAULT_MODEL")
- `COMPLEX_REASONING` (threshold=3, priority=3, model_env_var="DEFAULT_MODEL_COMPLEX")
- `CASUAL` (threshold=1, priority=4, model_env_var="DEFAULT_MODEL")

Can be overridden in `config.toml` via `[[categories]]`. Category names are a **public API contract** — renaming is a breaking change affecting routing configs, OpenAPI spec, shell test scripts, and dashboard HTML placeholders. Documented at `src/intent_classifier.rs:35-38`.

#### Hardcoded Endpoint (`src/config.rs:12-13`)
- `NVIDIA_ENDPOINT_DEFAULT` = `"https://integrate.api.nvidia.com/v1/chat/completions"`
- Env override: `NVIDIA_ENDPOINT`
- Used when `hardcoded_routing()` constructs route entries with `provider_type="nvidia_nim"`, `api_key_env=Some("NVIDIA_API_KEY")` (`src/config.rs:110-112`)

#### Regex Pattern Arrays (`src/intent_classifier.rs`)
- `FILE_READING`: 12 patterns (`src/intent_classifier.rs:418-431`)
- `COMPLEX_REASONING`: 16 patterns (`src/intent_classifier.rs:433-450`)
- `SYNTAX_FIX`: 11 patterns (`src/intent_classifier.rs:452-464`)
- `CASUAL`: 5 patterns (`src/intent_classifier.rs:466-472`)
- `NEGATIVE`: 4 patterns (`src/intent_classifier.rs:474-479`)
- `NEGATIVE_META`: 4 entries with `suppressed` and `penalty=2` (`src/intent_classifier.rs:483-500`)
- 4 weight arrays (`FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS`) at `src/intent_classifier.rs:407-410`
- `SHORT_PROMPT_LEN` = `30` (`src/intent_classifier.rs:414`)
- SYNTAX_FIX dual-threshold: `sf_score >= 4 || (sf_score >= 3 && fr_score == 0)` (`src/intent_classifier.rs:716`)
- Ambiguity fallback trigger: `met_count >= 2` (`src/intent_classifier.rs:728`)

**None** of these patterns or weights have any env/config override path.

#### LLM Classifier Defaults (`src/config.rs:473-530` + `src/intent_classifier.rs`)
| Field | Default | Location |
|---|---|---|
| `model` | `"gpt-4o-mini"` | `src/config.rs:490` |
| `endpoint` | `""` (empty) | `src/config.rs:496` |
| `api_key_env` | `"OPENAI_API_KEY"` | `src/config.rs:502` |
| `provider_type` | `"openai_compatible"` | `src/config.rs:508` |
| `timeout_secs` | `3` (min `1`) | `src/config.rs:519` |
| Prompt template | Built-in `build_llm_classifier_prompt()` | `src/intent_classifier.rs:369-385` |
| `max_tokens` | `20` | `src/intent_classifier.rs:263` |
| `temperature` | `0.0` | `src/intent_classifier.rs:264` |
| Key refresh interval | `60` seconds | `src/intent_classifier.rs:221` |

#### Backend Order Defaults (`src/config.rs:344-350`)
- `ClassifiersConfig::default()` → `enabled: true`, `order: ["regex", "llm"]`
- Overridable via `config.toml` `[classifiers]` section

#### `parse_env_int` / Numeric Defaults
| Env Var | Default | Min | Max | Location |
|---|---|---|---|---|
| `PORT` | 10000 | 1 | 65535 | `src/main.rs:221` |
| `MAX_UPSTREAM_BODY_BYTES` | 10,485,760 | 1,048,576 | 100,485,760 | `src/config.rs:32` |
| `KEEPALIVE_INTERVAL_SECS` | 15 | 1 | — | `src/config.rs:38` |
| `DB_CONNECTION_RETRIES` | 3 | 1 | 10 | `src/persistence.rs:105` |
| `DB_RETRY_BASE_MS` | 1000 | 100 | 60000 | `src/persistence.rs:106` |
| `LOG_CONCURRENCY_LIMIT` | 100 | 1 | 1000 | `src/persistence.rs:150` |
| `STREAMING_CHANNEL_CAPACITY` | 32 | — | — | `src/main.rs:467` |
| `CLASSIFY_DB_LOG` | `false` | — | — | `src/main.rs:116` |
| `BASELINE_MODEL` | `DEFAULT_MODEL_COMPLEX` | — | — | `src/main.rs:141` |

#### Hardcoded Limits (No Env Override)
- Request body limit layer: `10 * 1024 * 1024` (10MB) — `src/main.rs:765`
- Reqwest client timeout: `120s` — `src/main.rs:108`
- Reqwest connect timeout: `30s` — `src/main.rs:109`
- DB pool: `max_connections=10`, `acquire_timeout=30s`, `idle_timeout=1800s` — `src/persistence.rs:99-101`
- Snippet truncation: `200` chars — `src/persistence.rs:454`
- Full message cap: `10_000` chars — `src/persistence.rs:435`
- Messages array size limit (DoS): `1000` — `src/persistence.rs:423`
- Error body cap: `2 * 1024` bytes — `src/main.rs:392,521`
- Error text truncation: `512` chars — `src/main.rs:400,410,537`
- Cost token ratio: `4.0` chars-per-token — `src/persistence.rs:461`
- Dashboard default hours: `24` — `src/dashboard.rs:157,158,263,310`
- Dashboard hours clamp: `[1, 720]` — `src/dashboard.rs:262`
- Dashboard page limit: `20` (default), `100` (max) — `src/dashboard.rs:197-198`
- Dashboard recent count: `5` — `src/dashboard.rs:159`
- Bind address: always `"0.0.0.0:{port}"` — `src/main.rs:224`

#### Auth Provider Map (`src/intent_classifier.rs:507-514`)
| `provider_type` | Header | Value Format |
|---|---|---|
| `"openai_compatible"` / `""` | `authorization` | `Bearer {key}` |
| `"anthropic"` | `x-api-key` | `{key}` |
| `"ollama"` / `"local"` | (none) | — |
| unknown | `authorization` | `Bearer {key}` |

Fully hardcoded — adding a new provider requires a source change.

#### Dashboard UI
- 4 SVG icons (`src/dashboard.rs:38-41`)
- 4 `NavPage` entries in `PAGES` static array (`src/dashboard.rs:43-64`)
- Static file path: `ServeDir::new("static")` — `src/dashboard.rs:334`

**Approximately 65% of the ~65 hardcoded values have no runtime override path at all.**

---

### 2. Config Loading Chain (Full Trace)

The startup sequence in `src/main.rs:45-234` proceeds in strict order:

#### Phase A: Mandatory Startup (Panics on Failure)

1. **Tracing** (`main.rs:47-54`): `RUST_LOG` → `info` default; `LOG_FORMAT` → compact/JSON
2. **AuthConfig** (`main.rs:61-64` → `auth.rs:17-27`): Reads `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD` — **panics if any missing**
3. **PersistenceConfig** (`main.rs:66-75` → `persistence.rs:91-156`):
   - `DATABASE_URL` missing → graceful degradation (`None`)
   - `DATABASE_URL` present but DB unreachable after retries → **panic**
   - Migrations failure → **panic**
   - Connection retries: exponential backoff with jitter, configurable via `DB_CONNECTION_RETRIES` + `DB_RETRY_BASE_MS`

#### Phase B: Optional Config File

4. **`CONFIG_PATH` env var** (`main.rs:76`):
   - **Not set** → `config_root = None` → **all guards fall back to hardcoded defaults**
   - Set but file read fails → `warn!` + `config_root = None` → hardcoded defaults
   - Set but TOML parse fails → `warn!` + `config_root = None` → hardcoded defaults
   - Set + valid → `config_root = Some(toml::Value)` → used throughout

#### Phase C: Config Section Extraction from `toml::Value`

5. **Regex Classifier Config** (`main.rs:96-99` → `config.rs:420-437`):
   - If `config_root` is `None` → `RegexClassifierConfig::default()` (`enabled: true`)
   - Looks for `[regex_classifier]` section; absent → default
6. **Classifiers Config** (`main.rs:102-105` → `config.rs:355-385`):
   - If `config_root` is `None` → `ClassifiersConfig::default()` (`enabled: true`, `order: ["regex", "llm"]`)
   - Looks for `[classifiers]`; absent → default
7. **HTTP Client** (`main.rs:107-111`): Hardcoded `timeout=120s`, `connect_timeout=30s`
8. **`CLASSIFY_DB_LOG`** (`main.rs:113-116`): Direct env read, default `false`
9. **HTTP Config** (`main.rs:117-119`): `MAX_UPSTREAM_BODY_BYTES`, `KEEPALIVE_INTERVAL_SECS`

#### Phase D: Classifier Construction (the `else` branch at `main.rs:142-207`)

10. **Categories** (`main.rs:121-124`):
    - `load_categories_from_value(root)` succeeds → use parsed categories
    - Returns `Err` or `config_root` is `None` → `hardcoded_categories()` (4 entries)
11. **Routing** (`main.rs:125-138`):
    - `routing_from_value(root)` succeeds → parsed routing map + fallback
    - Returns `Err` → `warn!` + `hardcoded_routing(&categories)`
    - `config_root` is `None` → `hardcoded_routing(&categories)` directly
    - `hardcoded_routing()` (`config.rs:93-124`): Uses `NVIDIA_ENDPOINT` + per-category model env vars, all with `provider_type="nvidia_nim"`
12. **Model Costs** (`main.rs:139` → `config.rs:327-335`): Seeds `hardcoded_model_costs()` then applies per-route `cost_per_1m_input_tokens` overrides
13. **Baseline Model** (`main.rs:140-141`): `env_or_default("BASELINE_MODEL", DEFAULT_MODEL_COMPLEX)`
14. **Classifier Chain** (`main.rs:142-207`):
    - If `classifiers_config.enabled == false` → no backends
    - Iterates `classifiers_config.order`, instantiating `"regex"` and `"llm"` backends
    - `"llm"` requires explicit `enabled = true` in `config.toml` (defaults to `false` when section absent)
    - Builds `ClassifierChain`, merges routing from backends (`get_routing()`)

#### Phase E: Assembly

15. **AppState** (`main.rs:209-219`): All values collected into `Arc<AppState>`
16. **PORT** (`main.rs:221`): `parse_env_int("PORT", 10000, ...)`
17. **Routes** (`main.rs:223`): `build_app(auth_config, app_state)` — see `src/main.rs:732-767`

#### Key Insight: Config Flow is a Ladder of Fallbacks

```
CONFIG_PATH set?
  ├─ Yes → read file → parse TOML
  │   ├─ Valid → load_categories_from_value() → load routing_from_value() → ...
  │   └─ Invalid → WARN → hardcoded_categories() → hardcoded_routing()
  └─ No → hardcoded_categories() → hardcoded_routing() → hardcoded_model_costs()
```

Every loader function in `config.rs` operates on `&toml::Value` — pure, side-effect-free functions. This means they are **directly reusable** from a config-reload path — just parse a new TOML string and call the same functions.

---

### 3. Dashboard Integration & Runtime Reload Architecture

#### Current Dashboard-Config Coupling

The dashboard **reads** these config-derived values from `AppState` at handler time:
- `baseline_model: String` — displayed in Dashboard + Savings templates (`src/dashboard.rs:107,131`)
- `classifier_active: bool` — derived from `state.classifier.is_some()` (`src/dashboard.rs:137`)
- `model_costs: ModelCosts` — used in savings calculation (`src/dashboard.rs:158,309`)
- `db_connected: bool` — derived from `state.persistence.is_some()` (`src/dashboard.rs:136`)

**Critical**: All are read as snapshots — there is no mutation path. Dashboard routes are all `GET` only (`src/dashboard.rs:329-334`). No `POST`/`PUT` routes exist.

#### Existing Runtime-Updatable Pattern: LLMClassifier's API Key

The `LLMClassifier` struct (`src/intent_classifier.rs:175-186`) already has a **proven pattern**:

```rust
api_key: Arc<tokio::sync::RwLock<String>>,  // Runtime-updatable
```

**How it works** (`src/intent_classifier.rs:212-232`):
1. Key is read from env on construction → stored in `Arc<RwLock<String>>`
2. Background `tokio::spawn` task loops every 60s, re-reads env var, writes via `api_key.write().await`
3. Classification hot path reads with `api_key.read().await.clone()` — cheap, contention-free when no writer
4. Clean abort on drop via `AbortHandle` (`src/intent_classifier.rs:188-191`)

This pattern is **battle-tested in production** and directly applicable to `AppState` fields.

#### Proposed `AppState` Field Upgrades

| Field | Current | Target | Rationale |
|---|---|---|---|
| `model_costs` | `ModelCosts` | `Arc<RwLock<ModelCosts>>` | Config overrides change |
| `baseline_model` | `String` | `Arc<RwLock<String>>` | Dashboard can swap baseline |
| `routing` | `Arc<HashMap<...>>` | `Arc<RwLock<HashMap<...>>>` | Routes need updating |
| `classifier` | `Option<Arc<ClassifierChain>>` | `Arc<RwLock<Option<Arc<ClassifierChain>>>>` | **Hardest** — requires full rebuild |
| `classify_db_log` | `bool` | `Arc<AtomicBool>` | Atomic toggle |
| `max_upstream_body_bytes` | `usize` | `Arc<RwLock<usize>>` | Limits change |
| `keepalive_interval_secs` | `u64` | `Arc<RwLock<u64>>` | Timing changes |

Read locks on hot paths (`completion_handler` reads `routing` + `model_costs`) are cheap — `tokio::sync::RwLock::read()` is contention-free when no writer holds the lock.

#### The `classifier` Field is the Hardest

Swapping the classifier requires:
1. Parsing new config TOML → calling `load_categories_from_value()`, `load_classifiers_config_from_value()`, etc.
2. Building a new `RegexClassifier` (compile regex patterns) — **synchronous**, ~tens of ms
3. Building a new `LLMClassifier` (spawns background key-rotation task) — **async init**
4. Dropping old `Arc<ClassifierChain>` → drops old `LLMClassifier` → `AbortHandle` aborts its rotation task
5. Atomic swap under write lock

Impact during swap: new classification requests wait on write lock acquisition (~microseconds for pointer swap), then use new backends. In-flight classifications on old `Arc<ClassifierChain>` continue (Arc refcount keeps them alive).

#### Dashboard Reload Endpoint Design

Two viable approaches:

**A. Body-driven**: `POST /dashboard/config` with TOML body
- User pastes new config → parse → apply → return summary
- Pros: immediate, no file needed
- Cons: sensitive API keys in browser

**B. File-reload**: `POST /dashboard/config/reload`
- Re-reads `CONFIG_PATH` from disk → parse → apply
- Pros: single source of truth, keys stay on filesystem
- Cons: requires filesystem access, no preview

**Recommended**: Support both — `POST /dashboard/config` accepts a TOML body for testing, `POST /dashboard/config/reload` re-reads from disk for production. Both protected by existing dashboard basic auth.

---

### 4. In-Memory Default Config Strategy

The "temporary filesystem in memory" concept maps cleanly to Rust's `include_str!()`:

```rust
const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
```

This embeds the repo's canonical `config.toml` into the binary at compile time. No filesystem dependency. Multiple named defaults could be stored:

```rust
const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
const NVIDIA_NIM_CONFIG_TOML: &str = include_str!("../routing_examples/routing-nvidia-nim.toml");
const OPENROUTER_CONFIG_TOML: &str = include_str!("../routing_examples/routing-openrouter.toml");
```

The fallback chain becomes:
1. Try live file at `CONFIG_PATH` → parse → apply
2. If `CONFIG_PATH` unset or file read/parse fails → use `DEFAULT_CONFIG_TOML`
3. If embedded default parse fails (should not happen in CI-validated builds) → hardcoded values (last resort)

This eliminates the "CONFIG_PATH not set → all hardcoded" path for the first two layers, while keeping the hardcoded escape hatch as a panic-safe fallback.

#### Config Switching from Dashboard

With multiple embedded configs, a new `POST /dashboard/config/switch` endpoint could accept a named preset:
- `"default"` → `DEFAULT_CONFIG_TOML`
- `"nvidia-nim"` → `NVIDIA_NIM_CONFIG_TOML`
- `"openrouter"` → `OPENROUTER_CONFIG_TOML`

The handler parses the named TOML and performs the `AppState` atomic swap described above.

---

### 5. Impact Assessment by Field

| Field | Upgrade Complexity | Risk | Hot Path Impact |
|---|---|---|---|
| `model_costs` | **Low** | Minimal | Read lock per savings query |
| `baseline_model` | **Low** | Minimal | Read lock per savings query |
| `classify_db_log` | **Low** | Minimal | Atomic load per classify request |
| `max_upstream_body_bytes` | **Low** | Minimal | Read lock per buffered response |
| `keepalive_interval_secs` | **Low** | Minimal | Read lock per SSE stream creation |
| `routing` | **Medium** | Every `completion_handler` reads routing | Read lock on hot proxy path |
| `classifier` | **High** | Must rebuild chain, drop old with AbortHandle | Read lock on all classification paths |

---

## Architecture Insights

### Pattern: `Arc<RwLock<T>>` for Hot-Read/Cold-Write Config

The `LLMClassifier` API key pattern (`src/intent_classifier.rs:180`) demonstrates that `tokio::sync::RwLock` works well for config that is read on every request but written rarely. Read locks are contention-free when no writer holds the lock. Dashboard config changes are infrequent (human-triggered), so write contention is negligible.

### Pattern: `include_str!()` for Embedded Config

No need for `tempfile` or virtual filesystems. TOML parsing operates on `&str`, and `include_str!()` embeds file contents at compile time. The existing loader functions (`load_categories_from_value`, `routing_from_value`, etc.) already accept `&toml::Value` — they don't need a file.

### Pattern: Pure Config Loader Functions

All config loader functions in `config.rs` are pure (`&toml::Value` → `ConfigStruct`). This makes them trivially reusable from both startup and reload paths. No refactoring of the loaders themselves is needed — only how their output gets into `AppState`.

---

## Historical Context (from prior changes)

- `context/changes/classifier-config-boundary/` — Already formalized the generic/specific config boundary with `[classifiers]` section, per-backend enable/disable, and ordering. This established the `config.toml` frontend for classifier configuration. The current proposal extends this to runtime reloadability.
- `context/changes/classifier-config-boundary/research.md` — Documents how `RegexClassifier` and `LLMClassifier` config is extracted from `toml::Value` via pure loader functions. Validates that the loader functions are already structured for reuse from multiple call sites.

---

## Related Research

- `context/changes/classifier-config-boundary/research.md` — Prior exploration of the config boundary, classifier enable/disable, and TOML parsing (adjacent topic)
- `context/changes/in-memory-db-fallback/` — Related in-memory strategy (different domain: DB fallback)

---

## Open Questions

1. **Should the regex pattern arrays also become configurable?** Currently 48 hardcoded patterns with weights — making them loadable from TOML would eliminate one of the largest hardcoded blocks, but increases config complexity significantly. The patterns are tightly coupled to the `classify_internal()` algorithm (negative suppression, dual-threshold logic). This could be a separate change.

2. **Should `provider_type` → auth header mappings be configurable?** Currently a hardcoded match statement (`src/intent_classifier.rs:507-514`). Making this data-driven would allow new providers without code changes. Could be part of the routing config.

3. **Should the dashboard expose a "config preview/diff" before applying?** The `POST /dashboard/config` approach could return parsed validation results before applying. This adds safety but also complexity — the dashboard currently has no concept of "draft" state.

4. **What happens if a config reload fails mid-swap?** The `AppState` write lock should be held for the entire swap (validate new config, build new components, then swap). If validation fails, the lock is released and old config stays in place. Need to ensure the swap path is panic-safe.

5. **Should `CONFIG_PATH` file be watched for changes?** The `notify` crate could spawn a background task similar to the LLM key rotation task, auto-reloading when `config.toml` changes on disk. This plus the dashboard reload button gives two independent triggers. Worth doing only if it doesn't add significant complexity — the dashboard button alone might suffice.

---

## Follow-up Research (2026-06-09): Single Source of Truth Design

### Design Decision: Tri-Tier Configuration Model

After further analysis, the target architecture has exactly three tiers with clear boundaries:

---

### Tier 1: Environment Variables — Secrets & Keys ONLY

**What stays as env vars:**
- `PROXY_API_BEARER_TOKEN` — proxy endpoint auth (required, panics if missing)
- `DASHBOARD_BASIC_USER` — dashboard basic auth username (required)
- `DASHBOARD_BASIC_PASSWORD` — dashboard basic auth password (required)
- `DATABASE_URL` — database connection string (optional; if absent, persistence disabled)
- Any API key env vars referenced by `api_key_env` fields in routing entries (e.g., `OPENAI_API_KEY`, `NVIDIA_API_KEY`, `GROQ_API_KEY`) — these are dynamic by nature, may be rotated externally

**Rationale**: Secrets must never appear in config files that could be committed to version control. Env vars are the standard container/platform secret injection mechanism. The `LLMClassifier` already demonstrates runtime key rotation from env vars every 60s (`src/intent_classifier.rs:219-232`).

**What moves out of env vars:**
- `PORT`, `RUST_LOG`, `LOG_FORMAT` — infrastructure config, belongs in TOML
- `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX`, `DEFAULT_MODEL_READING` — model selection, belongs in categories/routing config
- `BASELINE_MODEL` — savings calculation baseline, belongs in TOML
- `NVIDIA_ENDPOINT` — endpoint config, belongs in routing
- `CLASSIFY_DB_LOG` — behavioral toggle, belongs in TOML
- All `parse_env_int` vars (`MAX_UPSTREAM_BODY_BYTES`, `KEEPALIVE_INTERVAL_SECS`, `DB_CONNECTION_RETRIES`, etc.) — operational params, belongs in TOML
- `ALLOWED_ORIGINS` — CORS config, belongs in TOML
- `CONFIG_PATH` — **becomes an override**, not a requirement. The app always has an embedded default config. Setting `CONFIG_PATH` just points to a different TOML file that overlays/overrides the embedded one.
- `STREAMING_CHANNEL_CAPACITY` — SSE config, belongs in TOML

---

### Tier 2: config.toml — Single Source of Truth for All Non-Secret Configuration

**Embedded default via `include_str!()`:**

```rust
// In config.rs or main.rs
const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
```

The binary always ships with a valid, internally-consistent `config.toml`. The `CONFIG_PATH` env var becomes an optional overlay — if set, it's parsed and merged on top of the embedded default (or replaces it entirely).

**What goes into config.toml (no more hardcoded fallbacks):**

| Section | Contains | Current Fallback | Eliminated? |
|---|---|---|---|
| `[[categories]]` | Category definitions (name, description, threshold, priority, model_env_var) | `hardcoded_categories()` (`src/intent_classifier.rs:49-80`) | Yes |
| `[FILE_READING]`, `[SYNTAX_FIX]`, etc. | Per-category routing entries (model, endpoint, cost, provider_type, api_key_env) | `hardcoded_routing()` (`src/config.rs:93-124`) | Yes |
| `[FALLBACK]` | Default routing entry for unclassified/ambiguous prompts | `hardcoded_routing()` fallback | Yes |
| `[model_costs]` | Per-model cost overrides for savings calculation | `hardcoded_model_costs()` (`src/intent_classifier.rs:16-23`) | Yes |
| `[classifiers]` | Master enable/disable + backend order | `ClassifiersConfig::default()` (`src/config.rs:344-350`) | Yes |
| `[regex_classifier]` | Regex enable/disable + pattern definitions + weights | `RegexClassifierConfig::default()` (`src/config.rs:394-396`) | Yes |
| `[llm_classifier]` | LLM backend config (model, endpoint, provider_type, timeout, prompt_template_path) | Defaults in `unwrap_or()` chains (`src/config.rs:490-519`) | Yes |
| `[patterns]` / `[[pattern_group]]` | Regex patterns per category, weights, negative suppression rules | 5 pattern arrays + 4 weight arrays + `NEGATIVE_META` (`src/intent_classifier.rs:407-500`) | **New** — currently pure hardcoded |
| `[server]` | Port, bind address, log level, log format | Various `parse_env_int()` defaults + `main.rs:47,224` | Yes |
| `[http]` | Max upstream body bytes, keepalive interval, request body limit, client timeouts, streaming channel capacity | Scattered hardcoded values in `main.rs:108-109,765` and `config.rs:32-38` | Yes |
| `[database]` | Connection retries, retry base ms, pool settings, log concurrency | `parse_env_int` defaults in `persistence.rs:99-106,150` | Yes |
| `[cors]` | Allowed origins, methods, headers | Hardcoded in `main.rs:755-756` | Yes |
| `[auth_providers]` | Provider type → header name/format mapping | Hardcoded match in `src/intent_classifier.rs:507-514` | **New** — currently pure hardcoded |
| `[dashboard]` | Default hours, page sizes, limits | Hardcoded in `src/dashboard.rs:157-198,262` | Yes |
| `baseline_model` | Top-level or in `[costs]` section | `env_or_default("BASELINE_MODEL", DEFAULT_MODEL_COMPLEX)` | Yes |

**Regex patterns in config.toml — the TOML structure:**

```toml
[[pattern_group]]
category = "FILE_READING"
patterns = [
    { regex = "(?i)\\b(?:read|show)...", weight = 3 },
    { regex = "(?i)\\b(?:show|display)...", weight = 3 },
    # ... 12 total
]

[[pattern_group]]
category = "SYNTAX_FIX"
patterns = [
    { regex = "(?i)\\b(?:fix|correct)...", weight = 3 },
    # ... 11 total
]

[[negative_rule]]
pattern = "(?i)\\b(?:read|show)...(?:architecture|design)"
suppressed = "COMPLEX_REASONING"
penalty = 2
# ... 4 total

[regex_classifier]
enabled = true
short_prompt_len = 30
```

This eliminates all compile-time pattern constants in `src/intent_classifier.rs:418-500` and the weight arrays at `src/intent_classifier.rs:407-410`. The `build_all_patterns()` function (`src/intent_classifier.rs:534-597`) becomes a pure transformation from parsed config → `RegexSet` instead of referencing `const` arrays.

**Auth provider mapping in config.toml:**

```toml
[[auth_provider]]
type = "openai_compatible"
header = "authorization"
value_template = "Bearer {api_key}"

[[auth_provider]]
type = "anthropic"
header = "x-api-key"
value_template = "{api_key}"

[[auth_provider]]
type = "ollama"
# no auth header

[[auth_provider]]
type = "local"
# no auth header
```

This eliminates the hardcoded match statement at `src/intent_classifier.rs:507-514`. `auth_headers_for()` becomes a lookup into a parsed map.

---

### Tier 3: Not Configurable — Protocol Constants & Operational Safety

**What stays hardcoded (by design choice):**

- `"Bearer "` prefix (`src/auth.rs:146`) — HTTP protocol constant
- `"Basic "` prefix (`src/auth.rs:157`) — HTTP protocol constant
- Dashboard realm string `"Basic realm=\"cerebrum-dashboard\""` (`src/auth.rs:216`) — protocol constant
- Dashboard SVG icons (`src/dashboard.rs:38-41`) — UI, not behavioral
- `PAGES` static array (`src/dashboard.rs:43-64`) — maps 1:1 to template files; adding a page requires code anyway
- Static file path `"static"` (`src/dashboard.rs:334`) — convention, not config
- Bind address format `"0.0.0.0:{port}"` (`src/main.rs:224`) — deployment convention
- Code block regex for prompt sanitization (`src/intent_classifier.rs:520`) — algorithm implementation detail
- `4.0` chars-per-token ratio (`src/persistence.rs:461`) — heuristic constant; could go either way
- Snippet truncation (200 chars), message cap (10000), messages array limit (1000), error body cap (2KB), error text truncation (512) — these are **DoS safety limits**, not user-facing config. Changing them without understanding the downstream impact is dangerous. They could move to TOML but with strongly argued defaults.

---

### Resulting Architecture

```
┌─────────────────────────────────────────────┐
│  Build time: include_str!("../config.toml")  │
│  ↓ embedded DEFAULT_CONFIG_TOML              │
├─────────────────────────────────────────────┤
│  Startup:                                    │
│    CONFIG_PATH set?                          │
│    ├─ Yes → read file → parse → use instead  │
│    └─ No  → use DEFAULT_CONFIG_TOML          │
│                                              │
│    All auth keys from env vars only          │
│    All API key env refs from env vars only   │
├─────────────────────────────────────────────┤
│  Runtime: Dashboard POST /dashboard/config   │
│    → parse TOML → validate → atomic swap     │
│    → AppState fields under Arc<RwLock<>>     │
└─────────────────────────────────────────────┘
```

**Key property**: The app is never in a "no config" state. It always has at least the embedded default. There is no fallback to hardcoded values — if the embedded TOML is broken, it's a build-time CI failure, not a runtime surprise.

---

### Migration Complexity by Module

| Module | What Changes | Hardcoded Eliminated |
|---|---|---|
| `routing.rs` | Remove `DEFAULT_MODEL*` constants, `ModelCosts` stays | 3 const strings |
| `intent_classifier.rs` | Remove `hardcoded_categories()`, `hardcoded_model_costs()`, all pattern arrays, all weight arrays, `NEGATIVE_META`, `SHORT_PROMPT_LEN`, `auth_headers_for()` match → data-driven | ~60 values |
| `config.rs` | Remove `hardcoded_routing()`, `hardcoded_model_default()`, `NVIDIA_ENDPOINT_DEFAULT`, all `unwrap_or()` defaults in loaders; add pattern group loader, add auth provider loader | ~15 values |
| `main.rs` | Clean startup to use embedded config; remove scattered `parse_env_int` for moved vars; route `CONFIG_PATH` as optional override | ~10 env reads |
| `persistence.rs` | DB pool/retry settings from config struct, not env; keep `DATABASE_URL` from env | 5 env reads |
| `dashboard.rs` | Dashboard defaults from config struct | 5 hardcoded numbers |
| `auth.rs` | No changes (secrets stay in env) | 0 |

---

### Open Questions (Resolved)

1. **Should regex patterns become configurable?** → **Yes**. They are data, not logic. Moving them to TOML is the single biggest hardcoded-value elimination and enables custom classification without recompilation.

2. **Should provider_type mappings be configurable?** → **Yes**. The `auth_providers` TOML table makes the system extensible without code changes. New providers just need a config entry.

3. **Should config preview/diff be exposed?** → Nice-to-have, not required for MVP. The atomic swap + validation-before-swap pattern is sufficient safety.

4. **What if config reload fails mid-swap?** → Validate the entire new config before taking the write lock. Build the new `ClassifierChain` (synchronous regex compile, async LLM client init) behind the lock, then pointer-swap. If anything fails, release lock without mutation.

5. **Should CONFIG_PATH be file-watched?** → Nice-to-have; the dashboard reload button provides operational parity. Add notify-based watching later if needed.
