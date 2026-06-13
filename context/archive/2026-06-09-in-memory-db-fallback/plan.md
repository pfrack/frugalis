# Three-Tier Persistence Backend Implementation Plan

## Overview

Implement config-driven persistence backend selection (memory / sqlite / postgres) via a `PersistenceBackend` trait with three implementations, replacing the current `Option<PersistenceConfig>` with a `DbBackend` enum. Enables running cerebrum with no external database dependency for demos and testing.

## Current State Analysis

- `src/persistence.rs:70` — `PersistenceConfig` holds `Arc<PgPool>`, all query methods are on `PersistenceConfig`
- `src/persistence.rs:266-267` — `fetch_latency_summary` uses PG-only `PERCENTILE_CONT` + `::INTEGER` casts
- `src/persistence.rs:270` — `fetch_savings_estimate` uses PG-only `NOW() - interval '1 hour' * $1`
- `src/persistence.rs:470` — `log_inference` takes `Arc<PgPool>` directly, spawned as fire-and-forget
- `src/main.rs:33` — `persistence: Option<PersistenceConfig>` gracefully degrades to `None` when no DB
- `src/main.rs:284` — `log_classification` clones `persistence.pool` and `task_semaphore`
- `Cargo.toml:13` — sqlx has `postgres` feature only, needs `sqlite` added
- `config.toml:24-30` — `[database]` section for pool/retry settings; no backend selection
- `src/dashboard.rs:141-155` — all dashboard routes call methods on `PersistenceConfig` directly

## Desired End State

A `[persistence]` config section selects the backend: `memory` (Vec+RwLock, no deps), `sqlite` (file-backed, survives restarts), or `postgres` (production). `DATABASE_URL` env var presence forces postgres as the only env override. All backends implement the same trait, so dashboard routes and proxy handlers work without knowing which backend is active. Tests use an always-available in-memory backend, eliminating the `SKIP: DATABASE_URL not set` pattern for all persistence tests.

### Key Discoveries:

- Codebase already uses trait-object dispatch for classifiers (`ClassifierChain` with `Arc<dyn IntentClassify>`) — `src/main.rs:145`
- Dynamic WHERE clause building pattern exists at `src/persistence.rs:164-183` — SQLite queries reuse this approach
- Config loading follows a consistent `load_*_from_value(&toml::Value)` pattern — `src/config.rs:17-151` provides templates
- The `async-trait` crate is already a dependency — `Cargo.toml:21`

## What We're NOT Doing

- **No SQLx migrate! for SQLite** — SQLite schema is a single `CREATE TABLE IF NOT EXISTS` hardcoded const. No migration directory.
- **No `DB_BACKEND` env var** — Backend is set in config.toml only. `DATABASE_URL` forces postgres as the sole env exception.
- **No changing the Postgres production path** — Existing PG queries, retry logic, and migration flow are preserved.
- **No replacing `Option<PersistenceConfig>` entirely** — Keep the Option for explicit opt-out (e.g., `DISABLE_PERSISTENCE=true`).
- **No query builder or ORM** — Raw SQLs continue for PG and SQLite backends.
- **No p99 via SQL for SQLite** — All non-PG backends compute p99 in Rust.
- **No `sqlx::Any` / `AnyPool`** — Research confirmed it does not translate bind params.

## Implementation Approach

Extract a `PersistenceBackend` trait, implement it for three structs (`MemoryBackend`, `SqliteBackend`, `PostgresBackend`), wrap them in a `DbBackend` enum that also implements the trait. `PersistenceConfig` becomes a thin wrapper holding `Arc<DbBackend>` + `Arc<Semaphore>`. Dashboard routes call methods through the backend field.

## Critical Implementation Details

- **Timing & lifecycle** — SQLite in-memory connections die when the pool drops. Use shared-cache URI (`sqlite:file:cerebrum?mode=memory&cache=shared`) with `min_connections(1)` to keep the DB alive. The file-backed SQLite path uses `connect_lazy` and does not validate an empty-file DB on startup.
- **State sequencing** — `SqliteBackend::from_path` must run `CREATE TABLE IF NOT EXISTS` before the backend is returned, ensuring the schema exists before any query runs. Failure is fatal (panic).
- **Performance constraints** — Memory backend's `fetch_inferences` does O(n) filter + sort. Acceptable for demo/test record counts (hundreds). For SQLite p99, fetch sorted durations with LIMIT, compute percentile index in Rust: `p99_idx = (0.99 * count).ceil() as usize - 1`.

## Phase 1: Foundation

### Overview

Establish the trait, enum, config section, and minimal stub structs. Add the `sqlite` feature to Cargo.toml. No method implementations yet — just the structural skeleton that phases 2-4 fill in.

### Changes Required:

#### 1. Cargo.toml — Add sqlite feature

**File**: `Cargo.toml`

**Intent**: Add `"sqlite"` to the sqlx features list so sqlx can compile against its SQLite driver.

**Contract**: Change `sqlx = { version = "0.8", features = ["postgres", "runtime-tokio", "tls-rustls", "macros", "uuid", "chrono", "migrate"] }` to add `"sqlite"` to the feature list.

#### 2. Define the `PersistenceBackend` trait

**File**: `src/persistence.rs`

**Intent**: Create the shared trait that all three backends implement, replacing the current method-impl-on-PersistenceConfig pattern.

**Contract**: Add `#[async_trait] pub trait PersistenceBackend: Send + Sync { ... }` with four methods: `insert_inference`, `fetch_inferences`, `fetch_latency_summary`, `fetch_savings_estimate`. Signatures match the current methods on `PersistenceConfig` except `insert_inference` takes `&InferenceRecord` and returns `Result<(), String>`.

```rust
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String>;
    async fn fetch_inferences(&self, offset: u32, limit: u32, filter_category: Option<&str>, filter_model: Option<&str>) -> Result<(Vec<InferenceLog>, i64), QueryError>;
    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError>;
    async fn fetch_savings_estimate(&self, hours: u32, model_costs: &dyn CostProvider, baseline_model: &str) -> Result<SavingsEstimate, QueryError>;
}
```

#### 3. Define stub backend structs and `DbBackend` enum

**File**: `src/persistence.rs`

**Intent**: Create empty structs for all three backends plus a dispatch enum. Phases 2-4 fill each struct with fields and method implementations.

**Contract**:

```rust
pub struct MemoryBackend { /* Phase 2 */ }

pub struct SqliteBackend { /* Phase 3 */ }

pub struct PostgresBackend { /* Phase 4 */ }

pub enum DbBackend {
    Memory(MemoryBackend),
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}
```

#### 4. Add `[persistence]` section to embedded config.toml

**File**: `config.toml`

**Intent**: Add a new `[persistence]` section with the `backend` field. Default to `"memory"` so the embedded config works with no dependencies.

**Contract**: Add below the `[database]` section:
```toml
[persistence]
backend = "memory"
# sqlite_path = "./cerebrum.db"
```

#### 5. Add `load_persistence_config_from_value()` to config.rs

**File**: `src/config.rs`

**Intent**: Follow the existing pattern (`load_database_config_from_value`, `load_dashboard_config_from_value`) to read the `[persistence]` section from TOML.

**Contract**: A new public struct `PersistenceSettings` with fields `backend: String` and `sqlite_path: String`. The loader function reads `persistence.backend` (default `"memory"`), `persistence.sqlite_path` (default `"./cerebrum.db"`). Same extract-then-default pattern as all other `load_*_from_value` functions.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with the sqlite feature added
- `cargo build` succeeds with the new trait + stub structs + enum definitions
- `cargo build` succeeds with the new config loading function

---

## Phase 2: Memory Backend

### Overview

Implement the full `PersistenceBackend` trait for `MemoryBackend` using `Arc<RwLock<Vec<InferenceRecord>>>`. All queries are Rust iterators. p99 is computed in Rust by sorting durations and picking the 99th percentile index.

### Changes Required:

#### 1. Complete `MemoryBackend` struct

**File**: `src/persistence.rs`

**Intent**: Add fields and `new()` constructor.

**Contract**: `MemoryBackend { records: Arc<RwLock<Vec<InferenceRecord>>> }`. Constructor: `pub fn new() -> Self` creates empty vec. No config needed.

#### 2. Implement `PersistenceBackend` for `MemoryBackend`

**File**: `src/persistence.rs`

**Intent**: Four trait method implementations: `insert_inference` (push), `fetch_inferences` (iter + filter + sort + skip + take), `fetch_latency_summary` (group_by category, compute avg/p99 in Rust), `fetch_savings_estimate` (group_by model, compute costs in Rust).

**Contract**:

- `insert_inference` — `write().push(record.clone())`, returns `Ok(())`
- `fetch_inferences` — `read().iter()`, apply filters via `filter()`, sort by created_at DESC, skip/offset + take/limit, count total matching rows
- `fetch_latency_summary` — filter by time window (comparing `record.created_at`), group by category, compute avg via fold, compute p99 via sorted vec
- `fetch_savings_estimate` — filter by time window, filter non-null category + model, group by model, compute char count + cost per model, compare to baseline

The p99 helper function: given a `&[i32]` of durations, `let idx = (0.99 * durations.len() as f64).ceil() as usize - 1; sorted[idx]`.

#### 3. Implement `DbBackend` dispatch

**File**: `src/persistence.rs`

**Intent**: `impl PersistenceBackend for DbBackend` delegates each method to the active variant.

**Contract**: A match on `self` in each method:
```rust
#[async_trait]
impl PersistenceBackend for DbBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        match self {
            DbBackend::Memory(b) => b.insert_inference(record).await,
            DbBackend::Sqlite(b) => b.insert_inference(record).await,
            DbBackend::Postgres(b) => b.insert_inference(record).await,
        }
    }
    // ... same pattern for other 3 methods
}
```

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with MemoryBackend fully implemented
- All methods compile and return correct types

---

## Phase 3: SQLite Backend

### Overview

Implement `PersistenceBackend` for `SqliteBackend` using `sqlx::SqlitePool`. File-backed (`./cerebrum.db`) or in-memory via shared-cache URI. Schema created via `CREATE TABLE IF NOT EXISTS` on construction.

### Changes Required:

#### 1. Complete `SqliteBackend` struct and constructor

**File**: `src/persistence.rs`

**Intent**: Fields and async `from_path(path: &str)` constructor that creates pool + initializes schema.

**Contract**: `SqliteBackend { pool: SqlitePool }`. Constructor takes `path: &str`. If path is `":memory:"`, use shared-cache URI `sqlite:file:cerebrum?mode=memory&cache=shared` with `min_connections(1)`. Otherwise, use `sqlite:{path}?mode=rwc`. On construction, execute `CREATE TABLE IF NOT EXISTS inferences (request_id TEXT PRIMARY KEY, status TEXT NOT NULL, category TEXT, upstream_model TEXT, duration_ms INTEGER, prompt_snippet TEXT NOT NULL, prompt_char_count INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')))` and `CREATE INDEX IF NOT EXISTS idx_inferences_created_at ON inferences(created_at)`.

#### 2. Implement `PersistenceBackend` for `SqliteBackend`

**File**: `src/persistence.rs`

**Intent**: Four query methods using SQLite-compatible SQL (no `$N` binds, no `PERCENTILE_CONT`, no `NOW()`, no `interval`).

**Contract**:

- `insert_inference` — INSERT with `? ` binds. UUID as TEXT (app-generated, not `gen_random_uuid()`). Includes retry_once logic.

- `fetch_inferences` — Same dynamic WHERE building pattern as current PG code but with `? ` binds. SQLite uses `?1`, `?2` etc for positional. COUNT query + data query pattern preserved.

- `fetch_latency_summary` — GROUP BY category. Use `datetime('now', '-? hours')` for time filter. Fetch `category, count, AVG(duration_ms)` from SQLite, then for p99 compute in Rust: fetch all durations sorted ASC from a separate query, compute percentile index.

- `fetch_savings_estimate` — GROUP BY upstream_model. Time filter via `datetime('now', '-? hours')`. Use `COALESCE(SUM(prompt_char_count), 0)` — portable. Exclude NULL category + model. Cost computation in Rust is shared with PG.

#### 3. `SqliteBackend::from_pool` for in-memory shared-cache

**File**: `src/persistence.rs`

**Intent**: Support creating a pool directly from a URI string for testing (each test gets unique shared-cache name).

**Contract**: `pub async fn from_uri(uri: &str) -> Self` — creates pool via `SqlitePoolOptions::new().max_connections(1).connect(uri)`, runs schema init.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with SqliteBackend
- Schema init creates the table correctly

---

## Phase 4: Postgres Backend Refactor

### Overview

Move the existing `PersistenceConfig` methods into a new `PostgresBackend` struct, implementing `PersistenceBackend`. The existing PG-specific SQL, retry logic, and migration flow are preserved unchanged. `PersistenceConfig::from_env` becomes `PostgresBackend::from_url`.

### Changes Required:

#### 1. Define `PostgresBackend` struct

**File**: `src/persistence.rs`

**Intent**: Extract the current `pool: Arc<PgPool>` from `PersistenceConfig` into the new struct.

**Contract**: `PostgresBackend { pool: PgPool }`. The pool is owned, not Arc-wrapped — the `DbBackend` enum wraps everything in Arc when stored.

#### 2. Move existing query methods into `impl PersistenceBackend for PostgresBackend`

**File**: `src/persistence.rs`

**Intent**: All four query methods currently on `PersistenceConfig` move into the trait impl on `PostgresBackend`. Logic is identical — only `self.pool` changes (was `self.pool.as_ref()`, becomes `&self.pool`).

**Contract**: Copy `fetch_inferences`, `fetch_latency_summary`, `fetch_savings_estimate` method bodies into the trait impl. Replace `self.pool.as_ref()` with `&self.pool`. `insert_inference` combines `insert_once` + retry logic internally.

#### 3. Create `PostgresBackend::from_env`

**File**: `src/persistence.rs`

**Intent**: Move the connection setup, health check retry, and migration logic from `PersistenceConfig::from_env` into `PostgresBackend::from_env`.

**Contract**: `pub async fn from_env(db_config: &DatabaseConfig) -> Result<Self, String>`. Reads `DATABASE_URL` env var, creates `PgPoolOptions`, runs health check with retries, runs `sqlx::migrate!()`, returns `PostgresBackend { pool }`. Same panic-on-failure behavior as current code.

#### 4. Shrink `PersistenceConfig` to a wrapper

**File**: `src/persistence.rs`

**Intent**: Replace the old `PersistenceConfig { pool, task_semaphore }` with a thin wrapper.

**Contract**: `PersistenceConfig { backend: Arc<DbBackend>, task_semaphore: Arc<Semaphore> }`. Keep `task_semaphore` — it remains used by `log_inference` to bound concurrent writes. Remove all query methods (now on `PersistenceBackend` trait).

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with all existing PG code moved into PostgresBackend
- Existing PG-specific tests compile with the new API

---

## Phase 5: Main.rs Integration

### Overview

Wire backend selection in `main()`, update `AppState`, change `log_inference` + `log_classification` signatures, and update dashboard route call sites to use the new API.

### Changes Required:

#### 1. Backend selection in `main()`

**File**: `src/main.rs`

**Intent**: Replace the current `PersistenceConfig::from_env` call with multi-backend selection logic.

**Contract**: Resolution order:
1. `DATABASE_URL` env var is set and non-empty → instantiate `PostgresBackend`, panic on failure (current behavior)
2. Read `persistence_settings` from config. If `backend` is `"postgres"` but `DATABASE_URL` is absent → warn + fall through
3. If `backend` is `"sqlite"` → `SqliteBackend::from_path(settings.sqlite_path).await`
4. Otherwise (`"memory"` or unrecognized) → `MemoryBackend::new()`

Wrap the result in `Arc::new(DbBackend::Postgres(...))` etc, store in `PersistenceConfig`.

#### 2. Update `AppState.persistence` type

**File**: `src/main.rs`

**Intent**: Change from `Option<PersistenceConfig>` to `Option<PersistenceConfig>` (struct stays, contents change).

**Contract**: The type annotation stays the same, but `PersistenceConfig` now holds `backend: Arc<DbBackend>` instead of `pool: Arc<PgPool>`. The field assignment at `src/main.rs:217-218` changes to use the new backend selection.

#### 3. Update `log_inference` signature

**File**: `src/persistence.rs`

**Intent**: Accept `Arc<DbBackend>` instead of `Arc<PgPool>`.

**Contract**: `pub fn log_inference(backend: Arc<DbBackend>, semaphore: Arc<Semaphore>, record: InferenceRecord) -> tokio::task::JoinHandle<()>`. Inside the spawned task, call `backend.insert_inference(&record).await` instead of `write_with_retry`.

#### 4. Update `log_classification` call site

**File**: `src/main.rs`

**Intent**: Pass `persistence.backend.clone()` instead of `persistence.pool.clone()`.

**Contract**: Change `src/main.rs:284-288` from `persistence::log_inference(persistence.pool.clone(), ...)` to `persistence::log_inference(persistence.backend.clone(), persistence.task_semaphore.clone(), record)`.

#### 5. Update dashboard route call sites

**File**: `src/dashboard.rs`

**Intent**: Switch from calling methods on `PersistenceConfig` to calling through `persistence.backend`.

**Contract**: Replace `persistence.fetch_latency_summary(...)` with `persistence.backend.fetch_latency_summary(...)` and similarly for all other method calls in `dashboard_handler`, `inferences_handler`, `latency_handler`, `savings_handler`.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with full backend selection wired
- `cargo test` passes all existing fast tests

---

## Phase 6: Test Infrastructure

### Overview

Rewrite `test_pool()` to return an in-memory `DbBackend` instead of `Arc<PgPool>`. Add new tests exercising the memory and SQLite backends. Convert existing persistence tests to use the in-memory backend so they no longer require `DATABASE_URL`.

### Changes Required:

#### 1. New `test_backend()` helper

**File**: `src/persistence.rs`

**Intent**: Replace the current `test_pool() -> Option<Arc<PgPool>>` with `test_backend() -> Option<PersistenceConfig>` that creates an in-memory backend. Add `test_sqlite_backend()` for SQLite-specific tests.

**Contract**: 
- `test_backend()` — creates `MemoryBackend`, wraps in `DbBackend::Memory`, returns `PersistenceConfig { backend: Arc::new(...), task_semaphore: Arc::new(Semaphore::new(100)) }`. Always succeeds (no `Option` needed).
- `test_sqlite_backend()` — creates `SqliteBackend::from_uri("sqlite:file:test?mode=memory&cache=shared")` for each test with unique DB name.

#### 2. Convert existing tests to use in-memory backend

**File**: `src/persistence.rs`

**Intent**: Rewrite all `#[cfg(test)] mod tests` to use `test_backend()` instead of `test_pool()`. Remove the `SKIP` pattern. Keep PG-specific tests (those testing PG-only features like PERCENTILE_CONT) behind a separate `test_pg_backend()` guard.

**Contract**: All test functions that previously used `let pool = match test_pool().await { Some(p) => p, None => { eprintln!("SKIP ..."); return; } }` now use `let pc = test_backend().await;`. Tests call `pc.backend.fetch_*()` instead of `pc.fetch_*()`.

#### 3. New memory-backend-specific tests

**File**: `src/persistence.rs`

**Intent**: Tests that exercise Rust-side p99 computation, filtering correctness, and concurrency behavior of `RwLock`.

**Contract**:
- `test_memory_p99_computation` — insert records with known durations, verify p99 matches expected value
- `test_memory_concurrent_reads` — spawn multiple concurrent reads, verify no deadlock
- `test_memory_time_filter` — insert records with past timestamps, verify time window filtering

#### 4. New SQLite-backend-specific tests

**File**: `src/persistence.rs`

**Intent**: Verify SQLite schema initialization, `?` bind param behavior, and in-memory shared-cache isolation.

**Contract**:
- `test_sqlite_schema_init` — verify table + index exist after construction
- `test_sqlite_insert_and_fetch` — insert via `insert_inference`, fetch back, verify fields
- `test_sqlite_isolation` — two backends with different URIs, verify no cross-contamination

#### 5. New `log_inference` integration test

**File**: `src/persistence.rs`

**Intent**: Test the full fire-and-forget path: log a record, wait briefly, fetch it back.

**Contract**: Spawn `log_inference(backend, semaphore, record)`, sleep 100ms, call `backend.fetch_inferences(...)`, assert the record appears.

### Success Criteria:

#### Automated Verification:

- `cargo test` runs all persistence tests without `DATABASE_URL`
- `cargo test` — zero `SKIP: DATABASE_URL not set` messages in persistence module
- Memory backend tests pass
- SQLite backend tests pass

---

## Phase 7: Verification

### Overview

Run the full test suite, verify the dashboard renders with the memory backend, and confirm backward compatibility with the Postgres path.

### Changes Required:

(No code changes — verification only.)

### Success Criteria:

#### Automated Verification:

- `cargo test` — all fast tests pass
- `cargo test auth` — auth tests pass
- `cargo test routes_auth` — route auth tests pass
- `cargo test slow_tests` — slow tests pass (if applicable)

#### Manual Verification:

- Start cerebrum with no env vars (defaults to memory backend): `cargo run`. Dashboard at `/dashboard` shows "DB Connected: Yes" and empty data.
- Send a POST to `/v1/classify` with a valid request. Dashboard shows the inference record.
- Start cerebrum with `DATABASE_URL` set — verifies Postgres path still works.
- Start cerebrum with `[persistence] backend = "sqlite"` in config — verifies SQLite path, `./cerebrum.db` is created.
- Restart (same SQLite path) — verify data survives restart.

---

## Testing Strategy

### Unit Tests:

- `PersistenceBackend` trait methods for each backend (memory, sqlite, postgres)
- p99 computation helper (pure function, testable without any backend)
- Config loading: `load_persistence_config_from_value` with various TOML values
- `log_inference` fire-and-forget with memory backend

### Integration Tests:

- End-to-end: classify a request → memory backend stores it → dashboard renders it
- Concurrent logging: spawn 10 `log_inference` tasks, verify all records appear
- Backend switching: start app with memory, send request, verify dashboard

### Manual Testing Steps:

1. `cargo run` with no env vars → dashboard loads, DB connected: Yes, memory backend active
2. POST `/v1/classify` → inference log appears on `/dashboard/inferences`
3. Set `backend = "sqlite"` in config, restart → `./cerebrum.db` created, data persists across restart
4. Set `DATABASE_URL`, restart → Postgres backend active, migrations run
5. Dashboard latency page → shows avg duration, p99 computed in Rust

## Performance Considerations

- Memory backend: O(n) for fetch/filter/sort. Acceptable for record counts under ~10k (demo/test scale).
- SQLite backend: Same query performance as PG for the record volumes expected in dev/CI.
- Postgres backend: No performance change — existing queries are preserved.
- p99 Rust computation: Sorting durations (up to ~hundreds for dev datasets) has negligible overhead.

## References

- Research: `context/changes/in-memory-db-fallback/research.md`
- Existing classifier trait pattern: `src/intent_classifier.rs` (`IntentClassify` trait, `ClassifierChain`)
- Existing config loading pattern: `src/config.rs:17-151` (`load_*_from_value` functions)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Foundation

#### Automated

- [x] 1.1 `cargo build` succeeds with sqlite feature added
- [x] 1.2 `cargo build` succeeds with trait + stub structs + enum defined
- [x] 1.3 `cargo build` succeeds with config loading function

### Phase 2: Memory Backend

#### Automated

- [x] 2.1 `cargo build` succeeds with MemoryBackend fully implemented

### Phase 3: SQLite Backend

#### Automated

- [x] 3.1 `cargo build` succeeds with SqliteBackend
- [x] 3.2 Schema init creates the table correctly

### Phase 4: Postgres Backend Refactor

#### Automated

- [x] 4.1 `cargo build` succeeds with all existing PG code moved into PostgresBackend
- [x] 4.2 Existing PG-specific tests compile with the new API

### Phase 5: Main.rs Integration

#### Automated

- [x] 5.1 `cargo build` succeeds with full backend selection wired

### Phase 6: Test Infrastructure

#### Automated

- [x] 6.1 `cargo test` runs all persistence tests without `DATABASE_URL`
- [x] 6.2 Zero `SKIP: DATABASE_URL not set` messages in persistence module
- [x] 6.3 Memory backend tests pass
- [x] 6.4 SQLite backend tests pass

### Phase 7: Verification

#### Automated

- [x] 7.1 `cargo test` — all fast tests pass
- [x] 7.2 `cargo test auth` — auth tests pass
- [x] 7.3 `cargo test routes_auth` — route auth tests pass

#### Manual

- [x] 7.4 Dashboard loads with memory backend (no env vars)
- [x] 7.5 POST /v1/classify creates visible inference log
- [x] 7.6 Postgres path works with DATABASE_URL set
- [x] 7.7 SQLite path creates ./cerebrum.db and survives restart
