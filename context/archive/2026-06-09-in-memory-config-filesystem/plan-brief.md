# In-Memory Config Filesystem — Plan Brief

> Full plan: `context/changes/in-memory-config-filesystem/plan.md`
> Research: `context/changes/in-memory-config-filesystem/research.md`

## What & Why

Cerebrum has ~65 hardcoded config values across 7 source files, with ~65% having no runtime override path. The app falls back to hardcoded defaults whenever `CONFIG_PATH` is unset. We embed `config.toml` at compile time via `include_str!()` so the binary is always self-sufficient, eliminating all hardcoded fallbacks from the normal startup path. `Arc<RwLock<T>>` wrappers on `AppState` fields prepare for dashboard-driven runtime reload in a follow-up change.

## Starting Point

The config loading chain (`src/main.rs:76-207`) is a ladder: `CONFIG_PATH` file → hardcoded fallbacks. Loader functions in `config.rs` are pure `&toml::Value → Config` — already reusable. The `LLMClassifier` (`src/intent_classifier.rs:180`) demonstrates a proven `Arc<RwLock<String>>` pattern for runtime-updatable config. The existing `config.toml` (42 lines) is a partial documentation template, not a functional default.

## Desired End State

The binary ships with a ~200-line embedded `config.toml` as the single source of truth for all non-secret configuration. `CONFIG_PATH` becomes an optional overlay that merges on top of the embedded default — users only specify what they want to override. Secrets stay in env vars. Mutable `AppState` fields (`routing`, `model_costs`, `baseline_model`, `classify_db_log`, `max_upstream_body_bytes`, `keepalive_interval_secs`) are wrapped in `Arc<RwLock<T>>` or `Arc<AtomicBool>`. Auth provider mappings, dashboard page defaults, server/http/database settings are all TOML-driven. Hardcoded fallback functions are retained only as a panic-safe last resort.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Scope | Full startup config + RwLock prep; dashboard reload endpoints deferred | Eliminates all hardcoded values and prepares state for reload without the complexity of UI and classifier swap | Plan |
| Field strategy | `Arc<RwLock>` per mutable field, not a single ConfigSnapshot lock | Avoids coarse lock contention — follows the proven `LLMClassifier.api_key` pattern exactly | Plan |
| Embedded config | Single `config.toml` via `include_str!()`, no named presets | Simplest approach; named presets add binary size and marginal benefit | Plan |
| Pattern groups | Defer to follow-up | Largest hardcoded block (48 patterns), but TOML schema design for regex patterns is a significant standalone task | Plan |
| Fallback behavior | Keep hardcoded functions as last-resort escape hatch | If embedded TOML fails to parse (CI catches), the app still starts with hardcoded values | Plan |
| `model_env_var` | Remove from `CategoryConfig` | Routing blocks in TOML make the env var indirection redundant; cleaner separation of concerns | Plan |
| Auth provider mappings | Include now — data-driven from `[[auth_provider]]` TOML | Small, self-contained change that eliminates a hardcoded match statement | Plan |
| Dashboard defaults | Include now — `[dashboard]` TOML section | Eliminates 5 hardcoded numbers; dashboard handlers already read from AppState | Plan |
| `CONFIG_PATH` behavior | Merges on top of embedded config (not full replacement) | Users write minimal configs — only the sections they want to override; everything else falls through to embedded defaults | Plan |
| Testing approach | Targeted unit + integration tests for config loading | New loader functions tested; existing route/auth tests verify no regression — no RwLock contention tests needed yet (no writers) | Plan |

## Scope

**In scope:**
- Restructure `config.toml` with all sections (~200 lines)
- `DashboardConfig`, `AuthProviderConfig`, `ServerConfig`, `HttpConfig`, `DatabaseConfig` structs and loaders
- Remove `model_env_var` from `CategoryConfig`
- `Arc<RwLock<T>>` on 6 `AppState` fields, `Arc<AtomicBool>` on 1
- `include_str!("../config.toml")` as embedded default
- `CONFIG_PATH` as optional merge overlay (user config merges on top of embedded)
- Data-driven `auth_headers_for()` replacing hardcoded match
- Dashboard reads from `DashboardConfig` instead of hardcoded numbers
- Update all test helpers for new AppState shape

**Out of scope:**
- Dashboard `POST` endpoints for config reload/switch
- Pattern groups in TOML (regex patterns stay as const arrays)
- File watching (`notify` crate)

- Dashboard config page UI
- Renaming category names

## Architecture / Approach

Bottom-up by dependency order: config.toml → loader structs → AppState → startup wiring → tests. Each phase is independently testable. The embedded TOML is parsed once at startup into config structs, which are then wrapped in `Arc<RwLock<T>>` and stored in `AppState`. All handler read sites use `.read().await` (or `.load(Relaxed)` for `AtomicBool`). No writer exists yet, so lock contention is zero.

**Key flow change:**
```
Before: CONFIG_PATH? → valid → use | invalid/missing → hardcoded_fallbacks
After:  embedded_TOML (base) → CONFIG_PATH overlay? → merge_recursive(base, overlay) | use embedded alone
        → if both fail → hardcoded (last resort, CI-caught)
```

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. config.toml restructure | Complete, self-sufficient ~200-line embedded default | Config format design — categories no longer have `model_env_var`; all routing blocks must be valid |
| 2. Config structs & loaders | `DashboardConfig`, `AuthProviderConfig`, `ServerConfig`, `HttpConfig`, `DatabaseConfig` + loaders + updated `auth_headers_for` | Loader defaults must match current hardcoded values exactly |
| 3. AppState refactor | `Arc<RwLock<T>>` on 6 fields, `Arc<AtomicBool>` on 1, new `dashboard_config` + `auth_providers` fields | Every read site must be updated — missed sites cause compile errors (safe) |
| 4. main.rs startup | Embedded config, CONFIG_PATH merge overlay, clean env var reads | Tracing subscriber init happens before TOML parsing — log level precedence must be correct |
| 5. persistence.rs cleanup | `DatabaseConfig` struct drives pool/retry settings; `HttpClientConfig` removed | DB connection behavior must be unchanged — `DATABASE_URL` still from env |
| 6. Tests | All test helpers updated; all test suites pass | Test helpers that construct `AppState` are scattered — ~6 construction sites |

**Prerequisites:** `config.toml` exists in repo root (it does). No new crates needed — `tokio::sync::RwLock`, `std::sync::atomic::AtomicBool`, and `toml` are already in `Cargo.toml`.
**Estimated effort:** ~3-4 sessions across 6 phases. Phase 1 is standalone. Phases 2-4 are the core work. Phases 5-6 are cleanup and test fixes.

## Open Risks & Assumptions

- **`serde` not a direct dependency**: All TOML parsing is manual via `toml::Value` getters — no `#[derive(Deserialize)]`. This keeps the existing pattern but is more verbose for new structs like `HttpConfig` with 6 fields.
- **Test helper proliferation**: ~6 places construct `AppState` (`test_app`, `make_test_app_state`, `test_app_with_classifier`, `test_app_with_enriched_classifier`, `test_app_with_http_client`, slow test helpers). Updating all of them is mechanical but error-prone — missing one breaks that test.
- **`RUST_LOG` vs `log_level` precedence**: Tracing subscriber init runs before TOML parsing. If `RUST_LOG` is set, should it override config.toml's `log_level`? The plan keeps `RUST_LOG` as the primary (it's the standard mechanism), with TOML as the default when `RUST_LOG` is unset.
- **`LOG_FORMAT` from env vs TOML**: Same precedence question. The plan keeps the existing env var check at startup for the tracing subscriber, with TOML as fallback.
- **Existing `CONFIG_PATH` users**: With merge semantics, existing configs at `CONFIG_PATH` work as partial overrides — no forced migration. However, `model_env_var` removal is breaking for `[[categories]]` entries; users should replace them with routing blocks (`[FILE_READING]`, etc.).

## Success Criteria (Summary)

- App starts and serves requests with no env vars except secrets (auth tokens, DATABASE_URL)
- `CONFIG_PATH` overlay works: custom TOML merges on top of embedded config, overriding only specified sections
- All existing tests pass (`cargo test`, `cargo test auth`, `cargo test routes_auth`, `cargo test slow_tests`)
- `cargo build --release` succeeds
- No hardcoded defaults used in normal startup path
