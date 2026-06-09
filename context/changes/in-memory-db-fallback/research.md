---
date: "2026-06-09T17:07:11+02:00"
researcher: kiro
git_commit: 83c2703a2f13b839b5955790e7086622a88ddf1c
branch: code-review-cleanup
repository: cerebrum
topic: "In-memory SQLite fallback when DATABASE_URL is absent"
tags: [research, codebase, persistence, sqlite, testing]
status: complete
last_updated: 2026-06-09
last_updated_by: kiro
---

# Research: In-memory SQLite fallback when DATABASE_URL is absent

**Date**: 2026-06-09T17:07:11+02:00
**Researcher**: kiro
**Git Commit**: 83c2703a2f13b839b5955790e7086622a88ddf1c
**Branch**: code-review-cleanup
**Repository**: cerebrum

## Research Question

What persistence backends can cerebrum support, and how should they be configured? Goal: three tiers — in-memory (no deps), SQLite file (survives restarts), PostgreSQL (production) — selected via configuration.

## Summary

**Three-tier persistence with config-driven backend selection:**

| Tier | Backend | Persistence | Dependencies | Use Case |
|------|---------|-------------|--------------|----------|
| `memory` | Rust `Vec` + `RwLock` | Lost on restart | None | Tests, quick demos |
| `sqlite` | SQLite file on disk | Survives restarts | None (bundled) | Local dev, CI |
| `postgres` | PostgreSQL (current) | Full production | External DB | Production |

Selected via `DB_BACKEND` env var (or config file). Default logic: if `DATABASE_URL` is set → `postgres`; else if `DB_BACKEND=sqlite` → SQLite file; else → `memory`.

**Effort breakdown:**
- In-memory backend: ~100 lines (Vec + RwLock, queries as Rust iterators)
- SQLite backend: ~200 lines (duplicate SQL with `?` binds, `datetime()` functions)
- Trait boundary + enum dispatch: ~50 lines
- Config/init changes: ~30 lines in `main.rs`
- Total: ~380 new lines, no changes to production Postgres path

## Detailed Findings

### PostgreSQL-Specific SQL (10/14 features incompatible)

| Feature | Location | SQLite Equivalent |
|---------|----------|-------------------|
| `PERCENTILE_CONT(0.99) WITHIN GROUP` | persistence.rs:266-267 | Subquery with ORDER BY + LIMIT OFFSET |
| `::INTEGER` / `::BIGINT` casts | persistence.rs:266-267, 335 | `CAST(expr AS INTEGER)` |
| `NOW()` | persistence.rs:270, 335 | `datetime('now')` |
| `interval '1 hour' * $1` | persistence.rs:270, 335 | `datetime('now', '-' \|\| ? \|\| ' hours')` |
| `$N` positional bind params | All queries | `?` positional |
| `gen_random_uuid()` | migrations/001:3 | App-generated UUID |
| `UUID` type | migrations/001:3-4 | `TEXT` |
| `TIMESTAMPTZ` | migrations/001:8 | `TEXT` (ISO-8601) |
| `= ANY($1)` array params | Test code only | `IN (?, ?, ...)` |
| `ALTER TABLE ADD CONSTRAINT` | migrations/002 | `CREATE UNIQUE INDEX` |

**Portable features (4/14):** `COALESCE`, `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`, `ADD COLUMN IF NOT EXISTS` (SQLite ≥ 3.35).

### sqlx SQLite Support

- **Feature flag:** Add `"sqlite"` to sqlx features in Cargo.toml
- **In-memory connection:** `sqlite:file:cerebrum?mode=memory&cache=shared`
- **Pool gotcha:** Each pool connection to `:memory:` gets a separate DB. Use shared-cache URI + `min_connections(1)` to keep the DB alive.
- **Migrations:** `sqlx::migrate!()` works with SQLite but needs SQLite-compatible SQL — cannot share PG migrations.
- **AnyPool verdict:** Does NOT translate `$1` → `?`. Useless for this case without full query rewrite.

### Architectural Approach: Trait-Based Backend Selection

```rust
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String>;
    async fn fetch_inferences(&self, offset: u32, limit: u32, filter_category: Option<&str>, filter_model: Option<&str>) -> Result<(Vec<InferenceLog>, i64), QueryError>;
    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError>;
    async fn fetch_savings_estimate(&self, hours: u32, model_costs: &dyn CostProvider, baseline_model: &str) -> Result<SavingsEstimate, QueryError>;
}
```

**Three implementations:**

```rust
pub enum DbBackend {
    Memory(MemoryBackend),     // Vec<InferenceRecord> + RwLock
    Sqlite(SqliteBackend),     // sqlx::SqlitePool (file-backed)
    Postgres(PostgresBackend), // sqlx::PgPool (current code)
}
```

**Configuration (via config.toml `[persistence]` section):**

```toml
# Persistence backend selection
[persistence]
backend = "memory"          # "memory" | "sqlite" | "postgres"
# sqlite_path = "./cerebrum.db"  # Only used when backend = "sqlite"
# database_url = "postgres://..."  # Only used when backend = "postgres"
# log_concurrency_limit = 100     # Max concurrent background log writes
# connection_retries = 3          # Postgres only: retry count
# retry_base_ms = 1000            # Postgres only: backoff base
```

**Env var override:** `DATABASE_URL` still works as an override — if set, forces `postgres` regardless of config file. This preserves backward compatibility and 12-factor app convention for production deploys.

**Resolution order:**
1. `DATABASE_URL` env var present → `postgres` (production override, always wins)
2. `[persistence]` section in config.toml → use `backend` field
3. Neither → default `memory`

| Setting | Config key | Env fallback | Default |
|---------|-----------|--------------|---------|
| Backend | `persistence.backend` | `DB_BACKEND` | `memory` |
| SQLite path | `persistence.sqlite_path` | `SQLITE_PATH` | `./cerebrum.db` |
| Postgres URL | `persistence.database_url` | `DATABASE_URL` | — |
| Log concurrency | `persistence.log_concurrency_limit` | `LOG_CONCURRENCY_LIMIT` | 100 |
| Connection retries | `persistence.connection_retries` | `DB_CONNECTION_RETRIES` | 3 |
| Retry base ms | `persistence.retry_base_ms` | `DB_RETRY_BASE_MS` | 1000 |

This aligns with the existing config.toml pattern (`[classifiers]`, `[regex_classifier]`, `[llm_classifier]`, `[[categories]]`, `[[routing]]`) — the persistence section becomes another top-level config block loaded by `config::load_persistence_config_from_value()`.

### Impact on Existing Code

| Component | Change Required |
|-----------|----------------|
| `AppState.persistence` | `Option<PersistenceConfig>` → `Option<Arc<dyn PersistenceBackend>>` (or always `Some`) |
| `PersistenceConfig` | Replaced by `DbBackend` enum implementing `PersistenceBackend` trait |
| `PersistenceConfig` methods | Moved into `PostgresBackend` impl (unchanged logic) |
| `log_inference()` | Signature: accepts `&dyn PersistenceBackend` instead of `Arc<PgPool>` |
| `insert_once()` | Stays in `PostgresBackend`; SQLite gets its own; Memory does `vec.push()` |
| `fetch_inferences()` | PG: unchanged. SQLite: `?` binds. Memory: `iter().filter()` |
| `fetch_latency_summary()` | PG: unchanged. SQLite: no PERCENTILE_CONT. Memory: compute in Rust |
| `fetch_savings_estimate()` | PG: unchanged. SQLite: datetime functions. Memory: compute in Rust |
| Dashboard routes | No change (call trait methods) |
| `main.rs` init | Backend selection logic (~30 lines) |
| Cargo.toml | Add `"sqlite"` to sqlx features |

### In-Memory Backend Design

```rust
pub struct MemoryBackend {
    records: Arc<RwLock<Vec<StoredInference>>>,
}

struct StoredInference {
    request_id: Uuid,
    status: String,
    category: Option<String>,
    upstream_model: Option<String>,
    duration_ms: Option<i32>,
    prompt_snippet: String,
    prompt_char_count: Option<i32>,
    created_at: chrono::DateTime<chrono::Utc>,
}
```

- **Insert**: `write().push(record)` — O(1)
- **Fetch with filters**: `read().iter().filter().skip().take()` — O(n), fine for dev datasets
- **Latency summary**: group by category, compute avg/p99 in Rust
- **Cost estimate**: same grouping + cost math
- **Concurrency**: `RwLock` allows concurrent reads, exclusive writes (matches async logging pattern)

### SQLite Backend Design

- **Connection**: `sqlx::SqlitePool` with file path (e.g. `./cerebrum.db`)
- **Schema init**: Hardcoded `CREATE TABLE IF NOT EXISTS` on startup (no migration directory — single table)
- **Bind params**: `?` instead of `$N`
- **Time filters**: `datetime('now', '-' || ? || ' hours')`
- **p99**: Fetch sorted durations with LIMIT, compute percentile in Rust
- **UUID**: Generated in app code (already the case), stored as TEXT

### Test Impact (Significant Improvement)

Currently: **all tests use `persistence: None`** — the entire persistence and dashboard-with-data path is untested in unit tests. Only a single `#[ignore]`-style integration test (requiring `DATABASE_URL`) exercises persistence.

With SQLite in-memory:
- `PersistenceConfig::in_memory()` constructor for tests — instant, no network
- Tests can exercise `log_inference` → `fetch_inferences` → dashboard rendering
- Each test gets a fresh isolated DB (unique shared-cache name per test)
- No Docker, no external dependencies

### SQLite Limitations to Accept

1. **No p99 latency** — `PERCENTILE_CONT` unavailable. Options: (a) return `None` for p99 on SQLite, (b) compute in Rust after fetching sorted durations, (c) skip p99 in dev mode.
2. **No native UUID validation** — stored as TEXT, validated in app code (already the case).
3. **Timestamp precision** — TEXT storage is fine; chrono handles parsing transparently.
4. **Not production-viable** — SQLite in-memory is ephemeral. Data lost on restart. This is explicitly a dev/test convenience, not a production path.

## Code References

- `src/persistence.rs:70` — `pub pool: Arc<PgPool>` (will become `DbPool`)
- `src/persistence.rs:92-93` — `DATABASE_URL` env check (decision point for fallback)
- `src/persistence.rs:266-267` — `PERCENTILE_CONT` + `::INTEGER` casts (PG-only)
- `src/persistence.rs:270` — `NOW() - interval '1 hour' * $1` (PG interval arithmetic)
- `src/persistence.rs:476` — `log_inference()` takes `Arc<PgPool>` directly
- `src/persistence.rs:528` — INSERT with `$1..$7` binds
- `src/main.rs:33` — `persistence: Option<persistence::PersistenceConfig>`
- `src/main.rs:66-75` — Graceful degradation when `from_env()` fails
- `src/main.rs:280` — `log_inference` call site with `persistence.pool.clone()`
- `migrations/001_create_inferences.sql` — PG-specific DDL (UUID, TIMESTAMPTZ, gen_random_uuid)
- `migrations/002_inferences_request_id_unique.sql` — `ADD CONSTRAINT` (PG-only ALTER)

## Architecture Insights

1. **Graceful degradation already exists** — `persistence: Option<...>` with `None` when no DB. The SQLite fallback replaces `None` with a working in-memory backend, reducing special-case handling.

2. **The enum approach aligns with existing patterns** — the codebase already uses enum dispatch for classifiers (`ClassifierChain` with `Arc<dyn IntentClassify>`). A `DbPool` enum is the same pattern.

3. **Lessons.md compliance** — "Favor dynamic WHERE clause building over duplicated SQL branches" (lesson 5) suggests the SQLite queries should also use dynamic WHERE construction, not per-filter branching.

4. **The `log_inference` fire-and-forget pattern works for both backends** — `tokio::spawn` + semaphore + retry logic is backend-agnostic.

5. **Schema simplification for SQLite is acceptable** — the dev/test backend doesn't need the full production schema fidelity (e.g., no UUID type enforcement, simplified p99).

## Historical Context (from prior changes)

- `context/archive/2026-05-26-data-persistence-async-logging/` — Original F-02 that established the PG-only persistence pattern.
- `context/changes/post-review-cleanup/plan.md` — Phase 5 introduces `sqlx::migrate!()` embedded migrations (already done). Compatible with adding a separate SQLite migration directory.

## Related Research

No prior research on SQLite or alternative backends exists in `context/changes/**/research.md`.

## Open Questions

1. **Should p99 be computed in Rust for SQLite?** Loading sorted duration values and computing percentile in application code adds memory pressure for large datasets — but for in-memory dev usage, datasets are small. Recommend: return `None` for p99 on SQLite (simplest).
2. **Should `Option<PersistenceConfig>` remain?** With SQLite always available, persistence never needs to be `None`. But an explicit `DISABLE_PERSISTENCE=true` env var could still suppress it. Recommend: keep `Option` but document it's only for explicit opt-out.
3. **Migration management** — Should SQLite migrations live in `migrations-sqlite/` or be hardcoded Rust strings? Recommend: hardcoded `CREATE TABLE` in a const — it's one table with three columns of interest and the schema won't diverge from PG at the same rate.
