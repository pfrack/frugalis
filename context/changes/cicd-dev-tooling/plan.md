# CI/CD & Dev-Tooling Implementation Plan

## Overview

Introduce dev-tooling to simplify local development, CI/CD, and manual verification. This change adds a `justfile` (task runner), `Dockerfile` (cargo-chef multi-stage build), `docker-compose.yml` (postgres + OTel collector), `deploy/otel-collector/config.yaml`, and `.env.example`. It also updates `test-plan.md` to reference the new workflow.

**Motivation**: Today there is no Makefile, Dockerfile, docker-compose, or .env.example. The build/test/lint matrix is documented in three places (AGENTS.md, README.md, test-plan.md) with drift. The `persistence_integration_*` tests silently skip in CI. The OTel client has zero scaffolding for local verification.

## Current State Analysis

- **No task runner**: Commands scattered across AGENTS.md, README.md, deploy.yml, test-plan.md
- **No Dockerfile**: No containerized build; Render uses native Rust runtime
- **No docker-compose**: No local postgres or OTel collector scaffolding
- **CI gaps**: No PR CI workflow; `persistence_integration_*` tests skip silently; no fmt/clippy/slow_tests gate
- **Command drift**: `cargo test slow_tests` vs `cargo test slow_tests -- --test-threads=1`; clippy `--all-targets` inconsistency
- **OTel verification gap**: No local collector setup; `OTEL_ENABLED=true` with no observable output

## Desired End State

After this plan:
1. `just` lists all available recipes; `just ci` mirrors the deploy.yml gate sequence
2. `docker compose up -d postgres` provisions a local postgres for integration tests
3. `docker compose --profile otel up -d` starts an OTel collector with debug exporter
4. `just run-otel` starts cerebrum with OTel enabled, exporting to local collector
5. `just test-persistence-integration` runs postgres-backed tests against compose postgres
6. `.env.example` documents all required/optional env vars
7. `test-plan.md` references the new justfile targets and compose workflow

### Key Discoveries:

- `src/telemetry.rs:42-49` — OTel gracefully no-ops when `OTEL_ENABLED` unset/false
- `src/persistence.rs:1219-1263` — `TestDb` uses testcontainers with `postgres:16-alpine`
- `src/persistence.rs:180-182` — app auto-migrates at startup; `make migrate` is optional for dev
- `src/main.rs:2107-2183` — `persistence_integration_*` tests skip cleanly when DB unreachable
- `.github/workflows/deploy.yml:25-40` — the gate sequence `just ci` will mirror
- `config.toml:32-33` — default backend is `memory`; postgres requires `DATABASE_URL`

## What We're NOT Doing

- **PR CI workflow** — `.github/workflows/ci.yml` is a follow-up change
- **render.yaml OTel env vars** — belongs to the `opentelemetry-integration` follow-up
- **README.md rewrite** — belongs to a separate `readme-bootstrap` change
- **.sqlx/ offline cache** — separate decision; not blocking this change
- **Jaeger UI** — v2 enhancement; v1 uses OTel collector `debug` exporter only
- **cargo-nextest** — not needed; standard test harness is sufficient

## Implementation Approach

Implementation order matters: Dockerfile → docker-compose + OTel config → justfile → .env.example → test-plan.md. Each piece composes multiplicatively: `just ci` assumes the justfile exists; `just test-persistence-integration` assumes postgres is up via compose; `just run-otel` assumes the OTel collector is running.

---

## Phase 1: Dockerfile

### Overview

Multi-stage Dockerfile using `cargo-chef` for layer caching. Base image `lukemathwalker/cargo-chef:0.1.77-rust-1.85.0-bookworm`; runtime `debian:bookworm-slim` with `ca-certificates`.

### Changes Required:

#### 1. Dockerfile

**File**: `Dockerfile`

**Intent**: Create a reproducible, cached container build for cerebrum. The cargo-chef pattern separates dependency compilation (cached) from application compilation (only on code change), reducing rebuild from ~10-12 min to ~1-2 min on incremental changes.

**Contract**: Three-stage build (planner → builder → runtime). `SQLX_OFFLINE=true` propagated to prevent sqlx macro compile-time DB queries. `--locked` for hermetic builds. `ca-certificates` in runtime for outbound HTTPS via reqwest (rustls). Binary at `/usr/local/bin/cerebrum`. `USER nobody` for non-root execution.

#### 2. .dockerignore

**File**: `.dockerignore`

**Intent**: Exclude build artifacts, git history, and dev files from Docker context to speed up `docker build`.

**Contract**: Exclude `target/`, `.git/`, `.sqlx/`, `context/`, `manual-test/`, `scripts/`, `*.md`, `*.log`, `/tmp/`.

### Success Criteria:

#### Automated Verification:

- `docker build -t cerebrum:test .` completes successfully
- `docker run --rm cerebrum:test --validate` exits 0 (the `--validate` CLI at `src/main.rs:104-138`)
- Image size is reasonable (< 200 MB for runtime stage)

#### Manual Verification:

- Incremental code change rebuilds only the final stage (not dependency cache)
- Binary runs correctly inside the container

---

## Phase 2: docker-compose.yml + OTel Collector Config

### Overview

Single `docker-compose.yml` with profiles: `postgres` always-on, `otel-collector` behind `--profile otel`, `cerebrum` app behind `--profile app`. Companion `deploy/otel-collector/config.yaml` with `debug` exporter for stdout inspection.

### Changes Required:

#### 1. docker-compose.yml

**File**: `docker-compose.yml`

**Intent**: Provide one-command provisioning of postgres and OTel collector for local development and integration testing.

**Contract**:
- **postgres service**: `postgres:16-alpine` (matches testcontainers at `src/persistence.rs:1236`), port `5432:5432`, credentials `POSTGRES_USER=test`, `POSTGRES_PASSWORD=test`, `POSTGRES_DB=test`, healthcheck `pg_isready -U test -d test`, named volume `cerebrum-postgres-data`
- **otel-collector service**: `otel/opentelemetry-collector-contrib:0.116.1` (pinned), ports `4317:4317` (gRPC), `4318:4318` (HTTP), `13133:13133` (health), volume mount `./deploy/otel-collector/config.yaml:/etc/otelcol/config.yaml:ro`, healthcheck on 13133, profile `otel`
- **cerebrum service**: builds from `Dockerfile`, ports `10000:10000`, env from `.env`, depends on postgres, profile `app`

#### 2. deploy/otel-collector/config.yaml

**File**: `deploy/otel-collector/config.yaml`

**Intent**: Configure OTel collector to receive OTLP/HTTP (port 4318, matching `src/telemetry.rs:60-63` Protocol::HttpBinary) and export to stdout via `debug` exporter for local inspection.

**Contract**:
```yaml
receivers:
  otlp:
    protocols:
      grpc: { endpoint: 0.0.0.0:4317 }
      http: { endpoint: 0.0.0.0:4318 }
exporters:
  debug: { verbosity: detailed }
extensions:
  health_check: { endpoint: 0.0.0.0:13133 }
  pprof: { endpoint: 0.0.0.0:1777 }
service:
  extensions: [health_check, pprof]
  pipelines:
    traces:  { receivers: [otlp], exporters: [debug] }
    metrics: { receivers: [otlp], exporters: [debug] }
    logs:    { receivers: [otlp], exporters: [debug] }
```

### Success Criteria:

#### Automated Verification:

- `docker compose up -d postgres` starts postgres and healthcheck passes
- `docker compose --profile otel up -d` starts both postgres and otel-collector
- `curl http://localhost:13133` returns 200 (collector health)
- `just test-persistence-integration` passes against compose postgres

#### Manual Verification:

- `docker compose logs otel-collector` shows received spans when app sends traces
- No port collisions (10000, 5432, 4317, 4318, 13133 are all distinct)

---

## Phase 3: justfile

### Overview

~30 named recipes wrapping the existing cargo command surface. `just` is the default goal (lists recipes). `ci` mirrors the exact deploy.yml gate sequence. `gates` mirrors the 2026-06-14 verification gate form.

### Changes Required:

#### 1. justfile

**File**: `justfile`

**Intent**: Single source of truth for all build/test/lint/run commands. Eliminates command drift across AGENTS.md, README.md, deploy.yml, and test-plan.md.

**Contract**:

**Settings:**
- `set dotenv-load` — auto-load `.env` if present
- `set shell := ["bash", "-euo", "pipefail", "-c"]` — consistent shell behavior

**Meta recipes:**
- `default` (alias `help`) — list all recipes with descriptions
- `print-tokens` — diagnose missing env vars

**Format:**
- `fmt` — `cargo fmt`
- `fmt-check` — `cargo fmt --check` (gate form)

**Lint:**
- `lint` — advisory `cargo clippy --all-targets`
- `lint-strict` — gate form `cargo clippy --all-targets -- -D warnings`
- `lint-otel` — `cargo clippy --features otel --all-targets -- -D warnings`

**Type-check:**
- `check` — `cargo check`
- `check-otel` — `cargo check --features otel`

**Build:**
- `build` — `cargo build`
- `build-otel` — `cargo build --features otel`
- `build-release` — `cargo build --release --locked`
- `build-release-otel` — `cargo build --release --locked --features otel`

**Test (fast):**
- `test *ARGS` — `cargo test $ARGS` (forwards args)
- `test-otel` — `cargo test --features otel`
- `test-auth` — `cargo test auth`
- `test-routes-auth` — `cargo test routes_auth`
- `test-persistence` — `cargo test persistence::tests`

**Test (DB integration):**
- `test-persistence-integration` — `docker compose up -d postgres && cargo test persistence_integration`
- `test-slow` — `cargo test slow_tests -- --test-threads=1`

**Test (focused/phase):**
- `test-chain` — `cargo test chain`
- `test-snippet` — `cargo test snippet`
- `test-format-sse` — `cargo test format_sse`
- `test-streaming-error` — `cargo test streaming_error`
- `test-json-shape` — `cargo test json_shape`

**DB / migration:**
- `migrate` — `sqlx migrate run` (redundancy check; app auto-migrates at startup)
- `migrate-status` — `sqlx migrate info`
- `migrate-revert` — `sqlx migrate revert`

**Run:**
- `run` — `RUST_LOG=info cargo run` (sensible dev defaults)
- `run-otel` — `RUST_LOG=info OTEL_ENABLED=true OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run --features otel`
- `run-release` — `RUST_LOG=info cargo run --release`
- `run-otel-release` — `RUST_LOG=info OTEL_ENABLED=true OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run --release --features otel`
- `validate-config` — `cargo run -- --validate`

**Manual / harness:**
- `manual` — exec `manual-test/run.sh` (interactive)
- `manual-basic` — exec `scripts/manual_tests.sh --basic`
- `manual-auto` — exec `manual-test/run.sh --auto`
- `manual-persistence` — 3-tier memory/sqlite/postgres
- `manual-fewshot` — focused fewshot subset

**CI composite:**
- `ci` — mirrors `.github/workflows/deploy.yml:25-45` exactly: `fmt-check`, `lint-strict`, `test`, `test-slow`, `build-release`
- `ci-otel` — `ci` + `lint-otel` + `test-otel`
- `gates` — `test && test-slow && lint-strict && fmt-check`

**Clean:**
- `clean` — `cargo clean`
- `clean-all` — `cargo clean` + remove `/tmp/cerebrum-*` artifacts

**OTel compose helpers:**
- `otel-up` — `docker compose --profile otel up -d`
- `otel-down` — `docker compose --profile otel down`
- `otel-logs` — `docker compose logs -f otel-collector`
- `compose-up` — `docker compose up -d`
- `compose-down` — `docker compose down`

### Success Criteria:

#### Automated Verification:

- `just` lists all recipes (exit 0)
- `just fmt-check` passes
- `just lint-strict` passes
- `just test` passes (all fast tests)
- `just build-release` produces `target/release/cerebrum`
- `just ci` runs the full gate sequence and passes

#### Manual Verification:

- `just run` starts cerebrum with sensible defaults
- `just test TEST=auth` forwards args correctly
- `just test-persistence-integration` runs against compose postgres

---

## Phase 4: .env.example

### Overview

Template file documenting all required and optional environment variables with sane defaults.

### Changes Required:

#### 1. .env.example

**File**: `.env.example`

**Intent**: Provide a copy-paste starting point for local development. Documents the env var contract from `src/auth.rs:18-20`, `src/main.rs:481-484`, `src/telemetry.rs:39-40`, and `src/persistence.rs:127-128`.

**Contract**:
```bash
# Required — auth (src/auth.rs:18-20)
PROXY_API_BEARER_TOKEN=test-token-123
DASHBOARD_BASIC_USER=admin
DASHBOARD_BASIC_PASSWORD=admin

# Optional — persistence (src/persistence.rs:127-128)
# Leave empty to use in-memory backend (src/main.rs:386-407)
DATABASE_URL=postgres://test:test@localhost:5432/test

# Optional — server port (src/main.rs:481-484, default: 10000)
# PORT=10000

# Optional — OpenTelemetry (src/telemetry.rs:39-49)
# OTEL_ENABLED=true
# OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
# OTEL_EXPORTER_OTLP_HEADERS=

# Optional — logging (default: info)
# RUST_LOG=info
```

### Success Criteria:

#### Automated Verification:

- `cp .env.example .env && just run` starts cerebrum successfully
- `just test-persistence-integration` works with `.env` containing `DATABASE_URL`

#### Manual Verification:

- All env vars in `.env.example` match the codebase's actual env var usage
- Comments reference source file anchors

---

## Phase 5: test-plan.md Updates

### Overview

Update `context/foundation/test-plan.md` §3-§6 to reference the new justfile targets and compose workflow. Bump freshness ledger dates.

### Changes Required:

#### 1. test-plan.md — §3 Phased Rollout

**File**: `context/foundation/test-plan.md`

**Intent**: Update Phase 4 row to reference `just ci` and `just gates` as the tooling that enables the CI floor.

**Contract**: Phase 4 row (line 73) — change folder column gains `cicd-dev-tooling`; goal extends to include "wire `just ci` and `just gates` into a new PR CI workflow" and "wire `just test-persistence-integration` into a workflow that provisions postgres via compose".

#### 2. test-plan.md — §4 Stack

**File**: `context/foundation/test-plan.md`

**Intent**: Add compose row to the stack table; mark e2e row with compose-based local verification note.

**Contract**:
- Add row: `Local dev / OTel verification` | `Docker Compose v2` | 2.20+ | `docker-compose.yml` with postgres + OTel collector profiles
- Update `e2e` row (line 90): note that "compose + justfile gives a local e2e analog; CI e2e is out of scope"

#### 3. test-plan.md — §5 Quality Gates

**File**: `context/foundation/test-plan.md`

**Intent**: Add "PR CI workflow" gate row; mark existing rows as "wired via `just lint-strict` and `just fmt-check`".

**Contract**: New row: "PR CI (`.github/workflows/ci.yml`): required; catches lint+typecheck+test+slow+build+compose-services-up". Update "lint + typecheck" row to note wiring via justfile.

#### 4. test-plan.md — §6 Cookbook

**File**: `context/foundation/test-plan.md`

**Intent**: Replace bare `cargo test <test_name>` instructions with `just test TEST=<name>` equivalents.

**Contract**:
- §6.1 (line 136): replace "`cargo test <test_name>` (fast) or `cargo test slow_tests`" with "`just test TEST=<name>` for fast tests, `just test-slow` for slow tests"
- §6.2 (line 144): same pattern
- §6.3 (line 151): same pattern
- §6.4 (line 158): same pattern
- §6.5 (line 166): same pattern

#### 5. test-plan.md — §8 Freshness Ledger

**File**: `context/foundation/test-plan.md`

**Intent**: Bump dates after edits.

**Contract**: Update "Strategy (§1–§5) last reviewed" and "Stack versions last verified" to today's date.

### Success Criteria:

#### Automated Verification:

- `grep -c "just" context/foundation/test-plan.md` shows new references
- No broken markdown links in test-plan.md

#### Manual Verification:

- All `cargo test <name>` references in §6 are replaced with `just test TEST=<name>`
- Phase 4 row accurately reflects the new tooling

---

## Testing Strategy

### Unit Tests:

- No new Rust code — this change is infrastructure only
- Existing tests continue to pass via `just test`

### Integration Tests:

- `just test-persistence-integration` validates compose postgres wiring
- `just ci` validates the full gate sequence

### Manual Testing Steps:

1. `just` — verify recipe listing
2. `just run` — verify cerebrum starts with sensible defaults
3. `docker compose up -d postgres && just test-persistence-integration` — verify postgres integration
4. `docker compose --profile otel up -d && just run-otel` — verify OTel traces appear in collector logs
5. `just ci` — verify full gate sequence passes

## Performance Considerations

- **Dockerfile layer caching**: cargo-chef reduces incremental rebuild from ~10-12 min to ~1-2 min
- **compose postgres**: named volume persists data across `docker compose up/down`; no re-migration needed
- **just recipes**: zero overhead; just wraps cargo commands

## Migration Notes

- **Existing CI**: `.github/workflows/deploy.yml` remains unchanged; this plan does not modify it
- **Existing scripts**: `scripts/manual_tests.sh` and `manual-test/run.sh` remain; justfile recipes delegate to them
- **AGENTS.md / README.md**: remain unchanged; this plan does not rewrite documentation (separate follow-up)

## References

- Research: `context/changes/cicd-dev-tooling/research.md`
- Deploy workflow: `.github/workflows/deploy.yml:25-40`
- Test plan: `context/foundation/test-plan.md`
- Lessons: `context/foundation/lessons.md:57` (PR scope drift rule)
- Telemetry: `src/telemetry.rs:42-49` (OTel no-op gate)
- Persistence: `src/persistence.rs:1219-1263` (TestDb + testcontainers)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Dockerfile

#### Automated

- [x] 1.1 `docker build -t cerebrum:test .` completes successfully
- [x] 1.2 `docker run --rm cerebrum:test --validate` exits 0
- [x] 1.3 Image size < 200 MB for runtime stage

#### Manual

- [ ] 1.4 Incremental code change rebuilds only the final stage

### Phase 2: docker-compose.yml + OTel Collector Config

#### Automated

- [ ] 2.1 `docker compose up -d postgres` starts and healthcheck passes
- [ ] 2.2 `docker compose --profile otel up -d` starts both services
- [ ] 2.3 `curl http://localhost:13133` returns 200

#### Manual

- [ ] 2.4 `just test-persistence-integration` passes against compose postgres
- [ ] 2.5 No port collisions verified (10000, 5432, 4317, 4318, 13133)

### Phase 3: justfile

#### Automated

- [ ] 3.1 `just` lists all recipes (exit 0)
- [ ] 3.2 `just fmt-check` passes
- [ ] 3.3 `just lint-strict` passes
- [ ] 3.4 `just test` passes (all fast tests)
- [ ] 3.5 `just build-release` produces `target/release/cerebrum`
- [ ] 3.6 `just ci` runs full gate sequence and passes

#### Manual

- [ ] 3.7 `just run` starts cerebrum with sensible defaults
- [ ] 3.8 `just test TEST=auth` forwards args correctly

### Phase 4: .env.example

#### Automated

- [ ] 4.1 `cp .env.example .env && just run` starts cerebrum successfully

#### Manual

- [ ] 4.2 All env vars match codebase usage; comments reference source anchors

### Phase 5: test-plan.md Updates

#### Automated

- [ ] 5.1 `grep -c "just" context/foundation/test-plan.md` shows new references
- [ ] 5.2 No broken markdown links in test-plan.md

#### Manual

- [ ] 5.3 All `cargo test <name>` references in §6 replaced with `just test TEST=<name>`
- [ ] 5.4 Phase 4 row accurately reflects new tooling
