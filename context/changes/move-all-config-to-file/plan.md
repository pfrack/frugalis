# Move All Config to File — Implementation Plan

## Overview

Move all remaining non-secret env var reads into `config.toml`. The codebase already embeds `config.toml` at compile time and loads most settings from it, but several env vars still act as configuration channels (`LOG_FORMAT`, `ALLOWED_ORIGINS`, `PORT`, `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`). After this plan, only secrets (API keys, auth credentials, `DATABASE_URL`) and meta-config (`CONFIG_PATH`, `RUST_LOG` as runtime override) remain as env vars.

## Current State Analysis

The config system already follows a consistent pattern: `config.toml` is parsed into a generic `toml::Value`, merged with an optional `CONFIG_PATH` overlay, then individual sections are extracted by dedicated loader functions into typed structs. However, several env var reads bypass this system:

- `LOG_FORMAT` env var (src/main.rs:53) — controls tracing output format; config.toml already has `log_format = "compact"` under `[server]` but the code ignores it
- `RUST_LOG` env var (src/main.rs:51) — config.toml has `log_level = "info"` under `[server]` but it's unused
- `ALLOWED_ORIGINS` env var (src/main.rs:803) — comma-separated CORS origins; config.toml already has `[cors] allowed_origins = []` but it's unused
- `PORT` env var (src/main.rs:279) — overrides `[server].port` from config.toml
- `DEFAULT_MODEL` env var — used via `env_or_default()` in `hardcoded_routing()`, `routing_from_value()`, `load_routing()`, and `ClassificationResult::fallback()` (src/config.rs:388,405,432,496,522,556; src/intent_classifier.rs:620)
- `NVIDIA_ENDPOINT` env var — used via `env_or_default()` in `hardcoded_routing()` only (src/config.rs:388)
- `ROUTING_CONFIG_PATH` env var — test-only legacy, unused in production (src/config.rs:471)

### Key Discoveries:

- `ServerConfig` (src/config.rs:235) currently only contains `port` — needs `log_level` and `log_format`
- No `CorsConfig` struct exists — the `[cors]` section in config.toml has no loader
- config.toml has `[FALLBACK]` section with full routing spec (model, endpoint, provider_type, api_key_env) — rename to `[DEFAULT]` and use it as the single source for default model/endpoint, replacing `env_or_default("DEFAULT_MODEL", ...)` and `env_or_default("NVIDIA_ENDPOINT", ...)`
- `env_or_default()` helper (src/config.rs:13) will become dead code after migration — remove it
- `hardcoded_routing()` (src/config.rs:385) is a last-resort fallback that currently sniffs env vars — should accept parameters from config
- `ClassificationResult::fallback()` (src/intent_classifier.rs:617) uses `env_or_default` for model — can use the const directly since endpoint is empty
- Render automatically injects `PORT` and `RUST_LOG` — we keep `RUST_LOG` as override and remove `PORT` env read
- `merge_toml_values()` (src/config.rs:192) recursively merges all nested tables indiscriminately. This is fine for operational sections (`[server]`, `[http]`, `[database]`, `[cors]`, `[dashboard]`, `[persistence]`) where partial overrides are useful. But for routing entries and classifier configs (`[classifiers]`, `[regex_classifier]`, `[llm_classifier]`, `[model_costs]`, `[[categories]]`, `[[auth_provider]]`), a partial merge would silently combine base+overlay incorrectly — these sections need complete replacement semantics.
- `AppState` already uses `Arc<RwLock<...>>` for runtime-mutable values (`keepalive_interval_secs`, `max_upstream_body_bytes`). New mutable config values (`allowed_origins`) should follow the same pattern to enable future live config reload.

## Desired End State

After this plan:

- All non-secret operational settings are sourced exclusively from `config.toml` (with `CONFIG_PATH` overlay support)
- `PORT`, `LOG_FORMAT`, `ALLOWED_ORIGINS` are no longer read from env vars in production
- `RUST_LOG` env var still works as an override for the `log_level` from config.toml (standard Rust ecosystem pattern)
- `CONFIG_PATH` remains as the only non-secret env var (meta-configuration — the path to the config file itself)
- `ROUTING_CONFIG_PATH` is removed entirely
- `env_or_default()` helper is removed
- Test helpers use programmatic config construction instead of env var sniffing
- `render.yaml` only lists secrets + `RUST_LOG` and `CONFIG_PATH`
- Config structs are in `src/config.rs`; their loader functions exist alongside them

## What We're NOT Doing

- NOT changing the TOML parsing approach (manual `toml::Value` extraction — no serde introduced)
- NOT changing how secrets are handled (API keys, auth tokens, `DATABASE_URL` stay as env vars)
- NOT moving `RUST_LOG` fully to config.toml (kept as runtime override)
- NOT removing `CONFIG_PATH` (meta-configuration must stay as env var)
- NOT refactoring the `merge_toml_values()` mechanism or compile-time embedding
- NOT adding new functionality beyond moving existing config channels

## Implementation Approach

Follow the existing config pattern: add structs → add loader functions → wire in main.rs → remove env reads → clean up tests. Each TOML value extracted from the generic tree with `get()`, `as_str()`, `as_integer()`, `as_array()`, etc., matching the established convention.

## Critical Implementation Details

- **`routing_from_value()` default model resolution**: The function needs the default model to fill missing `model` fields on routing entries, but the default model comes from the `[DEFAULT]` entry — which is extracted as the fallback after all entries are processed. Resolve by peeking at the `[DEFAULT]` entry's model before removing it from the map, or by doing two passes.
- **`RUST_LOG` override ordering**: Init tracing with `EnvFilter::new(&server_config.log_level)` first, then check for `RUST_LOG` env override. If `RUST_LOG` is set, use it instead — matching the principle that runtime env overrides config file.
- **Live-reload readiness**: Config values that could benefit from runtime reload should be stored behind `Arc<RwLock<...>>` in `AppState`, matching the existing pattern for `keepalive_interval_secs` and `max_upstream_body_bytes`. This applies to `CorsConfig.allowed_origins` (CORS can be updated at runtime via a future reload mechanism). Startup-only values (`log_level`, `log_format`, `port`) can remain plain struct fields since they're consumed once at init time and can't be meaningfully changed without a restart.

## Phase 1: Expand Config Structs & config.toml

### Overview

Add config structs and loader functions for settings currently only read from env vars. Expand existing structs where fields belong under an existing section.

### Changes Required:

#### 1.1 Expand `ServerConfig` with `log_level` and `log_format`

**File**: `src/config.rs`

**Intent**: `ServerConfig` currently only holds `port`. The `[server]` section in config.toml also has `log_level` and `log_format` — add them so the struct matches the TOML section.

**Contract**: Add `log_level: String` and `log_format: String` fields to `ServerConfig`. Default impl: `log_level: "info"`, `log_format: "compact"`. Update `load_server_config_from_value()` to extract these fields from the `[server]` table.

#### 1.2 Create `CorsConfig` struct and loader

**File**: `src/config.rs`

**Intent**: The `[cors]` section in config.toml currently has no corresponding struct or loader. Create one so CORS origins can be sourced from config.toml instead of the `ALLOWED_ORIGINS` env var.

**Contract**: New struct:
```
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
}
```
Default: empty vec. New function `load_cors_config_from_value(root: &toml::Value) -> CorsConfig` that reads `cors.allowed_origins` as a TOML array of strings.

#### 1.3 Rename `[FALLBACK]` → `[DEFAULT]` in config.toml and routing code

**File**: `config.toml`, `src/config.rs`

**Intent**: The existing `[FALLBACK]` section in config.toml already has the complete default routing spec (model, endpoint, provider_type, api_key_env). Rename it to `[DEFAULT]` and treat it as the single source for default model configuration — replacing `env_or_default("DEFAULT_MODEL", ...)` and `env_or_default("NVIDIA_ENDPOINT", ...)` which only provided model name and endpoint separately.

**Contract**:
- In `config.toml`: rename `[FALLBACK]` → `[DEFAULT]` (lines 100-104), values unchanged
- In `routing_from_value()` (src/config.rs:507): change the fallback lookup from `routing.remove("FALLBACK")` to `routing.remove("DEFAULT")` — the existing loop already uppercases keys so `"DEFAULT"` matches
- `baseline_model` top-level key stays where it is (it's separate — used for LLM classifier baseline comparison, not routing)
- `RouteEntry` already has all needed fields — no new struct needed

#### 1.4 Add per-key override mode to `merge_toml_values`

**File**: `src/config.rs`

**Intent**: Currently `merge_toml_values()` recursively merges all nested tables. This is correct for section-level config (`[server]`, `[http]`, `[database]`, `[dashboard]`, `[cors]`) where you want to override individual fields. But for routing entries and `[[categories]]` arrays, the overlay should **completely replace** the base — an overlay that sets only `model` on `[FILE_READING]` should not silently inherit the base's `endpoint` and `api_key_env`.

**Contract**: Modify `merge_toml_values()` to accept a set of top-level keys that should be completely replaced (not recursively merged). The set includes all routing entry keys (distinguishable by being all-uppercase: `FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL`, and the new `DEFAULT`). When a key in this set appears in the overlay, the overlay's value replaces the base's entirely.

The function signature changes from:
```
fn merge_toml_values(base: &mut toml::Value, overlay: &toml::Value)
```
to:
```
fn merge_toml_values(base: &mut toml::Value, overlay: &toml::Value, override_keys: &HashSet<&str>)
```

At the call site in `main()` (line 88), discover override keys from the overlay: all top-level keys that are all-uppercase (routing entries: `FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL`, `DEFAULT`) plus `"classifiers"`, `"regex_classifier"`, `"llm_classifier"`, `"categories"`, `"auth_provider"`, `"model_costs"`. Everything else (`server`, `http`, `database`, `cors`, `dashboard`, `persistence`) continues to merge field-by-field — users can override just `port` without re-specifying `log_level`.

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with new struct definitions
- `cargo test` — existing config tests pass (no regressions)

#### Manual Verification:

- `config.toml` `[FALLBACK]` section renamed to `[DEFAULT]`

---

## Phase 2: Wire Up config.toml in main.rs

### Overview

Replace env var reads with config struct values. This touches tracing init, CORS setup, port binding, and the config loading/fallback paths.

### Changes Required:

#### 2.1 Use `server_config.log_level` for tracing init with `RUST_LOG` override

**File**: `src/main.rs`

**Intent**: Replace the implicit `RUST_LOG` read with `server_config.log_level` as the default, while preserving `RUST_LOG` as a runtime override. This makes `log_level` from config.toml the primary source.

**Contract**: Change tracing init (lines 50-58) to:
1. Construct `EnvFilter::new(&server_config.log_level)` as the base
2. Check `std::env::var("RUST_LOG")` — if set and non-empty, use that `EnvFilter` instead
3. Use `server_config.log_format` instead of `LOG_FORMAT` env var for format selection

#### 2.2 Use `CorsConfig` instead of `ALLOWED_ORIGINS` env var

**File**: `src/main.rs`

**Intent**: Load CORS origins from config.toml instead of the comma-separated env var.

**Contract**: In `main()`: call `config::load_cors_config_from_value(&config_root)`. Store `allowed_origins` in `AppState` behind `Arc<RwLock<Vec<String>>>` (matching the `keepalive_interval_secs` pattern — enables future live reload). In `build_app()`: read from the lock to build the CORS layer.

#### 2.3 Source default model/endpoint from `[DEFAULT]` routing entry

**File**: `src/main.rs`, `src/config.rs`, `src/intent_classifier.rs`

**Intent**: Replace `env_or_default("DEFAULT_MODEL", ...)` and `env_or_default("NVIDIA_ENDPOINT", ...)` calls with values from the parsed `[DEFAULT]` routing entry. The `[DEFAULT]` entry (loaded via `routing_from_value`) already has the full spec — model, endpoint, provider_type, api_key_env.

**Contract**:
- In `main()`: after `routing_from_value()` returns the fallback entry, use `fallback_entry.model` and `fallback_entry.endpoint` wherever the env vars were read
- `hardcoded_routing()` (src/config.rs:385): this is the last-resort fallback when no config exists at all — keep using `DEFAULT_MODEL` const and hardcoded NVIDIA endpoint (no config to read from). Remove `env_or_default()` calls, use consts directly
- `routing_from_value()` (src/config.rs:507): already removes `"FALLBACK"` (now `"DEFAULT"`) from routing map and returns it as fallback. Replace `env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)` at lines 522 and 556 with the fallback entry's `model` (but note: this is a chicken-and-egg problem — the fallback model is needed to fill missing fields before the fallback is extracted. Solution: parse the `[DEFAULT]` entry first to extract its model, then use that for missing-field fallbacks, then remove it from the map)
- `ClassificationResult::fallback()` (src/intent_classifier.rs:617): use `DEFAULT_MODEL` const directly — it's a fire-and-forget result with empty endpoint, no routing lookup happens

#### 2.4 Remove `PORT` env var override

**File**: `src/main.rs`

**Intent**: Use `server_config.port` directly without env var override.

**Contract**: In `main()` (lines 279-282): replace `std::env::var("PORT").ok()...` with just `server_config.port`. Render already injects `PORT` — but Render's platform-level port is exposed via the `PORT` env var that the runtime should bind to. To handle Render deployment: keep the `PORT` override but move it to be Render-specific (check if running on Render, or document that Render users should set a `CONFIG_PATH` overlay). **Alternatively**: change `render.yaml` to set `CONFIG_PATH` instead and keep the port in an overlay config.

**Decision**: Remove the `PORT` env override from code. For Render, Render's automatic `PORT` injection is a platform concern — add a note in AGENTS.md that Render users who need a different port should use a `CONFIG_PATH` overlay.

### Success Criteria:

#### Automated Verification:

- `cargo build --release` compiles without errors
- `cargo test` — all tests pass
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass

#### Manual Verification:

- Start the app without `LOG_FORMAT` env set — verify log format comes from config.toml
- Set `RUST_LOG=debug` — verify it overrides config.toml `log_level`
- Verify CORS headers match `[cors].allowed_origins` from config.toml
- Verify app binds to `[server].port` from config.toml

---

## Phase 3: Clean Up Tests, render.yaml, and Dead Code

### Overview

Remove legacy env var code paths, update test helpers to use programmatic config, clean up `render.yaml`, and remove the `env_or_default()` helper.

### Changes Required:

#### 3.1 Remove `ROUTING_CONFIG_PATH` and clean up `load_routing()`

**File**: `src/config.rs`

**Intent**: `ROUTING_CONFIG_PATH` is a legacy env var only used in the test-only `load_routing()` function. Remove it.

**Contract**: In `load_routing()` (line 468): remove the `std::env::var("ROUTING_CONFIG_PATH")` fallback in the `or_else` chain. Keep only `CONFIG_PATH` with fallback to `CONFIG_DEFAULT`. Update `load_routing()` to use `DEFAULT_MODEL` const instead of `env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)`.

#### 3.2 Remove `env_or_default()` helper and its tests

**File**: `src/config.rs`

**Intent**: After Phase 2, no production code calls `env_or_default()`. Remove it.

**Contract**: Delete the `env_or_default()` function (line 13-15) and its two tests (`env_or_default_returns_env_var_when_set`, `env_or_default_returns_default_when_unset`).

#### 3.3 Update test helpers to use programmatic config

**File**: `src/main.rs`

**Intent**: Test helpers currently read `MAX_UPSTREAM_BODY_BYTES` and `KEEPALIVE_INTERVAL_SECS` from env vars. Replace with programmatic values.

**Contract**:
- `make_test_app_state()` (line 851): replace `std::env::var("MAX_UPSTREAM_BODY_BYTES")` with hardcoded `10_485_760` (matching the default)
- `test_streaming_keepalive_injected()` (line 2610): replace `std::env::set_var("KEEPALIVE_INTERVAL_SECS", "1")` with directly setting `app_state.keepalive_interval_secs` to `1` after constructing the test app state (or parameterize the helper)
- Any other test that sets/reads these env vars — update to programmatic approach

#### 3.4 Update `render.yaml`

**File**: `render.yaml`

**Intent**: Remove non-secret env vars, add `CONFIG_PATH`, fix drift.

**Contract**:
- Remove `RUST_LOG` (if `info` is the default in config.toml, or keep it for explicit logging)
- Remove `ROUTING_CONFIG_PATH` (unused)
- Remove `NVIDIA_NIM_API_KEY` (code uses `NVIDIA_API_KEY` — drift fix)
- Remove `OPENROUTER_API_KEY` (code doesn't read it — drift fix)
- **Keep**: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL`
- Decision on `RUST_LOG`: keep it for operational visibility (Render dashboard shows log level)

#### 3.5 Update `AGENTS.md`

**File**: `AGENTS.md`

**Intent**: Reflect the new config approach — `PORT` is no longer env-configured.

**Contract**: Update the "Required Setup" section to remove `PORT` from the env vars list, add reference to `config.toml`.

### Success Criteria:

#### Automated Verification:

- `cargo test` — all tests pass
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass
- `cargo build --release` — builds cleanly
- No remaining `env_or_default(` calls in non-test code
- No remaining `ROUTING_CONFIG_PATH` references

#### Manual Verification:

- Verify `render.yaml` only lists the 4 required secrets + optional `RUST_LOG`
- Verify app starts successfully with only secrets as env vars (plus optional `RUST_LOG` and `CONFIG_PATH`)
- Verify `RUST_LOG=debug cargo run` produces debug-level logs

---

## Testing Strategy

### Unit Tests:

- `load_server_config_from_value` returns log_level/log_format defaults when absent
- `load_cors_config_from_value` returns empty origins when section absent, parses array when present
- `routing_from_value` extracts `[DEFAULT]` as fallback when present
- `hardcoded_routing()` uses const defaults correctly (no env vars)

### Integration Tests:

- `cargo test auth` — auth still works (no changes to auth)
- `cargo test routes_auth` — route auth still works
- Full `cargo test` suite passes

### Manual Testing Steps:

1. Start app with only secrets as env vars: `PROXY_API_BEARER_TOKEN=x DASHBOARD_BASIC_USER=x DASHBOARD_BASIC_PASSWORD=x cargo run`
2. Verify log output format matches config.toml `log_format`
3. Set `RUST_LOG=debug` and restart — verify debug logs appear
4. Set `ALLOWED_ORIGINS` env var to a value and restart — verify it is **ignored** (origins come from config.toml)
5. Verify `/health` endpoint responds
6. Verify `/v1/chat/completions` requires auth

## Performance Considerations

None — this is a configuration refactor, no runtime performance impact.

## Migration Notes

- Existing deployments that set `LOG_FORMAT`, `ALLOWED_ORIGINS`, or `PORT` env vars must migrate those values to `config.toml`
- Existing deployments relying on `DEFAULT_MODEL` or `NVIDIA_ENDPOINT` env vars must set the default model/endpoint in the `[DEFAULT]` routing section of config.toml (currently `[FALLBACK]`, renamed by this plan)
- Render deployment: `PORT` is injected automatically by Render but the app will now use `[server].port` from config.toml. For Render, the embedded default `10000` is correct (Render's health check is at `/health` on the service port — Render routes external traffic to the container port)
- `ROUTING_CONFIG_PATH` is removed — any deployment still using it should switch to `CONFIG_PATH`
- `NVIDIA_NIM_API_KEY` and `OPENROUTER_API_KEY` in render.yaml were never read by code — removed as drift

## References

- Current config loading: `src/main.rs:49-178`
- Config struct definitions: `src/config.rs:209-824`
- `env_or_default` helper: `src/config.rs:13-15`
- `hardcoded_routing()`: `src/config.rs:385-412`
- `routing_from_value()`: `src/config.rs:507-563`
- CORS env read: `src/main.rs:803-808`
- Port env read: `src/main.rs:279-282`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Expand Config Structs & config.toml

#### Automated

- [x] 1.1 `cargo build` compiles with new struct definitions — 3e0a558
- [x] 1.2 `cargo test` — existing config tests pass — 3e0a558

#### Manual

- [x] 1.3 `config.toml` `[FALLBACK]` renamed to `[DEFAULT]`
- [x] 1.4 `merge_toml_values` supports per-key override mode for routing entries

### Phase 2: Wire Up config.toml in main.rs

#### Automated

- [x] 2.1 `cargo build --release` compiles without errors — 2390ac4
- [x] 2.2 `cargo test` — all tests pass — 2390ac4
- [x] 2.3 `cargo test auth` — auth tests pass — 2390ac4
- [x] 2.4 `cargo test routes_auth` — route auth tests pass — 2390ac4

#### Manual

- [ ] 2.5 App starts without LOG_FORMAT env, uses config.toml log_format
- [ ] 2.6 RUST_LOG=debug overrides config.toml log_level
- [ ] 2.7 CORS headers match config.toml [cors].allowed_origins
- [ ] 2.8 App binds to config.toml [server].port

### Phase 3: Clean Up Tests, render.yaml, and Dead Code

#### Automated

- [x] 3.1 `cargo test` — all tests pass
- [x] 3.2 `cargo test auth` — auth tests pass
- [x] 3.3 `cargo test routes_auth` — route auth tests pass
- [x] 3.4 `cargo build --release` builds cleanly
- [x] 3.5 No remaining `ROUTING_CONFIG_PATH` references

#### Manual

- [ ] 3.6 render.yaml only lists required secrets + optional RUST_LOG
- [ ] 3.7 App starts successfully with only secrets as env vars
- [ ] 3.8 RUST_LOG=debug cargo run produces debug-level logs
