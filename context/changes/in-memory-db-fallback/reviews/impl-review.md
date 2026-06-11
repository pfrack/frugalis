<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Three-Tier Persistence Backend

- **Plan**: context/changes/in-memory-db-fallback/plan.md
- **Scope**: Phases 1–7 of 7
- **Date**: 2026-06-11
- **Verdict**: NEEDS ATTENTION
- **Findings**: 1 critical, 5 warnings, 1 observation

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | FAIL |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Findings

### F1 — SQLite in-memory pool missing min_connections(1)

- **Severity**: ❌ CRITICAL
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:75
- **Detail**: The plan's "Critical Implementation Details" explicitly requires `min_connections(1)` to prevent the shared-cache in-memory DB from being garbage-collected on idle timeout. Current code only set `max_connections(1)`.
- **Fix**: Add `.min_connections(1)` to the SqlitePoolOptions builder.
- **Decision**: FIXED

### F2 — std::sync::RwLock in async context

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:35
- **Detail**: MemoryBackend used `std::sync::RwLock` which can block the tokio runtime thread under contention. Currently safe (lock never held across .await), but fragile to future refactoring.
- **Fix A ⭐ Recommended**: Switch to `tokio::sync::RwLock`
  - Strength: Eliminates the class of bugs entirely; idiomatic for async Rust.
  - Tradeoff: Slight API change (methods become async); negligible perf difference at demo scale.
  - Confidence: HIGH — standard practice for async Rust services.
  - Blind spot: None significant.
- **Fix B**: Add a code comment documenting the invariant
  - Strength: Zero code change; captures the constraint.
  - Tradeoff: Still fragile; relies on discipline, not compiler.
  - Confidence: MEDIUM — comments drift.
- **Decision**: FIXED via Fix A

### F3 — SQLite constructors panic instead of returning Result

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:72-80
- **Detail**: `from_uri()` and `from_path()` used `.expect()` on pool creation and schema init. A misconfigured sqlite_path panics the entire server. PostgresBackend's `from_env()` properly returns `Result<Self, String>`.
- **Fix A ⭐ Recommended**: Return `Result<Self, String>` from both methods
  - Strength: Matches PostgresBackend pattern; allows main.rs to log and fall back to memory gracefully.
  - Tradeoff: Requires updating call sites in main.rs and test helpers (~5 sites).
  - Confidence: HIGH — the Postgres precedent proves the pattern.
  - Blind spot: None significant.
- **Fix B**: Keep panics, add a documenting comment
  - Strength: Minimal change; schema init failure is arguably unrecoverable anyway.
  - Tradeoff: Inconsistent error handling across backends.
  - Confidence: MEDIUM — acceptable for dev-only backend.
- **Decision**: FIXED via Fix A

### F4 — Duplicated WHERE clause branches across backends

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/persistence.rs:460-480 (SQLite), :728-750 (Postgres)
- **Detail**: The dynamic WHERE clause building is duplicated between SQLite (`?` placeholders) and Postgres (`$N` placeholders). Violates lessons.md rule, but duplication is inherent to different SQL dialects with raw queries.
- **Fix**: Accept as intentional; add comment documenting why duplication exists.
- **Decision**: FIXED (comment added)

### F5 — Unbounded Vec growth in MemoryBackend

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence.rs:35
- **Detail**: MemoryBackend stored all records with no eviction or capacity cap. In a long-running dev session, memory would grow without bound.
- **Fix**: Add a 10,000-record capacity limit with oldest-record eviction on overflow.
- **Decision**: FIXED

### F6 — Test script uses non-functional env vars for persistence

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: manual-test/test.sh
- **Detail**: Script set `PERSISTENCE__BACKEND=sqlite` and `PERSISTENCE__SQLITE_PATH` env vars, but config.rs only reads from `[persistence]` in TOML. These env vars were silently ignored.
- **Fix**: Replace env vars with a test-specific config.toml that sets `[persistence] backend = "sqlite"`.
- **Decision**: FIXED

### F7 — Manual verification items 7.4–7.7 pending

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Success Criteria
- **Location**: N/A
- **Detail**: Four manual verification items were unchecked in the Progress section.
- **Fix**: Complete manual verification.
- **Decision**: FIXED
