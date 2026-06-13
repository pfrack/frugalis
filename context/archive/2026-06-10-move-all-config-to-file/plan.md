# Move All Config to File — Implementation Plan

## Overview

Two-part config migration: (1) move all non-secret env var reads into `config.toml`, then (2) move all hardcoded classifier data (regex patterns, weights, thresholds, model costs, negative suppression rules) into `config.toml` and make the `RegexClassifier` engine fully generic. After this plan, only secrets (API keys, auth credentials, `DATABASE_URL`) and meta-config (`CONFIG_PATH`, `RUST_LOG` as runtime override) remain as env vars, and the classifier works with any user-supplied category config — zero knowledge of FILE_READING, SYNTAX_FIX, etc.

## Current State Analysis

The config system already follows a consistent pattern: `config.toml` is parsed into a generic `toml::Value`, merged with an optional `CONFIG_PATH` overlay, then individual sections are extracted by dedicated loader functions into typed structs. However, several env var reads bypass this system:

- `LOG_FORMAT` env var (src/main.rs:53) — controls tracing output format; config.toml already has `log_format = "compact"` under `[server]` but the code ignores it
- `RUST_LOG` env var (src/main.rs:51) — config.toml has `log_level = "info"` under `[server]` but it's unused
- `ALLOWED_ORIGINS` env var (src/main.rs:803) — comma-separated CORS origins; config.toml already has `[cors] allowed_origins = []` but it's unused
- `PORT` env var (src/main.rs:279) — overrides `[server].port` from config.toml
- `DEFAULT_MODEL` env var — used via `env_or_default()` in `hardcoded_routing()`, `routing_from_value()`, `load_routing()`, and `ClassificationResult::fallback()` (src/config.rs:388,405,432,496,522,556; src/intent_classifier.rs:620)
- `NVIDIA_ENDPOINT` env var — used via `env_or_default()` in `hardcoded_routing()` only (src/config.rs:388)
- `ROUTING_CONFIG_PATH` env var — test-only legacy, unused in production (src/config.rs:471)

### Remaining Hardcoded Classifier Data (Phase 4–6 target):

Beyond the env vars, `src/intent_classifier.rs` (1232 lines) still mixes engine logic with hardcoded data that should be config-driven:

- **4 positive pattern arrays** (FILE_READING: 12 patterns, COMPLEX_REASONING: 16, SYNTAX_FIX: 11, CASUAL: 5) — lines 416–470
- **4 negative suppression patterns** (`NEGATIVE` + `NEGATIVE_META` arrays) — lines 472–498
- **4 weight arrays** (`FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS`) — lines 405–408
- **`SHORT_PROMPT_LEN` const** (30) — line 412
- **`hardcoded_model_costs()`** seeds `build_model_costs()` with 4 vendor costs — `intent_classifier.rs:16–23`, called from `config.rs:712`
- **`hardcoded_categories()`** returns 4 `CategoryConfig` entries — used as TOML parsing fallback in `main.rs:149`
- **`build_all_patterns()`** matches on hardcoded category name strings (`"FILE_READING"`, `"COMPLEX_REASONING"`, `"SYNTAX_FIX"`, `"CASUAL"`) — lines 539–602
- **`classify_internal()`** has hardcoded SYNTAX_FIX dual-threshold referencing `"SYNTAX_FIX"` and `"FILE_READING"` by string — lines 718–726
- **CASUAL special cases** in `route_match()` (line 749), `ClassificationResult::fallback()` (line 619), and `fallback_category()` (line 609)
- **`PatternMeta.category`** is `&'static str` — must become `String` since patterns come from runtime config
- **LLM few-shot examples** hardcode category names in `build_llm_classifier_prompt()` (lines 377–380)

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

- All non-secret operational **and classifier** settings are sourced exclusively from `config.toml` (with `CONFIG_PATH` overlay support)
- `PORT`, `LOG_FORMAT`, `ALLOWED_ORIGINS` are no longer read from env vars in production
- `RUST_LOG` env var still works as an override for the `log_level` from config.toml (standard Rust ecosystem pattern)
- `CONFIG_PATH` remains as the only non-secret env var (meta-configuration — the path to the config file itself)
- `ROUTING_CONFIG_PATH` is removed entirely
- `env_or_default()` helper is removed
- `hardcoded_model_costs()` and `hardcoded_categories()` are removed
- All regex patterns, weights, negative suppression rules, and model costs are defined in `config.toml`
- `CategoryConfig` carries `patterns: Vec<PatternEntry>` and optional `dual_threshold: Option<DualThreshold>`
- `PatternMeta.category` is `String` (owned, not `&'static str`)
- `build_all_patterns()` iterates categories generically — no `match` on category name
- `classify_internal()` drives dual-threshold from config, not hardcoded `"SYNTAX_FIX"`/`"FILE_READING"` string lookups
- LLM few-shot examples are generated dynamically from category config
- No `regex_defaults.rs` file — the embedded `config.toml` IS the default data
- Test helpers use programmatic config construction instead of env var sniffing
- `render.yaml` only lists secrets + `RUST_LOG` and `CONFIG_PATH`
- Config structs are in `src/config.rs`; their loader functions exist alongside them
- A user supplying a `CONFIG_PATH` with custom `[[categories]]` (different names, patterns, thresholds) gets a completely custom classifier with zero trace of built-in categories

## What We're NOT Doing

- NOT changing the TOML parsing approach (manual `toml::Value` extraction — no serde introduced)
- NOT changing how secrets are handled (API keys, auth tokens, `DATABASE_URL` stay as env vars)
- NOT moving `RUST_LOG` fully to config.toml (kept as runtime override)
- NOT removing `CONFIG_PATH` (meta-configuration must stay as env var)
- NOT refactoring the `merge_toml_values()` mechanism or compile-time embedding
- NOT adding new functionality beyond moving existing config channels
- NOT creating a `regex_defaults.rs` fallback file (embedded `config.toml` serves this role)
- NOT changing the scoring algorithm logic (only how thresholds and patterns are sourced)
- NOT moving `[[categories]].patterns` to a separate `patterns.toml` file

## Implementation Approach

**Phase 1–3**: Follow the existing config pattern: add structs → add loader functions → wire in main.rs → remove env reads → clean up tests. Each TOML value extracted from the generic tree with `get()`, `as_str()`, `as_integer()`, `as_array()`, etc., matching the established convention.

**Phase 4–6**: Split `intent_classifier.rs` into a pure engine and config-driven data. Extend `CategoryConfig` with `patterns` and optional `dual_threshold`, add `[[negative_patterns]]` loader, populate `config.toml` with all hardcoded data, then refactor `build_all_patterns()` and `classify_internal()` to be generic. Remove all hardcoded arrays, weight tables, and category name references. The embedded `config.toml` serves as the single source of default data — no separate Rust-level fallback file.

## Critical Implementation Details

**Phase 1–3 details:**

- **`routing_from_value()` default model resolution**: The function needs the default model to fill missing `model` fields on routing entries, but the default model comes from the `[DEFAULT]` entry — which is extracted as the fallback after all entries are processed. Resolve by peeking at the `[DEFAULT]` entry's model before removing it from the map, or by doing two passes.
- **`RUST_LOG` override ordering**: Init tracing with `EnvFilter::new(&server_config.log_level)` first, then check for `RUST_LOG` env override. If `RUST_LOG` is set, use it instead — matching the principle that runtime env overrides config file.
- **Live-reload readiness**: Config values that could benefit from runtime reload should be stored behind `Arc<RwLock<...>>` in `AppState`, matching the existing pattern for `keepalive_interval_secs` and `max_upstream_body_bytes`. This applies to `CorsConfig.allowed_origins` (CORS can be updated at runtime via a future reload mechanism). Startup-only values (`log_level`, `log_format`, `port`) can remain plain struct fields since they're consumed once at init time and can't be meaningfully changed without a restart.

**Phase 4–6 details:**

- **`PatternMeta.category` lifetime change cascades**: Changing from `&'static str` to `String` affects `build_all_patterns()` (return type), `classify_internal()` (reads `meta.category`), and test helpers that construct `PatternMeta` manually. All must be updated in the same pass.
- **TOML regex escaping**: The 44 regex patterns use `\b`, `\d`, `\w`, `\s`, `(?i)`, etc. TOML basic strings (`'...'`) don't interpret backslash escapes — correct for regex. But some patterns contain `'` (single quote, e.g., `doesn't`). Those must use TOML literal strings (`'''...'''`) or escape with `''`. Verify all patterns compile after copy.
- **Dual-threshold regression**: The current SYNTAX_FIX dual-threshold produces a specific boolean outcome. The config-driven replacement must produce identical results for the default config. The loop over `config.dual_threshold` iterates all categories — for categories without a dual_threshold, nothing changes. For the one category that has it (SYNTAX_FIX in defaults), the logic must match the old `sf_score >= 4 || (sf_score >= 3 && fr_score == 0)`.
- **`[[categories]]` in overlay config replaces via `merge_toml_values`**: Phase 1.4 established that `"categories"` is in the `override_keys` set — an overlay `[[categories]]` completely replaces the base. This means a user overlay must include ALL category entries (not partial). Verify this behavior is documented.
- **CASUAL fallback semantics**: `fallback_category()` currently returns `"CASUAL"` as the unconditional fallback. After de-hardcoding, it returns the **lowest-priority** category (highest `priority` value). With the default config, CASUAL has priority=4 (highest), so the fallback remains CASUAL. But for custom configs, the fallback is the category with the highest `priority` number. This is the correct generic behavior.

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

## Phase 4: Config Schema & Data Migration

### Overview

Extend `CategoryConfig` with patterns, weights, and dual-threshold fields. Add `NegativePatternConfig` struct and TOML loader. Populate `config.toml` with all 44 positive patterns, 4 negative patterns, and complete model costs. Change `PatternMeta.category` to `String`. Remove `hardcoded_model_costs()` so `build_model_costs()` starts from an empty map.

### Changes Required:

#### 4.1 Extend `CategoryConfig` and add supporting structs

**File**: `src/intent_classifier.rs`

**Intent**: `CategoryConfig` currently has only 4 fields (name, description, threshold, priority). Add `patterns: Vec<PatternEntry>` and `dual_threshold: Option<DualThreshold>`. Add `PatternEntry`, `DualThreshold`, and `NegativePatternConfig` structs. Change `PatternMeta.category` from `&'static str` to `String`.

**Contract**: New and modified structs:
- `CategoryConfig` gets `patterns: Vec<PatternEntry>` and `dual_threshold: Option<DualThreshold>`
- `PatternEntry { regex: String, weight: u8 }` — new
- `DualThreshold { alt_score: u32, suppress_if_present: String }` — new
- `NegativePatternConfig { regex: String, suppressed: String, penalty: u8 }` — new (replaces `NegativeMeta`)
- `PatternMeta.category` changes from `&'static str` to `String`

Keep `NegativeMeta` struct for now (removed in Phase 5).

#### 4.2 Extend `load_categories_from_value()` to parse patterns and dual_threshold

**File**: `src/config.rs`

**Intent**: Currently parses only name, description, threshold, priority from each `[[categories]]` entry. Extend to also parse `patterns` (array of inline tables `{ regex, weight }`) and optional `dual_threshold` (inline table `{ alt_score, suppress_if_present }`).

**Contract**: In `load_categories_from_value()` (line 648), after parsing the 4 existing fields, add:
- Parse `patterns`: if the key `"patterns"` exists, iterate its array of tables, extracting `"regex"` (required string) and `"weight"` (integer, default 1) for each
- Parse `dual_threshold`: if the key `"dual_threshold"` exists as a table, extract `"alt_score"` (integer) and `"suppress_if_present"` (string)
- Populate `CategoryConfig { patterns, dual_threshold, .. }`

If `patterns` is absent or empty, the category has an empty `Vec<PatternEntry>` — it won't match anything via regex (LLM-only category).

#### 4.3 Add `load_negative_patterns_from_value()`

**File**: `src/config.rs`

**Intent**: Parse the new `[[negative_patterns]]` TOML array into `Vec<NegativePatternConfig>`.

**Contract**: New function: `pub(crate) fn load_negative_patterns_from_value(root: &toml::Value) -> Vec<NegativePatternConfig>`. Reads the `"negative_patterns"` key from root, iterates its array of tables, extracts `"regex"` (string), `"suppressed"` (string), `"penalty"` (integer, default 2). Returns empty vec if section is absent.

#### 4.4 Extend `RegexClassifierConfig` with `short_prompt_len`

**File**: `src/config.rs`

**Intent**: `RegexClassifierConfig` currently has only `enabled: bool`. Add `short_prompt_len: usize` so the short-prompt threshold is configurable.

**Contract**: Add field `short_prompt_len: usize` to `RegexClassifierConfig` struct. Default: `30`. Update `load_regex_classifier_config_from_value()` to read `"short_prompt_len"` as integer from the `[regex_classifier]` table, defaulting to 30.

#### 4.5 Populate `config.toml` with all patterns, weights, and negative patterns

**File**: `config.toml`

**Intent**: Add all 44 positive regex patterns (with their weights), 4 negative suppression patterns, `dual_threshold` on SYNTAX_FIX, and `short_prompt_len` to the config.

**Contract**: 
- Each `[[categories]]` entry gets a `patterns` array of inline tables. Exact patterns and weights copied from the current `const` arrays in `intent_classifier.rs`:
  - FILE_READING: 12 patterns with weights [3,3,3,3,2,2,2,2,2,1,1,1] (from `FR_WEIGHTS`)
  - COMPLEX_REASONING: 16 patterns with weights [3,3,3,3,2,2,2,2,2,2,1,1,1,1,1,1] (from `CR_WEIGHTS`)
  - SYNTAX_FIX: 11 patterns with weights [3,3,3,2,2,2,2,2,1,1,1] (from `SF_WEIGHTS`) plus `dual_threshold = { alt_score = 4, suppress_if_present = "FILE_READING" }`
  - CASUAL: 5 patterns with weights [3,2,1,1,1] (from `CA_WEIGHTS`)
- Add `[[negative_patterns]]` section with 4 entries — regex, suppressed category name, and penalty copied from `NEGATIVE` + `NEGATIVE_META` arrays
- Add `short_prompt_len = 30` to `[regex_classifier]`
- The `[model_costs]` section already has the 4 vendor entries (line 106–110) — no change needed

**TOML escaping note**: Patterns containing single quotes (e.g., `doesn't`) must use TOML multi-line literal strings `'''...'''` or escape with `''`. All other regex escapes (`\b`, `\d`, etc.) work correctly in TOML basic strings since TOML doesn't interpret backslash sequences outside of double-quoted strings.

#### 4.6 Remove `hardcoded_model_costs()` and its call site

**Files**: `src/intent_classifier.rs`, `src/config.rs`

**Intent**: `hardcoded_model_costs()` seeds the cost map with 4 vendor entries. Since `config.toml` now has a complete `[model_costs]` section with the same entries, the hardcoded seed is redundant.

**Contract**: 
- Remove `hardcoded_model_costs()` function from `intent_classifier.rs` (lines 16–23)
- In `build_model_costs()` (`config.rs:711`): change `let mut costs = crate::intent_classifier::hardcoded_model_costs();` to `let mut costs = HashMap::new();`

### Success Criteria:

#### Automated Verification:

- `cargo build` compiles with new struct definitions and extended loaders
- `cargo test` — existing config tests pass (no regressions from struct changes)
- `config.toml` parses correctly: 4 categories each with patterns, 4 negative patterns, `short_prompt_len` loads

#### Manual Verification:

- Inspect `config.toml` — patterns match the current `const` arrays in intent_classifier.rs
- Verify TOML escaping: all regex patterns parse without errors (run the app, check for TOML parse warnings)

---

## Phase 5: Engine Refactor

### Overview

Make `build_all_patterns()` generic (iterate `CategoryConfig.patterns` instead of matching on name strings). Replace hardcoded dual-threshold in `classify_internal()` with a config-driven loop. Remove all CASUAL special cases. Generate LLM few-shot examples dynamically from category config. Remove all hardcoded pattern arrays, weight tables, `NEGATIVE_META`, `SHORT_PROMPT_LEN`, `NEG_COUNT`, and `hardcoded_categories()`. Wire `short_prompt_len` from config.

### Changes Required:

#### 5.1 Wire `RegexClassifier` construction with new fields

**File**: `src/main.rs`

**Intent**: Pass `regex_config.short_prompt_len` instead of the hardcoded `SHORT_PROMPT_LEN` const. Load `negative_patterns` from config and make them available (for Phase 5.3).

**Contract**: In `main()` at the classifier construction block (line 193–208):
- Replace `intent_classifier::SHORT_PROMPT_LEN` with `regex_config.short_prompt_len`
- Load negative patterns: `let negative_patterns = config::load_negative_patterns_from_value(&config_root);`
- The negative patterns are passed to `build_all_patterns()` — which is called inside `RegexClassifier::from_env()`. Update the `from_env()` signature (Phase 5.2).

#### 5.2 Update `RegexClassifier` construction signatures

**File**: `src/intent_classifier.rs`

**Intent**: `from_env()` and `from_values()` (test-only) currently call `build_all_patterns(&categories)` with no negative_patterns parameter. Update signatures to accept `&[NegativePatternConfig]` and pass it through.

**Contract**: 
- `from_env()` gets a new parameter: `negative_patterns: &[NegativePatternConfig]`
- `from_values()` (test-only) gets the same parameter
- Both pass `&categories` and `negative_patterns` to `build_all_patterns()`
- Update the call site in `main.rs` (line 195) to pass `&negative_patterns`

#### 5.3 Refactor `build_all_patterns()` to be generic

**File**: `src/intent_classifier.rs`

**Intent**: Replace the `match config.name.as_str()` arms with a generic loop over `CategoryConfig.patterns`. Accept `negative_patterns` as a parameter instead of the hardcoded `NEGATIVE` array.

**Contract**: New signature:
```rust
fn build_all_patterns(
    categories: &[CategoryConfig],
    negative_patterns: &[NegativePatternConfig],
) -> (Vec<String>, Vec<PatternMeta>, Range<usize>)
```

The function body:
1. Iterates each `CategoryConfig` → for each `PatternEntry` in `config.patterns`, pushes `entry.regex.clone()` and `PatternMeta { category: config.name.clone(), weight: entry.weight }`
2. After all positive patterns: records `negative_start`, then iterates `negative_patterns`, pushing each `neg.regex.clone()` with `PatternMeta { category: "NEG".to_string(), weight: 0 }`
3. Returns `(patterns, metadata, negative_start..patterns.len())`
4. No `match` on category name — entirely generic

The return type changes from `Vec<&'static str>` to `Vec<String>` because patterns are now cloned from config-driven data at runtime.

#### 5.4 Replace hardcoded dual-threshold with config-driven loop

**File**: `src/intent_classifier.rs`

**Intent**: Remove the hardcoded SYNTAX_FIX dual-threshold (lines 718–726) and replace with a generic loop that checks each category's `dual_threshold` config.

**Contract**: In `classify_internal()` (line 676), replace the hardcoded block:
```rust
let sf_score = *scores.get("SYNTAX_FIX").unwrap_or(&0);
let fr_score = *scores.get("FILE_READING").unwrap_or(&0);
let sf_met = sf_score >= 4 || (sf_score >= 3 && fr_score == 0);
if let Some(entry) = met.iter_mut().find(|(c, _)| c.name == "SYNTAX_FIX") {
    entry.1 = sf_met;
}
```

With a generic loop after the initial `met` calculation:
```rust
for (config, met_flag) in met.iter_mut() {
    if let Some(dt) = &config.dual_threshold {
        let score = *scores.get(config.name.as_str()).unwrap_or(&0);
        let suppress_score = *scores.get(dt.suppress_if_present.as_str()).unwrap_or(&0);
        *met_flag = score >= dt.alt_score || (score >= config.threshold && suppress_score == 0);
    }
}
```

#### 5.5 Update negative suppression to use `NegativeMeta` from config

**File**: `src/intent_classifier.rs`

**Intent**: The negative suppression loop (lines 689–699) indexes into the hardcoded `NEGATIVE_META` array. Replace with indexing into `self.negative_patterns` (a new field on `RegexClassifier`).

**Contract**: 
- Add `negative_patterns: Vec<NegativePatternConfig>` field to `RegexClassifier` struct
- Store the `negative_patterns` slice passed to `from_env()`/`from_values()`
- In the suppression loop: use `self.negative_patterns[neg_idx]` instead of `NEGATIVE_META[neg_idx]`
- The struct field is `Vec<NegativePatternConfig>` (owned clone) since `RegexClassifier` owns its data

#### 5.6 Remove CASUAL special cases

**File**: `src/intent_classifier.rs`

**Intent**: Three places reference `"CASUAL"` by hardcoded string. Make them generic.

**Contract**:
- `fallback_category()` (line 604): `unwrap_or("CASUAL")` on line 609 → change to `unwrap_or("unknown")`. The function already returns the lowest-priority category (highest `priority` value), which with the default config is CASUAL (priority=4). With an empty categories slice, `"unknown"` is a safe sentinel.
- `ClassificationResult::fallback()` (line 614): `category: "CASUAL".to_string()` on line 619 → already uses `DEFAULT_MODEL` for the model field. Change category to `DEFAULT_MODEL.to_string()` or keep a generic sentinel. **Alternative**: make `fallback()` accept a category name parameter so the caller provides the fallback category.
- `route_match()` (line 748): `if category != "CASUAL" && !self.routing.contains_key(category)` → remove the `category != "CASUAL"` check. Warn for ALL missing routing entries (including CASUAL). This is more consistent — if CASUAL has no routing entry, the warning is legitimate.

#### 5.7 Generate LLM few-shot examples dynamically

**File**: `src/intent_classifier.rs`

**Intent**: Replace the 4 hardcoded few-shot examples in `build_llm_classifier_prompt()` (lines 377–380) with examples generated from each category's `description` and first pattern.

**Contract**: Replace the hardcoded examples block:
```rust
prompt.push_str("\nReturn ONLY the category name, nothing else. Examples:\n");
prompt.push_str("- \"read the file src/main.rs\" -> FILE_READING\n");
// ... 3 more
```
With a loop that generates one example per category:
```rust
prompt.push_str("\nReturn ONLY the category name, nothing else. Examples:\n");
for cat in categories {
    let example_hint = cat.description.split(',').next().unwrap_or(&cat.description);
    prompt.push_str(&format!("- \"{}\" -> {}\n", example_hint, cat.name));
}
```
This adapts to any category set — if a user adds a "DATABASE" category with description "Database queries, schema migrations, SQL optimization", the prompt lists it.

#### 5.8 Remove all hardcoded data arrays and functions

**File**: `src/intent_classifier.rs`

**Intent**: After the engine refactor, these items are dead code. Remove them.

**Contract**: Delete the following items (line numbers pre-refactor):
- `FILE_READING` const (lines 416–429)
- `COMPLEX_REASONING` const (lines 431–448)
- `SYNTAX_FIX` const (lines 450–462)
- `CASUAL` const (lines 464–470)
- `NEGATIVE` const (lines 472–477)
- `NEGATIVE_META` const (lines 481–498)
- `FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS` (lines 405–408)
- `SHORT_PROMPT_LEN` const (line 412)
- `NEG_COUNT` const (line 401)
- `NegativeMeta` struct (lines 392–395) — replaced by `NegativePatternConfig`
- `hardcoded_categories()` function (lines 48–75)
- `#[allow(unused_imports)]` re-export block (lines 10–13) — remove `DEFAULT_MODEL`/`DEFAULT_MODEL_COMPLEX` re-exports if only used in removed code

### Success Criteria:

#### Automated Verification:

- `cargo build --release` compiles without errors
- `cargo test` — all tests pass with config-driven classifier
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass
- No remaining `match config.name.as_str()` in `build_all_patterns()`
- No hardcoded category name string references in engine code (`"FILE_READING"`, `"SYNTAX_FIX"`, `"COMPLEX_REASONING"`, `"CASUAL"`)
- `grep -r "SYNTAX_FIX\|FILE_READING\|COMPLEX_REASONING\|CASUAL" src/intent_classifier.rs` returns only `config.toml` file references or test-only occurrences

#### Manual Verification:

- Start the app with default `config.toml` — classification output matches pre-refactor behavior
- Create a custom `CONFIG_PATH` with different category names — verify the classifier uses them
- Verify `config.toml` patterns parse correctly by checking app startup logs (no regex compilation errors)

---

## Phase 6: Tests & Cleanup

### Overview

Update test helpers to use config-driven category construction instead of `hardcoded_categories()`. Remove or update tests that assert specific category names. Add engine-generality tests with custom categories. Remove dead test code. Verify full test suite passes.

### Changes Required:

#### 6.1 Update `test_classifier()` helper

**File**: `src/intent_classifier.rs`

**Intent**: `test_classifier()` (lines 780–831) currently calls `hardcoded_categories()` and constructs routing entries keyed by positions (cats[0], cats[1], etc.). Replace with inline category construction that includes patterns.

**Contract**: Build test categories inline:
```rust
fn test_categories() -> Vec<CategoryConfig> {
    // Build minimal test categories with known patterns
    // ... returns categories that exercise the engine generically
}
```
Or: read categories from a test TOML string using `load_categories_from_value()`. The key requirement: categories are constructed without calling `hardcoded_categories()`.

#### 6.2 Update test assertions that reference specific category names

**File**: `src/intent_classifier.rs`

**Intent**: Several tests assert on category names like `"CASUAL"`, `"FILE_READING"`, etc. Update to use the lowest-priority category name or the category at a known index.

**Contract**: Key test updates:
- `test_empty_prompt_falls_back_to_lowest_priority` (line 867): `== "CASUAL"` → `== lowest_priority_category.name`
- `test_ambiguous_two_categories_met_falls_back` (line 876): same pattern
- `test_syntax_fix_dual_threshold` (if it exists): verify against a category with `dual_threshold` set, not by hardcoded name
- Route tests in `main.rs`: `cats[3].name` is CASUAL (priority=4, last) — update to use `find_lowest_priority_category()` helper

#### 6.3 Add engine-generality tests

**File**: `src/intent_classifier.rs`

**Intent**: Verify the engine works with completely custom categories — different names, patterns, thresholds, dual_thresholds. This proves the engine is truly transparent.

**Contract**: Add tests:
- `test_engine_works_with_custom_categories`: Create 2 categories with known patterns, classify prompts, verify correct routing
- `test_engine_works_with_custom_dual_threshold`: Create a category with `dual_threshold`, verify the alt_score and suppress logic works
- `test_engine_works_with_no_categories`: Verify empty categories produce a graceful fallback
- `test_engine_works_with_custom_negative_patterns`: Verify negative suppression works with config-driven patterns

#### 6.4 Remove dead test code

**Files**: `src/intent_classifier.rs`, `src/main.rs`, `src/config.rs`

**Intent**: Remove test helpers and imports that reference removed functions/consts.

**Contract**: Check for and remove:
- Any test calling `hardcoded_categories()` directly (should use inline or TOML-based construction)
- Any test referencing `SHORT_PROMPT_LEN`, `NEG_COUNT`, weight arrays
- The `#[allow(unused_imports)]` re-export block if emptied
- `default_auth_providers()` in test module (line 1023) — may still be needed for LLM classifier tests, keep if used

#### 6.5 Run full verification suite

**Files**: N/A (commands only)

**Intent**: Confirm no regressions across the entire test suite.

**Contract**: Run:
- `cargo test` — all fast unit/integration tests
- `cargo test auth` — auth tests
- `cargo test routes_auth` — route auth tests
- `cargo build --release` — production build
- `grep -rn 'SYNTAX_FIX\|FILE_READING\|COMPLEX_REASONING\|CASUAL' src/` — verify engine code has zero hardcoded category names (config.toml and config.rs loaders are fine; test data may use them for readability)

### Success Criteria:

#### Automated Verification:

- `cargo test` — all tests pass
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass
- `cargo build --release` builds cleanly
- No `hardcoded_categories()` calls in test or production code
- No `SHORT_PROMPT_LEN`, `FR_WEIGHTS`, `CR_WEIGHTS`, `SF_WEIGHTS`, `CA_WEIGHTS`, `NEG_COUNT` references anywhere
- `grep 'pattern' src/intent_classifier.rs` on engine code (non-comment, non-test) shows zero hardcoded category name strings

#### Manual Verification:

- Run `RUST_LOG=info cargo run` with only secrets as env vars — verify startup logs show "Regex classifier initialized"
- Verify classification endpoint `/v1/classify` returns correct category for test prompts
- Create a temporary `custom.toml` overlay with different category names and patterns — verify the classifier uses them
- Verify the dashboard still shows classification data correctly



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

---

## Testing Strategy

### Unit Tests:

**Phase 1–3:**
- `load_server_config_from_value` returns log_level/log_format defaults when absent
- `load_cors_config_from_value` returns empty origins when section absent, parses array when present
- `routing_from_value` extracts `[DEFAULT]` as fallback when present
- `hardcoded_routing()` uses const defaults correctly (no env vars)

**Phase 4–6:**
- `load_categories_from_value` parses patterns, weights, and dual_threshold from TOML
- `load_categories_from_value` handles missing patterns (empty vec) and missing dual_threshold (None)
- `load_negative_patterns_from_value` parses `[[negative_patterns]]` array correctly
- `load_negative_patterns_from_value` returns empty vec when section absent
- `build_all_patterns` produces correct output order (positive → negative) from `CategoryConfig.patterns`
- `build_all_patterns` handles empty categories and empty negative patterns
- `classify_internal` dual-threshold produces same results as old hardcoded logic for default config
- `classify_internal` works with NO dual_threshold on any category
- `classify_internal` works with multiple categories having dual_threshold
- `build_llm_classifier_prompt` generates examples dynamically for any category set
- `fallback_category` returns lowest-priority category for non-empty slice

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
7. Verify classification with test prompts matches expected categories (e.g., "read the file" → FILE_READING)
8. Create a custom `custom.toml` with `CONFIG_PATH` — verify custom categories override defaults
9. Verify dashboard classification data is correct

## Performance Considerations

Phase 1–3: None — configuration refactor, no runtime performance impact.

Phase 4–6: Minor — `PatternMeta.category` changes from `&'static str` to `String`, adding one allocation per pattern (~48 patterns total). The `RegexSet` compilation (the expensive step) is unchanged. Pattern data is cloned once at startup during `from_env()` — no runtime allocation per classification request.

## Migration Notes

**Phase 1–3:**
- Existing deployments that set `LOG_FORMAT`, `ALLOWED_ORIGINS`, or `PORT` env vars must migrate those values to `config.toml`
- Existing deployments relying on `DEFAULT_MODEL` or `NVIDIA_ENDPOINT` env vars must set the default model/endpoint in the `[DEFAULT]` routing section of config.toml (currently `[FALLBACK]`, renamed by this plan)
- Render deployment: `PORT` is injected automatically by Render but the app will now use `[server].port` from config.toml. For Render, the embedded default `10000` is correct (Render's health check is at `/health` on the service port — Render routes external traffic to the container port)
- `ROUTING_CONFIG_PATH` is removed — any deployment still using it should switch to `CONFIG_PATH`
- `NVIDIA_NIM_API_KEY` and `OPENROUTER_API_KEY` in render.yaml were never read by code — removed as drift

**Phase 4–6:**
- Users with a custom `CONFIG_PATH` overlay that overrides `[[categories]]` MUST include `patterns` arrays — categories without patterns won't match via regex (LLM classifier still works)
- The `[[categories]]` overlay completely replaces the base (established in Phase 1.4) — partial category overrides are not supported
- Category names are still a public API contract — custom configs that change category names must also update routing entries and downstream consumers
- The embedded `config.toml` grows from ~140 to ~350 lines with inline pattern tables

## References

- Current config loading: `src/main.rs:49-178`
- Config struct definitions: `src/config.rs:209-824`
- `env_or_default` helper: `src/config.rs:13-15`
- `hardcoded_routing()`: `src/config.rs:385-412`
- `routing_from_value()`: `src/config.rs:507-563`
- CORS env read: `src/main.rs:803-808`
- Port env read: `src/main.rs:279-282`
- Classifier research: `context/changes/move-all-config-to-file/research-regex-split.md`
- Existing pattern arrays: `src/intent_classifier.rs:416-498`
- `build_all_patterns()`: `src/intent_classifier.rs:539-602`
- `classify_internal()`: `src/intent_classifier.rs:676-746`

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

- [x] 2.5 App starts without LOG_FORMAT env, uses config.toml log_format
- [x] 2.6 RUST_LOG=debug overrides config.toml log_level
- [x] 2.7 CORS headers match config.toml [cors].allowed_origins
- [x] 2.8 App binds to config.toml [server].port

### Phase 3: Clean Up Tests, render.yaml, and Dead Code

#### Automated

- [x] 3.1 `cargo test` — all tests pass — f3f96de
- [x] 3.2 `cargo test auth` — auth tests pass — f3f96de
- [x] 3.3 `cargo test routes_auth` — route auth tests pass — f3f96de
- [x] 3.4 `cargo build --release` builds cleanly — f3f96de
- [x] 3.5 No remaining `ROUTING_CONFIG_PATH` references — f3f96de

#### Manual

- [x] 3.6 render.yaml only lists required secrets + optional RUST_LOG
- [x] 3.7 App starts successfully with only secrets as env vars
- [x] 3.8 RUST_LOG=debug cargo run produces debug-level logs

### Phase 4: Config Schema & Data Migration

#### Automated

- [x] 4.1 `cargo build` compiles with new struct definitions and extended loaders — bf4fc16
- [x] 4.2 `cargo test` — existing config tests pass — bf4fc16

#### Manual

- [x] 4.3 `config.toml` patterns match current const arrays in intent_classifier.rs
- [x] 4.4 TOML escaping verified — all regex patterns parse without errors

### Phase 5: Engine Refactor

#### Automated

- [x] 5.1 `cargo build --release` compiles without errors — 13c72c2
- [x] 5.2 `cargo test` — all tests pass — 13c72c2
- [x] 5.3 `cargo test auth` — auth tests pass — 13c72c2
- [x] 5.4 `cargo test routes_auth` — route auth tests pass — 13c72c2
- [x] 5.5 Zero hardcoded category name strings in engine code — 13c72c2
 
#### Manual

- [x] 5.6 Default classification behavior matches pre-refactor output — 9fdb903
- [x] 5.7 Custom `CONFIG_PATH` overlay with different categories works — 9fdb903

### Phase 6: Tests & Cleanup

#### Automated

- [x] 6.1 `cargo test` — all tests pass — b947278
- [x] 6.2 `cargo test auth` — auth tests pass — b947278
- [x] 6.3 `cargo test routes_auth` — route auth tests pass — b947278
- [x] 6.4 `cargo build --release` builds cleanly — b947278
- [x] 6.5 No `hardcoded_categories()` calls anywhere — b947278
- [x] 6.6 No `SHORT_PROMPT_LEN`, `FR_WEIGHTS`, etc. references — b947278

#### Manual

- [x] 6.7 App starts with only secrets as env vars, classifier initializes — 9fdb903
- [x] 6.8 `/v1/classify` returns correct category for test prompts — 9fdb903
- [x] 6.9 Custom `CONFIG_PATH` overlay classification verified — 9fdb903
