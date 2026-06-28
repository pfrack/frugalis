---
date: 2026-06-28T11:11:42+02:00
researcher: kiro
git_commit: 70e808069e79dcd03d1166b2df95889f7ae12757
branch: code-structure-reorg
repository: frugalis
topic: "Replace sqlx with a better Rust database library"
tags: [research, codebase, persistence, sqlx, database, migration]
status: complete
last_updated: 2026-06-28
last_updated_by: kiro
---

# Research: Replace sqlx with a better Rust database library

**Date**: 2026-06-28T11:11:42+02:00
**Researcher**: kiro
**Git Commit**: 70e808069e79dcd03d1166b2df95889f7ae12757
**Branch**: code-structure-reorg
**Repository**: frugalis

## Research Question

Replace sqlx with Diesel or something newer/better — reduce verbosity, duplication, and friction in the persistence layer.

## Summary

The project uses **sqlx 0.8** with raw SQL (no compile-time `query!` macros). The main pain is **~400 lines of duplicated SQL** between Postgres and SQLite backends due to placeholder syntax differences (`$N` vs `?N`). After evaluating 5 alternatives, the recommendation is:

1. **Best fit (high risk)**: **bsql** — same `query!` macro for both PG and SQLite, compile-time validation, optional WHERE clauses built-in
2. **Safest choice (heavy)**: **SeaORM 2.0** — abstracts dialect differences via SeaQuery, mature ecosystem
3. **Least effort (incremental)**: **Upgrade to sqlx 0.9** — owned Arguments improve ergonomics but don't eliminate duplication

**Diesel is NOT recommended** — doesn't solve the dual-backend problem and adds ceremony for a single-table schema.

## Current State

### Architecture (`src/persistence/`)

```
backend.rs   — PersistenceBackend trait (4 methods) + DbBackend enum dispatch
sqlite.rs    — SqliteBackend: raw SQL with ?N placeholders, PRAGMA migrations
postgres.rs  — PostgresBackend: raw SQL with $N placeholders, sqlx::migrate!
types.rs     — InferenceRecord, InferenceLog, QueryError, helpers
memory.rs    — MemoryBackend (in-memory Vec for tests)
mod.rs       — PersistenceConfig, log_inference spawner
```

### Key Characteristics

- **Single table**: `inferences` with 14 columns
- **4 trait methods**: `insert_inference`, `fetch_inferences`, `fetch_latency_summary`, `fetch_savings_estimate`
- **Dynamic WHERE**: optional `category` and `upstream_model` filters
- **Aggregations**: COUNT, AVG, PERCENTILE_CONT (PG only), GROUP BY
- **No compile-time checking used**: despite sqlx `macros` feature being enabled, all queries use `sqlx::query()` (not `query!()`)
- **lessons.md rule**: "Favor dynamic WHERE clause building over duplicated SQL branches"

### Pain Points

1. **Duplicate SQL**: sqlite.rs and postgres.rs have nearly identical logic, differing only in placeholder syntax and date functions
2. **Manual `Row::try_get`**: every result row manually destructured field-by-field
3. **No type safety**: raw string SQL with no compile-time validation
4. **Migration divergence**: Postgres uses proper migration files; SQLite uses PRAGMA-based ALTER TABLE

## Detailed Findings

### 1. Diesel (v2.3.9 + diesel-async v0.9.2)

**Verdict: NOT RECOMMENDED for this use case**

| Aspect | Assessment |
|--------|-----------|
| Async | Via diesel-async; SQLite is `spawn_blocking` (fake async) |
| Dual-backend | Supported but query types are backend-specific — still need separate code |
| Dynamic WHERE | `into_boxed()` pattern — clean but boxed allocation |
| PERCENTILE_CONT | Requires raw SQL (`diesel::sql_query`) |
| Compile time | No DB connection needed (unlike sqlx `query!`) |
| Maintenance | Excellent — 14.1k stars, monthly releases |

**Why not**: Diesel's type system encodes the backend at compile time. You cannot write one generic function for both Postgres and SQLite without complex trait bounds. Since the project already has a clean trait dispatch pattern, Diesel would add ceremony (schema.rs, derive macros, diesel_cli) without eliminating the duplication.

### 2. SeaORM 2.0

**Verdict: SAFEST CHOICE if you want to eliminate duplication**

| Aspect | Assessment |
|--------|-----------|
| Async | First-class, built on sqlx 0.9 |
| Dual-backend | YES — SeaQuery generates correct SQL for each backend |
| Dynamic WHERE | Excellent: `Condition::all().add()` + `apply_if(Option, closure)` |
| PERCENTILE_CONT | Via `raw_sql!` macro or custom expressions |
| Migration | Full system: sea-orm-cli, up/down SQL files |
| Compile time | Moderate — entity codegen + proc macros |
| Maintenance | Very active — SeaQL team, v2.0 released Jan 2026 |

**How it solves the problem**: One entity definition, one set of queries using SeaQuery's cross-dialect SQL builder. The `.apply_if()` pattern handles optional filters elegantly:

```rust
Entity::find()
    .apply_if(filter_category, |q, cat| q.filter(Column::Category.eq(cat)))
    .apply_if(filter_model, |q, model| q.filter(Column::UpstreamModel.eq(model)))
    .order_by_desc(Column::CreatedAt)
    .paginate(db, limit)
```

**Trade-offs**: Heavy dependency tree for a single-table use case. Requires entity generation tooling. The full ORM abstraction layer adds conceptual overhead for simple queries.

### 3. bsql (v0.27.0, April 2026)

**Verdict: BEST ARCHITECTURAL FIT, highest risk**

| Aspect | Assessment |
|--------|-----------|
| Async | First-class (RPITIT, no block_in_place) |
| Dual-backend | YES — same `query!` macro for PG and SQLite |
| Dynamic WHERE | `[AND col = $param: Option<T>]` syntax, compile-time validated |
| PERCENTILE_CONT | If PG supports it, bsql validates it — pure SQL |
| Migration | None built-in; `bsql migrate --check` validates against cache |
| Compile time | Higher (validates every query at build time) |
| Maintenance | RISK: single developer (smir-ant), 24 releases, very new |

**How it solves the problem**: Single query macro works for both databases. Optional WHERE clauses are first-class:

```rust
let records = bsql::query!(
    "SELECT created_at, prompt_snippet, category, upstream_model, duration_ms
     FROM inferences
     WHERE created_at >= $since: OffsetDateTime
     [AND category = $cat: Option<&str>]
     [AND upstream_model = $model: Option<&str>]
     ORDER BY created_at DESC LIMIT $limit: i64 OFFSET $offset: i64"
).fetch_all(&pool).await?;
```

**Unique features**: N+1 detection, compile-time EXPLAIN plans, migration safety checking, smart NULL inference (COUNT(*) → i64 not Option<i64>), offline cache (`.bsql/` directory).

**Risks**:
- Very new (first release early 2026, ~3 months old)
- Single developer
- Max 10 optional clauses (generates 2^N variants)
- "Built with Claude Code" — unknown long-term maintenance story
- No community yet, no production battle-testing evidence

### 4. sqlx 0.9.0 (May 2026)

**Verdict: LEAST EFFORT, incremental improvement**

| Aspect | Assessment |
|--------|-----------|
| Key improvement | Owned `Arguments` without lifetimes |
| Dual-backend | Still separate placeholder syntax |
| Dynamic WHERE | Easier with owned Args but still manual |
| Breaking changes | `SqlSafeStr` for string interpolation |
| Maintenance | Extremely active, 9.5M downloads/month |

**What it improves**: The owned Arguments API eliminates the lifetime complexity that makes dynamic query building painful. This directly benefits the `fetch_inferences` pattern.

**What it doesn't solve**: Still need separate SQL strings for `?` vs `$N` placeholders.

### 5. sqlxplus (v0.2.9, April 2026)

**Verdict: Interesting thin wrapper, high bus-factor risk**

| Aspect | Assessment |
|--------|-----------|
| Approach | `#[derive(CRUD)]` + QueryBuilder on top of sqlx |
| Dual-backend | YES — DB type auto-inferred from pool |
| Dynamic WHERE | `.and_eq()`, `.and_like()`, `.group_by()`, `.limit()` |
| Risk | Single maintainer, ~1651 downloads |

**How it works**: Define model once with derive macros, pass either PgPool or SqlitePool — same code. The QueryBuilder handles dialect differences internally.

## Recommendation

### For Frugalis specifically:

Given the constraints (single table, 4 methods, both PG + SQLite, existing clean trait abstraction, `lessons.md` rule against ORMs):

#### Option A: bsql (recommended if comfortable with risk)

**Migration effort**: Medium-high (rewrite all queries with `bsql::query!`)
**Benefit**: Eliminates ALL duplication — one query works on both backends
**Risk mitigation**: Vendor-fork the crate; it's MIT/Apache-2.0

```toml
[dependencies]
bsql = { version = "0.27", features = ["chrono", "uuid", "tls"] }
```

The `PersistenceBackend` trait and `DbBackend` enum can collapse into a single implementation that takes a `bsql::Pool` (which internally handles both PG and SQLite).

#### Option B: Upgrade to sqlx 0.9 + refactor (recommended if risk-averse)

**Migration effort**: Low
**Benefit**: Owned Args improve dynamic queries; potential to use `sqlx::Any` pool for some unification

```toml
[dependencies]
sqlx = { version = "0.9", features = ["postgres", "sqlite", "runtime-tokio", "tls-rustls", "macros", "uuid", "chrono", "migrate"] }
```

Refactor the common logic into shared helper functions that take `sqlx::Any` or generate SQL strings parameterized by dialect.

#### Option C: SeaORM (recommended if the schema will grow)

**Migration effort**: High (new entity format, migration system, tooling)
**Benefit**: Full abstraction, works if more tables are added later

Only makes sense if the persistence layer is expected to grow significantly beyond one table.

## Migration Plan (Option A — bsql)

### Phase 1: Setup & Proof of Concept
1. Add `bsql` dependency alongside `sqlx` (both can coexist)
2. Set up `.bsql/` cache directory, add to `.gitignore` or commit
3. Rewrite `insert_inference` as single `bsql::query!` INSERT
4. Verify it works against both PG and SQLite

### Phase 2: Migrate Queries
5. Rewrite `fetch_inferences` with `[AND ...]` optional clauses
6. Rewrite `fetch_latency_summary` (note: PERCENTILE_CONT is PG-only — may need backend-specific query here)
7. Rewrite `fetch_savings_estimate`

### Phase 3: Simplify Architecture
8. Collapse `SqliteBackend` + `PostgresBackend` into single `BsqlBackend`
9. Remove `DbBackend` enum — single backend that takes bsql pool
10. Keep `MemoryBackend` for unit tests (or use bsql's test isolation)
11. Remove sqlx dependency entirely

### Phase 4: Migration System
12. Keep existing Postgres migration files, run via external tool (dbmate/refinery)
13. For SQLite, convert PRAGMA migrations to proper SQL migration files
14. Use `bsql migrate --check` for safety validation

### Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| bsql abandoned | Vendor-fork (25K SLOC, MIT); fall back to sqlx 0.9 |
| PERCENTILE_CONT not unified | Keep PG-specific raw query for latency summary |
| Compile time increase | Use `.bsql/` offline cache; validate in CI only |
| Unknown bugs | Comprehensive test suite already exists; run against both backends |

## Migration Plan (Option B — sqlx 0.9 upgrade)

### Phase 1: Upgrade
1. Bump `sqlx` from 0.8 to 0.9 in Cargo.toml
2. Fix any breaking changes (SqlSafeStr, API renames)
3. Verify all tests pass

### Phase 2: Reduce Duplication
4. Extract shared query logic into helper functions
5. Use `sqlx::Any` pool type where possible to unify simple queries
6. For complex queries (aggregations), keep backend-specific implementations but share row-mapping logic

### Estimated Effort
- Phase 1: 1-2 hours
- Phase 2: 4-6 hours

## Code References

- `src/persistence/sqlite.rs:1-569` — Full SQLite backend with PRAGMA migrations and 4 query implementations
- `src/persistence/postgres.rs:1-430` — Full Postgres backend with connection retry and migration
- `src/persistence/backend.rs:1-295` — PersistenceBackend trait definition and DbBackend dispatch
- `src/persistence/types.rs:1-404` — InferenceRecord struct (14 fields), query helpers
- `src/persistence/mod.rs:1-56` — PersistenceConfig, log_inference spawner
- `Cargo.toml:19` — sqlx 0.8 dependency with features

## Architecture Insights

1. The trait-dispatch pattern (`PersistenceBackend` → `DbBackend` enum) is well-designed and would survive any migration — the consumer code doesn't know or care which backend runs underneath.

2. The `lessons.md` rule "Favor dynamic WHERE clause building over duplicated SQL branches" already points toward the exact problem that bsql's `[AND ...]` syntax or SeaORM's `apply_if` solve at the library level.

3. The project explicitly chose against an ORM (comment in sqlite.rs source). bsql respects this — it's pure SQL, not DSL/method chains. SeaORM violates this principle.

4. The `MemoryBackend` (1044 LOC) duplicates all query logic in pure Rust — this is actually the largest backend. With bsql's test isolation (schema-per-test in 2ms), the MemoryBackend may become unnecessary.

## Historical Context

- `context/changes/code-structure-reorg/` — Active change restructuring the codebase; this research may feed into that effort
- `context/foundation/lessons.md` — "Favor dynamic WHERE clause building" rule directly relevant

## Open Questions

1. **PERCENTILE_CONT portability**: This is PG-only SQL. With bsql, can we have one query that works on both, or do we need a fallback (manual percentile in Rust, as SQLite already does)?
2. **bsql maturity**: Should we wait for v1.0 before adopting? Or is the risk acceptable given the vendor-fork escape hatch?
3. **MemoryBackend fate**: If bsql test isolation replaces it, that's -1044 LOC. But MemoryBackend is also used for "no persistence configured" mode — keep it?
4. **Migration coexistence**: During migration, can sqlx and bsql coexist in the same binary without conflicts (both use PgPool/SqlitePool types)?
