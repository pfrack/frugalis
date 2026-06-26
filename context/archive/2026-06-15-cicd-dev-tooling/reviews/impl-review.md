<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: CI/CD & Dev-Tooling

- **Plan**: context/changes/cicd-dev-tooling/plan.md
- **Scope**: All 5 phases of 5
- **Date**: 2026-06-26
- **Verdict**: NEEDS ATTENTION
- **Findings**: 1 critical, 8 warnings, 5 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | FAIL |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | NOT VERIFIED |

## Findings

### F1 — pprof extension exposed on all interfaces (0.0.0.0)

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Safety & Quality
- **Location**: deploy/otel-collector/config.yaml:10
- **Detail**: pprof extension enabled and bound to `0.0.0.0:1777`, mapped in docker-compose.yml. Exposes goroutine/heap profiling data (stack traces, memory profiles) over HTTP to anyone on the network.
- **Fix**: Remove `pprof` from extensions list and config block, or restrict to `127.0.0.1`.
- **Decision**: FIXED

### F2 — Docker images use mutable tags (no SHA pinning)

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: Dockerfile:3,19
- **Detail**: Both `cargo-chef` and `bookworm-slim` images use mutable tags. A tag overwrite produces unreproducible builds.
- **Fix**: Pin to SHA digests or add a CI check that logs the digest on build.
- **Decision**: FIXED

### F3 — Postgres port bound to all interfaces

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: docker-compose.yml:5
- **Detail**: `"5432:5432"` binds postgres to all interfaces (`0.0.0.0`). On shared machines this exposes the DB to LAN.
- **Fix**: Restrict to `"127.0.0.1:5432:5432"`.
- **Decision**: FIXED

### F4 — OTel health-check port 13133 exposed to host

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: docker-compose.yml:25
- **Detail**: Port 13133 exposed to host but only needed internally for Docker healthcheck.
- **Fix**: Remove from `ports` or use `127.0.0.1:13133:13133`.
- **Decision**: FIXED

### F5 — No restart policy on compose services

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: docker-compose.yml:2-44
- **Detail**: All services default to `restart: no` — a crash takes them down permanently.
- **Fix**: Add `restart: unless-stopped` to all services.
- **Decision**: FIXED

### F6 — Cerebrum service has no healthcheck

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: docker-compose.yml:34
- **Detail**: Compose waits for postgres healthy before starting cerebrum, but cannot detect if cerebrum itself is alive after startup.
- **Fix**: Add a healthcheck hitting the `/health` endpoint.
- **Decision**: FIXED

### F7 — justfile compose-up recipe omits --profile app

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: justfile:223-224
- **Detail**: `compose-up` recipe runs `docker compose up -d` without `--profile app`, so cerebrum never launches.
- **Fix A ⭐ Recommended**: Update `compose-up` to `docker compose --profile app up -d`.
  - Strength: Matches intent and makes the recipe useful.
  - Tradeoff: None.
  - Confidence: HIGH — one-line change.
  - Blind spot: None significant.
- **Fix B**: Remove the `app` profile from cerebrum service (only `otel` needs profiling).
  - Strength: Simpler compose file.
  - Tradeoff: cerebrum starts even when you only want postgres.
  - Confidence: MEDIUM — depends on usage pattern.
  - Blind spot: None significant.
- **Decision**: FIXED

### F8 — Raw {{ ARGS }} splat interpolation

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: justfile:76
- **Detail**: `test *ARGS` recipe uses raw `{{ ARGS }}` — shell metacharacters in user input could be injected.
- **Fix**: Use `{{ ARGS | quote }}` if just version supports it, or restructure to avoid passing arbitrary strings through the shell.
- **Decision**: FIXED

### F9 — No resource limits on compose services

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: docker-compose.yml:2-44
- **Detail**: No memory/CPU limits on any service; postgres or collector could starve the host.
- **Fix**: Add conservative resource limits.
- **Decision**: FIXED

### F10 — .env not in .dockerignore

- **Severity**: 👁 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: .dockerignore
- **Detail**: If `.env` exists at build time with live secrets, it enters the build context.
- **Fix**: Add `.env` to `.dockerignore`.
- **Decision**: FIXED
