# Data Persistence Async Logging Pipeline Implementation Plan

## Overview

Implement F-02 by adding PostgreSQL-backed persistence for inference metadata as an asynchronous side path. The core objective is to preserve primary proxy responsiveness while capturing enough inference data to unlock S-01 observability and S-02 dashboard views.

## Current State Analysis

The service has auth-gated routes and test scaffolding, but no data layer, migration system, or runtime database configuration. Existing CI validates auth behavior and build integrity, so this plan extends the same guardrail pattern to persistence without changing the current route protection contracts.

### Key Discoveries:

- Proxy lifecycle integration point is the protected completion route in [src/main.rs](src/main.rs#L51).
- Placeholder completion handler exists and is currently synchronous in [src/main.rs](src/main.rs#L44).
- Environment-driven fail-fast configuration pattern already exists in [src/auth.rs](src/auth.rs#L16).
- Current dependency set has no PostgreSQL driver or query layer in [Cargo.toml](Cargo.toml#L1).
- Deploy contract currently defines auth env keys but not database connectivity in [render.yaml](render.yaml#L8).
- CI currently runs auth tests and release build only in [.github/workflows/deploy.yml](.github/workflows/deploy.yml#L20).

## Desired End State

The application can asynchronously persist one inference metadata record per completed proxy request into PostgreSQL with this contract: category, upstream model, duration, timestamp, prompt snippet, request_id, and status. Logging failures never block or delay the main response path; they are handled in background with one short retry and structured error logging.

Verification is complete when automated tests validate persistence contract behavior (unit + selective DB integration) and manual checks confirm successful writes, privacy-safe snippet storage, and no visible response-path regression.

### Key Discoveries:

- Roadmap defines F-02 as non-blocking async logging and names required fields in [context/foundation/roadmap.md](context/foundation/roadmap.md#L79).
- PRD guardrail requires minimized snippet storage and excludes full prompt body in [context/foundation/prd.md](context/foundation/prd.md#L19).
- Existing module and test style favors small Rust modules with co-located tests in [src/main.rs](src/main.rs#L71) and [src/auth.rs](src/auth.rs#L155).

## What We're NOT Doing

- No full prompt persistence.
- No streaming analytics pipeline or event bus.
- No multi-tenant data model.
- No billing-grade cost attribution.
- No advanced retry queues, dead-letter handling, or guaranteed delivery semantics.
- No dashboard implementation work beyond enabling downstream data availability.

## Implementation Approach

Use a thin persistence layer with PostgreSQL access and explicit inference-record contract, wired into app state at startup. Logging is emitted after request handling completes and runs in a detached background task with bounded retry policy. Schema management is versioned via repository SQL migrations to keep environments reproducible and auditable.

## Critical Implementation Details

### Timing & lifecycle

The logging task must be scheduled only after request handling has completed and required metadata is finalized. Any database I/O, retries, or error formatting must remain outside the synchronous response path so response streaming latency is unaffected.

### State sequencing

Capture duration, status, and snippet before enqueueing background work. The background task receives an immutable payload and performs at most one retry; final failure is logged and dropped.

## Phase 1: Data Contract and Runtime Configuration

### Overview

Define and codify the persistence contract and environment contract so all environments can initialize a database connection and apply schema consistently.

### Changes Required:

#### 1. Migration contract for inference records

**File**: migrations/001_create_inferences.sql (new file)

**Intent**: Introduce a versioned SQL migration that creates the inference table and essential indexes for downstream reads.

**Contract**: Create table for request_id, status, category, upstream_model, duration_ms, created_at/timestamp, and prompt_snippet with an index strategy optimized for recent-record reads and request_id lookup.

#### 2. Persistence dependency contract

**File**: Cargo.toml

**Intent**: Add the minimum crates needed for async PostgreSQL persistence, record identifiers, and safe runtime time handling.

**Contract**: Dependency set supports async PostgreSQL access and typed record handling while remaining compatible with current Axum/Tokio runtime.

#### 3. Runtime environment contract

**File**: render.yaml

**Intent**: Declare required runtime database environment keys in deployment configuration.

**Contract**: Deployment requires DATABASE_URL plus optional persistence tuning envs (snippet limit, retry count, timeout budget) without embedding secrets in source.

### Success Criteria:

#### Automated Verification:

- Migration SQL parses successfully against PostgreSQL.
- Project compiles with new persistence dependencies: cargo build --release.
- Existing auth and route tests continue to pass: cargo test auth and cargo test routes_auth.

#### Manual Verification:

- DATABASE_URL is configured in deployment environment.
- Migration can be applied to Supabase target without manual table edits.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Persistence Layer and Async Logger

### Overview

Implement the internal persistence module and background logging flow with non-blocking failure semantics and one-retry policy.

### Changes Required:

#### 1. Persistence module and data contracts

**File**: src/persistence.rs (new file)

**Intent**: Centralize DB initialization, record model contract, insert operation, and one-retry background write behavior.

**Contract**: Expose startup initializer from env config and a non-blocking log enqueue/write API that accepts finalized inference payload fields (including request_id, status, snippet).

#### 2. Snippet minimization and payload shaping

**File**: src/persistence.rs

**Intent**: Enforce privacy guardrail by trimming and normalizing prompt snippets before persistence.

**Contract**: Snippet policy stores only bounded preview text; full prompt body is never persisted. Snippet extraction is performed in the completion handler from request JSON before background enqueue, and only minimized snippet text is passed to persistence APIs.

#### 3. Structured failure logging policy

**File**: src/persistence.rs

**Intent**: Make persistence failures observable without impacting request flow.

**Contract**: Final write failure emits structured error log containing request_id and failure class after one retry attempt.

### Success Criteria:

#### Automated Verification:

- Unit tests pass for snippet policy and payload validation: cargo test persistence.
- Unit tests pass for retry/drop behavior in background logging: cargo test persistence_retry.
- Full test suite still passes: cargo test.

#### Manual Verification:

- Simulated DB unavailability does not break protected proxy response behavior.
- Failed inserts are visible in logs with request_id and do not crash the process.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: App Integration, Verification, and Deploy Gate

### Overview

Wire persistence into application lifecycle, add selective integration tests, and update CI/deploy checks so regressions are blocked before production deployment.

### Changes Required:

#### 1. App state and lifecycle integration

**File**: src/main.rs

**Intent**: Inject persistence state at startup and attach async logging trigger after request completion path.

**Contract**: `build_app` accepts both auth and persistence state, completion handler signature is expanded to receive request payload + shared state, and the handler emits one finalized inference logging event per completed request after response assembly with no synchronous wait on DB operations. Persistence state ownership is centralized in a shared app-state struct (Arc-cloned into router/test app constructors) so production and tests use the same wiring shape. The plan explicitly avoids middleware body buffering/replay and keeps snippet capture at handler level.

#### 2. Selective DB integration tests

**File**: src/main.rs

**Intent**: Validate end-to-end persistence behavior when DATABASE_URL is available while keeping local test runs lightweight.

**Contract**: Integration tests for insert/read contract are conditionally executed only when DB test configuration is present.

#### 3. CI verification gate for persistence path

**File**: .github/workflows/deploy.yml

**Intent**: Extend pre-deploy checks to include persistence-focused test command(s).

**Contract**: Deployment pipeline fails before webhook trigger if persistence contract tests fail. CI defines deterministic behavior for DB-gated tests: if integration-test DB env is present, run migration bootstrap then `cargo test persistence_integration`; if env is absent, skip that command explicitly while still running auth/route tests and release build.

### Success Criteria:

#### Automated Verification:

- Persistence integration tests pass when test DB config is provided: cargo test persistence_integration.
- Existing route and auth tests remain green: cargo test routes_auth and cargo test auth.
- Release build remains successful: cargo build --release.

#### Manual Verification:

- Triggering protected completion route creates a new row in inference table with expected fields.
- Stored snippet respects configured length and excludes full prompt.
- When DB is temporarily unreachable, client response remains successful and failure appears in service logs.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

## Testing Strategy

### Unit Tests:

- Snippet normalization and truncation behavior.
- Request_id and status contract validation.
- Retry behavior: one retry max, then drop.
- Failure logging behavior does not panic.

### Integration Tests:

- Insert contract validation against PostgreSQL schema.
- Read-back validation for recent records ordering fields required by dashboard.
- Conditional test execution when DATABASE_URL is present.

### Manual Testing Steps:

1. Apply migration on Supabase and verify table/index creation.
2. Run service with valid auth and database env vars.
3. Send authenticated completion request and verify row insertion.
4. Temporarily break DB connectivity and verify response path remains healthy.
5. Restore DB and verify subsequent requests are logged again.

## Performance Considerations

Persistence path must stay off the synchronous response path. Runtime overhead target is near-zero for the request completion path, with DB latency and retries isolated to background tasks. Avoid unbounded in-memory queueing and cap retries to one short attempt.

## Migration Notes

- Use versioned SQL migration files committed in repository.
- Apply migration to Supabase before enabling persistence integration tests in CI environment.
- If schema changes are needed later, add new migration files rather than editing existing ones in place.

## References

- Roadmap requirement: context/foundation/roadmap.md
- Product guardrails: context/foundation/prd.md
- Existing app lifecycle: src/main.rs:14
- Existing config loading pattern: src/auth.rs:16
- Deploy contract: render.yaml
- CI pipeline: .github/workflows/deploy.yml

## Progress

> Convention: - [ ] pending, - [x] done. Append — <commit sha> when a step lands. Do not rename step titles. See references/progress-format.md.

### Phase 1: Data Contract and Runtime Configuration

#### Automated

- [ ] 1.1 Migration SQL parses successfully against PostgreSQL
- [ ] 1.2 Project compiles with new persistence dependencies: cargo build --release
- [ ] 1.3 Existing auth and route tests continue to pass: cargo test auth and cargo test routes_auth

#### Manual

- [ ] 1.4 DATABASE_URL is configured in deployment environment
- [ ] 1.5 Migration can be applied to Supabase target without manual table edits

### Phase 2: Persistence Layer and Async Logger

#### Automated

- [ ] 2.1 Unit tests pass for snippet policy and payload validation: cargo test persistence
- [ ] 2.2 Unit tests pass for retry/drop behavior in background logging: cargo test persistence_retry
- [ ] 2.3 Full test suite still passes: cargo test

#### Manual

- [ ] 2.4 Simulated DB unavailability does not break protected proxy response behavior
- [ ] 2.5 Failed inserts are visible in logs with request_id and do not crash the process

### Phase 3: App Integration, Verification, and Deploy Gate

#### Automated

- [ ] 3.1 Persistence integration tests pass when test DB config is provided: cargo test persistence_integration
- [ ] 3.2 Existing route and auth tests remain green: cargo test routes_auth and cargo test auth
- [ ] 3.3 Release build remains successful: cargo build --release

#### Manual

- [ ] 3.4 Triggering protected completion route creates a new row in inference table with expected fields
- [ ] 3.5 Stored snippet respects configured length and excludes full prompt
- [ ] 3.6 When DB is temporarily unreachable, client response remains successful and failure appears in service logs
