# In-Memory Config Filesystem — Implementation Plan

## Overview

Eliminate ~65 hardcoded default values by embedding `config.toml` at compile time via `include_str!()` as the single source of truth for all non-secret configuration. Add `Arc<RwLock<T>>` wrappers to mutable `AppState` fields, preparing them for dashboard-driven runtime reload in a follow-up change. The `CONFIG_PATH` env var becomes an optional overlay that merges on top of the embedded default — users only specify what they want to override. Hardcoded fallback functions are retained only as a last-resort panic-safe escape hatch.

## Current State Analysis

The config subsystem (`src/main.rs:76-207`) follows a ladder of fallbacks: try `CONFIG_PATH` file → try hardcoded defaults. This produces ~65 distinct hardcoded values across 7 source files, ~65% of which have no runtime override path. The `LLMClassifier` (`src/intent_classifier.rs:180`) already demonstrates a proven `Arc<RwLock<String>>` pattern for runtime-updatable config (API key rotation every 60s). All config loader functions in `config.rs` operate on `&toml::Value` — they are pure, side-effect-free, and directly reusable from a config-reload path.

The existing `config.toml` (42 lines) defines only 2 of 4 categories and no routing blocks. It serves as documentation, not a functional default — the app runs entirely on hardcoded values when `CONFIG_PATH` is unset.

### Key Discoveries:

- `LLMClassifier.api_key: Arc<RwLock<String>>` at `src/intent_classifier.rs:180` — proven, battle-tested hot-read/cold-write config pattern
- Loader functions (`load_categories_from_value`, `routing_from_value`, `load_classifiers_config_from_value`) are pure `&toml::Value → Config` — reusable without refactoring
- `AppState` has 9 fields, all immutable after construction; read across `main.rs:250,300,604,632,693,704,724` and `dashboard.rs:136-139,202,265,294,306-307`
- `auth_headers_for()` at `src/intent_classifier.rs:507` is a hardcoded match statement — 4 provider types, no extension path
- 48 regex patterns + 4 weight arrays at `src/intent_classifier.rs:407-500` are the largest hardcoded block — deferred to follow-up
- `serde` is not a direct dependency — any `#[derive(Deserialize)]` usage will require adding it to `Cargo.toml`

## Desired End State

The binary ships with an embedded, self-sufficient `config.toml` containing all default values for categories, routing, model costs, server settings, HTTP limits, auth provider mappings, dashboard defaults, and database pool settings. Setting `CONFIG_PATH` loads an alternate config file that merges on top of the embedded one — users only specify the sections they want to override. Secrets (auth tokens, API keys, database URL) remain exclusively in environment variables. `AppState` fields that may change at runtime are wrapped in `Arc<RwLock<T>>` or `Arc<AtomicBool>`, using the proven `LLMClassifier` pattern. All existing tests pass, and the config loading path has dedicated unit tests.

## What We're NOT Doing

- **Dashboard config reload endpoints** — `POST /dashboard/config`, `/reload`, `/switch` are deferred to a follow-up change
- **Pattern groups in config.toml** — 48 regex patterns, 4 weight arrays, and `NEGATIVE_META` remain hardcoded constants
- **File watching** — no `notify` crate; config changes require restart (until dashboard reload follow-up)
- **Renaming category names** — category names remain `FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL` (public API contract, documented at `src/intent_classifier.rs:35-38`)
- **Dashboard config page** — no UI for viewing/editing config in the dashboard

## Implementation Approach

**Strategy**: Bottom-up by dependency order. Start with the data (config.toml), then the loaders (structs + deserialization), then the state container (AppState), then the wiring (main.rs startup), and finally the tests.

**Merge semantics**: A `merge_recursive` function in `config.rs` walks two `toml::Value` trees, recursively merging tables and favoring overlay values for leaf keys. This enables the `CONFIG_PATH` file to be a sparse partial override rather than a full replacement. The embedded default is always the base; the user's file layers on top.

**Principle**: Each phase is independently testable. The config.toml can be validated with `toml::from_str` before anything else changes. New loader functions can be unit-tested against embedded TOML strings. AppState changes compile but don't affect behavior until main.rs wires them.

**Pattern**: Follow the existing `LLMClassifier` `Arc<RwLock<T>>` pattern exactly — construct with initial value, read with `.read().await`, write with `.write().await`. Read sites use `.read().await.clone()` for owned types, `.read().await` for Copy/Atomic types.

## Phase 1: Restructure `config.toml`

### Overview

Expand `config.toml` from a 42-line documentation template to a ~200-line self-sufficient default containing all configurable values currently resolved from env vars or hardcoded constants. This file is embedded via `include_str!()` in a later phase.

### Changes Required:

#### 1. Rewrite `config.toml`

**File**: `config.toml`

**Intent**: Transform the file into the complete single source of truth for all non-secret configuration. Every section that currently has a hardcoded fallback or env var default gets an explicit entry here.

**Contract**: The file must parse as valid TOML. Section structure:

```toml
[server]
port = 10000
log_level = "info"
log_format = "compact"

[http]
max_upstream_body_bytes = 10485760
keepalive_interval_secs = 15
request_body_limit_bytes = 10485760
client_timeout_secs = 120
client_connect_timeout_secs = 30
streaming_channel_capacity = 32

[cors]
allowed_origins = []

[database]
connection_retries = 3
retry_base_ms = 1000
max_connections = 10
acquire_timeout_secs = 30
idle_timeout_secs = 1800
log_concurrency_limit = 100

[classifiers]
enabled = true
order = ["regex", "llm"]

[regex_classifier]
enabled = true

# [llm_classifier] — commented out; enabled = false by default

[[categories]]
name = "FILE_READING"
description = "Reading, viewing, inspecting, searching, or navigating files or code"
threshold = 3
priority = 1

[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs, errors, typos, compilation issues, or broken code"
threshold = 3
priority = 2

[[categories]]
name = "COMPLEX_REASONING"
description = "Complex reasoning, debugging, architecture, design, or logic"
threshold = 3
priority = 3

[[categories]]
name = "CASUAL"
description = "Casual conversation, simple questions, or general chat"
threshold = 1
priority = 4

[FILE_READING]
model = "meta/llama-3.1-70b-instruct"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
provider_type = "nvidia_nim"
api_key_env = "NVIDIA_API_KEY"

[SYNTAX_FIX]
model = "meta/llama-3.1-8b-instruct"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
provider_type = "nvidia_nim"
api_key_env = "NVIDIA_API_KEY"

[COMPLEX_REASONING]
model = "meta/llama-3.3-70b-instruct"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
provider_type = "nvidia_nim"
api_key_env = "NVIDIA_API_KEY"

[CASUAL]
model = "meta/llama-3.1-8b-instruct"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
provider_type = "nvidia_nim"
api_key_env = "NVIDIA_API_KEY"

[FALLBACK]
model = "meta/llama-3.1-8b-instruct"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
provider_type = "nvidia_nim"
api_key_env = "NVIDIA_API_KEY"

[model_costs]
"claude-3.5-sonnet" = 3.00
"gpt-4o" = 2.50
"gpt-4o-mini" = 0.15
"deepseek-chat" = 0.14

baseline_model = "meta/llama-3.3-70b-instruct"

classify_db_log = false

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

[[auth_provider]]
type = "local"

[[auth_provider]]
type = "nvidia_nim"
header = "authorization"
value_template = "Bearer {api_key}"

[dashboard]
default_hours = 24
hours_min = 1
hours_max = 720
page_limit = 20
page_limit_max = 100
recent_count = 5
```

Note: Category entries no longer have `model_env_var` — that field is being removed in Phase 2. Model selection is now per-category routing blocks.

### Success Criteria:

#### Automated Verification:

- `toml::from_str::<toml::Value>(include_str!("../config.toml"))` succeeds without error
- All 4 `[[categories]]` parse correctly
- All 5 routing blocks (`[FILE_READING]`, `[SYNTAX_FIX]`, `[COMPLEX_REASONING]`, `[CASUAL]`, `[FALLBACK]`) have `model`, `endpoint`, `provider_type`, `api_key_env` fields
- `[auth_provider]` entries cover `openai_compatible`, `anthropic`, `ollama`, `local`, `nvidia_nim`
- `[model_costs]` contains all 4 hardcoded model costs
- Linting passes: `cargo build` (config.toml is not compiled yet, but ensures no accidental syntax issues)

#### Manual Verification:

- Review each section against the hardcoded values catalog in `context/changes/in-memory-config-filesystem/research.md:33-143`
- Confirm no secrets, tokens, or keys appear in the file

---

## Phase 2: New Config Structs & Loaders

### Overview

Add `DashboardConfig`, `AuthProviderConfig` structs and their loaders. Remove `model_env_var` from `CategoryConfig`. Replace the hardcoded `auth_headers_for()` match statement with a data-driven lookup. Update loaders in `config.rs` to parse the new config.toml sections.

### Changes Required:

#### 1. Add `DashboardConfig` struct and loader

**File**: `src/config.rs`

**Intent**: Define a struct for dashboard page defaults and a loader function that parses the `[dashboard]` section from TOML.

**Contract**:

- Struct: `DashboardConfig` with fields `default_hours: u32`, `hours_min: u32`, `hours_max: u32`, `page_limit: u32`, `page_limit_max: u32`, `recent_count: u32`
- Loader: `fn load_dashboard_config_from_value(root: &toml::Value) -> DashboardConfig` — returns defaults if section absent
- Defaults match current hardcoded values: `default_hours=24, hours_min=1, hours_max=720, page_limit=20, page_limit_max=100, recent_count=5`

#### 2. Add `AuthProviderConfig` struct and loader

**File**: `src/config.rs` (struct + loader), `src/intent_classifier.rs` (consumption)

**Intent**: Make provider-type → auth header mappings configurable via TOML instead of a hardcoded match statement.

**Contract**:

- Struct: `AuthProviderConfig` with fields `type_: String`, `header: Option<String>`, `value_template: Option<String>`. Both header and value_template are `None` for no-auth providers (ollama, local).
- Loader: `fn load_auth_providers_from_value(root: &toml::Value) -> Vec<AuthProviderConfig>` — parses `[[auth_provider]]` array
- Function: `auth_headers_for(&[AuthProviderConfig], provider_type: &str, api_key: &str) -> Vec<(String, String)>` — replaces the hardcoded match, accepts the provider list as a parameter

The existing hardcoded match statement at `src/intent_classifier.rs:507-514` is deleted. The new function performs a linear scan over the provider list (≤10 entries, negligible cost).

#### 3. Remove `model_env_var` from `CategoryConfig`

**File**: `src/intent_classifier.rs`

**Intent**: With per-category routing blocks in config.toml, the `model_env_var` indirection is no longer needed. Categories define classification logic (name, description, threshold, priority); routing defines model/endpoint.

**Contract**: Remove `model_env_var: Option<String>` from `CategoryConfig` struct. Update `hardcoded_categories()` to omit the field. Remove `model_env_var` parsing from `load_categories_from_value()` in `src/config.rs:307-310`.

#### 4. Add `ServerConfig` struct and loader

**File**: `src/config.rs`

**Intent**: Consolidate `PORT`, `RUST_LOG`, `LOG_FORMAT` into a single config struct loaded from the `[server]` TOML section.

**Contract**:

- Struct: `ServerConfig` with `port: u16`, `log_level: String`, `log_format: String`
- Loader: `fn load_server_config_from_value(root: &toml::Value) -> ServerConfig` — defaults: port=10000, log_level="info", log_format="compact"

#### 5. Add `HttpConfig` struct and loader (replaces `HttpClientConfig`)

**File**: `src/config.rs`

**Intent**: Consolidate all HTTP-layer settings from the `[http]` TOML section, replacing the current `HttpClientConfig::from_env()` that reads individual env vars.

**Contract**:

- Struct: `HttpConfig` with `max_upstream_body_bytes: usize`, `keepalive_interval_secs: u64`, `request_body_limit_bytes: usize`, `client_timeout_secs: u64`, `client_connect_timeout_secs: u64`, `streaming_channel_capacity: usize`
- Loader: `fn load_http_config_from_value(root: &toml::Value) -> HttpConfig` — defaults match current hardcoded values

#### 6. Add `DatabaseConfig` struct and loader

**File**: `src/config.rs`

**Intent**: Externalize DB pool and retry settings from `src/persistence.rs` hardcoded values.

**Contract**:

- Struct: `DatabaseConfig` with `connection_retries: u32`, `retry_base_ms: u64`, `max_connections: u32`, `acquire_timeout_secs: u64`, `idle_timeout_secs: u64`, `log_concurrency_limit: u32`
- Loader: `fn load_database_config_from_value(root: &toml::Value) -> DatabaseConfig` — defaults match current hardcoded values

#### 7. Update `build_model_costs` to parse `[model_costs]` section

**File**: `src/config.rs`

**Intent**: Parse the `[model_costs]` TOML table directly instead of seeding from `hardcoded_model_costs()` + applying per-route overrides.

**Contract**: `fn build_model_costs(root: &toml::Value, routing: &HashMap<String, RouteEntry>) -> ModelCosts` — reads `[model_costs]` table for base costs, then applies per-route `cost_per_1m_input_tokens` overrides from routing entries.

#### 8. Remove `NVIDIA_ENDPOINT_DEFAULT` constant

**File**: `src/config.rs`

**Intent**: The NVIDIA endpoint is now defined in config.toml's routing blocks, not in a constant.

**Contract**: Delete `pub(crate) const NVIDIA_ENDPOINT_DEFAULT: &str` at line 12-13. Remove all references (line 96 in `hardcoded_routing`).

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- New unit tests: `load_dashboard_config_from_value` returns defaults when section absent, parses explicit values when present
- New unit tests: `load_auth_providers_from_value` parses all provider types correctly
- New unit tests: `load_server_config_from_value`, `load_http_config_from_value`, `load_database_config_from_value` parse correctly from TOML
- New unit tests: `build_model_costs` reads from `[model_costs]` table and applies per-route overrides
- New unit tests: `auth_headers_for` with provider list returns correct headers for each provider type
- Existing config.rs tests still pass (some reference `model_env_var` — update or remove)
- Existing intent_classifier tests still pass (some reference `hardcoded_categories` — update field count)

#### Manual Verification:

- Verify `auth_headers_for` output matches old hardcoded match for all 5 provider types
- Verify `DashboardConfig` defaults match current hardcoded values in `src/dashboard.rs:157-198`

---

## Phase 3: Refactor `AppState`

### Overview

Wrap 6 mutable config fields in `Arc<RwLock<T>>` or `Arc<AtomicBool>`. Add `dashboard_config` and `auth_providers` fields. Update all read sites across `main.rs` and `dashboard.rs` to use `.read().await` or `.load()`.

### Changes Required:

#### 1. Update `AppState` struct definition

**File**: `src/main.rs:32-42`

**Intent**: Add `Arc<RwLock<T>>` wrappers to fields that will be runtime-updatable in the dashboard reload follow-up. Add new config struct fields.

**Contract**: New struct shape:

```rust
pub struct AppState {
    persistence: Option<persistence::PersistenceConfig>,
    classifier: Option<Arc<intent_classifier::ClassifierChain>>,
    routing: Arc<tokio::sync::RwLock<std::collections::HashMap<String, intent_classifier::RouteEntry>>>,
    model_costs: Arc<tokio::sync::RwLock<intent_classifier::ModelCosts>>,
    baseline_model: Arc<tokio::sync::RwLock<String>>,
    classify_db_log: Arc<std::sync::atomic::AtomicBool>,
    http_client: Option<reqwest::Client>,
    max_upstream_body_bytes: Arc<tokio::sync::RwLock<usize>>,
    keepalive_interval_secs: Arc<tokio::sync::RwLock<u64>>,
    dashboard_config: config::DashboardConfig,
    auth_providers: Arc<Vec<config::AuthProviderConfig>>,
}
```

Fields left unlocked (no writer planned yet): `persistence`, `classifier`, `http_client`, `dashboard_config`, `auth_providers`.

#### 2. Update hot-path reads in `completion_handler`

**File**: `src/main.rs:604`

**Intent**: Read routing and other RwLock-protected fields correctly on the proxy hot path.

**Contract**: 
- `state.routing.get(category)` → `state.routing.read().await.get(category)`
- `state.keepalive_interval_secs` → `*state.keepalive_interval_secs.read().await`
- `state.max_upstream_body_bytes` → `*state.max_upstream_body_bytes.read().await`

The `completion_handler` is already `async`, so adding `.await` to reads is straightforward.

#### 3. Update `classify_handler` reads

**File**: `src/main.rs:724`

**Intent**: Read `classify_db_log` as an atomic boolean.

**Contract**: `state.classify_db_log` → `state.classify_db_log.load(std::sync::atomic::Ordering::Relaxed)`

#### 4. Update dashboard handler reads

**File**: `src/dashboard.rs:136-139,306-307`

**Intent**: Dashboard handlers read `model_costs`, `baseline_model`, and now also `dashboard_config` for page defaults.

**Contract**:
- `state.model_costs.clone()` → `state.model_costs.read().await.clone()`
- `state.baseline_model.clone()` → `state.baseline_model.read().await.clone()`
- Hardcoded `24` → `state.dashboard_config.default_hours`
- Hardcoded `.clamp(1, 720)` → `.clamp(state.dashboard_config.hours_min, state.dashboard_config.hours_max)`
- Hardcoded `20` → `state.dashboard_config.page_limit`
- Hardcoded `.min(100)` → `.min(state.dashboard_config.page_limit_max)`
- Hardcoded `5` (recent count) → `state.dashboard_config.recent_count`

#### 5. Update `log_classification` helper

**File**: `src/main.rs:243-273`

**Intent**: This function takes `&AppState` — no changes needed since it only reads `state.persistence` (unchanged).

#### 6. Update `handle_streaming_response`

**File**: `src/main.rs:456-517`

**Intent**: This function takes `Arc<AppState>` and reads `keepalive_interval_secs`.

**Contract**: Read `keepalive_interval_secs` via the Arc (already cloned into `state`). The read happens inside the handler before spawning, so no lock contention concerns.

#### 7. Update `auth_headers_for` call in `build_upstream_request`

**File**: `src/main.rs:373`

**Intent**: Pass the configured auth providers list instead of relying on the hardcoded match.

**Contract**: `intent_classifier::auth_headers_for(&classification.provider_type, api_key)` → `intent_classifier::auth_headers_for(&state.auth_providers, &classification.provider_type, api_key)`. The `build_upstream_request` function signature adds a `&[AuthProviderConfig]` parameter, or the providers are read from `AppState` before calling.

#### 8. Update `auth_headers_for` call in `LLMClassifier::classify_async`

**File**: `src/intent_classifier.rs:284`

**Intent**: The LLM classifier also calls `auth_headers_for`. It needs access to the provider list (stored as a field or passed at construction).

**Contract**: Add `auth_providers: Arc<Vec<AuthProviderConfig>>` field to `LLMClassifier` struct. Pass it in `LLMClassifier::new`. The `call` method uses `auth_headers_for(&self.auth_providers, ...)`.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors — all read sites updated
- `cargo test auth` passes — route auth tests unchanged (AppState constructed in test helpers)
- `cargo test routes_auth` passes
- `cargo test` (all fast tests) passes — test helpers that construct `AppState` need updating first (Phase 6)

#### Manual Verification:

- Review every `state.` field access in `main.rs` and `dashboard.rs` to confirm RwLock/Atomic reads are correct
- Check no `.write().await` exists (no writer yet — all locks are read-only)

---

## Phase 4: Refactor `main.rs` Startup

### Overview

Replace the env-var-driven config loading chain with `include_str!("../config.toml")` as the embedded default. `CONFIG_PATH` becomes an optional overlay. Hardcoded fallback functions are called only as a last resort if the embedded TOML itself fails to parse. Remove env var reads for config values now in TOML sections. Wire `DashboardConfig`, `AuthProviderConfig`, and new config structs into `AppState` construction.

### Changes Required:

#### 1. Embed config.toml and restructure startup

**File**: `src/main.rs:76-219`

**Intent**: Replace the ladder of `config_root` → `unwrap_or_else(hardcoded_*)` with embedded default → CONFIG_PATH overlay → hardcoded last resort.

**Contract**: Startup flow becomes:

```
1. Load embedded TOML: const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");
   Parse: let embedded_root = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML)
       .expect("Embedded config.toml is invalid — this is a build-time error");

2. Build effective config_root:
   - Start with embedded_root as base
   - If CONFIG_PATH is set:
     - Read file → parse TOML → merge_recursive(base, overlay) → config_root
     - On file read/parse failure: warn! → config_root = embedded_root (unchanged)
   - Else: config_root = embedded_root

3. If config_root itself fails to parse (should only happen if embedded is broken):
   - error! → use hardcoded_categories() + hardcoded_routing() (last resort)
```

The `merge_recursive` function walks both `toml::Value` trees key by key. For each key in the overlay:
- If the key is a table in both → recurse
- Otherwise → overlay value wins (replaces base value)
- Keys only in the base are preserved as-is

This means a user's CONFIG_PATH file can be minimal — e.g., just a `[COMPLEX_REASONING]` block to change the model for one category, with all other settings falling through to embedded defaults.

#### 2. Load and wire new config structs

**File**: `src/main.rs:96-219`

**Intent**: Replace individual env var reads and `HttpClientConfig::from_env()` with TOML-loaded config structs.

**Contract**:
- `ServerConfig` → port, log level, log format
- `HttpConfig` → max_upstream_body_bytes, keepalive_interval_secs, streaming channel capacity, reqwest client timeouts
- `DatabaseConfig` → passed to `PersistenceConfig::from_env()` (updated signature in Phase 5)
- `DashboardConfig` → stored in AppState
- `AuthProviderConfig` list → stored in AppState, passed to `LLMClassifier::new`
- `classify_db_log` → read from TOML top-level field
- `baseline_model` → read from TOML top-level field
- All config structs that need RwLock wrappers are wrapped before inserting into `AppState`

#### 3. Implement `merge_recursive` for TOML overlay

**File**: `src/config.rs`

**Intent**: Enable the CONFIG_PATH overlay to be a sparse partial override rather than a full replacement. The embedded default is always the base; user-provided TOML layers on top.

**Contract**: `fn merge_toml_values(base: &mut toml::Value, overlay: &toml::Value)` — recursively walks both trees:
- If a key exists in both as tables → recurse
- If a key exists in overlay but not base → insert
- If a key exists in both and at least one is not a table → overlay value wins

The merge is destructive to `base` (mutated in place). The function is pure in the sense that `overlay` is never modified. Used once at startup in `main.rs` before passing the merged `toml::Value` to all loader functions.

#### 4. Remove env var reads for moved config

**File**: `src/main.rs`

**Intent**: Delete direct `std::env::var` calls for config values now in embedded TOML.

**Contract**: Remove env var reads for: `CLASSIFY_DB_LOG` (line 113), `BASELINE_MODEL` (line 141, via `env_or_default`), `STREAMING_CHANNEL_CAPACITY` (line 464), `ALLOWED_ORIGINS` (line 741). These values come from parsed TOML sections instead.

Keep env reads for secrets: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL`, and all `api_key_env` referenced values (dynamic).

#### 5. Remove `DEFAULT_MODEL*` constant usage in startup

**File**: `src/main.rs:141`, `src/config.rs:84-91`, `src/config.rs:93-124`

**Intent**: The `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX`, `DEFAULT_MODEL_READING` constants are no longer needed in the startup path — models come from routing blocks.

**Contract**: `hardcoded_routing()` and `hardcoded_model_default()` retain their references to these constants for the last-resort fallback. The constants stay in `src/routing.rs`. But `main.rs` no longer calls `env_or_default("BASELINE_MODEL", DEFAULT_MODEL_COMPLEX)` — it reads from parsed TOML.

#### 6. CORS configuration from TOML

**File**: `src/main.rs:741-757`

**Intent**: Read `[cors]` section from config instead of `ALLOWED_ORIGINS` env var.

**Contract**: `allowed_origins` parsed from `[cors]` section, defaulting to empty vec. CORS layer construction unchanged otherwise.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with embedded config.toml (validates `include_str!` resolves at compile time)
- App starts with no `CONFIG_PATH` set and no env vars for moved config → uses embedded defaults
- App starts with `CONFIG_PATH` pointing to a valid alternate config → merges on top of embedded config (user values win)
- App starts with `CONFIG_PATH` pointing to a missing/invalid file → falls back to embedded config alone (warn logged)
- If embedded TOML is corrupted (CI catches this, but for completeness): falls back to hardcoded (error logged)
- `cargo test` passes (all tests, after Phase 5+6 updates)
- Unit tests for `merge_toml_values`: overlay value wins, nested tables merge, keys only in base preserved, overlay-only keys added

#### Manual Verification:

- Start app with no env vars except secrets → verify it serves requests using embedded defaults
- Set `CONFIG_PATH` to a custom config → verify routing/categories reflect the custom config
- Verify `RUST_LOG` env var still works (tracing subscriber init happens before TOML parsing)
- Verify `LOG_FORMAT` from env still overrides config.toml value (or vice versa — decide precedence)

---

## Phase 5: Clean Up `persistence.rs` and Remaining Env Var Reads

### Overview

Update `PersistenceConfig::from_env()` to accept a `DatabaseConfig` struct instead of reading individual env vars for pool/retry settings. Clean up remaining env var reads that are now config-driven.

### Changes Required:

#### 1. Update `PersistenceConfig::from_env()` signature

**File**: `src/persistence.rs:91-156`

**Intent**: Accept `DatabaseConfig` for pool/retry settings instead of calling `parse_env_int` internally.

**Contract**: 
- `PersistenceConfig::from_env()` → `PersistenceConfig::from_env(db_config: &DatabaseConfig)` or `PersistenceConfig::new(db_config: DatabaseConfig)`
- Replace `parse_env_int("DB_CONNECTION_RETRIES", 3, ...)` with `db_config.connection_retries`
- Replace `parse_env_int("DB_RETRY_BASE_MS", 1000, ...)` with `db_config.retry_base_ms`
- Replace `parse_env_int("LOG_CONCURRENCY_LIMIT", 100, ...)` with `db_config.log_concurrency_limit`
- Replace hardcoded pool settings (`max_connections=10`, `acquire_timeout=30s`, `idle_timeout=1800s`) with `db_config` equivalents
- `DATABASE_URL` env var read stays (it's a secret/connection string)

#### 2. Remove dead `HttpClientConfig` struct and its `from_env()`

**File**: `src/config.rs:19-44`

**Intent**: `HttpClientConfig` is replaced by `HttpConfig` (Phase 2). Remove the old struct and its `from_env()` method.

**Contract**: Delete `HttpClientConfig` struct and `impl HttpClientConfig` block. Update any remaining references — primarily test helpers that call `HttpClientConfig::from_env()` (replace with `HttpConfig` defaults).

#### 3. Update `LLMClassifier::new` signature

**File**: `src/intent_classifier.rs:194-248`

**Intent**: Accept `auth_providers: Arc<Vec<AuthProviderConfig>>` for use in auth header construction.

**Contract**: Add `auth_providers` parameter to `LLMClassifier::new`. Store as field. Use in `classify_async` → `call` method instead of calling the old `auth_headers_for` directly.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles without errors
- `cargo test` passes
- DB connection still works when `DATABASE_URL` is set (integration tests)

#### Manual Verification:

- Verify persistence gracefully degrades when `DATABASE_URL` is absent (unchanged behavior)
- Verify DB connection retries use config values, not env vars

---

## Phase 6: Update Tests

### Overview

Update all test helpers that construct `AppState` to use the new `Arc<RwLock<T>>` fields and `DashboardConfig`. Update tests that reference `hardcoded_categories`, `hardcoded_routing`, `hardcoded_model_costs`, or `HttpClientConfig::from_env()`. Ensure all existing tests pass with the new config architecture.

### Changes Required:

#### 1. Update `test_app()` helper

**File**: `src/main.rs:820-841`

**Intent**: Construct `AppState` with RwLock wrappers and new fields.

**Contract**: Wrap `routing`, `model_costs`, `baseline_model`, `max_upstream_body_bytes`, `keepalive_interval_secs` in `Arc::new(RwLock::new(...))`. Add `dashboard_config: DashboardConfig::default()` or `config::DashboardConfig { ... }`. Add `auth_providers: Arc::new(vec![])`. Change `classify_db_log: false` to `classify_db_log: Arc::new(AtomicBool::new(false))`.

#### 2. Update `make_test_app_state()` helper

**File**: `src/main.rs:789-818`

**Intent**: Same as `test_app()` — wrap fields in RwLock and add new fields.

#### 3. Update `test_app_with_classifier()` and similar helpers

**File**: `src/main.rs:843-889` and other test helpers

**Intent**: All test helpers that construct `AppState` need the same updates.

#### 4. Update config.rs tests

**File**: `src/config.rs:532-1043`

**Intent**: 
- Tests that reference `model_env_var` on `CategoryConfig` — update to not include the field
- Tests that call `HttpClientConfig::from_env()` — replace with `HttpConfig` defaults
- Add tests for new loader functions (DashboardConfig, AuthProviderConfig, ServerConfig, HttpConfig, DatabaseConfig)
- Tests for `hardcoded_categories()` — update assertions for removed `model_env_var` field
- Tests for `hardcoded_routing()` — update for removed `NVIDIA_ENDPOINT_DEFAULT` constant

#### 5. Update intent_classifier.rs tests

**File**: `src/intent_classifier.rs` (test module)

**Intent**: 
- Tests referencing `auth_headers_for()` without provider list — update to pass `&[]` or the default providers
- Tests constructing `CategoryConfig` with `model_env_var` — remove the field
- Tests constructing `LLMClassifier` — pass `auth_providers: Arc::new(vec![])`
- Tests for `hardcoded_categories()` — update field count assertions

#### 6. Update persistence.rs tests

**File**: `src/persistence.rs` (test module)

**Intent**: Tests calling `hardcoded_model_costs()` — these are test-only references. Either keep `hardcoded_model_costs()` as a test utility or update to use `[model_costs]` from embedded TOML.

### Success Criteria:

#### Automated Verification:

- `cargo test auth` passes
- `cargo test routes_auth` passes
- `cargo test` (all fast tests) passes
- `cargo test slow_tests` passes (requires env setup)
- `cargo build --release` succeeds

#### Manual Verification:

- Run the test suite with various env var combinations to ensure no regression
- Verify classification tests still produce correct category routing

---

## Testing Strategy

### Unit Tests:

- Each new config loader function (DashboardConfig, AuthProviderConfig, ServerConfig, HttpConfig, DatabaseConfig) tested with valid TOML, missing sections, and invalid values
- `auth_headers_for` with provider list for all 5 provider types
- `build_model_costs` with `[model_costs]` table and per-route overrides
- Embedded TOML parses correctly as `toml::Value`
- CONFIG_PATH overlay path (valid file, missing file, invalid TOML)

### Integration Tests:

- Full app startup with embedded config (no CONFIG_PATH set)
- Full app startup with CONFIG_PATH overlay
- Classification still works through embedded config routing
- Dashboard pages render with config-driven defaults

### Manual Testing Steps:

1. Start app with no env vars except secrets → verify `/health` and proxy routes work
2. Set `CONFIG_PATH` to a custom TOML → verify custom routing takes effect
3. Verify dashboard pages show correct defaults
4. Verify `RUST_LOG=debug cargo run` produces expected log output
5. Verify existing production deployment path (Render) still works

## Performance Considerations

- `tokio::sync::RwLock::read()` is contention-free when no writer holds the lock — no writer exists yet, so zero overhead vs direct field access
- `Arc<AtomicBool>::load(Relaxed)` is a single CPU instruction — zero overhead vs `bool`
- Embedded TOML parsing at startup adds ~microseconds to startup time (200 lines of TOML)
- Binary size increases by ~3KB (the embedded config.toml string)

## Migration Notes

- **Existing `CONFIG_PATH` configs**: With merge semantics, existing configs at `CONFIG_PATH` continue to work as partial overrides on top of the embedded default. No migration is strictly required — any sections not in the user's file fall through to embedded defaults. However, users should remove `model_env_var` from their `[[categories]]` entries and replace them with corresponding routing blocks (`[FILE_READING]`, etc.).
- **`model_env_var` removal**: Any existing `config.toml` files with `model_env_var` in `[[categories]]` must remove the field and add corresponding routing blocks.
- **`ALLOWED_ORIGINS`**: Users relying on the `ALLOWED_ORIGINS` env var must move the value to `[cors].allowed_origins` in config.toml.
- **No database migration**: This change does not affect the database schema.
- **No API contract changes**: Category names, route paths, and response formats are unchanged.

## References

- Research: `context/changes/in-memory-config-filesystem/research.md`
- Prior related change: `context/changes/classifier-config-boundary/`
- AGENTS.md lessons: `context/foundation/lessons.md`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Restructure config.toml

#### Automated

- [ ] 1.1 `toml::from_str::<toml::Value>(include_str!("../config.toml"))` succeeds
- [ ] 1.2 All 4 `[[categories]]` parse correctly
- [ ] 1.3 All 5 routing blocks have required fields
- [ ] 1.4 `[auth_provider]` entries cover all 5 provider types
- [ ] 1.5 `[model_costs]` contains all 4 hardcoded model costs
- [ ] 1.6 `cargo build` succeeds

#### Manual

- [ ] 1.7 Review each section against hardcoded values catalog in research.md
- [ ] 1.8 Confirm no secrets, tokens, or keys appear in the file

### Phase 2: New Config Structs & Loaders

#### Automated

- [ ] 2.1 `load_dashboard_config_from_value` unit tests pass
- [ ] 2.2 `load_auth_providers_from_value` unit tests pass
- [ ] 2.3 `load_server_config_from_value` unit tests pass
- [ ] 2.4 `load_http_config_from_value` unit tests pass
- [ ] 2.5 `load_database_config_from_value` unit tests pass
- [ ] 2.6 `build_model_costs` reads from `[model_costs]` table correctly
- [ ] 2.7 `auth_headers_for` with provider list for all 5 provider types
- [ ] 2.8 `cargo build` compiles without errors
- [ ] 2.9 Existing config.rs tests pass (updated for model_env_var removal)

#### Manual

- [ ] 2.10 Verify `auth_headers_for` output matches old hardcoded match
- [ ] 2.11 Verify `DashboardConfig` defaults match current hardcoded values

### Phase 3: Refactor AppState

#### Automated

- [ ] 3.1 `cargo build` compiles — all read sites updated
- [ ] 3.2 `cargo test auth` passes
- [ ] 3.3 `cargo test routes_auth` passes
- [ ] 3.4 `cargo test` (all fast tests) passes

#### Manual

- [ ] 3.5 Review all `state.` field accesses for correct RwLock/Atomic reads
- [ ] 3.6 Confirm no `.write().await` exists on AppState fields

### Phase 4: Refactor main.rs Startup

#### Automated

- [ ] 4.1 `cargo build` compiles with embedded config.toml
- [ ] 4.2 App starts with no CONFIG_PATH set → uses embedded defaults
- [ ] 4.3 App starts with CONFIG_PATH → valid file → merges on top of embedded config
- [ ] 4.4 App starts with CONFIG_PATH → missing file → falls back to embedded alone
- [ ] 4.5 Unit tests for `merge_toml_values` pass
- [ ] 4.6 `cargo test` passes

#### Manual

- [ ] 4.7 Start app with no env vars except secrets → proxy routes work
- [ ] 4.8 Set CONFIG_PATH to custom config → verify merge: custom values override embedded, unspecified sections fall through
- [ ] 4.9 Verify RUST_LOG env var still works
- [ ] 4.10 Verify LOG_FORMAT from TOML takes effect

### Phase 5: Clean Up persistence.rs and Remaining Env Var Reads

#### Automated

- [ ] 5.1 `cargo build` compiles without errors
- [ ] 5.2 `cargo test` passes
- [ ] 5.3 DB connection works when DATABASE_URL is set (integration tests)

#### Manual

- [ ] 5.4 Persistence gracefully degrades when DATABASE_URL is absent
- [ ] 5.5 DB connection retries use config values, not env vars

### Phase 6: Update Tests

#### Automated

- [ ] 6.1 `cargo test auth` passes
- [ ] 6.2 `cargo test routes_auth` passes
- [ ] 6.3 `cargo test` (all fast tests) passes
- [ ] 6.4 `cargo test slow_tests` passes
- [ ] 6.5 `cargo build --release` succeeds

#### Manual

- [ ] 6.6 Run test suite with various env var combinations
- [ ] 6.7 Verify classification tests produce correct category routing
