# Config Package

`src/config/` is the single source of truth for all runtime configuration in Frugalis. It owns deserialization, validation, merging, and typed projection of every config section that the rest of the application consumes.

## File Layout

| File | Responsibility |
|---|---|
| `mod.rs` | `ConfigRoot` struct, file loading, format detection, schema validation, config merging |
| `types.rs` | Typed structs and serde defaults for every non-routing config section |
| `routing.rs` | `RouteEntry`, `ProviderEntry`, `ModelCosts`, and default model constants |
| `loader.rs` | Projection functions (`load_*_from_value`) that extract a typed sub-config from a parsed `ConfigRoot` |

---

## Config File Format

Frugalis uses a single config file (default: `config.toml`, override via `CONFIG_PATH` env var). YAML (`*.yaml`, `*.yml`) is also accepted; format is auto-detected from the file extension.

All sections are optional at the TOML level — missing sections produce safe, in-code defaults rather than startup failures. Validation (`run_validation`) runs before the server binds and will reject semantic errors (bad regex, unknown log level, negative costs, etc.) even if deserialization succeeds.

---

## Module: `mod.rs`

### `ConfigRoot`

```
ConfigRoot
├── server             → ServerConfig
├── http               → HttpConfig
├── cors               → CorsConfig
├── database           → DatabaseConfig
├── persistence        → PersistenceSettings
├── classifiers        → ClassifiersConfig
├── regex_classifier   → RegexClassifierConfig
├── llm_classifier     → LlmClassifierConfig
├── fewshot_classifier → FewShotConfig
├── categories         → HashMap<String, CategoryConfig>
├── patterns_dir       → PathBuf
├── negative_patterns  → Vec<NegativePatternConfig>
├── routing            → HashMap<String, RouteEntry>
├── auth_providers     → Vec<AuthProviderConfig>
├── model_costs        → HashMap<String, f64>
├── baseline_model     → String
├── classify_db_log    → bool
├── dashboard          → DashboardConfig
└── cache              → CacheConfig
```

Every field is `Option<T>` so the entire config file (or any individual section) can be absent. `#[serde(rename_all = "snake_case")]` is applied globally.

### `load_config_from_path(path: &str) -> Result<ConfigRoot, String>`

Reads a file from disk and deserializes it into `ConfigRoot`. Format is chosen automatically: `.yaml` / `.yml` → serde_yaml, anything else → TOML.

### `detect_format(path: &str) -> ConfigFormat`

Returns `ConfigFormat::Yaml` for `.yaml`/`.yml` extensions and `ConfigFormat::Toml` for everything else. Used only internally by `load_config_from_path`.

### `run_validation(config_path: Option<&str>) -> Result<(), Vec<String>>`

Full schema + semantic validation pass. Accepts an optional path; if `None` it validates the embedded `config.toml` compiled into the binary.

Checks performed:
- `server.port` must not be 0
- `server.log_level` must be one of `trace | debug | info | warn | error`
- `server.log_format` must be one of `compact | full | json | pretty`
- `http.client_timeout_secs` must be > 0
- Every `[categories]` entry must have `threshold > 0` and `priority > 0`
- `[categories]` section must exist
- Every routing key (excluding `DEFAULT`) must match a known category name
- Auth providers must have a non-empty `type`
- All model costs must be > 0.0
- `patterns_dir`, if it exists on disk, must be a directory (not a file)
- All regex patterns (inline or loaded from `patterns_file`) must compile without error

Returns `Ok(())` on success or `Err(Vec<String>)` with the full list of collected errors.

### `merge_configs(base: &mut ConfigRoot, overlay: ConfigRoot)`

Two-tier merge strategy:

| Tier | Sections | Behaviour |
|---|---|---|
| **Field-level merge** | `server`, `http`, `database`, `persistence`, `dashboard` | Overlay fields win; absent overlay fields leave base fields unchanged |
| **Full replacement** | `cors`, `cache`, `classifiers`, `regex_classifier`, `llm_classifier`, `fewshot_classifier`, `categories`, `auth_providers`, `model_costs`, `negative_patterns`, `patterns_dir`, scalars | Entire section replaced when overlay provides it |
| **Key-level merge** | `routing` | Overlay entries are upserted into the base routing table, not wholesale replaced |

---

## Module: `types.rs`

All config structs implement `Clone`, `Debug`, `Deserialize`, and `Default`. Serde `default` attributes wire each field to a private helper that supplies the compile-time constant. Struct `Default` implementations mirror those helpers so both `#[serde(default)]` and `Type::default()` produce identical values.

### `ServerConfig`

| Field | Default | Notes |
|---|---|---|
| `port` | `10000` | TCP port the server binds to |
| `log_level` | `"info"` | Passed to the tracing subscriber |
| `log_format` | `"compact"` | `compact`, `full`, `json`, or `pretty` |

### `HttpConfig`

Controls the Axum body limits and the reqwest client that proxies upstream.

| Field | Default | Notes |
|---|---|---|
| `max_upstream_body_bytes` | `10 MiB` | Maximum bytes read from an upstream response |
| `request_body_limit_bytes` | `10 MiB` | Maximum bytes accepted from an incoming request |
| `keepalive_interval_secs` | `15` | TCP keepalive probe interval |
| `client_timeout_secs` | `120` | Total request timeout for the proxy HTTP client |
| `client_connect_timeout_secs` | `30` | Connection establishment timeout |
| `streaming_channel_capacity` | `32` | Buffer depth for the SSE streaming channel |

### `CorsConfig`

| Field | Default | Notes |
|---|---|---|
| `allowed_origins` | `[]` | List of allowed CORS origins; empty disables CORS headers |

### `DatabaseConfig`

Controls the SQLite connection pool used by the persistence layer.

| Field | Default | Notes |
|---|---|---|
| `connection_retries` | `3` | Retry attempts on initial pool acquisition |
| `retry_base_ms` | `1000` | Base delay (ms) for exponential back-off |
| `max_connections` | `10` | Maximum pool size |
| `acquire_timeout_secs` | `30` | Maximum wait for a free connection |
| `idle_timeout_secs` | `1800` | How long an idle connection is kept alive |
| `log_concurrency_limit` | `100` | Max in-flight async log writes |

### `PersistenceSettings`

| Field | Default | Notes |
|---|---|---|
| `backend` | `"memory"` | `"memory"` or `"sqlite"` |
| `sqlite_path` | `"./frugalis.db"` | File path when backend is `"sqlite"` |

### `AuthProviderConfig`

Injected into upstream requests to authenticate against providers that require their own credentials (e.g., adding an `Authorization` header automatically).

| Field | Notes |
|---|---|
| `type_` (TOML: `type`) | Provider kind identifier |
| `header` | Header name to inject |
| `value_template` | Header value template; may reference env vars |

### `ClassifiersConfig`

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Master switch; disables all classifiers when false |
| `order` | `["regex", "fewshot", "llm"]` | Evaluation order; first non-`unknown` result wins |

### `RegexClassifierConfig`

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Enable/disable the regex classifier |
| `short_prompt_len` | `30` | Prompts shorter than this (chars) skip the regex scorer |

### `LlmClassifierConfig`

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Enable/disable the LLM classifier |
| `model` | `"gpt-4o-mini"` | Model name sent in the classification request |
| `endpoint` | `""` | Chat completions endpoint URL |
| `api_key_env` | `"OPENAI_API_KEY"` | Env var holding the API key |
| `provider_type` | `"openai_compatible"` | Protocol adapter name |
| `prompt_template_path` | `None` | Optional path to a Jinja/custom prompt template |
| `timeout_secs` | `3` | Max time to wait for a classification response |

### `FewShotConfig`

| Field | Default | Notes |
|---|---|---|
| `enabled` | `true` | Enable/disable the few-shot classifier |
| `confidence_threshold` | `0.4` | Minimum score to emit a label |
| `cold_start_threshold` | `0.6` | Raised threshold used until enough examples are seen |
| `cold_start_feedback_count` | `5` | Example count that ends cold-start mode |
| `feature_dimensions` | `1000` | Size of the sparse feature hashing space |
| `retraining_threshold` | `5` | New examples needed to trigger a retraining pass |
| `data_path` | `"data/fewshot_training.yaml"` | Training data file |
| `max_vocabulary_warn` | `5000` | Warn when vocabulary exceeds this size |
| `max_training_examples` | `10000` | Hard cap on training set size |

### `DashboardConfig`

Controls page-level query defaults for the dashboard UI.

| Field | Default | Notes |
|---|---|---|
| `default_hours` | `24` | Default lookback window in hours |
| `hours_min` | `1` | Minimum selectable lookback |
| `hours_max` | `720` | Maximum selectable lookback (30 days) |
| `page_limit` | `20` | Default rows per page |
| `page_limit_max` | `100` | Hard cap on rows per page |
| `recent_count` | `5` | Number of recent items shown in summary widgets |

### `CacheConfig`

| Field | Default | Notes |
|---|---|---|
| `ttl_secs` | `300` | Cache entry TTL in seconds |
| `max_entries` | `1000` | Maximum cached entries; `0` disables the cache |

Cache is disabled entirely when the `[cache]` section is absent or `max_entries == 0`.

---

## Module: `routing.rs`

### `ProviderEntry`

A single upstream model endpoint within a routing category.

| Field | Notes |
|---|---|
| `model` | Model name sent in the request body |
| `endpoint` | Full URL of the chat completions endpoint |
| `provider_type` | Protocol adapter (`openai_compatible`, `anthropic`, `ollama`, …) |
| `api_key_env` | Env var whose value is injected as the `Authorization` bearer token |
| `timeout_ms` | Per-provider request timeout override (ms) |

### `RouteEntry`

A routing category with an ordered list of providers. The first provider is primary; additional providers are cascade fallbacks.

Supports two TOML shapes:

**New (multi-provider):**
```toml
[routing.COMPLEX]
providers = [
  { model = "claude-sonnet-4", endpoint = "…", provider_type = "anthropic", api_key_env = "ANTHROPIC_API_KEY" },
  { model = "gpt-4o", endpoint = "…", provider_type = "openai_compatible", api_key_env = "OPENAI_API_KEY" },
]
```

**Legacy (flat):**
```toml
[routing.SYNTAX_FIX]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"
cost_per_1m_input_tokens = 0.15
```

Both are deserialized into the same `RouteEntry { providers: Vec<ProviderEntry>, … }` shape via `RouteEntryRaw` — an intermediate struct that handles the union of both field sets before `From<RouteEntryRaw>` normalises them.

`RouteEntry::primary()` returns the first provider and panics if the list is empty (the deserializer guarantees at least one entry, so this should never fire in practice).

### `ModelCosts`

A lookup table mapping model names to their cost per 1M input tokens. Built from two sources (later source wins for the same model name):
1. `[model_costs]` table in `config.toml`
2. `cost_per_1m_input_tokens` field on individual `RouteEntry` items

Implements `CostProvider` from `persistence::types`.

### Default Model Constants

| Constant | Value | Use |
|---|---|---|
| `DEFAULT_MODEL` | `"meta/llama-3.1-8b-instruct"` | Standard fallback |
| `DEFAULT_MODEL_COMPLEX` | `"meta/llama-3.3-70b-instruct"` | High-capability fallback |
| `DEFAULT_MODEL_LOCAL` | `"llama3.1"` | Hardcoded Ollama fallback (no API key required) |

---

## Module: `loader.rs`

Projection functions that extract a typed sub-section from an already-parsed `ConfigRoot`. Each function follows the same contract:

- Accepts `&ConfigRoot`
- Returns a typed value (or `Option<T>` / `Result<T, String>` when absence or error is meaningful)
- Emits a `debug!` log when a section is absent and defaults are applied

| Function | Returns | Absent behaviour |
|---|---|---|
| `load_server_config_from_value` | `ServerConfig` | `ServerConfig::default()` |
| `load_http_config_from_value` | `HttpConfig` | `HttpConfig::default()` |
| `load_database_config_from_value` | `DatabaseConfig` | `DatabaseConfig::default()` |
| `load_persistence_config_from_value` | `PersistenceSettings` | `PersistenceSettings::default()` (memory backend) |
| `load_cors_config_from_value` | `CorsConfig` | `CorsConfig::default()` (empty origins) |
| `load_dashboard_config_from_value` | `DashboardConfig` | `DashboardConfig::default()` |
| `load_cache_config_from_value` | `Option<CacheConfig>` | `None` (cache disabled) |
| `load_auth_providers_from_value` | `Vec<AuthProviderConfig>` | `vec![]` |
| `load_classifiers_config_from_value` | `ClassifiersConfig` | `ClassifiersConfig::default()` |
| `load_regex_classifier_config_from_value` | `RegexClassifierConfig` | `RegexClassifierConfig::default()` |
| `load_llm_classifier_config_from_value` | `Option<LlmClassifierConfig>` | `None` |
| `load_fewshot_config_from_value` | `Option<FewShotConfig>` | `None` |
| `load_categories_from_value` | `Result<Vec<CategoryConfig>, String>` | `Err("No [categories] section found")` |
| `load_negative_patterns_from_value` | `Vec<NegativePatternConfig>` | `vec![]` |
| `routing_from_value` | `Result<(HashMap<…>, RouteEntry), String>` | Empty map + `DEFAULT_MODEL` fallback |
| `build_model_costs` | `ModelCosts` | Empty cost table |

### `routing_from_value`

Reads the `[routing]` TOML section and produces a `(routing_map, fallback)` pair:
- All keys are uppercased before insertion (e.g., `casual` → `CASUAL`).
- The `DEFAULT` key, if present, is removed from the map and returned as the fallback `RouteEntry`.
- Missing `model`, `provider_type`, or `api_key_env` are warned about but not fatal.

### `hardcoded_routing`

Emergency fallback used when no config file is found. Wires every provided `CategoryConfig` to `llama3.1` on `http://localhost:11434` (Ollama default). Intended for local development without any API keys.

### `load_patterns_from_file`

Reads a plain-text pattern file. Path traversal is guarded: the resolved path must remain inside `patterns_dir`. File format per line:

```
<weight (u8)> | <regex>
```

Lines starting with `#` and blank lines are ignored. Returns `Err` with file:line context on any format or regex error.

### `build_model_costs`

Merges `[model_costs]` table entries with per-route `cost_per_1m_input_tokens` overrides into a single `ModelCosts` lookup. Route-level costs take priority over the top-level table.

---

## Config Lifecycle

```
CONFIG_PATH env var (or "config.toml")
        │
        ▼
load_config_from_path()    ← detects TOML vs YAML
        │
        ▼
   ConfigRoot              ← all fields are Option<T>
        │
        ├──► run_validation()          ← fails fast on semantic errors
        │
        ├──► merge_configs()           ← optional overlay (e.g. init_template.toml)
        │
        └──► load_*_from_value()       ← typed projection per subsystem
```

Each subsystem (auth, persistence, proxy, classification, dashboard, cache) calls exactly one `load_*_from_value` function and owns its config slice for the rest of the process lifetime.
