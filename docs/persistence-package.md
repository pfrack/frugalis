# Persistence Package

`src/persistence/` owns all inference logging for Frugalis. Every proxied request produces one [`InferenceRecord`](#inferencerecord) that is persisted asynchronously — the synchronous response path is never blocked by database latency. The package exposes a single public entry-point (`log_inference`), a pluggable backend abstraction (`PersistenceBackend`), and three backend implementations (memory, SQLite, Postgres).

## File Layout

| File | Responsibility |
|---|---|
| `mod.rs` | `PersistenceConfig`, `log_inference` — the public API surface |
| `types.rs` | Shared data types: `InferenceRecord`, `InferenceLog`, `LatencySummary`, `SavingsEstimate`, `CostProvider`, and message-extraction utilities |
| `backend.rs` | `PersistenceBackend` trait, `DbBackend` dispatch enum, `percentile_99`, `retry_once`, and test helpers |
| `memory.rs` | `MemoryBackend` — ephemeral in-process store (tests, local dev) |
| `sqlite.rs` | `SqliteBackend` — file-backed or in-memory SQLite via `sqlx` |
| `postgres.rs` | `PostgresBackend` — production Postgres via `sqlx` with automatic migrations |

---

## Architecture Overview

```
Proxy handler
      │
      ▼
log_inference(backend, semaphore, record)
      │
      ▼
tokio::spawn (detached background task)
      │  semaphore.acquire() ← bounds max concurrent tasks
      ▼
DbBackend::insert_inference(record)
      │
      ├── Memory  → Arc<RwLock<Vec<InferenceRecord>>>
      ├── SQLite  → sqlx::SqlitePool   (rwc file or shared-cache :memory:)
      └── Postgres → sqlx::PgPool      (DATABASE_URL, migrations via sqlx::migrate!)
```

The semaphore in `PersistenceConfig` caps the number of in-flight background tasks. If the database falls behind under burst load, tasks queue on the semaphore rather than spawning unboundedly and exhausting memory.

---

## Module: `mod.rs`

### `PersistenceConfig`

Shared handle injected into the Axum router state.

| Field | Type | Notes |
|---|---|---|
| `backend` | `Arc<DbBackend>` | The active storage backend; cheaply cloned per-request |
| `task_semaphore` | `Arc<Semaphore>` | Bounds concurrent background log tasks; capacity set by `[database].log_concurrency_limit` |

### `log_inference(backend, semaphore, record) -> JoinHandle<()>`

Enqueues one inference record for asynchronous persistence. Returns immediately; the caller never waits on the database.

**Flow:**
1. Clones `backend` and `semaphore` into a detached `tokio::spawn`.
2. Inside the task, acquires one semaphore permit (waits if limit reached).
3. Calls `backend.insert_inference(&record)`.
4. On final failure, logs `tracing::error!` with `request_id` — no panic, no retry beyond `retry_once` in the backend.

---

## Module: `types.rs`

### `CostProvider` (trait)

```rust
pub trait CostProvider: Send + Sync {
    fn get_cost(&self, model: &str) -> Option<f64>;
}
```

Allows the persistence layer to estimate inference costs without importing the config or classification modules. `ModelCosts` (in `src/config/routing.rs`) implements this trait.

### `QueryError`

Newtype wrapper around `String` for database query failures. Implements `Display`, `Error`, and `From<sqlx::Error>` so both SQL backends can use `?` directly and propagate typed errors to dashboard handlers.

### `InferenceRecord`

The complete metadata payload built by the proxy for one request, before it is handed to `log_inference`. All token and attribution fields are `Option` so that: (a) in-memory tests can use `..Default::default()`, and (b) existing DB rows without token data remain valid.

| Field | Type | Notes |
|---|---|---|
| `request_id` | `Uuid` | Unique identifier; also the primary key in the DB |
| `status` | `String` | HTTP status of the upstream response (e.g. `"200"`) |
| `category` | `Option<String>` | Intent label from the classifier; `None` if unclassified |
| `upstream_model` | `Option<String>` | Model name returned by or sent to the upstream |
| `duration_ms` | `Option<i32>` | End-to-end proxy latency in milliseconds |
| `prompt_snippet` | `String` | Truncated prompt for dashboard display |
| `prompt_char_count` | `Option<i32>` | Full prompt length in chars for cost estimation |
| `created_at` | `DateTime<Utc>` | Timestamp at record creation |
| `provider_attempts` | `u8` | How many providers were tried (1 = first provider succeeded) |
| `final_provider` | `String` | Provider type that ultimately served the request |
| `input_tokens` | `Option<i32>` | Non-cached input tokens; `None` when usage not captured |
| `output_tokens` | `Option<i32>` | Completion tokens |
| `cache_read_tokens` | `Option<i32>` | Cache-hit tokens (cost saving, Anthropic-specific) |
| `cache_creation_tokens` | `Option<i32>` | Cache-write tokens (cost, Anthropic-specific) |
| `client_session_id` | `Option<String>` | Claude Code session id from `x-claude-code-session-id` header |

### `InferenceLog`

A pre-formatted view of one database row for dashboard templates. All timestamps are pre-formatted as `"YYYY-MM-DD HH:MM:SS UTC"` strings; numeric fields remain `Option` to handle legacy rows with missing data.

### `LatencySummaryRow` / `LatencySummary`

Aggregated latency statistics per intent category for the dashboard latency page.

| Field | Notes |
|---|---|
| `rows` | One `LatencySummaryRow` per classified category |
| `unclassified_count` | Requests where `category IS NULL` |
| `total_classified_count` | Sum of `request_count` across all classified rows |

Each row contains `category`, `request_count`, `avg_duration_ms`, and `p99_duration_ms`.

### `SavingsEstimate`

Cost-savings computation result for the dashboard savings page. Built by comparing the actual cost (sum over all routed models) against what the same traffic would have cost on a single baseline model.

| Field | Notes |
|---|---|
| `savings_usd` | `baseline_cost − actual_cost`; can be negative if routing was more expensive |
| `formatted_savings_usd` | `"0.0042"` style string; empty string when savings ≤ 0 |
| `baseline_model` | The model used as the cost baseline |
| `classified_count` | Total requests with a known model cost |
| `unknown_cost_count` | Requests whose model has no cost in `ModelCosts` |
| `has_historical_fallback` | `true` when any cost estimate used snippet length instead of `prompt_char_count` |
| `baseline_model_unknown` | `true` when the baseline model itself has no configured cost |

### Message Extraction Utilities

#### `extract_last_user_message(body: &str) -> String`

Extracts the last `"user"` message from an OpenAI-compatible `{"messages": [...]}` body. Returns the content string capped at 10,000 characters. Returns `""` on any parse failure and logs a `WARN`. Hard limit of 1,000 messages prevents DoS via unbounded arrays.

#### `extract_last_user_message_anthropic(body: &str) -> String`

Same contract as above but handles Anthropic's polymorphic `content` field:
- `"content": "string"` → returned as-is
- `"content": [{"type": "text", "text": "..."}, ...]` → only `"text"` blocks are extracted; non-text blocks (images, tool results) are skipped; multiple text blocks are joined with a single space

#### `prompt_chars_to_cost(char_count: i32, cost_per_1m_input_tokens: f64) -> f64`

Converts character count to estimated USD cost using a 4-chars-per-token heuristic. Result is rounded to 6 decimal places.

```
tokens = char_count / 4
cost   = tokens × cost_per_1m / 1_000_000
```

---

## Module: `backend.rs`

### `PersistenceBackend` (trait)

```rust
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String>;
    async fn fetch_inferences(offset, limit, filter_category, filter_model) -> Result<(Vec<InferenceLog>, i64), QueryError>;
    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError>;
    async fn fetch_savings_estimate(&self, hours, model_costs, baseline_model) -> Result<SavingsEstimate, QueryError>;
}
```

All four methods are async and the trait is `Send + Sync` so implementors can be stored behind `Arc<dyn PersistenceBackend>`. In practice the enum dispatch via `DbBackend` is used instead of a trait object to avoid allocating a vtable on the hot insert path.

### `DbBackend` (enum)

Dispatch enum that implements `PersistenceBackend` by delegating to whichever variant is active. Adding a new backend requires: (1) a new variant here, (2) an arm in each `match`, (3) a new struct in its own module.

```rust
pub enum DbBackend {
    Memory(MemoryBackend),
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}
```

### `percentile_99(durations: &[i32]) -> Option<i32>`

Sorts a slice and returns the value at the 99th percentile index (`ceil(0.99 × n) − 1`). Used by `MemoryBackend` (Rust-side computation) and can be used by `SqliteBackend` since SQLite has no built-in `PERCENTILE_CONT`. The Postgres backend delegates p99 to the database via `PERCENTILE_CONT(0.99)`.

### `retry_once<F>(f: F) -> Result<T, E>`

Calls an async closure once; on failure, logs a `WARN` and calls it a second time. Returns the second error if both calls fail. Used by both `SqliteBackend` and `PostgresBackend` to silently recover from transient connection errors without requiring full retry infrastructure.

---

## Module: `memory.rs` — `MemoryBackend`

In-process store backed by `Arc<RwLock<Vec<InferenceRecord>>>`.

**Intended use:** tests and local development. Data is lost on process exit.

**Key behaviours:**

| Behaviour | Detail |
|---|---|
| **Insert cap** | At 10,000 records, the oldest entry is evicted before inserting |
| **Sorting** | `fetch_inferences` sorts by `created_at DESC` in Rust |
| **Filtering** | `filter_category` and `filter_model` applied via `Vec::retain` |
| **p99** | Computed by `percentile_99` in Rust |
| **Savings** | Iterates records; uses `prompt_char_count` when set, falls back to `prompt_snippet.len()` |
| **Failure injection** | `fail_next: AtomicBool` — set to `true` to make the next `insert_inference` return an error and auto-reset (test-only) |

---

## Module: `sqlite.rs` — `SqliteBackend`

File-backed or in-memory SQLite backend via `sqlx::SqlitePool`.

### Construction

| Method | Notes |
|---|---|
| `SqliteBackend::from_path(path)` | File URI: `sqlite:<path>?mode=rwc`; `:memory:` maps to shared-cache URI so multiple connections see the same data |
| `SqliteBackend::from_uri(uri)` | Lower-level; used internally and in tests |

### Schema Management

`init_schema()` runs on every construction:
1. `CREATE TABLE IF NOT EXISTS inferences (…)` — idempotent base schema
2. `CREATE INDEX IF NOT EXISTS idx_inferences_created_at` — index for time-window queries
3. **Runtime migrations** — detects missing columns via `PRAGMA table_info` and `ALTER TABLE … ADD COLUMN`:
   - `provider_attempts` (SMALLINT DEFAULT 1)
   - `final_provider` (TEXT)
   - `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens` (INTEGER, nullable)
   - `client_session_id` (TEXT, nullable)

> Using `PRAGMA table_info` instead of catching error strings is intentional — SQLite's error messages are not part of its stable contract.

### Query Dialect

SQLite uses positional `?` placeholders. The `fetch_inferences` WHERE clause is built dynamically based on which optional filters are non-null. Bind order must be consistent: filters first, then `LIMIT`, then `OFFSET`.

### Latency p99

SQLite has no `PERCENTILE_CONT`. The backend fetches all `duration_ms` values in the window and delegates to `percentile_99` (Rust-side computation).

### Insert Retry

Wraps `insert_once_sqlite` with `retry_once`; logs `error!` on final failure.

---

## Module: `postgres.rs` — `PostgresBackend`

Production backend backed by `sqlx::PgPool`.

### Construction

`PostgresBackend::from_env(db_config: &DatabaseConfig)`:
1. Reads `DATABASE_URL` from the environment (required; panics if absent).
2. Creates a lazy `PgPool` with limits from `DatabaseConfig` (`max_connections`, `acquire_timeout_secs`, `idle_timeout_secs`).
3. **Health check with exponential back-off + jitter** — retries `SELECT 1` up to `connection_retries` times; on all retries exhausted, panics.
4. **Runs `sqlx::migrate!()`** — applies all SQL files in `migrations/` in order; panics on failure.
5. Logs `"Migrations applied successfully"` after success.

### Query Dialect

Postgres uses numbered `$N` placeholders. The `fetch_inferences` WHERE clause uses an incrementing `bind_count` to generate the correct parameter numbers. The `LIMIT` and `OFFSET` parameters follow any filter bindings.

### Latency p99

Computed in the database using `PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms)`, which is more accurate and efficient than Rust-side computation for large datasets.

### Savings Estimate

A single aggregation query groups by `upstream_model`, summing `prompt_char_count` (preferred) and `LENGTH(prompt_snippet)` (fallback for older rows) in one pass. The Rust side then multiplies by model costs and computes the savings delta.

### Insert Retry

Wraps `insert_once` (the raw `sqlx::query` call) with `retry_once`; logs `error!` on final failure.

---

## Database Schema

```sql
CREATE TABLE inferences (
    request_id           TEXT PRIMARY KEY,         -- UUID string (SQLite) / UUID (Postgres)
    status               TEXT NOT NULL,
    category             TEXT,                      -- NULL = unclassified
    upstream_model       TEXT,
    duration_ms          INTEGER,
    prompt_snippet       TEXT NOT NULL,
    prompt_char_count    INTEGER,                   -- full prompt length; NULL for older rows
    created_at           TEXT NOT NULL,             -- ISO-8601 (SQLite) / TIMESTAMPTZ (Postgres)
    provider_attempts    SMALLINT DEFAULT 1,
    final_provider       TEXT,
    input_tokens         INTEGER,                   -- token usage (nullable)
    output_tokens        INTEGER,
    cache_read_tokens    INTEGER,
    cache_creation_tokens INTEGER,
    client_session_id    TEXT                       -- Claude Code attribution
);

CREATE INDEX idx_inferences_created_at ON inferences(created_at);
```

Migrations in `migrations/` add columns incrementally. SQLite applies them at startup via `PRAGMA table_info`; Postgres applies them via `sqlx::migrate!()`.

---

## Backend Selection

The backend is chosen at startup by `PersistenceSettings.backend`:

| `backend` value | `DbBackend` variant | Notes |
|---|---|---|
| `"memory"` | `DbBackend::Memory` | Default; no DB required |
| `"sqlite"` | `DbBackend::Sqlite` | Requires `sqlite_path` (default `./frugalis.db`) |
| `"postgres"` | `DbBackend::Postgres` | Requires `DATABASE_URL` env var |

---

## Testing

The memory backend is the default for unit and integration tests. `MemoryBackend::fail_next` provides deterministic failure injection without mocking.

For Postgres integration tests, `backend.rs` exposes two test-only helpers:
- **`TestDb`** — spins up an ephemeral Postgres 16 container via `testcontainers`; runs `sqlx::migrate!()` automatically.
- **`test_pool()`** — tries `TestDb` first; falls back to `DATABASE_URL` with a 3-second connect timeout; returns `None` when neither is available, allowing tests to be skipped gracefully.
