# Replace sqlx with sea-query — Plan Brief

> Full plan: `context/changes/replace-sqlx/plan.md`
> Research: `context/changes/replace-sqlx/research.md`

## What & Why

Replace raw SQL string duplication in the persistence layer by introducing sea-query 1.0 as a cross-dialect query builder on top of sqlx 0.9. The current codebase has ~1000 LOC split across two nearly-identical backend files (Postgres + SQLite) differing only in placeholder syntax and date functions. This makes every new column or filter a two-place change.

## Starting Point

`src/persistence/sqlite.rs` (569 LOC) and `src/persistence/postgres.rs` (430 LOC) each implement the same 4 `PersistenceBackend` trait methods with raw SQL strings. Dynamic WHERE clauses use manual string concatenation with `bind_count` tracking. Postgres uses `sqlx::migrate!` for schema; SQLite uses PRAGMA-based ALTER TABLE.

## Desired End State

A single `sql_backend.rs` (~200 LOC) builds queries once via sea-query, renders them to the correct dialect (PG or SQLite), and executes via sqlx. `DbBackend` has 2 variants (Memory | Sql). Migrations use refinery (works for both backends). Adding a filter or column is a one-place change.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Query builder library | sea-query 1.0 (standalone, no ORM) | Mature (4+ years), eliminates dialect duplication without violating anti-ORM philosophy | Plan |
| Time filter approach | Compute cutoff in Rust, pass as chrono param | Eliminates dialect-specific date SQL entirely (`datetime('now',...)` vs `NOW() - interval`) | Plan |
| MemoryBackend | Keep as-is | Still needed for "no persistence" mode and fast unit tests | Plan |
| Migration system | refinery 0.9.2 | Clean break from sqlx migrate; supports both PG and SQLite from same files | Plan |
| DbBackend enum | Memory \| Sql (2 variants) | Simplest dispatch; SqlBackend handles both PG and SQLite internally via dialect flag | Plan |
| Migration strategy | Phase by phase, remove sqlx migrate at end | Each phase independently testable; no big-bang risk | Plan |
| PERCENTILE_CONT | PG: `Expr::cust(...)`, SQLite: Rust-side `percentile_99()` | PG-only SQL function; SQLite already does this in Rust today | Research |

## Scope

**In scope:**
- Upgrade sqlx 0.8 → 0.9
- Add sea-query 1.0 + sea-query-sqlx 0.9
- New unified `SqlBackend` replacing both sqlite.rs and postgres.rs
- Switch migrations to refinery
- Collapse DbBackend to 2 variants

**Out of scope:**
- MemoryBackend changes
- PersistenceBackend trait signature changes
- Code outside `src/persistence/`
- New features or schema changes
- Adding an ORM

## Architecture / Approach

```
┌──────────────────────────────────────┐
│         PersistenceBackend trait      │
├──────────────┬───────────────────────┤
│ MemoryBackend│      SqlBackend       │
│ (unchanged)  │  ┌─────────────────┐  │
│              │  │ sea-query builds │  │
│              │  │ Query → SQL+Vals │  │
│              │  ├────────┬────────┤  │
│              │  │  PG    │ SQLite │  │ ← dialect flag selects QueryBuilder
│              │  │  Pool  │  Pool  │  │
│              │  └────────┴────────┘  │
└──────────────┴───────────────────────┘
```

sea-query builds the query once → `.build_sqlx(dialect)` renders correct SQL → sqlx executes it. One implementation, two outputs.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Upgrade deps | sqlx 0.9 + sea-query compile cleanly | sqlx 0.9 breaking changes |
| 2. SqlBackend (CRUD) | INSERT + fetch_inferences working via sea-query | Bridging pattern correctness |
| 3. Aggregation queries | latency + savings queries migrated | PERCENTILE_CONT dialect fork |
| 4. Remove old backends | Delete sqlite.rs + postgres.rs, refinery migrations | Ensuring nothing references old code |

**Prerequisites:** None — self-contained within persistence layer
**Estimated effort:** ~2-3 sessions across 4 phases

## Open Risks & Assumptions

- sea-query-sqlx 0.9's `AssertSqlSafe` wrapping is the documented pattern for dynamic SQL in sqlx 0.9 — needs verification
- refinery's `IF NOT EXISTS` handling for databases that already ran sqlx migrations (existing `_sqlx_migrations` table is harmless but refinery starts fresh)
- Assumption: sea-query's `Func::count()`, `Func::avg()`, and `Expr::cust(...)` produce correct SQL for both dialects

## Success Criteria (Summary)

- `sqlite.rs` and `postgres.rs` are deleted; one `sql_backend.rs` handles both
- All existing tests pass with no behavioral changes
- Dashboard shows identical data before and after migration
