# Three-Tier Persistence — Plan Brief

> Full plan: `context/changes/in-memory-db-fallback/plan.md`
> Research: `context/changes/in-memory-db-fallback/research.md`

## What & Why

Replace the hard dependency on PostgreSQL with config-driven backend selection across three tiers: memory (no deps, for demos/tests), SQLite file (survives restarts, for local dev/CI), and PostgreSQL (production). A `PersistenceBackend` trait unifies all three, so dashboard routes and proxy handlers work without knowing which backend is active.

## Starting Point

- `PersistenceConfig` holds `Arc<PgPool>` and implements query methods directly (`src/persistence.rs:70`)
- `main.rs` creates `persistence: Option<PersistenceConfig>` — graceful `None` when `DATABASE_URL` is absent
- All tests skip when `DATABASE_URL` isn't set (the `SKIP: DATABASE_URL not set` pattern in `src/persistence.rs:768-774`)
- Config loading already follows a consistent `load_*_from_value(&toml::Value)` pattern (`src/config.rs`)
- `async-trait` crate is already a dependency (`Cargo.toml:21`)

## Desired End State

Set `persistence.backend = "memory"` in config.toml and run `cargo run` — dashboard loads, DB connected, no external dependencies. Set `DATABASE_URL` and it auto-selects Postgres with full production fidelity. Tests run everywhere, always, with an in-memory backend that needs no setup.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| Architecture | Trait + enum dispatch (matching `ClassifierChain` pattern) | Codebase already uses `Arc<dyn IntentClassify>` — this is the same pattern. | Research |
| p99 on non-PG backends | Compute in Rust (sort durations, pick 99th percentile index) | Full feature parity across all backends; dataset sizes are small in dev/test. | Plan |
| Backend config source | Config.toml `[persistence]` section only | No `DB_BACKEND` env var; consistent with existing config-file patterns. | Plan |
| SQLite file path | `./cerebrum.db` (current working directory) | Simplest default; path is overridable via `persistence.sqlite_path`. | Plan |
| Memory backend availability | Runtime-selectable in production | Enables zero-dependency demos; `DISABLE_PERSISTENCE=true` still available for explicit opt-out. | Plan |
| SQLite schema management | Hardcoded `CREATE TABLE IF NOT EXISTS` const | Single table, no migration directory needed; schema won't diverge from PG at the same rate. | Research |
| `DATABASE_URL` env var | Always forces Postgres (sole env exception) | Preserves the 12-factor convention for production deploys. | Research |
| `sqlx::Any` / `AnyPool` | Not used | Does not translate `$1` → `?` binds — useless without full query rewrite. | Research |

## Scope

**In scope:**
- `PersistenceBackend` trait with `insert_inference`, `fetch_inferences`, `fetch_latency_summary`, `fetch_savings_estimate`
- `MemoryBackend` (Vec+RwLock, in-Rust p99)
- `SqliteBackend` (sqlx::SqlitePool, SQLite-compatible SQL, in-Rust p99)
- `PostgresBackend` (existing code moved into trait impl, unchanged logic)
- `DbBackend` enum with dispatch
- `[persistence]` config section with `load_persistence_config_from_value()`
- `cargo test` runs all persistence tests without `DATABASE_URL`
- Existing PG tests preserved behind `DATABASE_URL` guard

**Out of scope:**
- `DB_BACKEND` env var (config.toml only)
- SQLite migration directory
- `sqlx::Any` connection pool
- p99 via SQL on SQLite
- ORM or query builder

## Architecture

```
                    ┌────────────────────────────┐
                    │      AppState               │
                    │  persistence: Option<       │
                    │    PersistenceConfig>        │
                    │  { backend: Arc<DbBackend>, │
                    │    task_semaphore }          │
                    └──────────┬─────────────────┘
                               │
                    ┌──────────▼─────────────────┐
                    │    PersistenceBackend        │
                    │ (trait: Send + Sync)         │
                    │  insert_inference()          │
                    │  fetch_inferences()          │
                    │  fetch_latency_summary()     │
                    │  fetch_savings_estimate()    │
                    └──────────┬─────────────────┘
                               │
            ┌──────────────────┼──────────────────┐
            │                  │                  │
   ┌────────▼──────┐  ┌───────▼──────┐  ┌───────▼──────┐
   │ MemoryBackend │  │SqliteBackend │  │ PostgresBackend│
   │ Vec<RwLock>   │  │ SqlitePool   │  │    PgPool     │
   │ in-Rust p99   │  │ in-Rust p99  │  │ PERCENTILE_CONT│
   └───────────────┘  └──────────────┘  └───────────────┘
```

**Data flow**: Proxy handler → `log_classification` → `log_inference(backend, semaphore, record)` → background task → `backend.insert_inference()`. Dashboard routes → `backend.fetch_*()` → template render.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Foundation | Trait, enum, config section, stubs. All compiles. | Trait method signatures must match existing call sites exactly. |
| 2. Memory Backend | Full Vec+RwLock impl with in-Rust p99. | p99 index math (off-by-one) — validate against PG PERCENTILE_CONT output. |
| 3. SQLite Backend | Full SqlitePool impl with SQLite-compatible SQL. | `?` bind param positional matching against dynamic WHERE builder. |
| 4. Postgres Refactor | Move existing code into PostgresBackend trait impl. | Regression risk — must produce identical query results to current code. |
| 5. Main.rs Integration | Backend selection, wire AppState, update call sites. | Breaking dashboard routes if `PersistenceConfig` API changes unexpectedly. |
| 6. Test Infrastructure | In-memory test helper, convert tests, new backend tests. | Test rewriting is mechanical but high-volume — ~30 test functions. |
| 7. Verification | Full suite pass, manual dashboard check. | PG-specific test breakage from refactoring. |

**Prerequisites:** Rust toolchain, no external services needed for phases 1-6.

**Estimated effort:** ~380 new lines of code, ~200 lines of test changes. ~2-3 sessions.

## Open Risks & Assumptions

- **SQLite `?` bind ordering** — sqlx binds params sequentially to SQLite's `?` placeholders. The dynamic WHERE builder must bind in the exact order placeholders appear. Tested by running queries, but off-by-one is easy to miss.
- **p99 accuracy vs PG** — Rust-side p99 with small sample sizes may differ slightly from PG's `PERCENTILE_CONT` due to interpolation differences. Acceptable for dev/test.
- **`task_semaphore` remains on `PersistenceConfig`** — if `Option<PersistenceConfig>` is `None`, there's no semaphore. The `DISABLE_PERSISTENCE` path (explicit opt-out) already skips logging — no change in behavior.
- **SQLite shared-cache** — each pool connection to `:memory:` gets a separate DB. Shared-cache URI + `min_connections(1)` is the documented workaround. Tested in Phase 6.

## Success Criteria (Summary)

- `cargo run` with no env vars → dashboard loads, shows memory backend, data visible
- `cargo test` runs all persistence tests without `DATABASE_URL` — zero SKIP messages
- Postgres path unchanged: set `DATABASE_URL`, migrations run, all existing behavior preserved
