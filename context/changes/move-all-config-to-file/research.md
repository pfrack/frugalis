---
date: 2026-06-10T17:26:13Z
researcher: OpenCode
git_commit: 37ca6cb5e73b44601bac1474c5851cc8197e4955
branch: main
repository: cerebrum
topic: "Move all non-secret configuration from environment variables into config.toml"
tags: [research, codebase, configuration, environment-variables, config-toml, secrets]
status: complete
last_updated: 2026-06-10
last_updated_by: OpenCode
last_updated_note: "Added follow-up research for hardcoded models, categories, and internal limits"
---


# Research: Move All Config to File

**Date**: 2026-06-10T17:26:13Z
**Researcher**: OpenCode
**Git Commit**: `37ca6cb`
**Branch**: `main`
**Repository**: cerebrum

## Research Question

Move everything to config. From env there should be only API_KEYS, auth credentials (`PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`), and `DATABASE_URL`. Nothing else.

## Summary

The codebase already has a mature `config.toml` system (`in-memory-config-filesystem` change, commit `d131455`) that embeds defaults at compile time and supports optional overlay via `CONFIG_PATH`. However, **12 production env vars remain read directly in Rust code** that should be in `config.toml` instead. The prior change explicitly moved many env vars to config but left behind legacy env var reads and didn't complete the migration for CORS, logging, port, and fallback model names.

The categorization:

| Category | Count | Env Vars |
|----------|-------|----------|
| **Must stay in env (SECRETS)** | 5+ | `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL`, and all `api_key_env`-referenced keys (e.g., `NVIDIA_API_KEY`, `OPENAI_API_KEY`) |
| **Already in config.toml, env is merely an override** | 2 | `PORT`, `CONFIG_PATH` |
| **Not in config.toml, should be moved there** | 6 | `LOG_FORMAT`, `ALLOWED_ORIGINS`, `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`, `RUST_LOG`, `ROUTING_CONFIG_PATH` |
| **Test-only env vars that read PRODUCTION env var names** | 2 | `MAX_UPSTREAM_BODY_BYTES`, `KEEPALIVE_INTERVAL_SECS` (test helpers use `parse_env_int` which reads env) |

The prior `in-memory-config-filesystem` plan (`context/changes/in-memory-config-filesystem/plan.md`) explicitly listed `PORT`, `RUST_LOG`, `LOG_FORMAT`, `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`, `ALLOWED_ORIGINS`, `MAX_UPSTREAM_BODY_BYTES`, `KEEPALIVE_INTERVAL_SECS` as targets for migration but the implementation only partially completed this — notably the `PORT` env var read still exists at `src/main.rs:279` and several others were never cleaned up.

## Detailed Findings

### 1. Configuration Architecture (Current State)

The system uses a tri-tier model:

**Tier 1 — `config.toml` (embedded + overlay)**: Compiled in via `include_str!("../config.toml")` at `src/main.rs:73`. Optional overlay via `CONFIG_PATH` env var at `src/main.rs:70-80`, merged with `config::merge_toml_values()` at `src/config.rs:192-207`. Sections loaded by dedicated `load_*_from_value()` functions in `src/config.rs`.

**Tier 2 — Environment variables (inconsistent mix)**: Auth credentials + some config values still read via `std::env::var` directly throughout `src/main.rs`, `src/auth.rs`, `src/intent_classifier.rs`.

**Tier 3 — Hardcoded constants**: Pattern arrays, weight arrays, negative patterns in `src/intent_classifier.rs`; `DEFAULT_MODEL`/`DEFAULT_MODEL_COMPLEX` in `src/routing.rs:48-49`.

### 2. Current config.toml Coverage (`config.toml`)

Fully covered sections (loaded via `load_*_from_value()`, no env var fallback):

| TOML Section | Config Struct | Loaded at |
|-------------|--------------|-----------|
| `[server]` | `ServerConfig` (port only) | `src/config.rs:58` |
| `[http]` | `HttpConfig` (6 fields) | `src/config.rs:77` |
| `[database]` | `DatabaseConfig` (6 fields) | `src/config.rs:116` |
| `[persistence]` | `PersistenceSettings` (2 fields) | `src/config.rs:313` |
| `[classifiers]` | `ClassifiersConfig` (2 fields) | `src/config.rs:649` |
| `[regex_classifier]` | `RegexClassifierConfig` (1 field) | `src/config.rs:714` |
| `[llm_classifier]` | `LlmClassifierConfig` (6 fields) | `src/config.rs:767` |
| `[[categories]]` | `Vec<CategoryConfig>` (4 fields each) | `src/config.rs:566` |
| `[FILE_READING]` etc. | `RouteEntry` per category | `src/config.rs:507` |
| `[model_costs]` | `HashMap<String, f64>` | `src/config.rs:610` |
| `[[auth_provider]]` | `Vec<AuthProviderConfig>` | `src/config.rs:155` |
| `[dashboard]` | `DashboardConfig` (6 fields) | `src/config.rs:19` |

Not yet in config.toml — read from env vars:

| Setting | Env Var | Read at |
|---------|---------|---------|
| Log format (json vs compact) | `LOG_FORMAT` | `src/main.rs:53` |
| CORS allowed origins | `ALLOWED_ORIGINS` | `src/main.rs:803` |
| Fallback model name | `DEFAULT_MODEL` | `src/config.rs:13` → `intent_classifier.rs:620` |
| Hardcoded routing endpoint | `NVIDIA_ENDPOINT` | `src/config.rs:388` |
| Config file path override | `CONFIG_PATH` | `src/main.rs:70` |
| Log filter level | `RUST_LOG` | `src/main.rs:51` (via `EnvFilter`) |
| Routing config path (legacy) | `ROUTING_CONFIG_PATH` | `src/config.rs:471` (test helper) |

### 3. Environment Variables That MUST Stay (SECRETS)

These hold credentials, tokens, or connection strings and must **never** appear in config files:

| Env Var | Purpose | Read at | Required? |
|---------|---------|---------|-----------|
| `PROXY_API_BEARER_TOKEN` | Bearer token for `/v1/*` routes | `src/auth.rs:18` | **YES** (panics if missing) |
| `DASHBOARD_BASIC_USER` | Dashboard basic auth username | `src/auth.rs:19` | **YES** |
| `DASHBOARD_BASIC_PASSWORD` | Dashboard basic auth password | `src/auth.rs:20` | **YES** |
| `DATABASE_URL` | Postgres connection string | `src/main.rs:212`, `src/persistence.rs:121` | No (graceful fallback) |
| `NVIDIA_API_KEY` | NVIDIA NIM API key | `src/main.rs:703` (via api_key_env) | No (degrades) |
| `OPENAI_API_KEY` | OpenAI API key (LLM classifier) | `src/intent_classifier.rs:209` | No (degrades) |
| `OPENROUTER_API_KEY` | OpenRouter API key | Referenced in routing examples | No |
| `GROQ_API_KEY` | Groq API key | Referenced in manual-test scripts | No |

### 4. Environment Variables to Move to config.toml

These are non-sensitive and should live in `config.toml`:

#### 4.1 `LOG_FORMAT` → `[server].log_format` (`src/main.rs:53`)

Current code:
```rust
// src/main.rs:53-55
let fmt_layer = match std::env::var("LOG_FORMAT").as_deref() {
    Ok("json") => fmt::layer().json().with_filter(log_filter).boxed(),
    _ => fmt::layer().compact().with_filter(log_filter).boxed(),
};
```

**Action**: Add `log_format: Option<String>` to `ServerConfig` (or a new field). The `config.toml` already has `log_format = "compact"` at line 11. The `load_server_config_from_value()` at `src/config.rs:58` should read `log_format` from `[server]`. Replace the `std::env::var("LOG_FORMAT")` with a config read.

#### 4.2 `ALLOWED_ORIGINS` → `[cors].allowed_origins` (`src/main.rs:803`)

Current code:
```rust
// src/main.rs:803-808
let allowed_origin_headers: Vec<HeaderValue> = std::env::var("ALLOWED_ORIGINS")
    .unwrap_or_default()
    .split(',')
    .filter(|s| !s.trim().is_empty())
    .filter_map(|s| header::HeaderValue::from_str(s.trim()).ok())
    .collect();
```

**Action**: The `config.toml` already has a `[cors]` section placeholder with `allowed_origins = []` at line 22 but there is **no loader function** for it. Create `load_cors_config_from_value()` that reads an array of origin strings. Convert to `Vec<HeaderValue>` at use site. Add a `CorsConfig` struct.

#### 4.3 `DEFAULT_MODEL` → `baseline_model` or routing config (`src/config.rs:13`)

Current code:
```rust
// src/intent_classifier.rs:620
model: crate::config::env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
```

Where `env_or_default` at `src/config.rs:13` reads `std::env::var("DEFAULT_MODEL")`.

This is used in `ClassificationResult::fallback()` when all classifiers fail. The default is `"meta/llama-3.1-8b-instruct"` (from `src/routing.rs:48`).

**Action**: `config.toml` already has `baseline_model = "meta/llama-3.3-70b-instruct"` at line 112. Use that. The `baseline_model` is already loaded at `src/main.rs:136-140`. Change `ClassificationResult::fallback()` to accept the baseline model as a parameter rather than reading an env var.

#### 4.4 `NVIDIA_ENDPOINT` → routing config (`src/config.rs:388`)

Current code:
```rust
// src/config.rs:388 (inside hardcoded_routing())
let endpoint = env_or_default("NVIDIA_ENDPOINT",
    "https://integrate.api.nvidia.com/v1/chat/completions");
```

**Action**: The `hardcoded_routing()` fallback is used when `routing_from_value()` fails. Since every category now has its own `endpoint` field in `config.toml` (lines 78, 84, 90, 96, 102), this hardcoded fallback endpoint should come from a config value, not an env var. Add a `default_endpoint` field to some config section, or use the `[FALLBACK].endpoint` value.

#### 4.5 `RUST_LOG` → `[server].log_level` (`src/main.rs:51`)

Current code:
```rust
// src/main.rs:51
let log_filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"));
```

**Action**: The `EnvFilter::try_from_default_env()` reads `RUST_LOG`. `config.toml` already has `log_level = "info"` at line 10 but nothing reads it. Add `log_level: Option<String>` to `ServerConfig`, read it in `load_server_config_from_value()`, and construct `EnvFilter::new(config.log_level)` when set.

#### 4.6 `ROUTING_CONFIG_PATH` → deprecated (`src/config.rs:471`)

This is a **legacy** env var used only in the test helper `load_routing()` at `src/config.rs:468-503`, which itself is `#[cfg(test)]` only. The production path uses `routing_from_value()` which reads from the merged `config_root`. Since routing is already in `config.toml`, `ROUTING_CONFIG_PATH` is vestigial.

**Action**: Remove `ROUTING_CONFIG_PATH` entirely from the test helper. Use `CONFIG_PATH` or the embedded config for test routing.

### 5. Test Helpers That Read Production Env Vars

Two test-only functions use `config::parse_env_int()` which reads env vars by calling `std::env::var()`:

- `make_test_app_state()` at `src/main.rs:876` — reads `MAX_UPSTREAM_BODY_BYTES` with `parse_env_int`
- `build_app_with_persistence()` at `src/main.rs:1826-1837` — reads both `MAX_UPSTREAM_BODY_BYTES` and `KEEPALIVE_INTERVAL_SECS`
- `test_streaming_keepalive_injected()` at `src/main.rs:2614,2666` — reads `KEEPALIVE_INTERVAL_SECS`

These test helpers already have fallback defaults and the `parse_env_int` function is `#[cfg(test)]`. The values themselves (`max_upstream_body_bytes`, `keepalive_interval_secs`) are already in `config.toml`'s `[http]` section and loaded into `HttpConfig` in production code. The test helpers should just use hardcoded defaults directly rather than reading env vars.

### 6. Config Not Yet in config.toml (New Sections Needed)

#### `[cors]` section
```toml
[cors]
allowed_origins = ["https://app.example.com"]
```
Needs: `CorsConfig` struct + `load_cors_config_from_value()` in `src/config.rs`.

#### `[server]` section additions
```toml
[server]
port = 10000
log_level = "info"
log_format = "compact"   # already in config.toml but not read
```
Needs: `ServerConfig` to include `log_level: String`, `log_format: String`.

### 7. Hardcoded Values That Could Move to Config (Future)

These are currently hardcoded but could be configurable:

| Location | Value | Notes |
|----------|-------|-------|
| `src/persistence.rs:1026` | Snippet truncation (200 chars) | DoS safety limit |
| `src/persistence.rs:1059` | Message cap (10,000 chars) | DoS safety limit |
| `src/persistence.rs:1048` | Messages array limit (1000) | DoS safety limit |
| `src/main.rs:456` | Error body cap (2 KB) | DoS safety limit |
| `src/intent_classifier.rs:412` | `SHORT_PROMPT_LEN = 30` | Classification threshold |
| `src/routing.rs:48-49` | `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX` | Already overrideable via config.toml routing |
| `src/intent_classifier.rs:405-408` | Pattern weight arrays | The `in-memory-config-filesystem` plan explicitly deferred this |

Per the lessons learned (`context/foundation/lessons.md`), the DoS safety limits should stay hardcoded ("operational safety limits where changing them without understanding downstream impact is dangerous").

### 8. render.yaml Implications

Current `render.yaml` lists 8 env vars:
```yaml
- RUST_LOG          # → move to config.toml [server].log_level
- ROUTING_CONFIG_PATH # → remove (config.toml embedded)
- PROXY_API_BEARER_TOKEN  # KEEP (secret)
- DASHBOARD_BASIC_USER    # KEEP (secret)
- DASHBOARD_BASIC_PASSWORD # KEEP (secret)
- DATABASE_URL           # KEEP (secret)
- NVIDIA_NIM_API_KEY     # KEEP (secret)
- OPENROUTER_API_KEY     # KEEP (secret)
```

After migration, only 5 env vars remain: the 4 secrets + `DATABASE_URL`.

## Code References

### Env Var Reads in Production Code

- `src/auth.rs:17-27` — `from_env()` reads `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`
- `src/main.rs:51` — `EnvFilter::try_from_default_env()` reads `RUST_LOG`
- `src/main.rs:53` — `std::env::var("LOG_FORMAT")` for log format selection
- `src/main.rs:70` — `std::env::var("CONFIG_PATH")` for config overlay
- `src/main.rs:212` — `std::env::var("DATABASE_URL")` for Postgres override
- `src/main.rs:279` — `std::env::var("PORT")` override
- `src/main.rs:703` — dynamic `std::env::var(api_key_env)` for upstream API keys
- `src/main.rs:803` — `std::env::var("ALLOWED_ORIGINS")` for CORS
- `src/config.rs:13-15` — `env_or_default()` used for `DEFAULT_MODEL`
- `src/config.rs:385-412` — `hardcoded_routing()` reads `NVIDIA_ENDPOINT` and `DEFAULT_MODEL`
- `src/intent_classifier.rs:209,219` — LLM classifier reads its `api_key_env` at startup and every 60s
- `src/persistence.rs:121` — Postgres backend reads `DATABASE_URL`

### Config Loaders (all in `src/config.rs`)

- `src/config.rs:19` — `load_dashboard_config_from_value()` → `[dashboard]`
- `src/config.rs:58` — `load_server_config_from_value()` → `[server]` (port only, missing log_level/log_format)
- `src/config.rs:77` — `load_http_config_from_value()` → `[http]`
- `src/config.rs:116` — `load_database_config_from_value()` → `[database]`
- `src/config.rs:155` — `load_auth_providers_from_value()` → `[[auth_provider]]`
- `src/config.rs:313` — `load_persistence_config_from_value()` → `[persistence]`
- `src/config.rs:507` — `routing_from_value()` → per-category routing blocks
- `src/config.rs:566` — `load_categories_from_value()` → `[[categories]]`
- `src/config.rs:610` — `build_model_costs()` → `[model_costs]`
- `src/config.rs:649` — `load_classifiers_config_from_value()` → `[classifiers]`
- `src/config.rs:714` — `load_regex_classifier_config_from_value()` → `[regex_classifier]`
- `src/config.rs:767` — `load_llm_classifier_config_from_value()` → `[llm_classifier]`

### Config Structs (all in `src/config.rs`)

- `src/config.rs:211-231` — `DashboardConfig` (6 fields)
- `src/config.rs:235-245` — `ServerConfig` (1 field: `port`; missing `log_level`, `log_format`)
- `src/config.rs:249-269` — `HttpConfig` (6 fields)
- `src/config.rs:273-293` — `DatabaseConfig` (6 fields)
- `src/config.rs:297-309` — `PersistenceSettings` (2 fields)
- `src/config.rs:338-342` — `AuthProviderConfig` (3 fields)
- `src/config.rs:633-645` — `ClassifiersConfig` (2 fields)
- `src/config.rs:683-691` — `RegexClassifierConfig` (1 field)
- `src/config.rs:735-742` — `LlmClassifierConfig` (6 fields)

### Routing Types (`src/routing.rs`)

- `src/routing.rs:6-12` — `RouteEntry` (5 fields)
- `src/routing.rs:17-44` — `ModelCosts`
- `src/routing.rs:48` — `DEFAULT_MODEL = "meta/llama-3.1-8b-instruct"`
- `src/routing.rs:49` — `DEFAULT_MODEL_COMPLEX = "meta/llama-3.3-70b-instruct"`

## Architecture Insights

1. **The config.toml merge pattern is already proven**: `include_str!()` + optional `CONFIG_PATH` overlay + `merge_toml_values()` is working in production for 12 sections. Extending it is low-risk.

2. **`PORT` env var is a deliberate override pattern**: `PORT` is already in `config.toml` at `[server].port = 10000`. The env var at `src/main.rs:279` overrides it. This is the Render convention (Render injects `PORT`). This pattern should be preserved for `PORT` specifically.

3. **`CONFIG_PATH` must stay as an env var**: It's a meta-config value — it tells the binary where to find the config file. It cannot be in the config file itself (chicken-and-egg problem).

4. **`DATABASE_URL` must stay as an env var**: It's a connection string containing credentials. It's the standard deployment convention. The code already handles its absence gracefully.

5. **API key env var names are config, values are secrets**: The `api_key_env: "NVIDIA_API_KEY"` field in `config.toml` routing blocks is already correct — the *name* is in config, the *value* is in env. This pattern is sound and should not change.

6. **Test code duplicates config reading**: `make_test_app_state()` and `build_app_with_persistence()` use `parse_env_int()` for values that `HttpConfig` already provides. These should use constants directly in tests.

## Historical Context

- `context/changes/in-memory-config-filesystem/plan.md` — The tri-tier config model was designed here. Explicitly planned to move `PORT`, `RUST_LOG`, `LOG_FORMAT`, `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`, `ALLOWED_ORIGINS`, `MAX_UPSTREAM_BODY_BYTES`, `KEEPALIVE_INTERVAL_SECS` to `config.toml`. Some were completed; several were left behind.

- `context/changes/in-memory-db-fallback/plan.md` — Established the `DATABASE_URL`-forces-postgres pattern and the `[persistence]` config section.

- `context/changes/post-review-cleanup/plan.md` — Older plan (May 2026) that was env-vars-only. Superseded by `in-memory-config-filesystem` (June 2026).

## Related Research

- `context/changes/in-memory-config-filesystem/plan.md` — Prior implementation plan for the config.toml migration
- `context/changes/in-memory-db-fallback/plan.md` — Persistence backend configuration decisions

## Follow-up Research: Hardcoded Models & Categories

**Date**: 2026-06-10T17:26:13Z

### Research Scope Expansion

The user clarified: hardcoded model names, hardcoded categories, and category patterns should all move to `config.toml` — users may want completely different category schemes and different model sets.

### 9. Hardcoded Model Names

Two `pub const` values in `src/routing.rs:48-49` serve as the compiled-in defaults:

```rust
pub const DEFAULT_MODEL: &str = "meta/llama-3.1-8b-instruct";
pub const DEFAULT_MODEL_COMPLEX: &str = "meta/llama-3.3-70b-instruct";
```

**Where they're used in production:**

| Location | Usage | Should Change To |
|----------|-------|-----------------|
| `src/routing.rs:48` | `DEFAULT_MODEL` constant | Read from `config.toml` `[routing_defaults]` section |
| `src/routing.rs:49` | `DEFAULT_MODEL_COMPLEX` constant | Read from `config.toml` |
| `src/intent_classifier.rs:620` | `ClassificationResult::fallback()` uses `DEFAULT_MODEL` | Use `baseline_model` from config (already loaded at `main.rs:136`) |
| `src/config.rs:395,405` | `hardcoded_routing()` fallback model | Use config.toml value |
| `src/config.rs:431-432,521-522,556` | TOML parser fallback when `model` key is missing | Read from config.toml `[routing_defaults]` |
| `src/main.rs:139` | `baseline_model` falls back to `DEFAULT_MODEL_COMPLEX` if config.toml has no `baseline_model` key | Remove — `baseline_model` is already required in config.toml (line 112) |

**Also hardcoded: `hardcoded_model_costs()` in `src/intent_classifier.rs:15-23`:**

```rust
pub(crate) fn hardcoded_model_costs() -> HashMap<String, f64> {
    m.insert("claude-3.5-sonnet".to_string(), 3.00);
    m.insert("gpt-4o".to_string(), 2.50);
    m.insert("gpt-4o-mini".to_string(), 0.15);
    m.insert("deepseek-chat".to_string(), 0.14);
}
```

This seeds the cost map before TOML `[model_costs]` overrides are applied (`src/config.rs:611`). These 4 entries are vendor-specific and irrelevant for users who don't use those models.

**Action**:
- Add a `[routing_defaults]` section to `config.toml` with `default_model` and `fallback_endpoint`
- Remove `DEFAULT_MODEL` and `DEFAULT_MODEL_COMPLEX` constants from `src/routing.rs`
- Make the `hardcoded_model_costs()` empty — all costs come from `config.toml [model_costs]`
- Add `default_model` field to config along with `baseline_model`

### 10. Hardcoded Categories — Deep Dive

The `hardcoded_categories()` function at `src/intent_classifier.rs:48-75` is a **compile-time fallback** used when `config.toml` has no `[[categories]]` section. While categories are already configurable via `[[categories]]` in config.toml (loaded by `load_categories_from_value()` at `src/config.rs:566`), the hardcoded fallback exists in 3 places:

| Location | Context |
|----------|---------|
| `src/main.rs:124` | `.unwrap_or_else(|_| intent_classifier::hardcoded_categories())` |
| `src/config.rs:482` | `hardcoded_routing(&hardcoded_categories())` |
| `src/config.rs:492` | `hardcoded_routing(&hardcoded_categories())` |

#### 10a. `build_all_patterns()` hardcodes category name → pattern mapping

The function at `src/intent_classifier.rs:539-602` matches on category *names* explicitly:

```rust
match config.name.as_str() {
    "FILE_READING" => { /* populate FR_WEIGHTS */ }
    "COMPLEX_REASONING" => { /* populate CR_WEIGHTS */ }
    "SYNTAX_FIX" => { /* populate SF_WEIGHTS */ }
    "CASUAL" => { /* populate CA_WEIGHTS */ }
    unknown => { tracing::warn!(...); }
}
```

**The problem**: If a user defines categories like `CHAT`, `CODE_REVIEW`, `DOCS` in `config.toml`, the regex classifier won't have any patterns for them because `build_all_patterns()` only knows the 4 hardcoded names. This is the most critical blocker for custom categories.

#### 10b. `classify_internal()` has hardcoded category name references

The classification logic at `src/intent_classifier.rs:676-746` contains **two hardcoded category name strings** that would break with renamed categories:

```rust
// Line 718-721: SF dual-threshold special case — hardcoded "SYNTAX_FIX" and "FILE_READING"
let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);

// Line 724: Updates the met flag for SYNTAX_FIX by name
if let Some(entry) = met.iter_mut().find(|(c, _)| c.name == "SYNTAX_FIX") {
    entry.1 = sf_met;
}
```

This is a special-case rule: SYNTAX_FIX has a dual threshold (score ≥ 4, OR score ≥ 3 with zero FILE_READING matches). This logic is baked into the classifier and references category names by string. With config-driven categories, this special case needs to become a generic per-category threshold override.

#### 10c. Hardcoded `"CASUAL"` fallback throughout

```rust
// fallback_category() at line 609:
.unwrap_or("CASUAL")  // ultimate fallback category name

// ClassificationResult::fallback() at line 619:
category: "CASUAL".to_string()

// route_match() at line 749:
if category != "CASUAL" && !self.routing.contains_key(category) {
```

The string `"CASUAL"` is hardcoded in 3 places in the classification pipeline. If categories are renamed, the fallback category must be derived from the highest-priority category in config, not a hardcoded string.

#### 10d. `build_llm_classifier_prompt()` in lines 366-383

This function generates the LLM classifier's system prompt. It uses `cat.name` and `cat.description` from the config (already dynamic), but the 4 few-shot examples at lines 377-380 reference the 4 hardcoded category names:

```
- "read the file src/main.rs" -> FILE_READING
- "fix this compile error" -> SYNTAX_FIX
- "design a distributed system" -> COMPLEX_REASONING
- "hello how are you" -> CASUAL
```

These few-shot examples should use the actual category names from the config.

**Hardcoded pattern arrays that should be configurable:**

| Array | Lines | Count | Currently |
|-------|-------|-------|-----------|
| `FILE_READING` regex patterns | `src/intent_classifier.rs:416-429` | 12 | Hardcoded `const` |
| `COMPLEX_REASONING` regex patterns | `src/intent_classifier.rs:431-448` | 16 | Hardcoded `const` |
| `SYNTAX_FIX` regex patterns | `src/intent_classifier.rs:450-462` | 11 | Hardcoded `const` |
| `CASUAL` regex patterns | `src/intent_classifier.rs:464-470` | 5 | Hardcoded `const` |
| `NEGATIVE` regex patterns | `src/intent_classifier.rs:472-477` | 4 | Hardcoded `const` |
| `NEGATIVE_META` suppression rules | `src/intent_classifier.rs:481-498` | 4 | Hardcoded `const` |
| `FR_WEIGHTS` | `src/intent_classifier.rs:405` | `[3,3,3,3,2,2,2,2,2,1,1,1]` | Hardcoded |
| `CR_WEIGHTS` | `src/intent_classifier.rs:406` | `[3,3,3,3,2,2,2,2,2,2,1,1,1,1,1,1]` | Hardcoded |
| `SF_WEIGHTS` | `src/intent_classifier.rs:407` | `[3,3,3,2,2,2,2,2,1,1,1]` | Hardcoded |
| `CA_WEIGHTS` | `src/intent_classifier.rs:408` | `[3,2,1,1,1]` | Hardcoded |

**Action — New config-driven pattern model**:

The `CategoryConfig` struct gets a `patterns` field:

```rust
pub(crate) struct CategoryConfig {
    pub name: String,
    pub description: String,
    pub threshold: u32,
    pub priority: u8,
    pub patterns: Vec<CategoryPattern>,     // NEW
    pub dual_threshold: Option<DualThresholdConfig>, // NEW: replaces SF hardcode
}

pub(crate) struct CategoryPattern {
    pub regex: String,
    pub weight: u8,
}

pub(crate) struct DualThresholdConfig {
    pub score_a: u32,           // e.g., 4
    pub score_b: u32,           // e.g., 3
    pub suppress_category: String, // e.g., "FILE_READING" (category name whose score must be 0)
}
```

**config.toml schema**:

```toml
[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs, errors, compilation issues"
threshold = 3
priority = 2
patterns = [
    { regex = "(?i)\\b(?:fix|correct|repair)...", weight = 3 },
    { regex = "(?i)\\b(?:doesn't\\s+compile)...", weight = 3 },
    # ...11 total patterns
]

[SYNTAX_FIX.dual_threshold]
score_a = 4
score_b = 3
suppress_category = "FILE_READING"

[[negative]]
suppressed = "SYNTAX_FIX"
penalty = 2
patterns = [
    "(?i)\\b(?:design|architect)...",
]
```

**Code changes in `classify_internal()`** (lines 718-726):

The hardcoded SF dual-threshold block:
```rust
// CURRENT — hardcoded
let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);
```

Becomes a per-category loop over `DualThresholdConfig`:
```rust
// NEW — config-driven
for (config, _) in &mut met {
    if let Some(dt) = &config.dual_threshold {
        let score = *scores.get(config.name.as_str()).unwrap_or(&0);
        let suppress_score = *scores.get(dt.suppress_category.as_str()).unwrap_or(&0);
        *met_flag = score >= dt.score_a || (score >= dt.score_b && suppress_score == 0);
    }
}
```

**Code changes in `fallback_category()`** (line 604-610):

The hardcoded `"CASUAL"` becomes the highest-priority category:
```rust
fn fallback_category(categories: &[CategoryConfig]) -> &str {
    categories.iter()
        .max_by_key(|c| c.priority)
        .map(|c| c.name.as_str())
        .unwrap_or("CASUAL")  // "CASUAL" becomes the last-resort if categories is empty
}
```
(This function is mostly correct already — `.unwrap_or("CASUAL")` only fires if the vec is empty.)

**Code changes in `ClassificationResult::fallback()`** (line 617-627):

The hardcoded `"CASUAL"` and `DEFAULT_MODEL` env var become config-driven. The caller should pass the fallback category name and model:
```rust
pub fn fallback(fallback_category_name: &str, default_model: &str) -> Self {
    ClassificationResult {
        category: fallback_category_name.to_string(),
        model: default_model.to_string(),
        // ...
    }
}
```

**Code changes in `route_match()`** (line 748-761):

The `if category != "CASUAL"` check at line 749 skips the routing table warning for `"CASUAL"`. This should either be removed (warn for all missing categories) or made generic:
```rust
fn route_match(&self, category: &str) -> ClassificationResult {
    if !self.routing.contains_key(category) {
        tracing::warn!(%category, "route_match: category not in routing table — falling back");
    }
    // ...
}
```

**Code changes in `build_llm_classifier_prompt()`** (lines 377-380):

The few-shot examples should use the actual category names from the config rather than hardcoded strings. The existing loop at lines 371-373 already iterates `categories` — the few-shot examples just need to reference `cat.name` dynamically instead of hardcoding the names.

**Files with hardcoded category name strings that must become config-driven:**

These contain the literal strings `FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL`. The classification pipeline itself (build_all_patterns, classify_internal, fallback_category) has hardcoded references — these are the *implementation* blockers:

| File:Line | What's Hardcoded | Impact |
|-----------|-----------------|--------|
| `src/intent_classifier.rs:547-586` | `build_all_patterns()` — `match config.name { "FILE_READING" => ..., "COMPLEX_REASONING" => ... }` | **BLOCKER**: custom categories get zero patterns |
| `src/intent_classifier.rs:718-726` | `classify_internal()` — `scores.get("SYNTAX_FIX")`, `scores.get("FILE_READING")`, `c.name == "SYNTAX_FIX"` | **BLOCKER**: SF dual-threshold rule references category names |
| `src/intent_classifier.rs:609` | `fallback_category()` — `.unwrap_or("CASUAL")` | **BLOCKER**: ultimate fallback is hardcoded |
| `src/intent_classifier.rs:619` | `ClassificationResult::fallback()` — `"CASUAL"` | **BLOCKER**: global fallback is hardcoded |
| `src/intent_classifier.rs:749` | `route_match()` — `if category != "CASUAL"` | **BLOCKER**: skips routing check only for "CASUAL" |
| `src/intent_classifier.rs:377-380` | `build_llm_classifier_prompt()` — few-shot examples with hardcoded names | Non-blocking: examples use real category names, still work if renamed |

And the consumer files (documentation, tests, examples):

| File | Count | Notes |
|------|-------|-------|
| `openapi/completions.yaml` | 2 (lines 44, 111) | Enum constraints — must update to match config |
| `routing_examples/*.toml` | 4 files | Section headers — must rename |
| `manual-test/run.sh` | ~30 references | Test scripts — must update |
| `manual-test/test.sh` | ~50 references | Test scripts — must update |
| `manual-test/TEST.md` | ~10 references | Documentation — must update |
| `templates/dashboard/inferences.html` | 1 (line 19) | Placeholder text — should become dynamic |
| `src/intent_classifier.rs` (tests) | ~20 references | Test assertions — must update |
| `src/config.rs` (tests) | ~20 references | Test TOML fixtures — must update |
| `src/main.rs` (tests) | ~10 references | Test assertions — must update |

### 11. `hardcoded_routing()` Bakes in NVIDIA Defaults

The function at `src/config.rs:385-412` provides routing defaults when `config.toml` is missing/empty. It hardcodes:

```rust
// src/config.rs:388
let endpoint = env_or_default("NVIDIA_ENDPOINT",
    "https://integrate.api.nvidia.com/v1/chat/completions");

// src/config.rs:398,408 — every category entry gets:
provider_type: "nvidia_nim".to_string(),
api_key_env: Some("NVIDIA_API_KEY".to_string()),

// src/config.rs:395 — per-category model:
model: DEFAULT_MODEL.to_string(),  // "meta/llama-3.1-8b-instruct"
```

These vendor-specific defaults should come from config.toml, not from compiled-in constants. If `config.toml` is missing, the server should either refuse to start or use empty/memory-only defaults that don't assume any particular provider.

**Action**: Remove `NVIDIA_ENDPOINT`, `DEFAULT_MODEL` env var reads from `hardcoded_routing()`. Require `config.toml` with `[routing_defaults]` or at minimum the per-category routing blocks. The `DEFAULT_MODEL` env var gets removed entirely.

### 12. Config.toml Fields Present But Not Wired Up

Three config.toml sections exist but are never loaded by any `load_*` function:

| TOML Key | config.toml Line | Status |
|----------|-----------------|--------|
| `[server] log_level = "info"` | 10 | **Not wired** — `ServerConfig` has no `log_level` field; code reads `RUST_LOG` env var at `main.rs:51` |
| `[server] log_format = "compact"` | 11 | **Not wired** — `ServerConfig` has no `log_format` field; code reads `LOG_FORMAT` env var at `main.rs:53` |
| `[cors] allowed_origins = []` | 22 | **Not wired** — No `load_cors_config_from_value()` exists; code reads `ALLOWED_ORIGINS` env var at `main.rs:803` |

**Action**: Add `log_level` and `log_format` fields to `ServerConfig`, create `CorsConfig` with loader.

### 13. Additional Hardcoded Values to Move to Config

| Value | Location | Config Section | Notes |
|-------|----------|---------------|-------|
| `SHORT_PROMPT_LEN = 30` | `src/intent_classifier.rs:412` | `[regex_classifier]` | Threshold for shortcutting classification. Should be a config field alongside `enabled`. |
| `content.chars().take(10_000)` | `src/persistence.rs:1059` | `[persistence]` or `[http]` | Max chars extracted from last user message for intent analysis. Not a DoS limit — it's a classification quality/performance tradeoff. |
| `full.chars().take(200)` | `src/persistence.rs:1078` | `[persistence]` | Snippet length stored in DB. Currently hardcoded at 200 chars. |
| `messages.len() > 1000` | `src/persistence.rs:1047` | `[http]` or `[persistence]` | DoS guard on message array size. Currently a magic number. |
| `MAX_ERROR_BODY_BYTES = 2048` | `src/main.rs:455,581` | `[http]` | Error body buffer cap. Duplicated in two functions. |
| Error text truncation 512 | `src/main.rs:463,597` | `[http]` | Truncation for error text in JSON responses. Duplicated. |
| `model: "gpt-4o-mini"` (LLM default) | `src/config.rs:784` | `[llm_classifier]` | Default LLM model when `model` is absent. Already configurable but hardcoded default may not suit all users. |

### 14. Complete Migration Inventory

**Env vars to REMOVE and move to config.toml:**

| Env Var | Config Location | Priority |
|---------|----------------|----------|
| `RUST_LOG` | `[server].log_level` | HIGH — config.toml already has the key, just not read |
| `LOG_FORMAT` | `[server].log_format` | HIGH — config.toml already has the key, just not read |
| `ALLOWED_ORIGINS` | `[cors].allowed_origins` | HIGH — new section needed |
| `DEFAULT_MODEL` | `[routing_defaults].default_model` | HIGH — remove env var entirely |
| `NVIDIA_ENDPOINT` | `[routing_defaults].fallback_endpoint` | HIGH — remove env var entirely |
| `ROUTING_CONFIG_PATH` | (remove) | HIGH — vestigial, already migrated |
| `MAX_UPSTREAM_BODY_BYTES` (test) | (remove, use constant) | LOW — test-only via `parse_env_int` |
| `KEEPALIVE_INTERVAL_SECS` (test) | (remove, use constant) | LOW — test-only via `parse_env_int` |

**Hardcoded Rust values to move to config.toml:**

| Value | Config Location | Priority |
|-------|----------------|----------|
| `DEFAULT_MODEL` const | `[routing_defaults].default_model` | HIGH |
| `DEFAULT_MODEL_COMPLEX` const | `baseline_model` (already exists) | HIGH |
| `hardcoded_model_costs()` | `[model_costs]` (already exists, seed with empty) | MEDIUM |
| `hardcoded_categories()` | `[[categories]]` (already exists, make required) | HIGH |
| Regex pattern arrays + weights | `[[categories]].patterns` | HIGH |
| `NEGATIVE` patterns + `NEGATIVE_META` | `[[negative]]` | HIGH |
| `SHORT_PROMPT_LEN = 30` | `[regex_classifier].short_prompt_len` | MEDIUM |
| `10_000` char message limit | `[http].max_message_chars` | LOW |
| `200` char snippet limit | `[persistence].snippet_chars` | LOW |
| `1000` max messages | `[http].max_messages` | LOW |
| `2048` error body bytes | `[http].max_error_body_bytes` | LOW |
| `512` error text truncation | `[http].max_error_text_chars` | LOW |

### 15. Files Requiring Changes (Summary)

| File | Changes Needed |
|------|---------------|
| `config.toml` | Add `[routing_defaults]`, `[cors]` (wire up), add fields to `[server]`, `[regex_classifier]`, `[http]`, `[persistence]`; remove hardcoded seeding |
| `src/routing.rs` | Remove `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX` constants; add `RoutingDefaults` struct |
| `src/config.rs` | Add `RoutingDefaults`, `CorsConfig` structs + loaders; extend `ServerConfig`; add `RegexClassifierConfig.short_prompt_len`; make `hardcoded_routing()` use config defaults or fail; remove `env_or_default` for `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`; empty `hardcoded_model_costs()` |
| `src/main.rs` | Remove env var reads for `LOG_FORMAT`, `ALLOWED_ORIGINS`, `PORT`; use config structs; update `baseline_model` fallback logic; update test helpers |
| `src/intent_classifier.rs` | Remove `hardcoded_categories()`; refactor `build_all_patterns()` to read from config `CategoryConfig.patterns`; replace const pattern arrays with config-driven assembly; update tests |
| `src/persistence.rs` | No changes (backend selection via config.toml already works) |
| `src/auth.rs` | No changes (secrets stay in env) |
| `src/dashboard.rs` | No functional changes needed |
| `render.yaml` | Remove `RUST_LOG`, `ROUTING_CONFIG_PATH`; keep secrets |
| `openapi/completions.yaml` | Remove hardcoded category enum (or document as config-driven) |
| `routing_examples/` | Update category names to match whatever the user configures |
| `manual-test/` | Update for new config schema |

## Open Questions

1. Should `PORT` env var override be kept for Render compatibility, or should Render be configured to read `config.toml`?
2. Should `RUST_LOG` still be read as a fallback (after config.toml `log_level`) for debugging convenience?
3. Should `config.toml` be made **required** (panic if missing/unparseable) rather than falling back to hardcoded defaults?
4. Should regex patterns be in `config.toml` as TOML arrays, or in separate `.toml` files referenced by path (like `prompt_template_path`)?
5. What's the migration path for existing users with `routing.toml` files?


## Follow-up Research 2: Remaining Hardcoded Values

**Date**: 2026-06-10T17:26:13Z

### 16. LLM Classifier Inference Parameters

Two values in the LLM classifier's OpenAI request body are hardcoded:

| Location | Value | Config Section |
|----------|-------|---------------|
| `src/intent_classifier.rs:261` | `"max_tokens": 20` | `[llm_classifier].max_tokens` |
| `src/intent_classifier.rs:262` | `"temperature": 0.0` | `[llm_classifier].temperature` |

```rust
let body = serde_json::json!({
    "model": self.model,
    "messages": [...],
    "max_tokens": 20,        // hardcoded
    "temperature": 0.0,      // hardcoded
});
```

These control the LLM classifier's own inference quality. Different users might want different values depending on their LLM provider. Should be fields on `LlmClassifierConfig`.

### 17. LLM API Key Refresh Interval

| Location | Value | Config Section |
|----------|-------|---------------|
| `src/intent_classifier.rs:218` | `Duration::from_secs(60)` | `[llm_classifier].key_refresh_interval_secs` |

```rust
tokio::spawn(async move {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;  // hardcoded
        if let Ok(new_key) = std::env::var(&key_env) { ... }
    }
})
```

The 60-second polling interval for API key rotation should be configurable. Some environments may rotate keys faster or slower.

### 18. Bind Address

| Location | Value | Config Section |
|----------|-------|---------------|
| `src/main.rs:285` | `"0.0.0.0"` | `[server].bind_host` |

```rust
let bind_addr = format!("0.0.0.0:{port}");
```

The bind address is hardcoded to all interfaces. For production deployments behind a reverse proxy, binding to `127.0.0.1` is often preferred. Should be in `[server]` as `bind_host` with default `"0.0.0.0"`.

### 19. Prompt Extraction Limits

Three hardcoded limits in `src/persistence.rs` for DoS protection and storage efficiency:

| Location | Value | Config Section |
|----------|-------|---------------|
| `src/persistence.rs:1047` | `messages.len() > 1000` | `[http].max_messages_array` |
| `src/persistence.rs:1059` | `content.chars().take(10_000)` | `[persistence].max_prompt_chars` |
| `src/persistence.rs:1078` | `full.chars().take(200)` | `[persistence].snippet_length` |

The 1000-message array limit prevents DoS via oversized request bodies. The 10,000-char prompt extraction cap controls how much user text is saved for intent analysis. The 200-char snippet limit controls what's stored in the DB's `prompt_snippet` column.

### 20. Complete Hardcoded Value Inventory (Final)

Adding the 6 new items from this sweep to the overall inventory:

| # | Value | Location | Config Section | Priority |
|---|-------|----------|---------------|----------|
| 1 | `"0.0.0.0"` bind address | `src/main.rs:285` | `[server].bind_host` | MEDIUM |
| 2 | `max_tokens: 20` for LLM | `src/intent_classifier.rs:261` | `[llm_classifier].max_tokens` | MEDIUM |
| 3 | `temperature: 0.0` for LLM | `src/intent_classifier.rs:262` | `[llm_classifier].temperature` | MEDIUM |
| 4 | 60s key refresh interval | `src/intent_classifier.rs:218` | `[llm_classifier].key_refresh_interval_secs` | LOW |
| 5 | `take(10_000)` prompt cap | `src/persistence.rs:1059` | `[persistence].max_prompt_chars` | LOW |
| 6 | `take(200)` snippet length | `src/persistence.rs:1078` | `[persistence].snippet_length` | LOW |
| 7 | `> 1000` messages limit | `src/persistence.rs:1047` | `[http].max_messages_array` | LOW |

### 21. Confirmed NOT Configurable (Protocol/Structure Constants)

These were examined and confirmed as correct to leave hardcoded:

| Category | Examples | Reason |
|----------|----------|--------|
| HTTP auth scheme prefixes | `"Bearer "`, `"Basic "` | RFC 6750 / RFC 7617 |
| IANA media types | `"application/json"`, `"text/event-stream"` | IANA registry |
| SSE spec strings | `": keepalive\n\n"`, `"event: error\ndata: {}\n\n"` | SSE protocol |
| HTTP header names | `"x-cerebrum-category"`, `"x-cerebrum-model"` | API contract |
| OpenAI API field names | `"messages"`, `"role"`, `"content"` | OpenAI schema |
| Route paths | `/health`, `/v1`, `/dashboard`, etc. | API contract + Render convention |
| Template paths | `"dashboard/index.html"`, etc. | Compile-time Askama paths |
| Cache-Control | `"no-cache"` | SSE spec requirement |
| `"openai_compatible"` fallback | `intent_classifier.rs:506` | Sensible default for empty provider_type |
| Error body cap (2048) | `main.rs:455,581` | Internal safety guard |
| Error text truncation (512) | `main.rs:463,597` | Error message formatting |
