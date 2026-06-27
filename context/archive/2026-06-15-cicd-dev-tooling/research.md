---
date: 2026-06-15T22:06:50+00:00
researcher: Claude
git_commit: fec14d4
branch: testing-cricital-path-regression-guards
repository: cerebrum
topic: "Simplify CI/CD and testing with a Makefile, Dockerfile, and docker-compose stack; verify the OTel client locally; refresh the test plan"
tags: [research, codebase, ci-cd, docker, docker-compose, makefile, taskfile, render, opentelemetry, postgres, developer-experience, test-plan]
status: complete
last_updated: 2026-06-15
last_updated_by: Claude
last_updated_note: "Added follow-up research for task-runner alternatives (Makefile vs Taskfile vs just vs cargo-make) and Render free-tier Docker support verification"
---

# Research: CI/CD & Dev-Tooling Simplification (Task runner, Dockerfile, docker-compose, OTel)

**Date**: 2026-06-15T22:06:50+00:00
**Researcher**: Claude
**Git Commit**: fec14d4
**Branch**: testing-cricital-path-regression-guards
**Repository**: cerebrum

## Research Question

> "Simplify integration CI/CD, testing by introducing Makefile, introducing Docker-compose, dockerfile to test properly manually and to check otel client. Probably that touches also testplan."

Decompose: how can a `Makefile`, `Dockerfile`, and `docker-compose.yml` reduce the surface area of distinct commands a developer or CI run has to remember; close the gap that currently blocks manual verification of the OpenTelemetry client (`src/telemetry.rs`); and what minimal edits to `context/foundation/test-plan.md` are required to reflect the new workflow?

## Summary

Cerebrum today has **no** `Makefile`, **no** `Dockerfile`, **no** `docker-compose.yml`, **no** `.env.example`, **no** `.dockerignore` (verified with `find` and `ls` on the working tree). The build/test/lint matrix is documented in three places — `AGENTS.md`, `README.md`, and `context/foundation/test-plan.md` — each with a different subset of commands and at least one well-known drift (e.g. `cargo test slow_tests` vs `cargo test slow_tests -- --test-threads=1`).

The CI workflow (`.github/workflows/deploy.yml:1-61`) runs only on push to main: `cargo test auth`, `cargo test routes_auth`, `cargo test persistence`, conditional `cargo test persistence_integration`, `cargo build --release`, then a `curl` to the Render deploy webhook. There is **no PR CI workflow, no `cargo fmt --check`, no `cargo clippy`, no `cargo test slow_tests`, no coverage gate** — even though the test plan calls for all of them in Phase 4 (`context/foundation/test-plan.md:73`).

The OpenTelemetry client (`src/telemetry.rs:42-150`) is feature-gated behind `--features otel`, auto-detects `OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4318` per OTel spec), and gracefully no-ops when `OTEL_ENABLED` is unset/false (`src/telemetry.rs:43-49`). But there is **no scaffolding today** for a developer to actually see traces flow — a `docker compose up otel-collector` one-liner would close that gap, paired with a `make run-otel` target that wires the env vars.

The persistence cross-backend tests already use `testcontainers` (`src/persistence.rs:1230-1262`, image `postgres:16-alpine`) — the only thing missing is CI provisioning. A compose file plus a one-line CI change turns `persistence_integration_*` from "always skipped" to "always run", matching Test Plan Phase 2 intent (`test-plan.md:71`).

**Recommended baseline:**

1. **Task runner** (see §C.0 for the choice) — at the repo root, with ~30 named targets wrapping the existing command surface. `help` is the default goal. `ci` mirrors the exact deploy.yml gate sequence. `test TEST=name` forwards to `cargo test name`. Recommendation: **Makefile** as the default (zero install, universal on every CI runner, matches every plan/docs reference in the repo) with a documented escape hatch to **Taskfile (go-task)** if YAML syntax is preferred (see §C.0 for the full comparison and the trade-off matrix).
2. **`Dockerfile`** at the repo root, multi-stage with `cargo-chef` for layer caching (per `context/foundation/infrastructure.md:104` risk register). Base `lukemathwalker/cargo-chef:0.1.77-rust-1.85.0-bookworm`; runtime `debian:bookworm-slim` with `ca-certificates`. `SQLX_OFFLINE=true` propagated. `--features otel` toggled by build arg.
3. **`docker-compose.yml`** at the repo root, single file with profiles: `postgres` always-on, `otel-collector` behind `--profile otel`, optional `cerebrum` app service behind `--profile app`. Companion `deploy/otel-collector/config.yaml` with the `debug` exporter for stdout inspection.
4. **Minimal `test-plan.md` updates** (§3, §4, §5, §6.1) to reference the new targets and the new compose-based verification path for OTel.

The combined scope fits cleanly inside a single change folder (already created as `context/changes/cicd-dev-tooling/change.md`).

## Detailed Findings

### A. Current state — what is and isn't there

- **Files searched**: full `find` of repo root (no `Makefile*`, `Dockerfile*`, `docker-compose*`); full `ls` of `context/changes/`, `context/foundation/`, `.github/workflows/`; full `ls` of `scripts/`, `manual-test/`, `migrations/`, `supabase/`.
- **What exists today**:
  - `render.yaml` (18 lines) — Render native Rust runtime, `cargo build --release`, no OTel env vars.
  - `.github/workflows/deploy.yml` (61 lines) — deploy-only, no PR CI, no fmt/clippy/slow_tests gate.
  - `scripts/manual_tests.sh` (260 lines) — three modes (`--auto`, `--basic`, default=interactive).
  - `manual-test/run.sh` (1021 lines), `manual-test/test.sh` (1129 lines), `manual-test/lib.sh` (115 lines), `manual-test/TEST.md`, `manual-test/README.md`.
  - `migrations/001_create_inferences.sql`, `002_inferences_request_id_unique.sql`, `003_add_prompt_char_count.sql` — applied at startup by `sqlx::migrate!()` in `src/persistence.rs:1256` and `:180-182`.
  - `config.toml` — operational settings, no `PORT` (defaults to 10000 via `src/main.rs:481-484`).
  - `.sqlx/` offline cache — **not committed** (verified by `ls` and `.gitignore`).
  - `.env.example` — **not present**.
  - `.dockerignore` — **not present**.
- **What is missing**: `Makefile`, `Dockerfile`, `docker-compose.yml`, any compose override, any OTel collector config.

### B. Command inventory & duplication

The Explore agent produced a 60+ row command inventory (B1–B5 builds, T1–T21 tests, L1–L7 clippy, F1–F3 fmt, R1–R13 run, M1–M5 sqlx, S1–S17 shell, M-h1–M-h8 harness entry points, P1–P5 planned). The most relevant findings for the Makefile design:

- **No single source of truth** for the test-command set. The same commands are listed in `AGENTS.md:11-15`, `README.md:118-128`, `context/foundation/test-plan.md:136,144,151,158,166`, `.github/workflows/deploy.yml:25-40`, and `context/changes/testing-critical-path-regression-guards/change.md:31` — each with slightly different coverage. See D1 below.
- **`cargo test slow_tests` vs `cargo test slow_tests -- --test-threads=1`** (drift D2). AGENTS.md:15 says the bare form; the *actual* gate run on 2026-06-14 includes `--test-threads=1` (`testing-critical-path-regression-guards/change.md:31`). The Makefile target must own the correct form.
- **`render.yaml` does not include `--features otel`** (drift D3). The OTel plan called this out as intentional per `impl-review-2` F1, but it remains an active gap when the production binary needs to ship with telemetry. A Makefile target `build-release-otel` will exist before the YAML catches up.
- **Clippy drift** (D4, D5). The verification gate form is `cargo clippy --all-targets -- -D warnings` (`testing-critical-path-regression-guards/change.md:31`); some archived plans used `cargo clippy -- -D warnings` (no `--all-targets`) which is insufficient. The `lint-strict` Makefile target should pin the form.

Full duplication & drift section reproduced from the Explore agent's output (file `tool_ecd63c1f60018XLvMxfBtYHhHe`):

| # | Issue | Evidence | Risk |
|---|-------|----------|------|
| D1 | No single source of truth for the test-command set across AGENTS.md, README.md, test-plan §6, deploy.yml, per-phase plans | README:118-128 vs. AGENTS.md:11-15 vs. deploy.yml:25-40 | medium |
| D2 | `cargo test slow_tests -- --test-threads=1` is the *gates* form, not the doc'd form | testing-critical-path-regression-guards/change.md:31 vs. AGENTS.md:15 | medium |
| D3 | `render.yaml:5` build command lacks `--features otel` (intentional per impl-review-2 F1) | opentelemetry-integration/plan.md:374 | low |
| D4 | Clippy invocation drift across historical plans | archive/2026-05-26-auth-scaffold-access-keys/plan.md:136 vs. testing-critical-path-regression-guards/change.md:31 | low |
| D5 | Some archived clippy invocations omit `--all-targets` (insufficient) | fewshot-classifier/plan.md:195 vs. testing-critical-path-regression-guards/change.md:31 | medium |
| D6 | `cargo test persistence` runs without DB; `cargo test persistence_integration` requires DB — only deploy.yml captures this split | deploy.yml:34-40 vs. AGENTS.md:11-15 | low |
| D7 | `cargo test --lib` / `--bins` split recommended in reqwest-upstream-routing review never adopted | archive/2026-06-01-reqwest-upstream-routing/reviews/impl-review.md:102 | low |
| D8 | Three overlapping harness scripts (`scripts/manual_tests.sh`, `manual-test/run.sh`, `manual-test/test.sh`) with no Makefile-style entry to disambiguate | scripts/manual_tests.sh:223-235, manual-test/run.sh:38-1021, manual-test/test.sh:1074-1129 | medium |
| D9 | `RUST_LOG=info cargo run` doesn't centralise the full env-var set needed | AGENTS.md:5-7 vs. config-ux/plan.md:53 | low |
| D10 | `sqlx migrate run` in CI is a *no-op duplicate* of `sqlx::migrate!()` at startup (src/persistence.rs:1256) but kept as a fail-loud schema-drift check | deploy.yml:36 vs. src/persistence.rs:1256 | low |
| D11 | `--features otel` is intentionally absent from CI per user decision; tracked in plan addendum | opentelemetry-integration/plan.md:374 vs. render.yaml:5 | low |
| D12 | OTel env vars documented in README but no scaffolding to actually verify them locally | README.md:159-160, src/telemetry.rs:43-56 | low |
| D13 | `cargo test auth` and `cargo test routes_auth` are substring matches, not module filters | deploy.yml:27-28 | low |
| D14 | `cargo test persistence` substring pulls in `persistence_integration_*` which gates on DATABASE_URL | deploy.yml:34 | low |
| D15 | No named targets for per-phase test subsets (chain, snippet, format_sse, streaming_error, json_shape) | testing-critical-path-regression-guards/plan.md:281,390,558-561,764-769 | low |

### C.0 Task runner choice

The user explicitly opened the door to alternatives. Below is the realistic candidate set for a small Rust project. **Mage (Go-based) is out** — wrong language for the project and the user already flagged it. Other Rust-ecosystem runners (cargo-script, cargo-xtask, build.rs) are also out — too heavy for "wrap a few cargo commands". The honest candidates are **Makefile**, **Taskfile (go-task)**, **just (justfile)**, and **cargo-make (Makefile.toml)**.

| Dimension | Makefile | Taskfile (`go-task`) | `just` | `cargo-make` (`Makefile.toml`) |
|---|---|---|---|---|
| Install | Universal on every Linux/macOS box (incl. GitHub Actions `ubuntu-latest`); absent on stock Windows | Single static binary; `brew install go-task`, `apt install go-task`, scoop, or `sh -c "$(curl ...)"`; one extra prereq | Single static binary; `cargo install just` (Rust-only install path) or distro packages; one extra prereq | `cargo install cargo-make` — one extra prereq; pure Rust |
| Syntax | Tab-indented Make DSL; shell-quoting footguns; no native YAML | YAML; clean, no tab-vs-space issue | `justfile` Make-like DSL but modern (no tabs required, functions, settings) | TOML; cargo-native |
| Cross-platform shell | Calls `/bin/sh` (POSIX) on Linux/macOS, `cmd.exe` on Windows; tricky to make portable | Calls the user's shell (bash/zsh/pwsh) but each task can declare its own `cmds:` block — portable by design | Calls `/bin/sh` on Unix, `cmd.exe` on Windows — same caveat as Make | Calls `sh`/`cmd` per task, with `script: ["sh", "-c"]` overrides |
| First-class Windows | No (workable via WSL/Cygwin) | Yes | Yes | Yes |
| Cargo integration | Indirect (`cargo test` etc.) | Indirect | Indirect | Direct (Rust-aware: task names like `cargo-make test`, `cargo-make ci`) |
| Help / introspection | DIY with `awk` + `## description` convention | Built-in `task --list` with `desc:` field | Built-in `just --list` with comment-as-doc convention | Built-in `cargo make --list` with `description` field |
| Arg forwarding | `$(filter-out $@,$(MAKECMDGOALS))` + `%: ; @:` rule (the canonical idiom) | `task test -- auth` (native CLI flag passthrough) | `just test auth` (native CLI flag passthrough — cleanest) | `cargo make test ARGS="auth"` (env-var convention) |
| CI cache / reproducibility | File-only; no version lock unless you commit a specific Make version | `Taskfile.yml` + `version:` field; tasks can declare a `go-task` min version | `justfile` syntax stable, binary pinned by `cargo install just --locked` | TOML stable, binary pinned via `cargo install cargo-make --locked` |
| Pre-commit / hooks | External (e.g. `lefthook.yml`) | Built-in `pre-commit:` / `post:` task fields | External | External |
| Community (Rust projects) | Ubiquitous; every CI doc uses it | Growing (tokio-rs/axum CI uses Taskfile-adjacent patterns); popular in Go ecosystem | Popular in Rust (used by ripgrep, deno, bun, helix, fd, etc.) | Niche; seen in some larger Rust codebases |
| Learning curve for new dev | None (everyone knows it) | Low (YAML is universal) | Low (Make-like, but cleaner) | Medium (cargo-make-specific TOML keys) |
| Project's existing footprint | AGENTS.md, README.md, test-plan.md, all archive plans, all review reports, all impl-reviews reference `cargo X` — adding `make Y` is a 1-line wrapper per command | Same shape, but the file is `Taskfile.yml` not `Makefile` | Same shape, file is `justfile` | Same shape, file is `Makefile.toml` (Rust community finds this name confusing) |
| "I just want to run a test" path | `make test TEST=foo` | `task test foo` | `just test foo` | `cargo make test TEST=foo` |

**Sources for the comparison** (the four runners' canonical docs):
- `Taskfile`: `https://taskfile.dev/usage/` (task DSL, includes, vars, OS-specific commands); installation `https://taskfile.dev/installation/`.
- `just`: `https://github.com/casey/just` (justfile syntax, modules, settings, .env auto-loading); installation `https://github.com/casey/just#packages`.
- `cargo-make`: `https://github.com/sagiegurari/cargo-make` (TOML config, tasks, conditionals, script_runner).
- Make: GNU Make Manual `https://www.gnu.org/software/make/manual/html_node/index.html` (target-specific variables, automatic variables, .PHONY).

**Trade-off analysis for cerebrum specifically:**

- **Makefile** wins on **zero install** and **universal recognition**. The downside is the shell-quoting landmines and Windows friction. The project deploys to Render (Linux) and the dev box is Linux per `AGENTS.md` (Polish locale + bash in the env dump), so the Windows friction is moot.
- **Taskfile** wins on **YAML ergonomics** and **first-class Windows support**. Costs: one extra prereq on every dev machine + CI runner; a new file name (`Taskfile.yml`) that none of the archive plans, AGENTS.md, or test-plan reference today. Adoption cost: the entire team's mental model has to shift from `make X` to `task X`.
- **`just`** wins on **arg forwarding ergonomics** (`just test foo` is the cleanest of the four). Costs: similar to Taskfile (extra prereq). Community fit is strong in the Rust ecosystem (ripgrep, fd, helix all use it). Adoption cost: same as Taskfile.
- **cargo-make** wins on **Rust-nativeness** (TOML config, `cargo make` invocation). Costs: the file is named `Makefile.toml` which collides visually with `Makefile`; the community is smaller; the TOML schema is cargo-make-specific rather than universal.

**Recommendation: Makefile (default), Taskfile as documented escape hatch.** Three reasons:

1. **Adoption cost is the dominant cost.** Every existing doc, every archive plan, every review report, AGENTS.md, and the test plan all reference raw `cargo X` invocations. Wrapping them in `make X` is a one-line target per command. Wrapping them in `task X` / `just X` is the same one-line per command **plus** a one-paragraph "we use Taskfile" preamble in every doc, and a "go-task not installed" error the first time a new dev clones the repo. Makefile is the lowest-friction choice.
2. **The project has no Windows target.** The risk register (context/foundation/infrastructure.md) is Linux-only (Render, testcontainers, OTel collector). Makefile's POSIX-only shell is fine.
3. **The escalation path is cheap.** If/when Windows support matters, swapping `Makefile` for `Taskfile.yml` is a mechanical rename of the same 30 targets. The dependency and compose work is unaffected.

**If the team prefers YAML:** Taskfile is the strongest alternative; `just` is a close second. The plan step should surface this choice to the user as a single question before drafting targets.

### C. Proposed target set (runner-agnostic)

The Explore agent's proposed target inventory (28 named targets, all additive wrappers, no behavior change). Reproduced in condensed form here; the full table is in the Explore agent's output file:

- **Meta**: `help` (default), `print-tokens` (diagnose missing env vars).
- **Format**: `fmt`, `fmt-check` (matches `testing-critical-path-regression-guards/change.md:31` gate).
- **Lint**: `lint` (advisory `cargo clippy --all-targets`), `lint-strict` (gate form `--all-targets -- -D warnings`), `lint-otel` (matches `opentelemetry-integration/reviews/impl-review-3.md:29`).
- **Type-check**: `check`, `check-otel` (matches `opentelemetry-integration/plan.md:113,158,215`).
- **Build**: `build`, `build-otel`, `build-release` (matches `AGENTS.md:11`, `deploy.yml:45`), `build-release-otel` (matches `opentelemetry-integration/plan.md:62,244`; documents D3 drift in a header comment).
- **Test (fast)**: `test` (matches `AGENTS.md:14`), `test-otel` (matches `impl-review-3.md:28` 221/221 PASS), `test-auth`, `test-routes-auth`, `test-persistence` (substring match — documents D13 in a header comment).
- **Test (DB integration)**: `test-persistence-integration` (uses testcontainers if Docker present, else `DATABASE_URL`), `test-slow` (with `--test-threads=1`).
- **Test (focused/phase)**: `test-chain`, `test-snippet`, `test-format-sse`, `test-streaming-error`, `test-json-shape` (per `testing-critical-path-regression-guards/plan.md:281,390,558-561,764-769`).
- **DB / migration**: `migrate` (`sqlx migrate run`), `migrate-status`, `migrate-revert`.
- **Run**: `run` (sensible dev defaults: `RUST_LOG=info`, test-token-123, admin/admin, port 10000), `run-otel`, `run-release`, `run-otel-release`, `validate-config` (wraps `cerebrum --validate` at `src/main.rs:104-138`).
- **Manual / harness**: `manual` (default = interactive, execs `manual-test/run.sh`), `manual-basic` (matches `scripts/manual_tests.sh:148-217`), `manual-auto` (delegates to `manual-test/run.sh --auto`), `manual-persistence` (3-tier memory/sqlite/postgres), `manual-fewshot` (focused fewshot subset).
- **CI composite**: `ci` (mirrors `.github/workflows/deploy.yml:25-45` exactly — the source of truth for CI), `ci-otel` (with `--features otel`), `gates` (mirrors the 2026-06-14 verification gate: `cargo test && cargo test slow_tests -- --test-threads=1 && cargo clippy --all-targets -- -D warnings && cargo fmt --check`).
- **Clean**: `clean` (cargo clean), `clean-all` (also `/tmp/cerebrum-config-*.toml`, `/tmp/cerebrum-config-*.yaml`, `/tmp/cerebrum-test-*.log`, `/tmp/cerebrum_test_*.db`, `/tmp/cerebrum.db`, `/tmp/cerebrum-patterns/`, `/tmp/fewshot_int_*.yaml` — consolidates cleanup from `manual-test/lib.sh:100-109`, `scripts/manual_tests.sh:64-68`, `TEST.md:141-142`).

Open questions on the Makefile are collected in §"Open Questions" below.

### D. External service inventory

| # | Service | Image / version | Port (host:container) | Required env vars | Code anchor | Fallback path |
|---|---------|-----------------|----------------------|-------------------|-------------|---------------|
| 1 | **PostgreSQL** (prod + persistence_integration) | `postgres:16-alpine` | `5432:5432` | `POSTGRES_USER=test`, `POSTGRES_PASSWORD=test`, `POSTGRES_DB=test` | src/persistence.rs:1236-1247 (testcontainers), src/persistence.rs:127-128 (production `PostgresBackend::from_env`) | testcontainers in tests (src/persistence.rs:1275-1277) → `DATABASE_URL` env with 3s timeout (src/persistence.rs:1278-1284); app falls back to `MemoryBackend` when `DATABASE_URL` is empty (src/main.rs:386-407) |
| 2 | **OTel Collector** (only when `OTEL_ENABLED=true`) | `otel/opentelemetry-collector-contrib:0.116.1` or newer | `4317:4317` (gRPC), `4318:4318` (HTTP), `13133:13133` (health) | none for `debug` exporter; custom for prometheus | src/telemetry.rs:60-109 (builder), src/telemetry.rs:39-40 (env var contract) | App no-ops when `OTEL_ENABLED` unset/false (src/telemetry.rs:43-49); if exporter builder fails, logs warn and returns None (src/telemetry.rs:66-69, 83-86, 100-103) |
| 3 | **httpmock** (test-only, in-process) | `httpmock 0.7` (Cargo dev-dep) | ephemeral (`127.0.0.1:0`) | none | src/main.rs:1644, 1652, 2713, 2787, 2883; src/intent_classifier.rs:1276, 1278, 1316, 1318, 1374, 1376 | n/a (Rust crate) |
| 4 | **cerebrum** (the app itself) | built from this repo | `10000:10000` (Render default; matches config.toml:9) | `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD` (all non-empty, src/auth.rs:18-20); optional `PORT` (default 10000, src/main.rs:481-484); optional `OTEL_*`; optional `DATABASE_URL` | src/main.rs:481-494 (bind), src/main.rs:168 (telemetry init) | n/a |
| 5 | **OTLP backend** (production) | n/a (SaaS — Grafana Cloud, SigNoz, Axiom) | HTTPS 443 from app egress | `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS` | src/telemetry.rs:39-40; not yet set in render.yaml:8-17 (gap) | n/a |

### E. OTel client verification workflow

- **Minimum env to actually export** (src/telemetry.rs:42-49):
  - Required: `OTEL_ENABLED=true` (or `"1"`; the gate at line 43-46 short-circuits to `None` otherwise).
  - Auto-detected by the `opentelemetry-otlp 0.32` builder (called with `.with_http().with_protocol(Protocol::HttpBinary)` at src/telemetry.rs:60-63, 77-80, 94-97):
    - `OTEL_EXPORTER_OTLP_ENDPOINT` — e.g. `http://localhost:4318` (HTTP). Default per OTel spec when unset.
    - `OTEL_EXPORTER_OTLP_HEADERS` — optional, for SaaS auth.
    - `OTEL_SERVICE_NAME` — falls back to the literal string passed in `telemetry::init("cerebrum")` at src/main.rs:168.
- **Protocol & port** — `Protocol::HttpBinary` is hard-coded at all three sites (src/telemetry.rs:62, 79, 96) → **OTLP/HTTP** (protobuf, content-type `application/x-protobuf`), **not** gRPC. HTTP default port **4318**; gRPC default port **4317** (not used).
- **Graceful no-op** — `OTEL_ENABLED` unset → `init()` returns `None` immediately at src/telemetry.rs:47-49. `OtelGuard` and `Metrics` are never created. `AppState.metrics` is `None` (src/main.rs:478), and `RequestMetrics::new(state.metrics.clone(), ...)` (line 580, 955) short-circuits inside `Drop` (line 53-66) when `self.metrics` is `None`. **Zero overhead confirmed** by the plan note at opentelemetry-integration/plan.md:310. Exporter builder failure → `tracing::warn!` + return `None` (line 66-69, 83-86, 100-103). Collector unreachable at runtime → SDK uses `with_batch_exporter` (traces + logs, src/telemetry.rs:73, 90) and `with_periodic_exporter` (metrics, line 107) — both fire-and-forget.
- **Cheapest verification approach** (ranked):
  1. **`otel/opentelemetry-collector-contrib`** with `debug` exporter → writes every received span/metric/log to its own stdout. Single container, ~250 MB image, no extra services. **Recommended for default dev workflow.**
  2. **`nc -l 4318`** — only proves that a TCP connect happened; doesn't parse protobuf. Useful as a smoke test for "did the exporter fire?", not for content verification.
  3. **Jaeger all-in-one** (`jaegertracing/all-in-one:latest`, ports 16686/4317/4318) — UI at `http://localhost:16686`. Best for visual trace inspection. Only stores traces; metrics + logs would need a separate backend.
  4. **Grafana Alloy** — single binary, native OTel, can fan out. Heavier than `otelcol --debug`, lighter than the full LGTM stack.

### F. Postgres integration test wiring

- **`TestDb::new()`** (src/persistence.rs:1219-1263):
  - Container: `GenericImage::new("postgres", "16-alpine")` (line 1236) — image and tag **must** match the compose file.
  - Port: `.with_exposed_port(5432.tcp())` (line 1237). `container.get_host_port_ipv4(5432.tcp())` (line 1248) returns the **random** host port.
  - Credentials: `POSTGRES_USER=test`, `POSTGRES_PASSWORD=test`, `POSTGRES_DB=test` (lines 1241-1243). Compose must use the same triple.
  - Wait condition: `WaitFor::message_on_stderr("database system is ready to accept connections")` (line 1238-1240).
  - Startup timeout: 60s (line 1244).
  - Migration: `sqlx::migrate!().run(&pool).await.ok()?` (line 1256) — runs the embedded migrations from `migrations/` automatically.
  - Driver version: `sqlx 0.8` with `postgres`, `runtime-tokio`, `tls-rustls` features (Cargo.toml:24).
- **`test_pool()` priority** (src/persistence.rs:1272-1285):
  1. Try `TestDb::new()` (testcontainers, Docker-backed) — returns `None` if Docker isn't reachable.
  2. Fall back to `DATABASE_URL` with a 3-second `tokio::time::timeout` on `PgPool::connect`.
- **Call sites** (all gated by `#[cfg(test)]`):
  - src/persistence.rs:2368 — inside `test_pg_log_concurrency_limit_parsed_from_env`.
  - src/main.rs:2109 — `persistence_integration_prompt_char_count_column_exists`.
  - src/main.rs:2134 — `persistence_integration_insert_and_read_back`.
  - src/main.rs:2191 — `persistence_integration_sse_streaming_success` (`#[serial]`).
  - src/main.rs:2261 — `persistence_integration_sse_streaming_error` (`#[serial]`).
  - All four main.rs tests begin with the same early-return pattern: if `test_pool()` returns `None`, print `"SKIP <name>: DATABASE_URL not set or unreachable"` and `return;` — they don't fail.
- **CI path today** — deploy.yml:35-40 only runs `persistence_integration` if `$DATABASE_URL` is set in the workflow env. The workflow never sets it, so the four tests are silently skipped on every push to main. A compose file would let the workflow do `docker compose up -d postgres && export DATABASE_URL=postgres://test:test@localhost:5432/test && cargo test`, turning the cross-backend test plan from Phase 2 (`test-plan.md:71`) into the always-on default.
- **App-side auto-migration** — `src/persistence.rs:180-182` runs `sqlx::migrate!().run(&pool).await` in `PostgresBackend::from_env` and panics on failure. `src/persistence.rs:1256` runs the same macro in the test path. So `make migrate` (or `sqlx migrate run` in CI) is a *redundancy check*; the app will self-migrate at startup either way.

### G. Migration + sqlx-cli story

- **`sqlx migrate run` in CI** — deploy.yml:36 runs it only inside the `if [ -n "$DATABASE_URL" ]` branch. There is no separate `sqlx migrate run` step in the build-only branch (line 42-45).
- **`SQLX_OFFLINE=true`** is set at deploy.yml:32 (before `cargo test persistence`) and deploy.yml:44 (before `cargo build --release`). The test code uses `sqlx::query()` (plain string form, not compile-time-checked) in the four `persistence_integration_*` tests (e.g. src/main.rs:2116-2122, 2162-2167, 2237-2240, 2309-2312), so the offline flag is technically only required for the production binary's compile. The CI sets it blanket-style, which is safe.
- **`.sqlx/` offline cache** — **not committed** (verified by `ls` and `.gitignore`). A developer on a fresh checkout who runs `cargo build --release` without `SQLX_OFFLINE=true` and without a live DB will hit compile errors in the production code only if there are `sqlx::query!()` macro calls. Worth a `grep "sqlx::query!"` during implementation to confirm.
- **Fresh-checkout dev workflow (real DB)** — the app auto-migrates in two places (src/persistence.rs:180-182 production, src/persistence.rs:1256 test). Developer who starts a local Postgres (or `docker compose up -d postgres`), exports `DATABASE_URL=postgres://test:test@127.0.0.1:5432/test`, and runs `cargo run --release` will get migrations applied automatically — no `sqlx migrate run` step needed for the **app** itself. The only place `sqlx-cli` is needed is **CI** (deploy.yml:36), and only as a redundancy check.

### H. Recommended compose file (proposed structure)

Three-file split is recommended; the simpler single-file-with-profiles alternative is also documented in the General agent's output.

- **`docker-compose.yml`** (base, always-on) — just `postgres`. `docker compose up -d postgres` is enough for the testcontainers-free path. `postgres:16-alpine` to match testcontainers; `POSTGRES_USER/PASSWORD/DB=test`; port `5432:5432`; healthcheck `pg_isready -U test -d test` every 5s; named volume `cerebrum-postgres-data:/var/lib/postgresql/data`.
- **`docker-compose.otel.yml`** (override, opt-in) — adds `otel-collector` to the base. Use with `docker compose -f docker-compose.yml -f docker-compose.otel.yml up`. Image `otel/opentelemetry-collector-contrib:0.116.1` (or newer — version pin TBD per Open Questions); ports `4317:4317`, `4318:4318`, `13133:13133`; volume `./deploy/otel-collector/config.yaml:/etc/otelcol/config.yaml:ro`; healthcheck on 13133.
- **`docker-compose.full.yml`** (override, all-in-one for manual demos) — adds `otel-collector` + optional `jaeger` + the `cerebrum` app. This is what `scripts/manual_tests.sh` and `manual-test/run.sh` should be wired against (replacing the current `cargo build --release` step inside the script).

Minimal `deploy/otel-collector/config.yaml` per the General agent's research (using `debug` exporter, pprof and health_check extensions, no prometheus exporter for v1):
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

Single-file-with-profiles alternative (also valid): the OTel collector is under `profiles: ["otel"]`; the `cerebrum` app service is under `profiles: ["app"]`; `docker compose up` brings up postgres only; `docker compose --profile otel --profile app up` brings up everything.

### I. Network & port alignment

| Port | Service | Where set | Collides? |
|---|---|---|---|
| **10000** | cerebrum (Axum) | src/main.rs:481-484 (defaults to config.toml:9 value of 10000); manual-test/lib.sh:16, 68; scripts/manual_tests.sh:41 | Default across all paths. Render injects `PORT` env var; default 10000. |
| **5432** | postgres | src/persistence.rs:1237 (testcontainers) | Fixed in compose (`5432:5432`). testcontainers maps to a random host port via `get_host_port_ipv4(5432.tcp())` (src/persistence.rs:1248), so **no collision** when both run on the same host. |
| **4317** | OTel gRPC | OTel spec default | App **does not use gRPC** (Protocol::HttpBinary at src/telemetry.rs:62, 79, 96). Exposed by the collector for Jaeger/Alloy compatibility. Safe to leave or remove. |
| **4318** | OTel HTTP | OTel spec default; app reads via `OTEL_EXPORTER_OTLP_ENDPOINT` | App reads the env var — defaults to `http://localhost:4318` per OTel spec. **No collision with the app (10000).** |
| **13133** | OTel collector health | collector's standard | Not used by the app. |
| **16686** | Jaeger UI | optional | Not used by the app. |
| **ephemeral** | httpmock (Rust test) | `httpmock::MockServer::start()` | 13 call sites in src/main.rs and src/intent_classifier.rs. **No collision.** |
| **ephemeral** | `spawn_slow_sse_server` in slow_tests | src/main.rs:4202, 4315, 4338, 4365, 4575 — `TcpListener::bind("127.0.0.1:0")` | Binds random port per test. **No collision.** |

**Conclusion: zero port collisions in any sane configuration.** The only thing to be careful about is not exposing `10000` to the public internet in dev (it carries basic-auth dashboard credentials). Default `127.0.0.1:10000` is fine.

### J. Gaps the new tooling closes

1. **OTel client cannot currently be manually verified without manual collector setup.** Today, a developer who runs `cargo run --features otel` with `OTEL_ENABLED=true` will see **no observable output** if they don't already have a collector. The plan at opentelemetry-integration/plan.md:296-303 says "Build with `cargo build --features otel`, run with `OTEL_ENABLED=true` and a local OTLP collector" — but the **collector** step has zero scaffolding. A `docker compose -f docker-compose.yml -f docker-compose.otel.yml up -d otel-collector` one-liner closes this.
2. **`persistence_integration_*` tests are silently skipped in CI today.** deploy.yml:35-40 says "if `[ -n "$DATABASE_URL" ]`", and the workflow never sets it, so on every push to main the four `persistence_integration_*` tests in `mod tests` (src/main.rs:2107-2183, 2188-2253, 2258-2325) are skipped. With a compose file, the workflow becomes:
   ```yaml
   - run: docker compose up -d postgres
   - env:
       DATABASE_URL: postgres://test:test@localhost:5432/test
     run: cargo test
   ```
3. **`manual-test/run.sh` builds the binary but never starts a real DB or runs migrations.** At scripts/manual_tests.sh:31 and manual-test/lib.sh:48-50 (`build_server`), the script does `cargo build --release` and starts the binary with the **default** `[persistence] backend = "memory"` (config.toml:32-33). The `test_postgres_backend` test at manual-test/test.sh:1008-1069 does check `DATABASE_URL` and skip cleanly, but there's no script that **provisions** the DB. Compose + a small Makefile target closes this.
4. **`render.yaml` has no `OTEL_EXPORTER_OTLP_*` env vars yet.** render.yaml:8-17 lists only `RUST_LOG`, the three auth tokens, and `DATABASE_URL`. The plan at opentelemetry-integration/plan.md:31 says "deploy to Render with Grafana Cloud OTLP endpoint configured" as the verification step, but the env vars aren't wired. The implementation phase should add `OTEL_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS` to `render.yaml` (with `sync: false` on the headers, since they contain a secret). The build command also needs to gain `--features otel` — currently render.yaml:5 is just `cargo build --release`.
5. **No Dockerfile in repo.** `grep "docker-compose"` finds zero `docker-compose.yml` / `Dockerfile` in the working tree. The `cicd-dev-tooling` change will need to introduce a `Dockerfile` (the cargo-chef pattern from context/foundation/infrastructure.md:104 is the recommended template). The compose `cerebrum` service assumes this exists.
6. **No `.sqlx/` offline cache committed.** Fresh-checkout builds under `SQLX_OFFLINE=true` will fail for any future `sqlx::query!()` macro invocations. The implementation phase should decide between (a) committing a regenerated `.sqlx/`, (b) running `cargo sqlx prepare` as a CI pre-build step, or (c) switching production queries to the non-macro form (which is what the test code already does). **Out of scope** for compose but worth flagging in the plan.

### K. Dockerfile baseline (from General agent's research)

```dockerfile
# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=lukemathwalker/cargo-chef:0.1.77-rust-1.85.0-bookworm

FROM ${RUST_IMAGE} AS chef
WORKDIR /app
# Force offline-mode compilation for sqlx macros inside the container.
ENV SQLX_OFFLINE=true

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin cerebrum

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/cerebrum /usr/local/bin/cerebrum
USER nobody
ENTRYPOINT ["/usr/local/bin/cerebrum"]
```

Verified by the General agent's research:
- **Three-stage skeleton** is the canonical `cargo-chef` pattern (`https://github.com/LukeMathWalker/cargo-chef/blob/main/README.md`, "Dockerfile Example" block).
- **`debian:bookworm-slim`** is the right runtime base — matches `rust:slim-bookworm`; apt available for `ca-certificates`; matches cargo-chef tag matrix.
- **`ca-certificates`** is required for outbound HTTPS to LLM providers via `reqwest`; `debian:bookworm-slim` does not include it by default. No `libssl` needed because the binary uses `rustls` (rustls-tls for reqwest, tls-rustls for sqlx).
- **`SQLX_OFFLINE=true`** is required so `sqlx::query!` macros don't try to reach a DB at cook time.
- **`--locked`** for hermetic, reproducible builds.
- **Feature gating `--features otel`** — Pattern A (build with `--features otel` always) is recommended for v1 to keep one cached layer. Pattern B (build arg) is more complex for marginal benefit.
- **Pre-built image** `lukemathwalker/cargo-chef:<ver>-rust-<ver>-bookworm` is published with tags matching every `library/rust` alias.

### L. Makefile idioms (from General agent's research)

- **`.DEFAULT_GOAL := help`** is the standard.
- **`.PHONY`** discipline — all non-file targets declared.
- **Arg forwarding** — `test: cargo test $(TEST)` or `cargo test $(filter-out $@,$(MAKECMDGOALS))` with a leading `%: ; @:` rule.
- **`ci` aggregator** — chains `fmt-check`, `lint`, `test`, `build`.
- **Tool-presence detection** — `command -v docker 2>/dev/null` (preferred over `which` for portability).
- **Fast-feedback targets** — `make test TEST=foo`, `make test-auth`, `make test-persistence-integration`.

The General agent's "alternatives" section is also relevant:
- `cargo-chef` is the right answer for cerebrum (the risk register at context/foundation/infrastructure.md:104 already names it as the fix for 12-min cold builds). Skip `cargo-nextest` in the Dockerfile (it's a test runner, not a compile-time tool).
- Single `docker-compose.yml` with profiles vs split files: **profiles recommended for v1** (one app, one DB, optional OTel, optional migration tool). Split files pay off when there are ≥3 distinct deployment shapes.
- `sqlx-cli` should be a **sidecar container**, not embedded in the runtime image (saves 5-10 min build time and ~200 MB image size).
- **Never auto-migrate in `ENTRYPOINT`** for a stateless service on Render with a managed Postgres (race condition between replicas). `make migrate` as a one-shot sidecar is the right pattern.

### M. Test plan touchpoints (`context/foundation/test-plan.md` updates)

The user's hypothesis that this change touches the test plan is **correct**. Specific edits:

- **§3 Phased Rollout — Phase 4 row** (test-plan.md:73): the row's status will move from "not started" to something like "in progress" (or split into "tooling" and "coverage" sub-phases), and the change-folder column will gain `cicd-dev-tooling`. The phase's goals should be extended to include "wire `make ci` and `make gates` into a new PR CI workflow" and "wire `make test-persistence-integration` into a workflow that provisions postgres via compose".
- **§4 Stack — `e2e` row** (test-plan.md:90): currently "none yet". The compose stack plus the `app` profile turns the local manual-test harness into a true e2e layer for OTel; this row should note that "compose + Makefile gives a local e2e analog; CI e2e is out of scope".
- **§4 Stack — new row**: add `compose` (Docker Compose v2) under a new category "Local dev / OTel verification" with version 2.20+ and the canonical OTel contrib image version.
- **§5 Quality Gates** (test-plan.md:111-122): add a "PR CI workflow" gate. The existing 8 rows stay; new row: "PR CI (`.github/workflows/ci.yml`): required; catches lint+typecheck+test+slow+build+compose-services-up". Mark the existing "lint + typecheck" row as "wired via `make lint-strict` and `make fmt-check`" rather than "required (local + CI)".
- **§6.1 "Run locally"** (test-plan.md:136): replace "`cargo test <test_name>` (fast) or `cargo test slow_tests`" with "use `make test TEST=<name>` for fast tests, `make test-slow` for slow tests (with `--test-threads=1`)".
- **§6.1–§6.5 "Run locally" lines** (test-plan.md:136, 144, 151, 158, 166): same pattern — replace the bare `cargo test <test_name>` form with the Makefile equivalent, while keeping the in-source test pattern instructions intact.
- **§7 (negative space)** (test-plan.md:172-177): no edit needed. The CSS snapshot exclusion stands.
- **§8 Freshness ledger** (test-plan.md:179-187): bump "Strategy (§1–§5) last reviewed" and "Stack versions last verified" dates.

No edits needed in §1 (strategy), §2 (risk map — none of the 7 risks changes), or §6.6 (per-rollout-phase notes — stays blank until a phase ships).

## Code References

- `AGENTS.md:5-16` — current test/build/run commands; does not mention Makefile or Docker.
- `Cargo.toml:8-14, 50-53` — `otel` feature flag; `httpmock`, `serial_test`, `testcontainers` dev-deps.
- `Cargo.toml:24` — sqlx 0.8 with `postgres`, `sqlite`, `runtime-tokio`, `tls-rustls`, `macros`, `uuid`, `chrono`, `migrate` features.
- `.github/workflows/deploy.yml:1-61` — full deploy workflow; no PR CI, no fmt/clippy/slow_tests gate.
- `.github/workflows/deploy.yml:25-40` — the test sequence that `make ci` will mirror.
- `.github/workflows/deploy.yml:57-59` — Render deploy webhook (out of scope for Makefile).
- `render.yaml:1-18` — Render native Rust runtime; no OTel env vars.
- `render.yaml:5` — `buildCommand: cargo build --release` (intentionally lacks `--features otel` per impl-review-2 F1).
- `src/main.rs:168` — `telemetry::init("cerebrum")` call site.
- `src/main.rs:386-407` — `MemoryBackend` fallback when `DATABASE_URL` empty.
- `src/main.rs:478` — `AppState.metrics: Option<telemetry::Metrics>` field.
- `src/main.rs:481-484` — port binding (default 10000).
- `src/main.rs:580, 955` — `RequestMetrics::new(state.metrics.clone(), ...)` callsites.
- `src/main.rs:104-138` — `--validate` CLI implementation.
- `src/main.rs:1356` — `mod tests { ... }` (all fast tests).
- `src/main.rs:1388, 1426, 1458, 1841, 1879, 2710, 2783, 2883, 2743` — `test_app()` harness family.
- `src/main.rs:2107-2183, 2188-2253, 2258-2325` — four `persistence_integration_*` tests with DB-skip pattern.
- `src/main.rs:4178-4179` — `mod slow_tests { ... }` (keepalive timing tests).
- `src/main.rs:4202, 4315, 4338, 4365, 4575` — `spawn_slow_sse_server` random-port binding.
- `src/auth.rs:18-20` — auth token non-empty requirement.
- `src/persistence.rs:127-128` — `PostgresBackend::from_env`.
- `src/persistence.rs:180-182` — production auto-migration.
- `src/persistence.rs:1219-1263` — `TestDb` struct + `new()`.
- `src/persistence.rs:1272-1285` — `test_pool()` priority (testcontainers → DATABASE_URL).
- `src/persistence.rs:1288+` — `mod tests` (snippet extraction, DB unit tests).
- `src/telemetry.rs:1-192` — full module (init, OtelGuard, Metrics, layers, shutdown).
- `src/telemetry.rs:42-49` — `OTEL_ENABLED` env gate; no-op when unset/false.
- `src/telemetry.rs:54-56` — `svc_name: &'static str` via intentional `leak()`.
- `src/telemetry.rs:60-63, 77-80, 94-97` — OTLP HTTP-binary exporter builders.
- `src/telemetry.rs:67, 84, 101` — `tracing::warn!` on exporter build failure (post impl-review-3 F3).
- `src/telemetry.rs:73, 90, 107` — `with_batch_exporter` (traces, logs) and `with_periodic_exporter` (metrics).
- `src/telemetry.rs:181-191` — `OtelGuard::shutdown` order (traces → logs → metrics).
- `config.toml:9, 32-33` — `port = 10000`, `backend = "memory"`.
- `context/foundation/test-plan.md:11-29` — §1 strategy (no edit).
- `context/foundation/test-plan.md:32-60` — §2 risk map (no edit; the 7 risks don't change).
- `context/foundation/test-plan.md:62-73` — §3 phased rollout, Phase 4 row (edit).
- `context/foundation/test-plan.md:75-98` — §4 stack (edit: add compose row, mark e2e row).
- `context/foundation/test-plan.md:105-122` — §5 quality gates (edit: add PR CI row, mark existing rows wired via Makefile).
- `context/foundation/test-plan.md:124-167` — §6 cookbook (edit "Run locally" lines to use `make`).
- `context/foundation/test-plan.md:172-177` — §7 negative space (no edit).
- `context/foundation/test-plan.md:179-187` — §8 freshness ledger (bump dates).
- `context/foundation/infrastructure.md:104` — risk register names `cargo-chef` as the fix for 12-min cold builds.
- `context/changes/opentelemetry-integration/plan.md:62, 244, 298, 374` — OTel build command and intentional render.yaml drift.
- `context/changes/opentelemetry-integration/plan.md:296-303` — manual OTel verification steps (no collector scaffolding today).
- `context/changes/opentelemetry-integration/plan.md:310` — zero overhead confirmed when `OTEL_ENABLED` unset.
- `context/changes/opentelemetry-integration/reviews/impl-review-3.md:29, 113` — verification gate `cargo clippy --features otel --all-targets -- -D warnings` PASS.
- `context/changes/testing-critical-path-regression-guards/change.md:31` — gates verified on 2026-06-14 (the form `make gates` will mirror).
- `context/changes/testing-critical-path-regression-guards/plan.md:281, 390, 558-561, 764-769, 1068, 1082, 1097, 1098, 1127-1131` — per-phase substring test patterns.
- `scripts/manual_tests.sh:148-217` — `--basic` mode (quick smoke).
- `scripts/manual_tests.sh:223-235` — `--auto` mode (delegates to `manual-test/run.sh --auto`).
- `scripts/manual_tests.sh:241-248` — interactive default (execs `manual-test/run.sh`).
- `scripts/manual_tests.sh:41` — `PORT=10000` env for the harness.
- `scripts/manual_tests.sh:64-68` — `clean-all`-equivalent cleanup.
- `manual-test/lib.sh:16, 48-50, 65-68, 100-109` — port default, `build_server`, env defaults, cleanup.
- `manual-test/run.sh:38-1021, 1023-1265, 1444-1447` — `--auto`, interactive, `--fewshot` modes.
- `manual-test/test.sh:931, 1002, 1008-1069, 1074-1129` — SQLite cleanup, `test_postgres_backend`, 3-tier suite.
- `migrations/001_create_inferences.sql`, `002_inferences_request_id_unique.sql`, `003_add_prompt_char_count.sql` — applied by `sqlx::migrate!()` at startup.

## Architecture Insights

1. **OTel already implements a `None`-as-zero-overhead pattern** that the Makefile/compose can build on. `telemetry::init()` returns `Option<(OtelGuard, Metrics)>`; `AppState.metrics` is `Option<Metrics>`; `RequestMetrics::new(metrics, ...)` short-circuits in its `Drop` when `metrics.is_none()`. This means `make run-otel` is the only target that needs to set `OTEL_ENABLED=true` — `make run` is a no-op for OTel by design. **No init-time guard is needed in the Makefile beyond setting the env var correctly.**

2. **The app auto-migrates at startup** (src/persistence.rs:180-182). This means `make migrate` is **optional for dev** (the app will do it) but **required for CI** as a fail-loud schema-drift check (deploy.yml:36). The Makefile target should document this distinction.

3. **The `mod tests` and `mod slow_tests` split is intentional** (AGENTS.md:11-15). `make test` runs only the fast module; `make test-slow` runs the slow module with `--test-threads=1` (required for keepalive timing). A single `make test` that runs both would re-introduce the timing flake the gate form was designed to avoid. **Do not collapse them.**

4. **Cerebrum is not a workspace** — it's a single binary crate (Cargo.toml:1-3, no `[workspace]`). This simplifies the Makefile: no recursive `$(MAKE)`, no `--workspace` flag needed for any target. The `cargo` invocations are all on the default member.

5. **The CI workflow is a single file** (.github/workflows/deploy.yml) that runs only on push to main. There is no PR CI workflow, no scheduled job, no matrix build. The test plan §5 calls for a "scheduled CI job" to run `slow_tests` (test-plan.md:73) and a coverage-fail threshold (test-plan.md:73). The implementation phase should introduce a new `.github/workflows/ci.yml` that runs on PR + push to main, and modify deploy.yml to call `make ci` (or to keep its current sequence verbatim — the Makefile `ci` target is a port of the workflow, not a replacement).

6. **The "5-second build → 1-2 minute build" delta from cargo-chef is the single largest DX win** this change enables. Without cargo-chef, every CI run starts from a cold `cargo build --release` (~10-12 min). With cargo-chef, only the changed code recompiles (~1-2 min). For a service that deploys multiple times per day, this is the difference between "edit + push + wait" and "edit + push + coffee". The risk register at context/foundation/infrastructure.md:104 already names this.

7. **The OTel collector's `debug` exporter is the right DX choice for v1** — zero extra containers beyond the collector itself, output is `docker compose logs otel-collector`, no UI to learn. Jaeger all-in-one is a v2 enhancement, not a v1 requirement. (See open question Q4.)

8. **The `persistence_integration_*` tests already have a skip path** (src/main.rs:2107+ early return with `SKIP <name>: ...` print). Wiring them into CI via compose is a pure addition — no test code changes needed, just `docker compose up -d postgres && export DATABASE_URL=... && cargo test`.

9. **There is a lesson in `lessons.md:57` about plan-vs-PR scope drift** ("Squash merges must not bundle unrelated in-flight changes into one PR"). The `cicd-dev-tooling` plan should explicitly enumerate scope (Makefile, Dockerfile, docker-compose, OTel collector config, test-plan.md edits) and explicitly exclude OTel production code, OTel env-var additions to `render.yaml` (which is a separate `opentelemetry-integration` follow-up), and README rewrites (which is a separate `readme-bootstrap` change). The plan's "What We're NOT Doing" section should name each.

10. **The composition is multiplicative, not additive.** Each piece (Makefile, Dockerfile, docker-compose, OTel collector config) is small; the value is in how they compose. `make ci` mirrors the deploy workflow; `make run-otel` assumes `docker compose --profile otel up`; `make test-persistence-integration` assumes postgres is up. None of these work in isolation. **The implementation order matters**: postgres → collector config → Dockerfile → compose → Makefile → test-plan.md → new PR CI workflow.

## Historical Context (from prior changes)

- `context/changes/opentelemetry-integration/change.md:1-6` — umbrella change for OTel work; status `impl_reviewed`; this research is complementary to that work (provides the dev infrastructure to verify it locally).
- `context/changes/opentelemetry-integration/plan-brief.md:64-67` — plan brief lists "Render deployment config with OTLP env vars" as in-scope for Phase 4. The render.yaml env-var gap (item J4 above) is tracked there.
- `context/changes/opentelemetry-integration/plan.md:374` — phase 4.2 ("Build command includes `--features otel`") is "intentionally unmet per impl-review-2 F1 (render.yaml kept as showcase only; user accepted)". The Makefile target `build-release-otel` will exist even when render.yaml does not invoke it.
- `context/changes/opentelemetry-integration/reviews/impl-review-3.md:29, 113` — the OTel verification gates verified post-merge: `cargo check --features otel`, `cargo test --features otel` (221/221 PASS), `cargo clippy --features otel --all-targets -- -D warnings`, `cargo tree --features otel`. These become the `make ci-otel` and `make lint-otel` invocations.
- `context/changes/testing-critical-path-regression-guards/change.md:31` — the 2026-06-14 gate form (`cargo build --release` ✓, `cargo test` 215 passed ✓, `cargo test slow_tests -- --test-threads=1` 5 passed ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo fmt --check` ✓). The Makefile `gates` target mirrors this form.
- `context/changes/testing-critical-path-regression-guards/plan.md:652, 901, 930, 989, 1112, 1148` — repeated uses of `cargo test slow_tests -- --test-threads=1`; confirms the form is canonical for cerebrum.
- `context/changes/config-ux/change.md:1-6` — the in-progress `config-ux` change (`--help/--init/--quickstart` flags) will benefit from the Makefile's `validate-config` target (calls `cerebrum --validate`). Coordinate so the flag and the target ship together.
- `context/changes/readme-bootstrap/` (status: preparing) — the in-progress README rewrite; will need to reference the new `make help` output and the compose workflow. The Makefile's `print-tokens` and `help` targets should be the canonical "what commands exist" reference.
- `context/changes/bootstrap-verification/` — exists in the changes directory; not directly relevant but confirms the `cicd-dev-tooling` change follows the project's own bootstrap pattern.
- `context/archive/2026-05-26-data-persistence-async-logging/plan.md:69, 98, 183` — original sqlx migrate plan; the persistence wiring is now in `src/persistence.rs`. The Compose file makes the same migrations locally available.
- `context/archive/2026-05-26-data-persistence-async-logging/reviews/plan-review.md:33, 39, 58, 60` — historical `sqlx-cli` decision and offline mode toggle.
- `context/archive/2026-06-09-in-memory-db-fallback/plan.md:419-420, 533-534` — historical context for why `cargo test persistence` runs in default CI without DB; the `test-persistence` target's header comment can reference this.
- `context/archive/2026-06-01-reqwest-upstream-routing/reviews/impl-review.md:102` — historical "use `cargo test --lib` to split" recommendation that was never adopted (D7); not blocking but worth noting that we considered it and rejected.
- `context/archive/2026-05-26-auth-scaffold-access-keys/plan.md:81, 136, 246, 259` — historical `cargo fmt -- --check` and `cargo clippy --all-targets --all-features -- -D warnings` invocations; superseded by the 2026-06-14 form.
- `context/archive/2026-06-11-config-format-upgrade/research-config-format.md:23, 54, 196` — historical context for why TOML was chosen over YAML (one of the trade-offs cited is "YAML linting for structure" — the Makefile doesn't depend on this directly but the test plan's stack table might).
- `context/foundation/lessons.md:5-59` — all 8 lessons apply to the implementation phase of this change:
  - Lesson 1 (OpenAPI Generator) — not directly relevant.
  - Lesson 2 (re-run review after follow-up change) — applies: this change touches the same files as OTel (render.yaml, src/main.rs if any metrics callsites are touched, src/telemetry.rs is not touched). The plan's "What We're NOT Doing" section should explicitly exclude OTel production code.
  - Lesson 3 (handle upstream error bodies) — not directly relevant.
  - Lesson 4 (self-describing comments) — applies: each Makefile target should have a `## description`; compose file service comments should be self-explanatory.
  - Lesson 5 (dynamic WHERE clause) — not relevant.
  - Lesson 6 (log operational failures before falling back) — applies: `make migrate` should log what's happening; `docker compose up` should print `DATABASE_URL=...` so dev can copy it.
  - Lesson 7 (delete dead code) — applies: if old `cargo clippy` / `cargo fmt` references are removed from any plan, delete them rather than suppress.
  - Lesson 8 (squash merges must not bundle unrelated work) — applies: the change folder's plan should list exactly Makefile, Dockerfile, docker-compose, OTel collector config, test-plan.md edits, and explicitly exclude everything else.

## Related Research

- `context/changes/opentelemetry-integration/research.md` — the OTel research from 2026-06-09. Key data: same env vars, same exporter protocol, same port mapping. This research is the dev-tooling complement to that work.
- `context/changes/opentelemetry-integration/plan.md` — the OTel plan from 2026-06-09. Phases 1-3 are complete; Phase 4 is "intentionally unmet" (render.yaml build command); the manual verification steps in plan.md:296-303 are the gap this research closes.
- `context/foundation/infrastructure.md` — Render selection doc; the risk register at line 104 names cargo-chef as the fix for 12-min builds. The Dockerfile baseline in this research implements that fix.
- `context/archive/2026-06-09-in-memory-db-fallback/plan.md` — research on in-memory DB fallback, including the testcontainers pattern. The compose `postgres` service replaces the testcontainers-only path with a persistent local DB.
- `context/archive/2026-05-26-data-persistence-async-logging/plan.md` — original persistence design; the `sqlx::migrate!()` at startup, the `SQLX_OFFLINE=true` env var, and the `cargo test persistence` substring gate all originate here.
- `context/changes/bootstrap-verification/` — the project's own bootstrap verification artifacts; not directly relevant but useful as a reference for the `cicd-dev-tooling` change's structure.

## Open Questions

These should be answered in the follow-up `/10x-plan` step (or escalated to the user before planning). Each is a discrete decision with concrete tradeoffs; the recommended path is noted.

1. **`cargo-chef` version pin.** Tag `0.1.77-rust-1.85.0-bookworm` is a safe recent default, but cerebrum's actual Rust toolchain version (per `rust-toolchain.toml` if present, or the toolchain that ran `cargo build --release` last) should dictate the exact pin. The implementation agent should check for `rust-toolchain.toml` first and pin cargo-chef to match.
2. **Should `otel` be a default feature?** Two patterns: (A) build with `--features otel` always in the Dockerfile (one cached layer, no build arg complexity), or (B) build arg `--build-arg FEATURES=otel` (two cached layers, more flexibility). Pattern A is recommended for v1 to keep Dockerfile simple; revisit if binary size becomes a concern.
3. **Single compose file with profiles vs split files.** Recommendation: single file with profiles for v1 (postgres always-on, otel + app behind profiles). Split files are cleaner when there are ≥3 distinct deployment shapes (e.g. dev / staging / prod). Escalate to user if the team anticipates adding `staging` or `prod` overlays soon.
4. **`otel-collector-contrib` version pin.** Two agents disagreed on the current LTS — General agent suggested `0.121.0`; Explore agent suggested `0.116.1`. The actual current version (as of 2026-06-15) should be verified at implementation time against `https://hub.docker.com/r/otel/opentelemetry-collector-contrib/tags` and the chosen tag documented in the OTel collector config file's header comment.
5. **`make run` defaults for env vars.** AGENTS.md (AGENTS.md:5-7) requires all three auth tokens to be non-empty but doesn't give defaults. `manual-test/lib.sh:65-68` hardcodes `admin`/`admin` and `info`. Options: (a) same defaults (good for solo dev, bad if dev ever commits a `.env` by accident), (b) source a `.env` if present, (c) refuse to start with a clear error if the three required env vars are missing — matching `AuthConfig::from_env`'s panic at src/main.rs:207-209. The `config-ux` change will add `--help` that lists these; the Makefile target should reference it.
6. **`make migrate` and `sqlx-cli` prereq.** deploy.yml:36 runs `sqlx migrate run` as part of CI. Locally, `make migrate` will fail if `sqlx-cli` isn't installed. Options: (a) `make migrate` skips and prints a reminder, (b) installs `sqlx-cli` via `cargo install` (slow), (c) documents `sqlx-cli` as a prereq. Recommended: (c) with a prereq note and a separate `make migrate-install` target that runs `cargo install sqlx-cli --version 0.8 --locked --no-default-features --features rustls,postgres,sqlite`.
7. **`build-release-otel` drift from `render.yaml`.** A target that's never invoked by CI is a maintenance hazard. Two paths: (a) keep it with a header comment explaining the intentional drift and tracking the OTel `plan.md:374` addendum, (b) drop it and let `make build-release FEATURES=otel` parameterize. Recommendation: (a) — explicit is better than implicit, and a Makefile target with a clear "this is for local production-shape builds; CI uses `build-release` until `render.yaml` catches up" is more discoverable than a feature toggle.
8. **`make test-otel` and `OTEL_ENABLED`.** Should the target also set `OTEL_ENABLED=true` to exercise the providers' code paths, or just compile-test the OTel feature? The OTel init is a no-op when `OTEL_ENABLED` is unset (src/telemetry.rs:43-49), so a default-form `make test-otel` only verifies that the OTel code path compiles. Recommendation: default to no env var; document `make test-otel OTEL_ENABLED=1` for the full path.
9. **Should the compose `cerebrum` app service be in the default `docker compose up` or behind `--profile app`?** Putting it behind a profile keeps the dev path minimal for someone who only cares about the DB. Recommendation: behind `--profile app` (opt-in).
10. **Compose `Dockerfile.migrate` (sqlx-cli sidecar).** The General agent recommended baking a separate `Dockerfile.migrate` from `rust:slim-bookworm` that runs `cargo install sqlx-cli --locked --no-default-features --features rustls,postgres,sqlite` and ENTRYPOINTs to `sqlx`. Cost: another 5-10 min build, 200 MB image. The Explore agent recommended running `sqlx-cli` from the host (no container). Recommendation: host-side first (simpler, no new image to maintain); revisit if cross-platform CI workers need it.
11. **What about `.sqlx/` offline cache?** Not committed today; the deployment's compile relies on `SQLX_OFFLINE=true` with no cache, which only works if there are no `sqlx::query!()` macro invocations in the production code. Implementation phase should grep `sqlx::query!` across `src/` and decide between (a) commit a regenerated cache, (b) add `cargo sqlx prepare --check` to CI, (c) migrate production queries to the non-macro form. **Out of scope for this change but worth flagging.**
12. **What about adding a `make test-coverage` target now or deferring?** Test plan §3 Phase 4 (test-plan.md:73) calls for a coverage-fail threshold. Adding a `cargo llvm-cov` target is a 5-line Makefile addition. Recommendation: defer to the Phase 4 follow-up; not in scope for `cicd-dev-tooling` v1.
13. **PR CI workflow file (`.github/workflows/ci.yml`).** The current deploy.yml is a deploy workflow, not a CI workflow. The implementation phase should introduce a new `ci.yml` (or rename deploy.yml) that runs on PR + push to main with the `make ci` sequence. Should the deploy hook step move to a separate `deploy.yml` triggered on push to main only? Recommendation: yes, split. Keep deploy.yml's current content (minus the `cargo test auth` etc. steps, which move to ci.yml).
14. **`OTEL_ENABLED` value coercion** (src/telemetry.rs:43-49). The OTel client accepts `"true"` or `"1"`; everything else is "off". The Makefile's `run-otel` should set the canonical `"true"` form. A `make run-otel-false` target is overkill but documented in the OTel plan as a manual verification step (opentelemetry-integration/plan.md:379).
15. **Coverage tool choice** for the test-plan §3 Phase 4 follow-up. `cargo-llvm-cov` is the de-facto standard in 2025-2026; `grcov` and `tarpaulin` are alternatives. Defer to that change; out of scope here.

## Follow-up Research 2026-06-15 (task-runner alternatives)

User note: *"It can be sth else than Makefile, like mage (that's go so probably not needed) or taskfile or whatever."*

Added §C.0 above. Summary of the decision:

- **Default recommendation: Makefile** (zero install, universal recognition, lowest adoption cost given that every doc/plan in the repo already references raw `cargo` commands).
- **Documented escape hatch: Taskfile (go-task)** — strongest fit if the team prefers YAML over Make DSL; single static binary, built-in `task --list`, native arg forwarding via `task test -- auth`.
- **Runner-agnostic in the rest of the document**: the proposed targets in §C and the recommendations in §L describe the **target semantics** (name, command, env vars, purpose), not the DSL syntax. The plan step will instantiate them in whichever runner the user picks.

The rest of the research document (command inventory, compose, Dockerfile, OTel verification, test plan touchpoints) is unaffected — those artifacts don't depend on the runner choice.

No new open questions; existing Q1–Q15 in the "Open Questions" section above are unchanged.

---

## Follow-up Research 2026-06-15 (Render free tier + Docker)

User question: *"If render com has in free tier possibility to send docker?"*

**Short answer: yes, Render's free tier supports Docker**, but the practical constraints on the free tier are *not* Docker support itself — they are build minutes, RAM/CPU, spin-down, and absent shell/persistent-disk support. The Dockerfile path is *not blocked* by Render's free tier Docker support; it is (in the broader sense) blocked for production by the free tier's general fitness for a stateless inference gateway, which the team has already accepted by paying for the Starter plan per `context/foundation/infrastructure.md:65`.

### Sources

- **Render pricing** — `https://render.com/pricing` (verified 2026-06-15):
  - "Convenience" comparison table: **Docker builds ✓ Hobby, Pro, Scale, Enterprise** (Hobby is the $0/month workspace plan; the question's "free tier" maps to Hobby + Free instance).
  - "Services" section: web service instance types include **Free ($0/month, 512 MB RAM, 0.1 CPU)**, **Starter ($7/month, 512 MB, 0.5 CPU)**, **Standard ($25/month, 2 GB, 1 CPU)**, etc.
  - "Custom Docker containers" is listed as an available web service capability at all tiers.
- **Render free docs** — `https://render.com/docs/free` (verified 2026-06-15):
  - "Free web services support many (but not all) features available to web services on paid instance types."
  - "Render spins down a Free web service that goes 15 minutes without receiving any inbound traffic … This process takes about one minute."
  - "Local files lost on redeploy … *Paid* services can preserve local filesystem changes by attaching a persistent disk, but Free web services *cannot*."
  - "750 Free instance hours … per calendar month."
  - "Free Render Postgres databases expire 30 days after creation." (Confirms 30-day, not 90-day.)
  - "Free web services don't support the following features of paid instance types: Scaling … Persistent disks … Edge caching … Running one-off jobs … Shell access."
  - "Free web services can't *receive* private network traffic. They can *send* private network requests to your data stores and paid services in the same region."

### Free-tier constraint matrix (per the verified docs above)

| Constraint | Free tier value | Impact on cerebrum's Dockerfile path | Already known? |
|---|---|---|---|
| Workspace plan | Hobby (free) | No cost blocker for trying `runtime: docker` on free | n/a |
| Web service instance | Free, 512 MB RAM, 0.1 CPU | Tight for Rust binary with OTel SDK + reqwest + sqlx + tokio; will OOM under load | Known (infra.md:65 — team is on Starter, not Free) |
| Build pipeline minutes | 500/month | Cold `cargo build --release --features otel` = 10–15 min; 2 deploys/day = ~900 min/month, exceeds the cap | **Already flagged** in `context/foundation/infrastructure.md:83` ("Free workspace build minutes. 500 free minutes/month. A cold Rust release build consumes 10–15 minutes; 2 deploys/day = up to 900 minutes/month — exceeds the free tier.") |
| Spin-down | 15 min idle → ~1 min cold start | Unacceptable for an AI gateway; team already moved to Starter for this reason | **Already flagged** in `context/foundation/infrastructure.md:65` ("Free tier spins down after 15 minutes of inactivity") |
| Persistent disk | Not available on Free | Local SQLite `/tmp/cerebrum.db` is wiped on every redeploy; auto-migration to Postgres at `src/persistence.rs:180-182` is fine, but tests relying on local file state would break | New (worth flagging in the plan: don't rely on file-system state on free Render) |
| Shell access | Not available on Free | Can't `docker exec` for ad-hoc debugging; logs only | Known (infra.md:91) |
| One-off jobs | Not available on Free | A `make migrate` style one-shot container needs a paid instance | New (free Render can't run sqlx-cli as a sidecar) |
| Free Postgres | 1 GB, 30-day expiration | Cannot be a long-term production DB; sufficient for the 30-day test window | New (already constrained — see render.yaml:17 `DATABASE_URL sync: false`) |
| Private network inbound | Not available on Free | A `cerebrum` app + a paid postgres in the same region can talk; two free services cannot | New (not relevant for a single-service deploy) |
| Outbound ports | 25, 465, 587 blocked (SMTP) | No impact — cerebrum doesn't send email | n/a |
| Outbound bandwidth | 100 GB/month on Hobby, charged overage | Not a near-term concern at the project's traffic level | n/a |

### What this means for the `cicd-dev-tooling` change

- **The Dockerfile target** — `Dockerfile` at the repo root remains in scope. It is a **dev / CI artifact**; production stays on Render native Rust runtime (`render.yaml:5` `buildCommand: cargo build --release`) per the team's existing decision. The Dockerfile's purpose is to give every dev machine and the PR CI workflow the same reproducible build environment, and to be the image the compose `cerebrum` service can build from when a developer wants to run the binary in a container.
- **`runtime: docker` on render.yaml** — **not in scope for this change.** Switching `render.yaml:3` from `runtime: rust` to `runtime: docker` would be a separate decision (likely tied to a future cost-down exercise if the team wanted to standardize on Docker across all services). The risk register at `context/foundation/infrastructure.md:104` already names "Add `cargo-chef` Dockerfile for layer caching" as the motivation, but the fix it names is **for the build pipeline minutes**, not for changing the runtime. Implementing the Dockerfile makes that future swap mechanical.
- **No new open questions** — Render's free-tier posture does not block the proposed scope. The existing Q1–Q15 cover the in-scope decision space.

### New file: `.dockerignore`

A small addition that should ship with the Dockerfile: `.dockerignore` at the repo root. Without it, `docker build` will copy `target/` (which is hundreds of MB and gets rebuilt anyway), `data/`, `.git/`, and `manual-test/test-*.sh` logs into the build context. Suggested contents (verified against the working tree):
```
target/
.git/
.gitignore
.opencode/
.claude/
.vscode/
data/
*.log
/tmp/
**/*.swp
.DS_Store
*.tmp
```
The `target/` exclusion is the critical one — without it, `cargo chef cook` sees a populated `target/` and may not invalidate the cache correctly across `--features otel` toggles.

---

**End of research.** Ready to hand off to `/10x-plan` for the implementation plan. The plan step should ask the user to confirm **Makefile vs Taskfile** as the first decision, then proceed with the dependency / Dockerfile / compose / test-plan edits.
