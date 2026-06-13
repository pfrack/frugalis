---
date: 2026-06-13T20:46:00+00:00
researcher: pfrack
git_commit: d0149a092cd0826fcd3a1132a6bbddcdaec26ad7
branch: main
repository: cerebrum
topic: "Bootstrap README.md for the Cerebrum repository"
tags: [research, codebase, readme, rust, axum, gateway, intent-routing]
status: complete
last_updated: 2026-06-13
last_updated_by: pfrack
---

# Research: Bootstrap README.md for the Cerebrum repository

**Date**: 2026-06-13T20:46:00+00:00
**Researcher**: pfrack
**Git Commit**: `d0149a092cd0826fcd3a1132a6bbddcdaec26ad7`
**Branch**: main
**Repository**: cerebrum

## Research Question

The repository has no top-level `README.md`. The user asked for a
comprehensive, operator + developer README (~400–600 lines) that lets a new
reader understand what Cerebrum is, run it, configure it, deploy it, and
contribute to it — without having to read every source file.

## Summary

Cerebrum is a **single-binary Rust/Axum intent-aware LLM gateway** that
sits in front of one or more upstream model providers, classifies each
incoming chat-completion prompt into a category (via a pluggable
classifier chain), and routes the request to the cheapest acceptable
upstream model for that category. It exposes a basic-auth operator
dashboard for inspecting recent inferences, per-intent latency, and an
estimated cost-savings figure. It persists inference metadata (with
privacy-safe 200-char prompt snippets) to PostgreSQL / SQLite / in-memory
and ships with an OpenAPI spec, an SSE streaming proxy with keepalive, an
optional OTel exporter, and a GitHub Actions → Render deploy pipeline.

Key facts the README must surface:
- **Single Rust binary** (`cerebrum`), Axum 0.8, Tokio 1, Askama 0.16, sqlx 0.8.
- **Three required env vars** at runtime: `PROXY_API_BEARER_TOKEN`,
  `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`. `DATABASE_URL` is
  optional (falls back to SQLite → memory).
- **Three persistable backends**: PostgreSQL (production, via `DATABASE_URL`),
  SQLite (file-backed `./cerebrum.db`), in-memory (ephemeral, default).
- **Pluggable classifier chain** (`regex` → `fewshot` → `llm`); first
  non-`Fallback` wins. The default is regex-only; fewshot and LLM backends
  are opt-in via `config.toml`.
- **OpenAI-compatible chat completions API surface** with three
  authenticated proxy routes (`/v1/chat/completions`, `/v1/classify`,
  `/v1/feedback`) and a four-page basic-auth dashboard
  (`/dashboard`, `/dashboard/inferences`, `/dashboard/latency`,
  `/dashboard/savings`).
- **SSE streaming with keepalive** and a 2 KB upstream-error body cap.
- **Privacy by construction**: 200-char `prompt_snippet`, never the full
  prompt; `prompt_char_count` records the true length for cost math.
- **Deploy**: `cargo build --release` → `target/release/cerebrum`. CI
  workflow `.github/workflows/deploy.yml` runs `cargo test auth`,
  `cargo test routes_auth`, then `cargo test persistence`, then
  `cargo build --release`, then POSTs the Render deploy webhook.

## Detailed Findings

### What the project is (PRD + tech-stack)

- **Vision** — `context/foundation/prd.md:18-22`: a "lightweight
  intent-aware gateway [that] can apply a low-cost first-pass decision,
  choose an appropriate processing path, and expose routing outcomes so
  the operator can continuously tune efficiency."
- **Primary persona** — `prd.md:27-28`: "Solo developer/operator running
  autonomous agent workflows who needs lower inference cost and direct
  visibility into routing behavior."
- **Stack** — `context/foundation/tech-stack.md`: Rust + Axum, self-host
  on Render, GitHub Actions with auto-deploy on merge to `main`.

### Source layout (`src/`)

- `src/main.rs` — Axum router, route wiring, request handlers, SSE
  keepalive, the `AppState` struct. Routes built in `build_app`
  (`src/main.rs:1155-1194`):
  - `GET  /health` (public)
  - `POST /v1/chat/completions`, `POST /v1/classify`, `POST /v1/feedback` (bearer)
  - `nest /dashboard/*` (basic auth) — routes built in
    `src/dashboard.rs:349-357`.
- `src/auth.rs` — `AuthConfig`, constant-time bearer/basic auth
  (`constant_time_eq_str` via HMAC-SHA256 + `subtle`),
  `proxy_auth_layer` / `dashboard_auth_layer` built on
  `AsyncRequireAuthorizationLayer`.
- `src/config.rs` — `ConfigRoot` (mirrors `config.toml` 1:1), loaders for
  every section, format auto-detection (`.toml` vs `.yaml`), `--validate`
  CLI mode, and the `merge_configs` overlay semantics.
- `src/intent_classifier.rs` — `IntentClassify` trait, `RegexClassifier`,
  `LLMClassifier`, `ClassifierChain`, plus the `ClassificationTier`
  enum (`Regex | FewShot | Fallback`).
- `src/fewshot_classifier.rs` — TF-style bag-of-words cosine-similarity
  classifier with bootstrap examples from `data/fewshot_bootstrap.yaml`,
  feedback learning via `POST /v1/feedback`, thresholded by
  `confidence_threshold` and `cold_start_threshold`.
- `src/persistence.rs` — `PersistenceBackend` trait, three impls
  (`MemoryBackend`, `SqliteBackend`, `PostgresBackend`), `InferenceRecord`
  / `InferenceLog` / `LatencySummary` / `SavingsEstimate`,
  fire-and-forget `log_inference` with a bounded semaphore.
- `src/dashboard.rs` — `PAGES` registry (`Dashboard`, `Inference Logs`,
  `Latency`, `Savings`), `dashboard_page!` macro, four Askama-backed
  handlers, sidebar nav auto-rendered in `templates/base.html:27-34`.
- `src/routing.rs` — `RouteEntry`, `ModelCosts`, default model
  constants `meta/llama-3.1-8b-instruct` and
  `meta/llama-3.3-70b-instruct`.
- `src/telemetry.rs` — OTel providers (`OtelGuard` + `Metrics`),
  behind the `otel` Cargo feature and the `OTEL_ENABLED=true` env var.

### Configuration (`config.toml` + overlays)

- **Embedded defaults** are compiled into the binary
  (`src/main.rs:98` — `include_str!("../config.toml")`) so a freshly
  built binary runs with no config file.
- **`CONFIG_PATH=/path/to/overlay.toml`** overlays fields on top of the
  embedded defaults (`src/main.rs:107-119`,
  `src/config.rs:872-965` — `merge_configs`). Override sections
  (`classifiers`, `categories`, `routing`, `auth_providers`,
  `model_costs`, `negative_patterns`) are fully replaced; other sections
  are field-by-field merged.
- **YAML is also accepted** — auto-detected by extension
  (`src/config.rs:636-641`).
- **Validation mode**: `cerebrum --validate` parses the config (overlay
  if `CONFIG_PATH` is set, else embedded) and exits non-zero with
  per-error messages (`src/config.rs:702-865`).

### Persistence (3 backends, ordered by precedence)

`src/main.rs:343-412` resolves the backend in this order:

1. `DATABASE_URL` env var set → **Postgres** via
   `PostgresBackend::from_env` (`src/persistence.rs:126-187`), with health
   retries + `sqlx::migrate!()` applied on startup.
2. `DATABASE_URL` unset, `[persistence].backend = "sqlite"` →
   `SqliteBackend::from_path("./cerebrum.db")` (or `:memory:`).
3. Otherwise → `MemoryBackend` (capped at 10 000 records,
   `src/persistence.rs:217-221`).

Migrations live in `migrations/` (3 SQL files); applied automatically
for Postgres only.

### API surface (matches `openapi/completions.yaml`)

| Method | Path                       | Auth        | Purpose                                             |
| ------ | -------------------------- | ----------- | --------------------------------------------------- |
| GET    | `/health`                  | public      | Liveness check, returns `200 "ok"`.                 |
| POST   | `/v1/chat/completions`     | Bearer      | Classify → route → upstream (SSE if `stream:true`). |
| POST   | `/v1/classify`             | Bearer      | Classify-only JSON response.                        |
| POST   | `/v1/feedback`             | Bearer      | Submit feedback for few-shot retraining.            |
| GET    | `/dashboard/`              | Basic auth  | Overview (status, quick stats, recent inferences).  |
| GET    | `/dashboard/inferences`    | Basic auth  | Paginated, filterable inference log.                |
| GET    | `/dashboard/latency`       | Basic auth  | Per-category avg + p99 over a time window.          |
| GET    | `/dashboard/savings`       | Basic auth  | Estimated cost savings vs `baseline_model`.         |
| GET    | `/dashboard/static/*`      | Basic auth  | `static/dashboard.css`.                             |

### Classification (the `IntentClassify` trait)

- **Trait** — `src/intent_classifier.rs:104-114`:
  `async fn classify(&self, prompt: &str) -> ClassificationResult`
  + `fn get_routing()` returning the backend's `HashMap<String, RouteEntry>`.
- **Chain** — `ClassifierChain::new(backends)` iterates them in order,
  returning the first non-`Fallback` result; if all return `Fallback`,
  the last `Fallback` is returned
  (`src/intent_classifier.rs:158-175`).
- **Tiers** — `Regex | FewShot | Fallback`. Successful LLM matches
  currently report as `Regex` (architectural detail — see
  `src/main.rs:1649-1653` comment).
- **Default categories** in `config.toml:64-135`:
  `FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL`.
  Category names are a public API contract (see
  `src/intent_classifier.rs:53-63`).
- **Negative patterns** — `[[negative_patterns]]` entries subtract from
  category scores (`src/intent_classifier.rs:592-603`).

### Privacy & invariants (must document in README)

- 200-char snippet cap (`src/persistence.rs:1118-1121`,
  `extract_snippet`).
- `prompt_char_count` stores the true message length separately for
  cost math (`src/persistence.rs:1084-1110`, `extract_last_user_message`).
- 2 KB upstream error body cap in both streaming and non-streaming paths
  (`src/main.rs:651-661, 825-836`).
- JSON-escape rule for SSE error events
  (`src/main.rs:790-796`, `format_sse_error_event`).
- Constant-time credential comparison
  (`src/auth.rs:169-185`, `constant_time_eq_str`).

### Deployment surface

- `render.yaml` — Render web service: `cargo build --release` →
  `target/release/cerebrum`; env vars `PROXY_API_BEARER_TOKEN`,
  `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL` are
  `sync: false` (set in Render dashboard, not committed).
- `.github/workflows/deploy.yml` — runs on push to `main`; runs
  `cargo test auth`, `cargo test routes_auth`, `cargo test persistence`
  (DB integration only if `DATABASE_URL` is set in the runner),
  `cargo build --release`, then POSTs the `RENDER_DEPLOY_HOOK` secret.
  Missing webhook secret fails the build loudly.

### Test layout

- `src/main.rs:1196-1256` (`test_categories`, `test_negative_patterns`) and
  `mod tests` (`src/main.rs:1264+`) — fast unit + integration tests
  run by `cargo test`.
- `mod slow_tests` (referenced from `AGENTS.md`) — tests with real
  delays, run with `cargo test slow_tests`.
- Tools: `httpmock 0.7` (mock upstream), `serial_test 3` (env-var
  serialization), `testcontainers 0.27` (real Postgres in
  `persistence.rs:1218-1262`).
- Manual harness: `scripts/manual_tests.sh` (`--auto` for full suite,
  `--basic` for smoke, default for interactive) and
  `manual-test/run.sh`.

### Project conventions (from `AGENTS.md` + `lessons.md`)

- **Constant-time comparison** for all security-sensitive string matching.
- **`Arc<AuthConfig>` via Axum state** to middleware; never hardcode secrets.
- **Middleware signature** —
  `State(config): State<Arc<AuthConfig>>`, `headers: HeaderMap`,
  `request: Request<Body>`, `next: Next`.
- **Dashboard pages** — register in `PAGES` (`src/dashboard.rs:44-65`),
  use the `dashboard_page!` macro, extend `base.html`, only override
  `{% block content %}`.
- **Lessons** in `context/foundation/lessons.md` — append-only register
  of recurring rules (delete dead code, log before fallback, dynamic
  WHERE building, OpenAPI generator, etc.).

## Code References

(GitHub permalinks: `https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/…`)

- `src/main.rs:1155-1194` — [`build_app`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/main.rs#L1155-L1194) (router assembly).
- `src/main.rs:519-574` — [`classify_and_log`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/main.rs#L519-L574) (shared classify + log path).
- `src/main.rs:868-1067` — [`completion_handler`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/main.rs#L868-L1067) (classify → route → proxy).
- `src/main.rs:715-775` — [`handle_streaming_response`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/main.rs#L715-L775) (SSE + keepalive).
- `src/main.rs:790-861` — [`format_sse_error_event`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/main.rs#L790-L861) + `handle_streaming_error`.
- `src/auth.rs:16-58` — [`AuthConfig::from_env`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/auth.rs#L16-L58) and constant-time checks.
- `src/config.rs:872-1008` — [`merge_configs`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/config.rs#L872-L1008) + `ConfigRoot` schema.
- `src/config.rs:702-865` — [`run_validation`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/config.rs#L702-L865) (for `--validate`).
- `src/intent_classifier.rs:104-175` — [`IntentClassify` trait + `ClassifierChain`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/intent_classifier.rs#L104-L175).
- `src/intent_classifier.rs:528-676` — [`RegexClassifier` scoring algorithm](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/intent_classifier.rs#L528-L676).
- `src/fewshot_classifier.rs:314-366` — [`FewShotClassifier` classify path](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/fewshot_classifier.rs#L314-L366).
- `src/persistence.rs:18-34` — [`PersistenceBackend` trait](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/persistence.rs#L18-L34).
- `src/persistence.rs:481-746` — [`SqliteBackend`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/persistence.rs#L481-L746) (schema, queries, p99).
- `src/persistence.rs:748-986` — [`PostgresBackend`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/persistence.rs#L748-L986) (PERCENTILE_CONT).
- `src/persistence.rs:1118-1130` — [`extract_snippet`, `prompt_chars_to_cost`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/persistence.rs#L1118-L1130).
- `src/dashboard.rs:44-65` — [`PAGES` registry](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/dashboard.rs#L44-L65).
- `src/dashboard.rs:81-134` — [`dashboard_page!` macro + 4 template structs](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/dashboard.rs#L81-L134).
- `src/dashboard.rs:349-357` — [`routes()`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/dashboard.rs#L349-L357) (dashboard sub-router).
- `src/telemetry.rs:41-148` — [`init`](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/telemetry.rs#L41-L148) (OTel providers + metrics).
- `src/routing.rs:1-50` — [`RouteEntry`, `ModelCosts`, default models](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/src/routing.rs#L1-L50).
- `config.toml:1-224` — [full embedded configuration](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/config.toml).
- `render.yaml:1-18` — [Render service definition](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/render.yaml).
- `.github/workflows/deploy.yml:1-61` — [CI build + deploy webhook](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/.github/workflows/deploy.yml).
- `openapi/completions.yaml:1-257` — [full OpenAPI 3.0.3 spec](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/openapi/completions.yaml).
- `migrations/001-003` — [`inferences` table schema](https://github.com/pfrack/cerebrum/blob/d0149a092cd0826fcd3a1132a6bbddcdaec26ad7/migrations/001_create_inferences.sql).

## Architecture Insights

- **Single-binary, single-async-runtime design.** Everything runs in one
  Tokio process; the persistence layer is the only side effect. The
  gateway never blocks the response on the DB — `log_inference` spawns a
  detached task and immediately returns (`src/persistence.rs:1141-1160`).
- **Classifier chain as a strategy pattern.** `IntentClassify` is a
  trait; the chain composition lets you add backends (e.g. an ML model
  classifier) without touching routing or proxy code.
- **Routing is data, not code.** Every category in `config.toml` has a
  matching `routing.<CATEGORY>` entry with `model`, `endpoint`,
  `provider_type`, `api_key_env`, and optional `cost_per_1m_input_tokens`.
  Adding a new category is purely a config change, gated by
  validation in `src/config.rs:769-780` (every routing key must match a
  category).
- **SSE proxy is a hard-fork** of buffered response:
  `completion_handler` checks `req_body.stream` and either delegates to
  `handle_streaming_response` (with keepalive) or
  `handle_buffered_response`. Both paths share the same upstream-error
  envelope (`{"error":"upstream_error","status":N,"message":...}`).
- **Dashboard nav is a single source of truth** —
  `PAGES` (`src/dashboard.rs:44-65`) drives the sidebar via
  `base.html:27-34`. Adding a dashboard page is a 5-step recipe
  (template, `PAGES` entry, `dashboard_page!`, handler, route line).
- **Config is layered** — embedded defaults, optional overlay, env vars
  for secrets. The `--validate` mode makes the config self-documenting.

## Historical Context (from prior changes)

The 28 changes in `context/archive/` chronicle the system's evolution
from the auth scaffold (2026-05-26) to the latest test-rollout phases.
Highlights relevant to a README:

- `archive/2026-05-26-auth-scaffold-access-keys` — established the
  bearer/basic auth model and the `AuthConfig::from_env` panic on missing
  vars.
- `archive/2026-05-26-data-persistence-async-logging` — established the
  fire-and-forget `log_inference` + bounded semaphore pattern.
- `archive/2026-06-01-classify-endpoint` and
  `archive/2026-06-01-upstream-proxy-routing` — the
  classification-only vs routed-to-upstream path split.
- `archive/2026-06-01-sse-streaming-proxy` — introduced the
  keepalive + 2 KB error-cap + `format_sse_error_event` invariants
  (now codified as F2 review fixes in `lessons.md`).
- `archive/2026-06-01-dashboard-template-scaffold` and
  `archive/2026-06-06-dashboard-mvp-rewrite` — the Askama
  + `dashboard_page!` macro + `PAGES` registry pattern.
- `archive/2026-06-06-intent-classifier-trait` — introduced
  `IntentClassify` + `ClassifierChain`.
- `archive/2026-06-07-llm-classifier` and
  `archive/2026-06-09-fewshot-classifier` — the LLM and fewshot
  classifier backends.
- `archive/2026-06-09-in-memory-db-fallback`,
  `archive/2026-06-10-move-all-config-to-file`,
  `archive/2026-06-11-config-format-upgrade` — the three-backend
  fallback chain and the move to a single `config.toml`.
- `context/changes/testing-critical-path-regression-guards` — current
  rollout of integration tests around the classifier chain and
  `completion_handler` invariants.

## Related Research

- `context/changes/config-ux/research.md` — research on the config UX
  follow-up (related to the `config.toml` reference section in the
  README).
- `context/archive/2026-06-07-llm-classifier/plan.md` — design rationale
  for the LLM classifier (relevant to the "Classifier chain" section).
- `context/archive/2026-06-01-sse-streaming-proxy/plan.md` — design
  rationale for the streaming path (relevant to the "Streaming &
  keepalive" section).

## Open Questions

- Whether the README should reference the in-flight `config-ux` change
  (`/dashboard/config`-style quickstart) — left as a follow-up for the
  user, since the change is still in `preparing` status.
- Whether to call out the "feature-gated OTel" as stable or experimental
  — the OTel integration is recent (`Otel (#11)`); README will mark it
  as opt-in.
