# Move All Config to File — Plan Brief

> Full plan: `context/changes/move-all-config-to-file/plan.md`

## What & Why

Move all non-secret env var reads into `config.toml`, the single source of truth for operational configuration. The codebase already embeds `config.toml` at compile time and loads most settings from it, but several env vars (`LOG_FORMAT`, `ALLOWED_ORIGINS`, `PORT`, `DEFAULT_MODEL`, `NVIDIA_ENDPOINT`) still bypass the config file. After this change, only secrets (API keys, auth tokens, `DATABASE_URL`) and meta-config (`CONFIG_PATH`, `RUST_LOG` as runtime override) remain as env vars.

## Starting Point

`config.toml` is embedded at compile time, parsed into a generic `toml::Value`, merged with an optional `CONFIG_PATH` overlay, and individual sections are extracted by loader functions (`load_server_config_from_value`, `load_http_config_from_value`, etc.) into typed structs in `src/config.rs`. The pattern is well-established — the work is extending it to cover the remaining env vars that duplicate or bypass it.

## Desired End State

- All non-secret settings sourced from `config.toml` (with `CONFIG_PATH` overlay)
- `RUST_LOG` env var kept as runtime override for `log_level` from config (standard Rust pattern)
- `CONFIG_PATH` kept as the only non-secret env var (meta-configuration)
- `render.yaml` only lists secrets + optional `RUST_LOG`
- `env_or_default()` helper and `ROUTING_CONFIG_PATH` removed as dead code
- Tests use programmatic config instead of env var sniffing

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| RUST_LOG handling | Keep as env override of config.toml `log_level` | Standard Rust ecosystem pattern; enables per-module debug at runtime without config changes. | Plan |
| CONFIG_PATH | Keep as env var | Meta-configuration — the path to config file can't be inside the config file itself. | Plan |
| DEFAULT_MODEL / NVIDIA_ENDPOINT | Rename `[FALLBACK]` → `[DEFAULT]` in config.toml, use as default routing spec | `[FALLBACK]` already has model+endpoint+provider+api_key_env — the full routing context. Rename and use it instead of scattered env vars. | Plan |
| ROUTING_CONFIG_PATH | Remove entirely | Only used in test-only `load_routing()` — dead code in production; render.yaml has it wrong too. | Plan |
| Port override | Remove PORT env read | `config.toml` is the source; Render's PORT injection is a platform concern handled by config overlay. | Plan |
| CORS origins format | TOML array of strings | Natural TOML format; config.toml already has `[cors] allowed_origins = []`. | Plan |
| Config merge strategy | Routing/classifier sections get complete override; server/http/db/cors get field-level merge | Routing entries and classifier configs are complete specs — partial merge would silently combine base+overlay incorrectly. Operational config benefits from partial overrides. | Plan |
| Test config overrides | Programmatic struct construction | Tests already build AppState manually; env vars for test config pollute the global namespace. | Plan |

## Scope

**In scope:**
- Expand `ServerConfig` with `log_level`, `log_format`
- Create `CorsConfig` + loader (TOML array of origins)
- Rename `[FALLBACK]` → `[DEFAULT]` in config.toml; use as default model/endpoint source
- Replace `LOG_FORMAT`/`ALLOWED_ORIGINS`/`PORT`/`DEFAULT_MODEL`/`NVIDIA_ENDPOINT` env reads with config values
- Remove `ROUTING_CONFIG_PATH` and `env_or_default()` helper
- Clean up test env var usage → programmatic config
- Update `render.yaml` and `AGENTS.md`

**Out of scope:**
- Serde-based TOML deserialization (keeping manual `toml::Value` extraction)
- Moving secrets to config file
- Removing `CONFIG_PATH` env var
- Removing `RUST_LOG` env override

## Architecture / Approach

Follow the existing pattern for every change: define struct → write loader function using `toml::Value` getters → call loader in `main()` → use struct instead of env var.

Key data flow change: settings that were read at point-of-use via `std::env::var()` or `env_or_default()` are now loaded once in `main()`, stored in a typed struct, and passed to consumers as parameters.

```
Before:  env var → std::env::var() at point of use
After:   config.toml → toml::Value → loader → typed struct → passed as param
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Expand config structs & config.toml | `ServerConfig` gains `log_level`/`log_format`; new `CorsConfig` struct + loader; `[FALLBACK]` renamed to `[DEFAULT]`; merge supports per-key override for routing entries | Struct field defaults must match current behavior |
| 2. Wire up config.toml in main.rs | `LOG_FORMAT`, `ALLOWED_ORIGINS`, `PORT` env reads replaced; `RUST_LOG` override wired; `DEFAULT_MODEL`/`NVIDIA_ENDPOINT` sourced from config | Silent behavior change if config.toml defaults differ from env defaults |
| 3. Clean up tests, render.yaml, dead code | `env_or_default()` removed; `ROUTING_CONFIG_PATH` removed; tests use programmatic config; render.yaml cleaned | Test regressions from env var removal |

**Prerequisites:** None (self-contained refactor)
**Estimated effort:** ~1 session, 3 phases

## Open Risks & Assumptions

- Render injects `PORT` automatically but the app will now use `[server].port` (default `10000`) — Render routes external traffic to the container's `PORT`, so the process must bind to the Render-assigned port. Mitigation: embed `PORT` fallback logic for Render or document CONFIG_PATH overlay usage.
- Removing `ALLOWED_ORIGINS` env var breaks any deployment that sets it but hasn't updated config.toml. This is intentional — the config.toml becomes the single source.
- `routing_from_value()` needs the default model to fill missing fields on per-category entries, but the default model comes from the `[DEFAULT]` entry which is extracted after all entries are processed. Requires a peek-ahead or two-pass approach.
- Runtime-mutable config values (`allowed_origins`) stored behind `Arc<RwLock<...>>` matching existing keepalive/max_body pattern — ready for future live config reload. Startup-only values (`log_level`, `port`) stay as plain fields.

## Success Criteria (Summary)

- App starts and operates correctly with only secrets as env vars
- `RUST_LOG` env still works as a log-level override
- All existing tests pass
- `render.yaml` only lists secrets + optional `RUST_LOG`
