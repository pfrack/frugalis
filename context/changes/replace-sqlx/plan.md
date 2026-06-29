# Replace sqlx with sea-query — Implementation Plan

## Overview

Replace raw SQL string duplication in the persistence layer by upgrading to sqlx 0.9 + sea-query 1.0 as a cross-dialect query builder. This collapses `sqlite.rs` (~570 LOC) and `postgres.rs` (~430 LOC) into a single `sql_backend.rs` (~200 LOC), while switching migrations from sqlx's built-in system to refinery.

## Current State Analysis

The persistence layer has 3 backends behind `PersistenceBackend` trait:
- `MemoryBackend` (1044 LOC) — in-memory Vec, tests + "no DB" mode
- `SqliteBackend` (569 LOC) — raw SQL with `?N` placeholders, PRAGMA-based schema
- `PostgresBackend` (430 LOC) — raw SQL with `$N` placeholders, `sqlx::migrate!`

The last two are 80% identical — same logic, different placeholder syntax and date functions.

### Key Discoveries:

- `src/persistence/sqlite.rs:119-173` — Dynamic WHERE with manual `has_where` bool and string concatenation
- `src/persistence/postgres.rs:94-147` — Same logic with `bind_count` tracker for `$N`
- `src/persistence/backend.rs:42-98` — `DbBackend` enum with 3 variants doing pass-through dispatch
- `migrations/001_create_inferences.sql` through `005_*.sql` — 5 Postgres migration files (sqlx format, no `V` prefix)
- `Cargo.toml:19` — sqlx 0.8 with features: postgres, sqlite, runtime-tokio, tls-rustls, macros, uuid, chrono, migrate
- sea-query 1.0 + sea-query-sqlx 0.9 provides `.build_sqlx(PostgresQueryBuilder)` / `.build_sqlx(SqliteQueryBuilder)` outputting correct dialect from one query definition
- refinery 0.9.2 cannot use sqlx PgPool directly — must run via `Config` before pool creation

## Desired End State

A single `SqlBackend` struct that holds either a `PgPool` or `SqlitePool` plus a dialect flag. All 4 `PersistenceBackend` methods use sea-query to build queries once, render to the correct dialect, and execute via sqlx. The `DbBackend` enum has 2 variants: `Memory | Sql`. Migrations use refinery with embedded SQL files that work for both Postgres and SQLite.

**Verification**: `cargo test` passes all existing persistence tests. The `inferences` table schema is identical before and after migration. Dashboard queries return the same results.

## What We're NOT Doing

- Not adding an ORM (SeaORM) — sea-query is just a query builder
- Not removing MemoryBackend — it stays for "no persistence" mode and fast unit tests
- Not changing the `PersistenceBackend` trait signature
- Not modifying any code outside `src/persistence/` (the trait boundary isolates this)
- Not adding new features or columns — pure refactor

## Implementation Approach

Phase-by-phase migration: add new deps alongside old, build the new `SqlBackend` module, migrate one method at a time, then remove old code. Each phase is independently compilable and testable.

## Critical Implementation Details

**Timing & lifecycle**: refinery migrations must run *before* the sqlx pool is created in `PostgresBackend::from_env`. Parse the URL via `refinery::config::Config`, run migrations, then create the pool. This replaces the current `sqlx::migrate!().run(&pool)` call.

**State sequencing**: The `SqlBackend` needs a dialect enum (`Dialect::Postgres | Dialect::Sqlite`) stored at construction time to select the correct `QueryBuilder` at query time. This replaces the current separate struct types.

---

## Phase 1: Upgrade Dependencies & Add sea-query

### Overview

Bump sqlx 0.8→0.9, add sea-query 1.0 + sea-query-sqlx 0.9 + refinery 0.9.2. Fix any breaking changes from the sqlx upgrade. Verify everything still compiles and tests pass.

### Changes Required:

#### 1. Update Cargo.toml

**File**: `Cargo.toml`

**Intent**: Upgrade sqlx and add new query-building and migration dependencies.

**Contract**: Replace `sqlx = { version = "0.8", ... }` with `0.9`. Add `sea-query`, `sea-query-sqlx`, and `refinery`. The sqlx features list changes slightly (runtime/tls split in 0.9).

#### 2. Fix sqlx 0.9 breaking changes

**File**: `src/persistence/postgres.rs`, `src/persistence/sqlite.rs`, `src/persistence/backend.rs`

**Intent**: Address any API changes from sqlx 0.8→0.9 (primarily the runtime/TLS feature rename and any deprecated method removals).

**Contract**: All existing `sqlx::query(...)` calls continue to work. The `sqlx::migrate!()` macro still works (it's preserved in 0.9). Tests pass unchanged.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with new deps
- `cargo test --lib` passes all existing tests
- `cargo clippy` has no new warnings

#### Manual Verification:

- Verify the dependency tree is clean: `cargo tree -d` shows no unexpected duplicates of sqlx or tokio

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Create Unified SqlBackend — INSERT + fetch_inferences

### Overview

Create a new `sql_backend.rs` that implements `PersistenceBackend` using sea-query for query building. Start with `insert_inference` and `fetch_inferences` — the two simplest methods. Wire it into `DbBackend` as a new variant.

### Changes Required:

#### 1. Define the Iden enum and dialect type

**File**: `src/persistence/sql_backend.rs` (new)

**Intent**: Define the column identifiers and a dialect enum that controls which QueryBuilder to use.

**Contract**: A `#[derive(Iden)]` enum `Inferences` with variants matching all 14 columns. A `Dialect` enum with `Postgres` / `Sqlite` variants. A `SqlBackend` struct holding either pool type + the dialect.

#### 2. Implement insert_inference with sea-query

**File**: `src/persistence/sql_backend.rs`

**Intent**: Build the INSERT statement via `Query::insert()` with all 14 columns, render to correct dialect, execute via `sqlx::query_with`.

**Contract**: `async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String>` — same signature as existing backends. Uses `retry_once` wrapper. The INSERT omits `created_at` (uses DB default).

#### 3. Implement fetch_inferences with sea-query

**File**: `src/persistence/sql_backend.rs`

**Intent**: Build the SELECT + COUNT queries using `.and_where_option()` for the optional category/model filters. Compute time cutoff in Rust (not needed here — `fetch_inferences` has no time filter).

**Contract**: Uses `Query::select()` with `.and_where_option(filter_category.map(...))` and `.and_where_option(filter_model.map(...))`. Executes both count and data queries. Maps rows via `Row::try_get` (same pattern as current code, but one implementation instead of two).

#### 4. Add SqlBackend to DbBackend enum

**File**: `src/persistence/backend.rs`

**Intent**: Add `DbBackend::Sql(SqlBackend)` as a fourth variant (temporarily alongside Sqlite/Postgres during migration).

**Contract**: The match arms in the `PersistenceBackend` impl for `DbBackend` gain a `DbBackend::Sql(b) => b.method().await` arm for each method. For methods not yet migrated (`fetch_latency_summary`, `fetch_savings_estimate`), SqlBackend returns a placeholder error.

#### 5. Add SqlBackend construction path

**File**: `src/persistence/mod.rs`

**Intent**: Expose a constructor for `SqlBackend` that takes a database URL string and determines dialect from the URL scheme (`postgres://` vs `sqlite:`).

**Contract**: `SqlBackend::connect(url: &str, db_config: &DatabaseConfig) -> Result<Self, String>` — creates the pool, runs refinery migrations, returns the backend.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- New unit test: `SqlBackend` insert + fetch round-trip against SQLite in-memory
- New unit test: `SqlBackend` insert + fetch with category filter
- Existing tests still pass (old backends unchanged)

#### Manual Verification:

- Start the app with `PERSISTENCE_BACKEND=sql` pointing at a test SQLite DB; verify insert works via dashboard

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Migrate Aggregation Queries

### Overview

Implement `fetch_latency_summary` and `fetch_savings_estimate` in `SqlBackend` using sea-query. Handle the PERCENTILE_CONT divergence: PG uses the SQL function; SQLite computes p99 in Rust (same as current `SqliteBackend` does).

### Changes Required:

#### 1. Implement fetch_latency_summary

**File**: `src/persistence/sql_backend.rs`

**Intent**: Build the grouped aggregation query (COUNT + AVG + optional PERCENTILE_CONT) with a time cutoff computed in Rust as a chrono timestamp parameter.

**Contract**: For Postgres dialect: single query using `Expr::cust("PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms)")`. For SQLite dialect: two queries — one for COUNT/AVG grouped by category, one fetching raw durations for Rust-side `percentile_99()`. The time filter uses `Expr::col(Inferences::CreatedAt).gte(cutoff)` where `cutoff = Utc::now() - chrono::Duration::hours(hours)`.

#### 2. Implement fetch_savings_estimate

**File**: `src/persistence/sql_backend.rs`

**Intent**: Build the grouped aggregation query (COUNT + SUM + conditional SUM) with time filter and GROUP BY upstream_model.

**Contract**: Uses `Func::count()`, `Func::coalesce()`, `Expr::cust("SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END)")` for the fallback count. Time cutoff as chrono param, same as latency summary. Both dialects use the same query (no PG-specific functions here).

#### 3. Remove placeholder errors from SqlBackend

**File**: `src/persistence/sql_backend.rs`

**Intent**: All 4 trait methods now have real implementations. SqlBackend is fully functional.

**Contract**: No method returns a placeholder error. All pass the same test suite as existing backends.

### Success Criteria:

#### Automated Verification:

- New test: `fetch_latency_summary` returns correct aggregation for SqlBackend (SQLite)
- New test: `fetch_savings_estimate` returns correct cost calculation for SqlBackend
- All existing persistence tests pass
- `cargo clippy` clean

#### Manual Verification:

- Start app with SqlBackend pointing at Postgres; verify dashboard latency page shows correct p99 values
- Compare dashboard output between old PostgresBackend and new SqlBackend against same dataset

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: Switch Migrations to Refinery & Remove Old Backends

### Overview

Convert migration files to refinery format, remove `sqlite.rs` and `postgres.rs`, collapse `DbBackend` to 2 variants (Memory | Sql), and remove the sqlx `migrate` feature.

### Changes Required:

#### 1. Convert migration files to refinery format

**File**: `migrations/` directory

**Intent**: Rename existing files from sqlx's `NNN_name.sql` format to refinery's `V{n}__name.sql` format. Ensure SQL is compatible with both Postgres and SQLite where possible.

**Contract**: `V1__create_inferences.sql` through `V5__add_token_and_attribution_columns.sql`. The content stays the same for Postgres. For SQLite compatibility, refinery runs against whichever DB is configured — SQLite will use its own `init_schema` path (or separate migration set if needed).

#### 2. Wire refinery into SqlBackend construction

**File**: `src/persistence/sql_backend.rs`

**Intent**: Run embedded refinery migrations at startup before creating the sqlx pool (for Postgres) or after creating the pool (for SQLite, via rusqlite connection).

**Contract**: `embed_migrations!("./migrations")` at module level. For Postgres: parse URL into `refinery::config::Config`, run `runner().run_async(&mut config).await`, then create PgPool. For SQLite: open a rusqlite connection, run migrations, close it, then create SqlitePool.

#### 3. Remove old backend files

**Files**: `src/persistence/sqlite.rs`, `src/persistence/postgres.rs`

**Intent**: Delete the old backend implementations entirely. They're fully replaced by `sql_backend.rs`.

**Contract**: These files no longer exist. No code references them.

#### 4. Collapse DbBackend enum

**File**: `src/persistence/backend.rs`

**Intent**: Remove `Sqlite` and `Postgres` variants, leaving only `Memory` and `Sql`.

**Contract**: `pub enum DbBackend { Memory(MemoryBackend), Sql(SqlBackend) }`. The `PersistenceBackend` impl has 2 match arms. Remove unused imports (`SqlitePool`, `PgPool` from this file). Remove `TestDb` struct and `test_pool()` helper (replace with SqlBackend-based test helpers).

#### 5. Update module declarations and config

**File**: `src/persistence/mod.rs`

**Intent**: Remove `pub(crate) mod sqlite;` and `pub(crate) mod postgres;`, add `pub(crate) mod sql_backend;`. Update re-exports.

**Contract**: `mod.rs` declares `backend`, `memory`, `sql_backend`, `types`. The `PersistenceConfig` construction in `src/main.rs` routes to `SqlBackend::connect()` for both `sqlite` and `postgres` config values.

#### 6. Clean up Cargo.toml

**File**: `Cargo.toml`

**Intent**: Remove the `migrate` feature from sqlx (refinery handles it now). Potentially remove `macros` if no longer used.

**Contract**: sqlx features: `["postgres", "sqlite", "runtime-tokio", "tls-rustls", "uuid", "chrono"]`. Add `refinery` with features `["tokio-postgres", "rusqlite-bundled"]`.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds (no references to deleted files)
- All persistence tests pass with new SqlBackend
- `cargo clippy` clean
- `cargo test` full suite passes
- No dead code warnings from removed modules

#### Manual Verification:

- Fresh Postgres database: app starts, migrations apply, dashboard works
- Fresh SQLite file: app starts, schema created, insert + dashboard works
- Memory mode (no DB): app starts with MemoryBackend, dashboard shows "no persistence" message

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests:

- SqlBackend insert + fetch round-trip (SQLite in-memory)
- SqlBackend fetch_inferences with all filter combinations (None/None, Some/None, None/Some, Some/Some)
- SqlBackend fetch_latency_summary correctness (verify p99 computation)
- SqlBackend fetch_savings_estimate cost calculation
- MemoryBackend tests remain unchanged

### Integration Tests:

- SqlBackend against Postgres via testcontainers (existing pattern)
- Verify refinery migrations apply cleanly to fresh Postgres
- Verify refinery migrations apply cleanly to fresh SQLite

### Manual Testing Steps:

1. Run app with Postgres backend, make requests via proxy, verify dashboard shows correct data
2. Run app with SQLite backend, verify same behavior
3. Run app with memory backend, verify graceful degradation
4. Verify that starting with a pre-existing database (already has data) works correctly

## Performance Considerations

- sea-query query building adds negligible overhead (~microseconds) vs the current string concatenation
- The `sqlx::query_with(AssertSqlSafe(...), values)` path has identical runtime performance to current `sqlx::query(...).bind(...)`
- Connection pooling is unchanged (sqlx manages it the same way)
- Computing time cutoffs in Rust (`Utc::now() - hours`) instead of DB-side `NOW()` is semantically equivalent for this use case (sub-second clock drift is irrelevant for hour-granularity filters)

## Migration Notes

- Existing databases with data require no migration — the schema is unchanged
- refinery tracks its own migration state in a `refinery_schema_history` table (auto-created)
- For Postgres databases that already ran sqlx migrations: refinery will see no prior history and attempt to re-run. Set `refinery::Runner::set_abort_missing(false)` or add a baseline marker. Alternatively, use `IF NOT EXISTS` / `IF NOT EXISTS` guards in migration SQL (already present in migration 001).
- The sqlx `_sqlx_migrations` table can be left in place (harmless) or dropped manually after migration

## References

- Related research: `context/changes/replace-sqlx/research.md`
- sea-query docs: https://docs.rs/sea-query/latest
- refinery docs: https://docs.rs/refinery/latest
- Current SQLite backend: `src/persistence/sqlite.rs`
- Current Postgres backend: `src/persistence/postgres.rs`
- Existing migrations: `migrations/001_create_inferences.sql` through `migrations/005_*.sql`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Upgrade Dependencies & Add sea-query

#### Automated

- [x] 1.1 `cargo build` succeeds with new deps — 8032544
- [x] 1.2 `cargo test --lib` passes all existing tests — 8032544
- [x] 1.3 `cargo clippy` has no new warnings — 8032544

#### Manual

- [x] 1.4 Verify dependency tree is clean via `cargo tree -d`

### Phase 2: Create Unified SqlBackend — INSERT + fetch_inferences

#### Automated

- [x] 2.1 `cargo build` succeeds — 8863240
- [x] 2.2 SqlBackend insert + fetch round-trip test passes — 8863240
- [x] 2.3 SqlBackend insert + fetch with category filter test passes — 8863240
- [x] 2.4 Existing tests still pass — 8863240

#### Manual

- [x] 2.5 App starts with SqlBackend (SQLite), insert works via dashboard

### Phase 3: Migrate Aggregation Queries

#### Automated

- [x] 3.1 fetch_latency_summary test passes for SqlBackend — 8863240
- [x] 3.2 fetch_savings_estimate test passes for SqlBackend — 8863240
- [x] 3.3 All existing persistence tests pass — 8863240
- [x] 3.4 `cargo clippy` clean — 8863240

#### Manual

- [ ] 3.5 Dashboard latency page shows correct p99 values (Postgres)
- [ ] 3.6 Dashboard output matches between old and new backend

### Phase 4: Switch Migrations to Refinery & Remove Old Backends

#### Automated

- [x] 4.1 `cargo build` succeeds (no references to deleted files)
- [x] 4.2 All persistence tests pass with new SqlBackend
- [x] 4.3 `cargo clippy` clean
- [x] 4.4 `cargo test` full suite passes
- [x] 4.5 No dead code warnings from removed modules

#### Manual

- [ ] 4.6 Fresh Postgres: migrations apply, dashboard works
- [ ] 4.7 Fresh SQLite: schema created, insert + dashboard works
- [ ] 4.8 Memory mode: app starts, shows "no persistence" message
