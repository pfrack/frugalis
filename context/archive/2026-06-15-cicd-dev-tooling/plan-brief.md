# CI/CD & Dev-Tooling — Plan Brief

> Full plan: `context/changes/cicd-dev-tooling/plan.md`
> Research: `context/changes/cicd-dev-tooling/research.md`

## What & Why

Introduce a `justfile` (task runner), `Dockerfile` (cargo-chef build), `docker-compose.yml` (postgres + OTel collector), `.env.example`, and update `test-plan.md`. Eliminates command drift across AGENTS.md/README.md/deploy.yml, enables local postgres integration tests, and closes the OTel verification gap.

## Starting Point

No Makefile, Dockerfile, docker-compose, or .env.example exists. Commands are scattered across 3+ docs with drift. `persistence_integration_*` tests silently skip in CI (no DATABASE_URL). OTel client has zero local verification scaffolding.

## Desired End State

- `just` lists all recipes; `just ci` mirrors the deploy.yml gate sequence
- `docker compose up -d postgres` provisions local postgres for integration tests
- `docker compose --profile otel up -d` starts OTel collector with debug exporter
- `just run-otel` starts cerebrum with OTel exporting to local collector
- `just test-persistence-integration` runs postgres-backed tests against compose
- `.env.example` documents all env vars with source anchors

## Key Decisions Made

| Decision | Choice | Why |
|----------|--------|-----|
| Task runner | `just` | Rust-ecosystem favorite (ripgrep, fd, helix); cleanest arg forwarding (`just test foo`) |
| Compose structure | Single file with profiles | One file to maintain; `docker compose up` is the entrypoint |
| OTel version | Pin to 0.116.1 | Reproducibility; same image across dev/CI |
| Scope | justfile + Dockerfile + compose + OTel config + test-plan.md + .env.example | PR CI workflow is a follow-up |

## Scope

**In scope:** justfile (~30 recipes), Dockerfile (cargo-chef), docker-compose.yml (postgres + OTel profiles), deploy/otel-collector/config.yaml, .env.example, test-plan.md §3-§6 updates

**Out of scope:** PR CI workflow, render.yaml OTel env vars, README.md rewrite, .sqlx/ offline cache, Jaeger UI

## Architecture / Approach

Implementation order: Dockerfile → docker-compose + OTel config → justfile → .env.example → test-plan.md. Each piece composes multiplicatively. The justfile is the single source of truth for all commands; compose provides the infrastructure; Dockerfile enables containerized builds.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. Dockerfile | Multi-stage cargo-chef build | Image size; SQLX_OFFLINE propagation |
| 2. compose + OTel config | Postgres + OTel collector provisioning | Port alignment; healthcheck timing |
| 3. justfile | ~30 named recipes | Command drift if recipes don't match deploy.yml |
| 4. .env.example | Env var template | Missing vars; stale defaults |
| 5. test-plan.md | §3-§6 references to new tooling | Markdown link integrity |

**Prerequisites:** Docker installed locally; `just` installed (`cargo install just`)
**Estimated effort:** ~2-3 sessions across 5 phases

## Open Risks & Assumptions

- `just` must be installed on every dev machine and CI runner (not zero-install like Makefile)
- compose postgres healthcheck may need retry logic in justfile recipe if startup is slow
- `.env.example` may drift if new env vars are added without updating it

## Success Criteria (Summary)

- `just ci` passes (mirrors deploy.yml gate sequence)
- `docker compose up -d postgres && just test-persistence-integration` passes
- `docker compose --profile otel up -d && just run-otel` shows traces in collector logs
- All `cargo test <name>` references in test-plan.md replaced with `just test TEST=<name>`
