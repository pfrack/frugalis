# Cerebrum

> Intent-aware request routing for autonomous agent workflows.

Cerebrum is a single-binary Rust/Axum gateway that sits in front of one or
more LLM providers, classifies each incoming chat-completion prompt, and
routes it to the cheapest acceptable upstream model for the inferred
intent. It exposes an OpenAI-compatible proxy API, a basic-auth operator
dashboard, asynchronous inference metadata logging, and an OpenTelemetry
exporter for observability.

It is built for a solo operator running an autonomous agent workflow who
wants lower inference cost and direct visibility into how requests are
being routed, without operating a multi-service mesh.

---

## Table of contents

- [Features](#features)
- [Quick start](#quick-start)
- [Configuration](#configuration)
- [API surface](#api-surface)
- [Architecture](#architecture)
- [Dashboard](#dashboard)
- [Deployment](#deployment)
- [Project layout](#project-layout)
- [Development](#development)
- [Security](#security)
- [License](#license)

---

## Features

- **Intent-aware proxy** — every chat-completion request is classified
  into a category (`FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`,
  `CASUAL`, …) and routed to the matching upstream model.
- **Pluggable classifier chain** — `regex` → `fewshot` → `llm`; the
  first backend that returns a non-`Fallback` result wins. All three
  backends are opt-in via `config.toml`.
- **OpenAI-compatible chat completions** with optional **server-sent
  event streaming** and a 15-second `: keepalive` comment to keep
  long-running completions readable.
- **Async inference metadata logging** with privacy-safe 200-character
  prompt snippets (full prompts are never persisted).
- **Three persistence backends**: PostgreSQL (production), SQLite
  (single-file), in-memory (default; ephemeral).
- **Operator dashboard** (basic auth) with four pages: overview,
  inference log, per-intent latency (avg + p99), and estimated cost
  savings.
- **OpenTelemetry** integration (feature-gated) exporting traces,
  metrics, and logs over OTLP/HTTP.
- **Single static binary**, deployed automatically on merge to `main`
  via GitHub Actions → Render.
- **OpenAPI 3.0.3** spec at `openapi/completions.yaml`.

## Quick start

### Prerequisites

- Rust **stable** toolchain (the deploy workflow pins
  `dtolnay/rust-toolchain@stable`).
- A POSIX shell, `curl`, and (for the manual test harness) `bash`.
- Optional: a PostgreSQL or SQLite instance if you want inference data
  to survive a restart.

### Build

```bash
cargo build --release
```

The binary is `./target/release/cerebrum`.

### Run with the embedded defaults

The binary compiles `config.toml` into itself, so a fresh checkout runs
with no config file:

```bash
export PROXY_API_BEARER_TOKEN="replace-me"
export DASHBOARD_BASIC_USER="admin"
export DASHBOARD_BASIC_PASSWORD="change-me"
./target/release/cerebrum
```

The server listens on `0.0.0.0:10000` (configurable via `[server].port`
in `config.toml` or the `PORT`-style keys).

Smoke test:

```bash
curl -i http://127.0.0.1:10000/health
# 200 ok
```

### Run with a config overlay

```bash
export CONFIG_PATH=/etc/cerebrum/config.toml
./target/release/cerebrum
```

The overlay file is merged on top of the embedded defaults — see
[Configuration](#configuration).

### Validate config without starting the server

```bash
./target/release/cerebrum --validate
# "Configuration valid" on success; per-line errors on failure
```

### Run the test suite

```bash
cargo test                  # fast unit + integration tests
cargo test slow_tests       # tests that depend on real timing
```

The CI pipeline runs a tighter subset:

```bash
cargo test auth
cargo test routes_auth
cargo test persistence       # DB integration only if DATABASE_URL is set
```

See [Development → Testing](#testing) for details.

## Configuration

Cerebrum has two configuration surfaces:

| Surface         | Where                                        | What goes here                            |
| --------------- | -------------------------------------------- | ----------------------------------------- |
| **`config.toml`** (or `.yaml`) | `config.toml` at repo root, or `CONFIG_PATH` overlay | Server, HTTP, CORS, persistence, classifiers, categories, routing, model costs, dashboard, auth providers, patterns dir, baseline model. |
| **Environment** | Shell / Render dashboard / GitHub secrets    | Secrets only: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL`, `NVIDIA_API_KEY`, `OPENAI_API_KEY`, `OTEL_*`. |

The three runtime env vars marked required below are enforced at
startup — `AuthConfig::from_env` panics on a missing or empty value.

### Required environment variables

| Variable                     | Purpose                                                       |
| ---------------------------- | ------------------------------------------------------------- |
| `PROXY_API_BEARER_TOKEN`     | Bearer token required by the `/v1/*` proxy routes.            |
| `DASHBOARD_BASIC_USER`       | Username for HTTP basic auth on `/dashboard/*`.               |
| `DASHBOARD_BASIC_PASSWORD`   | Password for HTTP basic auth on `/dashboard/*`.               |

### Optional environment variables

| Variable             | Purpose                                                                                       |
| -------------------- | --------------------------------------------------------------------------------------------- |
| `DATABASE_URL`       | Postgres connection string. If set, Cerebrum uses Postgres; otherwise it falls back to `[persistence].backend`. |
| `NVIDIA_API_KEY`     | Default upstream key used by the embedded routing entries (NVIDIA NIM).                       |
| `OPENAI_API_KEY`     | Used by the optional LLM classifier backend.                                                   |
| `OTEL_ENABLED`       | `true`/`1` enables OTLP export (also requires the `otel` Cargo feature at build time).        |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` | Standard OTel env vars, auto-detected by the exporter. |
| `CONFIG_PATH`        | Path to an overlay `config.toml` or `config.yaml` (merged on top of the embedded defaults).   |
| `RUST_LOG`           | Standard `tracing` log filter, e.g. `info,cerebrum=debug`.                                    |
| `CLASSIFY_DB_LOG`    | `true` to log records on `POST /v1/classify` in addition to `/v1/chat/completions`.           |

### `config.toml` sections (with defaults)

The full embedded default file is `config.toml` at the repo root — it is
the single source of truth for every non-secret knob. Key sections:

```toml
[server]
port = 10000
log_level = "info"        # trace | debug | info | warn | error
log_format = "compact"    # compact | full | json | pretty

[http]
max_upstream_body_bytes = 10485760   # 10 MB cap for upstream responses
keepalive_interval_secs = 15         # SSE keepalive cadence
request_body_limit_bytes = 10485760
client_timeout_secs = 120
client_connect_timeout_secs = 30
streaming_channel_capacity = 32

[cors]
allowed_origins = []      # empty = no CORS headers (secure default)

[persistence]
backend = "memory"        # memory | sqlite | postgres
# sqlite_path = "./cerebrum.db"

[database]                # pool / retry / semaphore for SQL backends
connection_retries = 3
retry_base_ms = 1000
max_connections = 10
acquire_timeout_secs = 30
idle_timeout_secs = 1800
log_concurrency_limit = 100

[classifiers]
enabled = true
order = ["regex", "fewshot", "llm"]   # first non-Fallback wins

[regex_classifier]
enabled = true
short_prompt_len = 30     # below this char count, unmatched → CASUAL

# [fewshot_classifier] — uncomment to enable; pulls bootstrap from
# data/fewshot_bootstrap.yaml and persists learned data to data_path.
# [llm_classifier] — uncomment to enable an LLM fallback tier.

[categories.FILE_READING]        # name, description, threshold, priority,
[categories.SYNTAX_FIX]          # patterns: [{ regex, weight }]
[categories.COMPLEX_REASONING]
[categories.CASUAL]

[[negative_patterns]]            # subtract from a category's score
regex = "..."
suppressed = "CATEGORY"
penalty = 2

[routing.FILE_READING]           # name → { model, endpoint,
[routing.SYNTAX_FIX]             # provider_type, api_key_env,
[routing.COMPLEX_REASONING]      # cost_per_1m_input_tokens? }
[routing.CASUAL]
[routing.DEFAULT]

baseline_model = "meta/llama-3.3-70b-instruct"
classify_db_log = false

[model_costs]                    # used by the savings estimate
"claude-3.5-sonnet" = 3.00
"gpt-4o" = 2.50
"gpt-4o-mini" = 0.15
"deepseek-chat" = 0.14

[dashboard]
default_hours = 24
hours_min = 1
hours_max = 720
page_limit = 20
page_limit_max = 100
recent_count = 5

[[auth_provider]]                # maps provider_type → HTTP auth header
type = "openai_compatible"
header = "authorization"
value_template = "Bearer {api_key}"
# anthropic, ollama, local, nvidia_nim are also supported.
```

### Patterns directory

Categories can reference a `patterns_file` (path is relative to
`patterns_dir`, which defaults to `./patterns` and is sandboxed to
prevent path-escape). Each line is `<weight> | <regex>`:

```text
3 | (?i)\b(?:read|show|display)\s+(?:the\s+)?file\b
2 | (?i)\b(?:line|lines)\s+\d+
```

## API surface

The full machine-readable contract is in `openapi/completions.yaml`.

| Method | Path                     | Auth        | Purpose                                                     |
| ------ | ------------------------ | ----------- | ----------------------------------------------------------- |
| GET    | `/health`                | public      | Liveness check — returns `200 "ok"`.                        |
| POST   | `/v1/chat/completions`   | Bearer      | Classify → route → upstream. JSON or SSE (when `stream:true`). |
| POST   | `/v1/classify`           | Bearer      | Classify-only JSON response (no upstream call).             |
| POST   | `/v1/feedback`           | Bearer      | Submit feedback for few-shot retraining.                    |
| GET    | `/dashboard/`            | Basic auth  | Overview (status, quick stats, recent inferences).          |
| GET    | `/dashboard/inferences`  | Basic auth  | Paginated, filterable inference log.                        |
| GET    | `/dashboard/latency`     | Basic auth  | Per-category avg + p99 over a time window.                  |
| GET    | `/dashboard/savings`     | Basic auth  | Estimated cost savings vs `baseline_model`.                 |
| GET    | `/dashboard/static/*`    | Basic auth  | `static/dashboard.css` and other static assets.             |

### Proxy example

```bash
curl -X POST http://127.0.0.1:10000/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "messages":[{"role":"user","content":"fix this bug"}]
  }'
```

Successful response (routed upstream, JSON):

```json
{ "id": "…", "choices": […] }
```

When no upstream is configured for the matched category, the response
is a classification-only envelope:

```json
{ "status": "classified", "category": "SYNTAX_FIX",
  "model": "meta/llama-3.1-8b-instruct", "tier": "Regex" }
```

### Streaming example

```bash
curl -N -X POST http://127.0.0.1:10000/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "messages":[{"role":"user","content":"explain a hash map"}],
    "stream": true
  }'
```

- Successful responses stream raw SSE bytes from the upstream.
- A `: keepalive\n\n` comment is injected every `keepalive_interval_secs`
  of silence.
- Non-2xx upstream responses emit a single
  `event: error\ndata: {"error":"<msg>"}\n\n` frame, with the upstream
  status code preserved and a 2 KB error-body cap.

### Skip classification (advanced)

Send `X-Cerebrum-Category` and `X-Cerebrum-Model` headers to skip the
classifier and use the matching routing entry directly. Useful for
debugging and integration tests.

### Feedback example

```bash
curl -X POST http://127.0.0.1:10000/v1/feedback \
  -H "Authorization: Bearer $PROXY_API_BEARER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "text": "show me the contents of config.toml",
    "predicted_category": "FILE_READING",
    "actual_category": "FILE_READING",
    "satisfaction": 0.9
  }'
```

A `satisfaction < 0.99` is treated as a feedback signal; once
`retraining_threshold` feedbacks have been collected, the few-shot
classifier retrains in the background. The actual response is
`200 {"status":"accepted"}`.

## Architecture

### Request flow

```text
client ─POST /v1/chat/completions─▶  Axum router
                                          │
                  ┌───────────────────────┼────────────────────────┐
                  │                       │                        │
        AuthConfig::validate       extract_last_user_message       │
        (constant-time bearer)            │                        │
                  │                       ▼                        │
                  │       ┌──── ClassifierChain (in order) ───┐  │
                  │       │  regex  →  fewshot  →  llm        │  │
                  │       │  first non-Fallback wins           │  │
                  │       └───────────────────────────────────┘  │
                  │                       │                        │
                  │                       ▼                        │
                  │       resolve api_key_env from routing       │
                  │                       │                        │
                  │                       ▼                        │
                  │       build upstream request (auth headers,  │
                  │       override "model" field, JSON body)     │
                  │                       │                        │
                  │                       ▼                        │
                  │              reqwest POST upstream           │
                  │                       │                        │
                  │       ┌───────────────┴──────────────┐        │
                  │       ▼                              ▼        │
                  │   stream:true?                  stream:false  │
                  │   SSE proxy + keepalive         buffered JSON │
                  │       │                              │        │
                  │       └──────────────┬───────────────┘        │
                  │                      ▼                        │
                  │   return response                            │
                  │                      │                        │
                  │                      ▼                        │
                  │   fire-and-forget log_inference               │
                  │   (semaphore-bounded, 200-char snippet)       │
                  └──────────────────────────────────────────────┘
```

### Module map

| Module                   | Responsibility                                                       |
| ------------------------ | -------------------------------------------------------------------- |
| `src/main.rs`            | Router assembly, request handlers, SSE keepalive, CLI entrypoint.    |
| `src/auth.rs`            | `AuthConfig`, constant-time bearer/basic checks, Axum auth layers.  |
| `src/config.rs`          | `ConfigRoot` schema, loaders, `--validate`, overlay merge.           |
| `src/intent_classifier.rs` | `IntentClassify` trait, `RegexClassifier`, `LLMClassifier`, `ClassifierChain`. |
| `src/fewshot_classifier.rs` | Bag-of-words cosine-similarity classifier with feedback retraining. |
| `src/persistence.rs`     | `PersistenceBackend` trait + `Memory` / `Sqlite` / `Postgres` impls. |
| `src/dashboard.rs`       | Page registry, Askama templates, dashboard sub-router.               |
| `src/routing.rs`         | `RouteEntry`, `ModelCosts`, default model constants.                 |
| `src/telemetry.rs`       | OTel providers + pre-allocated metrics (feature-gated).             |

### Persistence backend selection

```text
DATABASE_URL set?
  ├─ yes → Postgres (sqlx pool, retries, sqlx::migrate! on boot)
  └─ no  → [persistence].backend
              ├─ "sqlite" → SqliteBackend("./cerebrum.db" or :memory:)
              ├─ "postgres" → warn + fall back to memory
              └─ <other/empty> → MemoryBackend (capped at 10 000 records)
```

### Classifier chain

`IntentClassify` is a trait with one async method:
`async fn classify(&self, prompt: &str) -> ClassificationResult`.

`ClassifierChain` iterates backends in `classifiers.order` and returns
the first result whose `tier` is not `Fallback`. If all backends return
`Fallback`, the last fallback is returned. The default order is
`["regex", "fewshot", "llm"]`; you can disable or reorder them in
`config.toml`.

The `tier` enum is currently `Regex | FewShot | Fallback` — successful
LLM matches report as `Regex` (an architectural detail; see
`src/main.rs:1649-1653`).

### Privacy

Persisted `prompt_snippet` is **capped at 200 characters** by
`extract_snippet` (`src/persistence.rs:1118-1121`). The true message
length is recorded separately as `prompt_char_count` so cost math
remains accurate without retaining the full prompt body. Messages
arrays over 1 000 entries are silently dropped to bound work.

## Dashboard

The four pages are registered in `PAGES` (`src/dashboard.rs:44-65`).
The sidebar is rendered from that single source of truth via
`templates/base.html`.

| Page                  | Path                       | What you see                                              |
| --------------------- | -------------------------- | --------------------------------------------------------- |
| **Dashboard**         | `/dashboard/`              | Status (gateway/DB), quick stats, recent inferences, est. savings. |
| **Inference Logs**    | `/dashboard/inferences`    | Paginated, filterable by category and model, with snippet, category, model, duration. |
| **Latency**           | `/dashboard/latency`       | Per-category request count, avg duration, p99 over a time window. |
| **Savings**           | `/dashboard/savings`       | Estimated $ saved vs the configured `baseline_model`.     |

The dashboard uses Askama templates compiled at build time
(`askama = "0.16"`, `askama_web`). All dashboard templates extend
`templates/base.html` and override only `{% block content %}`. To add
a new page: add a `NavPage` entry to `PAGES`, define a struct with the
`dashboard_page!` macro, write a handler, and add one `.route()` line in
`src/dashboard.rs:349-357`.

## Deployment

### Render (current production target)

`render.yaml` declares a `web` service:

- `buildCommand`: `cargo build --release`
- `startCommand`: `./target/release/cerebrum`
- `healthCheckPath`: `/health`
- Env vars (`PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`,
  `DASHBOARD_BASIC_PASSWORD`, `DATABASE_URL`) are `sync: false` — set
  them in the Render dashboard, never commit them.

### GitHub Actions (`.github/workflows/deploy.yml`)

Triggered on push to `main`. Sequence:

1. Checkout + install stable Rust.
2. `cargo test auth` and `cargo test routes_auth` (auth gates the rest).
3. `cargo test persistence` (with `SQLX_OFFLINE=true`); DB integration
   tests run only if the runner has `DATABASE_URL` set.
4. `cargo build --release`.
5. POST the commit SHA to the `RENDER_DEPLOY_HOOK` secret. A missing
   webhook secret fails the job loudly.

The workflow uses `concurrency: render-production` with
`cancel-in-progress: true` so superseded pushes don't fight for the
deploy slot.

### Manual / on-prem

```bash
cargo build --release
export PROXY_API_BEARER_TOKEN=... DASHBOARD_BASIC_USER=... DASHBOARD_BASIC_PASSWORD=...
export DATABASE_URL=postgres://user:pass@host:5432/cerebrum      # optional
./target/release/cerebrum
```

Run the process under a supervisor (systemd, runit, Docker, etc.) and
make sure it receives `SIGTERM` for a clean shutdown — the
`shutdown_signal` handler in `src/main.rs:460-473` flushes in-flight
SSE streams and OTel providers.

## Project layout

```text
cerebrum/
├── AGENTS.md                     # contributor guide (conventions, layout, testing)
├── Cargo.toml                    # Rust manifest (axum, tokio, sqlx, askama, …)
├── config.toml                   # embedded default config (single source of truth)
├── render.yaml                   # Render service definition
├── openapi/
│   └── completions.yaml          # OpenAPI 3.0.3 spec for the proxy API
├── src/
│   ├── main.rs                   # router, handlers, SSE, telemetry init, tests
│   ├── auth.rs                   # AuthConfig + Axum auth layers
│   ├── config.rs                 # ConfigRoot, loaders, --validate, merge_configs
│   ├── intent_classifier.rs      # RegexClassifier, LLMClassifier, ClassifierChain
│   ├── fewshot_classifier.rs     # bag-of-words classifier + feedback learning
│   ├── persistence.rs            # MemoryBackend / SqliteBackend / PostgresBackend
│   ├── dashboard.rs              # PAGES registry, Askama templates, dashboard routes
│   ├── routing.rs                # RouteEntry, ModelCosts, default models
│   └── telemetry.rs              # OTel providers + metrics (feature-gated)
├── templates/
│   ├── base.html                 # sidebar layout (auto-nav)
│   └── dashboard/
│       ├── index.html            # overview
│       ├── inferences.html       # log table + filters
│       ├── latency.html          # per-category latency
│       └── savings.html          # cost-savings estimate
├── static/
│   └── dashboard.css             # dashboard styles
├── migrations/                   # sqlx migrations for the inferences table
│   ├── 001_create_inferences.sql
│   ├── 002_inferences_request_id_unique.sql
│   └── 003_add_prompt_char_count.sql
├── data/
│   └── fewshot_bootstrap.yaml    # initial few-shot training examples
├── routing_examples/             # sample routing TOML files
├── manual-test/                  # interactive manual test harness
├── scripts/
│   └── manual_tests.sh           # --auto / --basic smoke + integration entrypoints
├── context/                      # foundation/ + changes/ + archive/ (10x workflow)
│   ├── foundation/               # PRD, tech stack, lessons, roadmap, test plan
│   ├── changes/                  # active change folders with plan/research/change.md
│   └── archive/                  # completed changes (date-prefixed)
└── .github/workflows/
    └── deploy.yml                # CI: test → build → Render deploy webhook
```

## Development

### Conventions (enforced by `AGENTS.md`)

- **Constant-time comparison** for all security-sensitive string
  matching (via `subtle` + HMAC-SHA256 in `src/auth.rs:169-185`).
- **`Arc<AuthConfig>` via Axum state** for middleware; never hardcode
  secrets.
- **Middleware signature** —
  `State(config): State<Arc<AuthConfig>>`, `headers: HeaderMap`,
  `request: Request<Body>`, `next: Next`.
- **No comments in code** unless explicitly requested.
- **Tests are inline** with the code they test (see
  `src/main.rs:1196+` and the `#[cfg(test)]` modules in every file).
- **Dashboard pages** are added via the `dashboard_page!` macro and the
  `PAGES` registry — never by editing `base.html` directly.

A small set of **recurring rules** is captured in
`context/foundation/lessons.md` (e.g. delete dead code rather than
suppress warnings, log before falling back, prefer dynamic SQL
`WHERE` building over duplicated branches, etc.). Re-read this file
before touching the classifier or proxy code paths.

### Testing

| Test type        | Command                                      | When                                         |
| ---------------- | -------------------------------------------- | -------------------------------------------- |
| Fast unit + integration | `cargo test`                          | Local + CI on every push.                    |
| Auth gate        | `cargo test auth`                            | First gate in CI; blocks deploy on failure.  |
| Route auth gate  | `cargo test routes_auth`                     | First gate in CI.                            |
| Persistence contract | `cargo test persistence`                 | CI; DB integration tests run if `DATABASE_URL` is set. |
| Slow / timing    | `cargo test slow_tests`                      | Manual or on-demand.                         |
| Manual harness   | `bash scripts/manual_tests.sh --auto`        | Pre-release smoke.                           |
| Basic smoke      | `bash scripts/manual_tests.sh --basic`       | Quick local check.                           |
| Interactive      | `bash manual-test/run.sh`                    | Manual exploration.                          |

Test-only dependencies: `httpmock 0.7` (mock upstreams),
`serial_test 3` (env-var serialization), `testcontainers 0.27`
(real Postgres for persistence integration tests).

### Telemetry (OpenTelemetry)

The OTel integration is **feature-gated** and **opt-in**:

```bash
cargo build --release --features otel
OTEL_ENABLED=true \
OTEL_EXPORTER_OTLP_ENDPOINT=https://collector.example.com \
OTEL_SERVICE_NAME=cerebrum \
./target/release/cerebrum
```

Pre-allocated instruments (`src/telemetry.rs:28-33`):

- `cerebrum.requests.total` — counter of incoming requests.
- `cerebrum.request.duration_seconds` — end-to-end latency histogram.
- `cerebrum.classification.total` — counter of classifications.
- `cerebrum.upstream.duration_seconds` — upstream-only latency histogram.

The OTel providers are shut down in the recommended order (traces →
logs → metrics) on `SIGTERM`/`SIGINT`.

### OpenAPI

`openapi/completions.yaml` is the machine-readable spec for the three
proxy endpoints (`/v1/chat/completions`, `/v1/classify`, `/v1/feedback`).
When you add or change an endpoint, update this file in the same
change. The category enum on the response (`COMPLEX_REASONING`,
`FILE_READING`, `SYNTAX_FIX`, `CASUAL`) is a public contract — see
`src/intent_classifier.rs:53-63`.

### Change workflow

All non-trivial work goes through a `context/changes/<change-id>/`
folder containing a `change.md`, `plan.md` (when planned), and
`research.md` (after research). The `10x-` skill family
(`/10x-shape`, `/10x-prd`, `/10x-plan`, `/10x-implement`,
`/10x-research`, …) drives this. `AGENTS.md` documents the layout
and naming.

## Security

- Bearer and basic-auth credentials are compared in **constant time**
  via HMAC-SHA256 + `subtle` (`src/auth.rs:169-185`).
- Dashboard auth responds with a `WWW-Authenticate: Basic` challenge
  on unauthorized requests (`src/auth.rs:211-220`).
- CORS headers are **off by default** — the `[cors].allowed_origins`
  list is empty in the embedded defaults. Add origins only for
  trusted frontends.
- Prompt bodies are **never** persisted. Only a 200-character snippet
  and the true character count are stored.
- The `request_body_limit_bytes` and `max_upstream_body_bytes` knobs
  default to 10 MB to bound memory pressure.
- Upstream error bodies are **capped at 2 KB** in both the streaming
  and non-streaming paths, with a 512-character display truncation
  (`src/main.rs:651-661, 825-836`).
- SSE error events go through `format_sse_error_event`
  (`src/main.rs:790-796`) which JSON-escapes `\`, `"`, `\n`, `\r` to
  prevent a malicious upstream from injecting SSE frames.
- Secrets are loaded **only** from environment variables
  (`AuthConfig::from_env` panics on missing/empty values to fail
  closed). `config.toml` has no `secret`-shaped fields.

If you find a security issue, please report it privately rather than
opening a public issue.

## License

Add a `LICENSE` file at the repo root before publishing. Until one is
committed, all rights are reserved by the project authors.
