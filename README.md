# Cerebrum

[![CI](https://github.com/pfrack/cerebrum/actions/workflows/deploy.yml/badge.svg)](https://github.com/pfrack/cerebrum/actions/workflows/deploy.yml)
![Rust Edition](https://img.shields.io/badge/edition-2021-blue)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

**Intent-aware LLM request routing gateway.** Cerebrum is a single Rust binary that sits between your agents and upstream model providers. It classifies each incoming prompt into an intent category, selects the cheapest acceptable model for that category, and proxies the request with full SSE streaming support.

## Features

Cerebrum is designed for solo developers and operators running autonomous agent workflows who need lower inference cost and direct visibility into routing behavior. Its key capabilities:

- **Intent-aware routing** — classifies prompts by category and dispatches to the cheapest suitable upstream model
- **Pluggable classifier chain** — configurable backends: regex (built‑in), few‑shot (TF‑IDF cosine similarity), and LLM (opt‑in); first non‑fallback wins
- **OpenAI‑compatible API** — drop-in `POST /v1/chat/completions`, `POST /v1/classify`, and `POST /v1/feedback` endpoints
- **SSE streaming with keepalive** — real‑time streaming proxy with configurable keepalive interval and a 2 KB upstream‑error body cap
- **Three persistence backends** — PostgreSQL (production), SQLite (file‑backed), or in‑memory (zero config); selected automatically from environment and config
- **Operator dashboard** — four basic‑auth pages (Overview, Inference Logs, Latency, Savings) with auto‑generated sidebar navigation
- **Privacy by construction** — stores a 200‑character prompt snippet, never the full prompt; records character count separately for cost math
- **Bearer + Basic authentication** — bearer token for proxy routes, basic auth for dashboard routes; constant‑time credential comparison
- **Config‑driven routing** — every category, model mapping, and provider is defined in `config.toml`; adding a category is a config change, not a code change
- **CI/CD to Render** — GitHub Actions runs auth, route, and persistence tests on push to `main`, then deploys via Render webhook
- **Opt‑in OpenTelemetry** — experimental OTel support behind the `otel` Cargo feature and `OTEL_ENABLED=true` env var

## Table of Contents

- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Persistence Backends](#persistence-backends)
- [Architecture](#architecture)
- [Intent Classification](#intent-classification)
- [Request Flow](#request-flow)
- [Privacy & Security](#privacy--security)
- [API Reference](#api-reference)
- [Dashboard](#dashboard)
- [Deployment](#deployment)
- [CI/CD](#cicd)
- [Testing](#testing)
- [Development](#development)
- [OpenTelemetry (Experimental)](#opentelemetry-experimental)
- [FAQ](#faq)
- [License](#license)

## Quick Start

### Prerequisites

- **Rust toolchain** (stable) — install via [rustup](https://rustup.rs/) if needed
- No other runtime dependencies (database, Docker, or external services are optional)

### Run the Gateway

```bash
# Clone and enter the repo
git clone https://github.com/pfrack/cerebrum.git
cd cerebrum

# Set the three required environment variables
export PROXY_API_BEARER_TOKEN="your-secret-token"
export DASHBOARD_BASIC_USER="admin"
export DASHBOARD_BASIC_PASSWORD="your-dashboard-password"

# Build and run (this downloads dependencies on first run)
cargo run
```

The server starts on the port configured in `config.toml` (default: `10000`):

```bash
# Verify the gateway is running
curl http://localhost:10000/health
# → "ok"
```

To set a custom port without editing `config.toml`, override the embedded defaults via `CONFIG_PATH`:

```bash
echo '[server]
port = 8080' > my-config.toml
CONFIG_PATH=my-config.toml cargo run
```

Run the config validator to catch errors before starting the server:

```bash
CONFIG_PATH=my-config.toml cargo run -- --validate
```

The gateway runs with an in-memory persistence backend by default — no database required. See [Persistence Backends](#persistence-backends) to configure PostgreSQL or SQLite.

## Configuration

Cerebrum uses a **layered configuration model**:

1. **Embedded defaults** — every compiled binary contains a complete `config.toml` with sensible defaults, so a freshly built binary runs with no config file.
2. **Config overlay** — set `CONFIG_PATH=/path/to/your/config.toml` to override specific sections. Overlay sections (`[classifiers]`, `[categories]`, `[routing]`, etc.) are fully replaced; other sections are merged field-by-field. YAML files are also accepted (auto-detected by extension).
3. **Environment variables** — secrets (API keys, auth tokens, database URLs) are always read from the environment, never stored in config files.

To validate your configuration without starting the server:

```bash
CONFIG_PATH=/path/to/config.toml cargo run -- --validate
```

Run `cerebrum --init` to generate a commented starter configuration file.

A minimal overlay config that changes the port and enables SQLite persistence:

```toml
[server]
port = 3000

[persistence]
backend = "sqlite"
sqlite_path = "./data/cerebrum.db"
```

### Environment Variables

| Variable | Required | Description | Default |
|---|---|---|---|
| `PROXY_API_BEARER_TOKEN` | Yes | Bearer token for API proxy routes | — |
| `DASHBOARD_BASIC_USER` | Yes | Basic auth username for the dashboard | — |
| `DASHBOARD_BASIC_PASSWORD` | Yes | Basic auth password for the dashboard | — |
| `DATABASE_URL` | No | PostgreSQL connection URL | Falls through to SQLite or memory |
| `CONFIG_PATH` | No | Path to config overlay file | Uses embedded defaults |
| `OTEL_ENABLED` | No | Enable OpenTelemetry export (`true`/`false`) | `false` (requires `otel` feature) |

### Configuration File Reference

The full configuration schema is documented in `config.toml` at the repo root. Key sections:

- `[server]` — port, log level, log format
- `[http]` — timeouts, body limits, keepalive, streaming channel capacity
- `[classifiers]` — enabled classifiers and their evaluation order
- `[categories]` — intent category definitions with regex patterns and thresholds
- `[routing]` — model, endpoint, provider, and API key env var per category
- `[persistence]` — backend selection and SQLite path
- `[dashboard]` — time window defaults, pagination limits
- `[model_costs]` — cost per 1M input tokens per model (used for savings estimates)
- `[[negative_patterns]]` — suppression rules that subtract from category scores
- `[[auth_provider]]` — upstream authentication provider definitions

## Persistence Backends

Cerebrum supports three persistence backends, resolved in priority order:

| Priority | Condition | Backend | Notes |
|---|---|---|---|
| 1 | `DATABASE_URL` is set | **PostgreSQL** | Production-grade. Requires a running Postgres instance. Migrations are applied automatically on startup. |
| 2 | `[persistence].backend = "sqlite"` | **SQLite** | File-backed (default: `./cerebrum.db`). No external process required. Falls back to in-memory on connection failure. |
| 3 | Default (no `DATABASE_URL`, no explicit backend) | **In-memory** | Ephemeral — data is lost on restart. Capped at 10,000 records. Zero configuration. |

The persistence layer is fire-and-forget: inference records are written to a background task bounded by a semaphore, so database latency never blocks the response path.

## Architecture

Cerebrum is a **single-binary Axum gateway** running on a single Tokio async runtime. The persistence layer is the only side effect — all other processing is in-memory and synchronous from the request handler's perspective.

At startup the binary:
1. Reads configuration from embedded defaults, merges any overlay from `CONFIG_PATH`, and overrides secrets from environment variables
2. Parses categories, patterns, routing entries, model costs, negative patterns, and auth provider definitions from the merged config
3. Initializes the classifier chain — builds the configured backend classifiers (regex is always available; fewshot and LLM are loaded if enabled)
4. Resolves the persistence backend (Postgres if `DATABASE_URL` is set, SQLite if configured, otherwise in-memory)
5. Builds the Axum router — mounts `/health`, `/v1/*` proxy routes, and `/dashboard/*` with their respective auth middleware layers
6. Binds to the configured port and starts accepting requests

### Intent Classification

Classification is performed by a **pluggable chain** of backends implementing the `IntentClassify` trait. The chain iterates backends in configured order (default: `regex → fewshot → llm`) and returns the first result whose tier is not `Fallback`. If all backends return `Fallback`, the last fallback result is used.

| Backend | Default | Mechanism |
|---|---|---|
| **Regex** | Enabled | Built-in pattern matching with weighted regex rules per category. Supports dual-threshold suppression and negative patterns that subtract from competing categories. |
| **FewShot** | Opt-in | TF-IDF bag-of-words cosine similarity with bootstrap examples from `data/fewshot_bootstrap.yaml`. Learns from feedback submitted via `POST /v1/feedback`. Controlled by `confidence_threshold` and `cold_start_threshold`. |
| **LLM** | Opt-in | Delegates classification to an external LLM (e.g., `gpt-4o-mini`). Configured via the `[llm_classifier]` section in `config.toml`. |

Routing is **data-driven**: each category in `config.toml` has a corresponding `[routing.<CATEGORY>]` entry specifying the model, endpoint, provider type, and API key environment variable. Adding a new category is a configuration change — no code modification required.

### Request Flow

```
Request → Bearer auth → Classify → Select route → Proxy upstream → Log inference → Response
```

1. **Receive** — the gateway accepts HTTP requests on the configured port
2. **Authenticate** — proxy routes require a valid bearer token (`PROXY_API_BEARER_TOKEN`); dashboard routes require HTTP basic auth (`DASHBOARD_BASIC_USER` / `DASHBOARD_BASIC_PASSWORD`)
3. **Classify** — the classifier chain evaluates the last user message against all configured backends (regex → fewshot → llm)
4. **Select route** — the winning category maps to a `[routing.*]` entry with model, endpoint, and provider configuration
5. **Proxy upstream** — the request is forwarded to the upstream provider. If `stream: true` in the request body, the response is streamed as Server-Sent Events (SSE); otherwise it is buffered and returned as JSON. OpenAI-compatible and Anthropic Messages API formats are both supported
6. **Log inference** — a fire-and-forget background task persists the inference metadata (snippet, category, duration, model, token usage) bounded by a semaphore
7. **Respond** — the upstream response (or classification-only result when no upstream is configured) is returned to the client

### SSE Streaming

When a request includes `"stream": true`, the gateway proxies the upstream response as Server-Sent Events:

- Upstream data chunks are forwarded directly as SSE `data:` events
- A `: keepalive` comment is injected after every `keepalive_interval_secs` (default: 15s) of silence on the stream — this prevents proxy-level or client-level timeouts during long generations
- Upstream error responses emit an `event: error` SSE event with a JSON-escaped body capped at **2 KB** (prevents large error payloads from consuming memory on the proxy path)
- The streaming channel capacity is configurable (default: 32 messages) — controls backpressure between the upstream reader and the SSE response writer
- Both OpenAI-compatible and Anthropic-compatible streaming are supported; the gateway detects the upstream format and forwards appropriately

### Privacy & Security

- **Prompt snippet** — only the first 200 characters of the last user message are stored (`extract_snippet`). The full prompt is never persisted
- **Character count** — `prompt_char_count` records the true message length for cost estimation without exposing content
- **Constant-time comparison** — all credential comparisons (bearer token, basic auth user/password) use HMAC-SHA256 via the `subtle` crate to prevent timing attacks
- **Auth separation** — bearer token authentication for proxy routes (`/v1/*`), HTTP basic authentication for dashboard routes (`/dashboard/*`), each with its own middleware layer

## API Reference

Cerebrum exposes an OpenAI-compatible API surface for chat completions, plus dedicated classify and feedback endpoints. All proxy routes are mounted under `/v1/` and require the bearer token set via `PROXY_API_BEARER_TOKEN`. The dashboard routes are mounted under `/dashboard/` with basic authentication.

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/health` | Public | Liveness check; returns `200 "ok"` |
| POST | `/v1/chat/completions` | Bearer | Classify intent and proxy to upstream model. Supports SSE streaming when `stream: true` |
| POST | `/v1/messages` | Bearer | Anthropic Messages API pass-through. Same classification + routing, Anthropic-format body |
| POST | `/v1/classify` | Bearer | Classify-only endpoint; returns the category, model, and tier without proxying |
| POST | `/v1/feedback` | Bearer | Submit classification feedback for few-shot retraining |
| GET | `/dashboard/` | Basic auth | Dashboard overview — status, quick stats, recent inferences |
| GET | `/dashboard/inferences` | Basic auth | Paginated, filterable inference log |
| GET | `/dashboard/latency` | Basic auth | Per-category average and p99 latency over a time window |
| GET | `/dashboard/savings` | Basic auth | Estimated cost savings vs. baseline model |
| GET | `/dashboard/static/*` | Basic auth | Static assets (dashboard CSS) |

See `openapi/completions.yaml` for full request/response schemas.

## Dashboard

The operator dashboard is served under `/dashboard/` with HTTP basic authentication (credentials from `DASHBOARD_BASIC_USER` and `DASHBOARD_BASIC_PASSWORD`). Sidebar navigation is auto-generated from a single `PAGES` registry at `src/dashboard.rs:44-65` — adding a new dashboard page requires registering a `NavPage` entry, creating a template, writing a handler, and adding a route. It has four pages:

- **Dashboard** (`/dashboard/`) — overview page with server status indicator, quick stats (total inferences, classified count), and recent inference records
- **Inference Logs** (`/dashboard/inferences`) — paginated table of inference records with search/filter by category, model, and date range. Each row shows the prompt snippet, category, model, duration, and timestamp
- **Latency** (`/dashboard/latency`) — per-category latency breakdown showing average and p99 response times over a configurable time window (default: 24 hours)
- **Savings** (`/dashboard/savings`) — estimated cost savings compared to the configured `baseline_model`, based on actual model costs and per-category routing decisions

## Deployment

### Building

```bash
cargo build --release
```

The compiled binary is at `target/release/cerebrum`. It has no runtime dependencies beyond the system libraries required by the Rust standard library — no interpreter, JVM, or container runtime is required.

### Render

The repository includes a `render.yaml` that defines a web service with the following configuration:

- **Build command**: `cargo build --release`
- **Start command**: `./target/release/cerebrum`
- **Health check path**: `/health`
- **Environment variables** (set in the Render dashboard, not committed):
  - `PROXY_API_BEARER_TOKEN` — required
  - `DASHBOARD_BASIC_USER` — required
  - `DASHBOARD_BASIC_PASSWORD` — required
  - `DATABASE_URL` — optional, for PostgreSQL

All secret environment variables are marked `sync: false` in `render.yaml`, meaning they must be set manually in the Render dashboard.

### Production Checklist

- Set `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, and `DASHBOARD_BASIC_PASSWORD` to strong, unique values
- Configure a PostgreSQL database via `DATABASE_URL` for persistent inference logs
- Review `[http]` timeouts (`client_timeout_secs`, `client_connect_timeout_secs`) and body limits (`max_upstream_body_bytes`, `request_body_limit_bytes`) for your workload
- Run behind a TLS-terminating reverse proxy (Render handles this automatically; for self-hosted deployments, use nginx, Caddy, or a similar proxy)
- Monitor logs — the gateway uses structured logging with `RUST_LOG` for filtering
- Test authentication and routing with `scripts/manual_tests.sh --basic`

## CI/CD

On every push to `main`, the GitHub Actions workflow (`.github/workflows/deploy.yml`):

1. Checks out the code and installs the stable Rust toolchain
2. Runs auth verification tests (`cargo test auth`, `cargo test routes_auth`)
3. Runs persistence contract tests (`cargo test persistence::tests`; PostgreSQL integration tests run only when `DATABASE_URL` is set)
4. Builds the release binary (`cargo build --release`)
5. Triggers a Render deployment via `RENDER_DEPLOY_HOOK` webhook (fails loudly if missing)

## Testing

The test suite is organized into two groups within `src/main.rs`:

- **Fast tests** (`mod tests`) — unit and integration tests that run with `cargo test`. Includes classifier tests, route handlers, auth middleware, and persistence contract tests
- **Slow tests** (`mod slow_tests`) — tests with real delays (e.g., keepalive interval), run with `cargo test slow_tests`

Targeted test commands:

| Command | Scope |
|---|---|
| `cargo test` | All fast unit/integration tests |
| `cargo test auth` | Auth middleware tests |
| `cargo test routes_auth` | Route authorization tests |
| `cargo test persistence` | Persistence backend contract tests |
| `cargo test slow_tests` | Tests with timing delays |
| `cargo test persistence_integration` | PostgreSQL integration (requires `DATABASE_URL`) |

A manual test harness is available at `scripts/manual_tests.sh` with three modes:
- `--auto` — full automated scenario suite
- `--basic` — quick smoke tests (health, auth, classification, graceful shutdown)
- Default — interactive mode

### Test Infrastructure

- **httpmock 0.7** — mock upstream server for proxy route tests (verifies correct request forwarding, streaming, retry behavior, and error handling)
- **serial_test 3** — serializes tests that modify environment variables (prevents cross-test contamination of `DATABASE_URL`, `CONFIG_PATH`, and other env-dependent state)
- **testcontainers 0.27** — disposable PostgreSQL container for integration tests (falls back to `DATABASE_URL` when Docker is unavailable)
- **`#[tokio::test]`** — all async tests use Tokio's test runtime; test apps are constructed via `test_app()` and tested with `Request::builder()` against the full router

### Writing Tests

Tests live inline with the code they test (follow the patterns in `src/main.rs`). Test names follow the convention `test_<component>_<case>`. The `mod tests` block in each file contains fast unit tests; `mod slow_tests` contains tests with real timing delays.

## Development

### Source Layout

| Path | Role |
|---|---|
| `src/main.rs` | Axum router, route handlers, SSE keepalive, `AppState`, test harness |
| `src/auth.rs` | `AuthConfig`, constant-time bearer/basic auth middleware |
| `src/config.rs` | `ConfigRoot` (mirrors `config.toml`), loaders, `--validate` mode, overlay merge |
| `src/intent_classifier.rs` | `IntentClassify` trait, `RegexClassifier`, `LLMClassifier`, `ClassifierChain` |
| `src/fewshot_classifier.rs` | Few-shot TF-IDF classifier with feedback learning |
| `src/persistence.rs` | `PersistenceBackend` trait, three backends (Memory, SQLite, Postgres), `InferenceRecord`, `log_inference` |
| `src/dashboard.rs` | `PAGES` registry, `dashboard_page!` macro, Askama template handlers, sidebar nav |
| `src/routing.rs` | `RouteEntry`, `ModelCosts`, default model constants |
| `src/telemetry.rs` | OTel providers (behind `otel` feature) |
| `src/protocol_translation.rs` | Anthropic ↔ OpenAI request/response translation |
| `src/quickstart.rs` | Quickstart config generation (`--init` mode) |

### Conventions

- **Constant-time comparison** for all security-sensitive string matching (HMAC-SHA256 via the `subtle` crate)
- **Dashboard page recipe** — register a `NavPage` in the `PAGES` static array, define a template struct with the `dashboard_page!` macro, write the handler, and add a route line
- **Config as data** — routing, categories, and provider definitions are all data-driven from `config.toml`. Adding a new category or route requires no code changes
- **Fire-and-forget logging** — `log_inference` spawns a detached background task; the response path is never blocked on database I/O
- **Privacy-first design** — `extract_snippet` caps stored prompt text at 200 characters; full prompts exist only in transient memory during classification and upstream proxying
- **Single source of truth for nav** — the `PAGES` registry in `src/dashboard.rs` drives the entire sidebar; adding a page is a 5-step recipe (template, registry entry, macro, handler, route)

## OpenTelemetry (Experimental)

OpenTelemetry support is opt-in and requires two things:

1. **Cargo feature**: build with `--features otel` (adds dependencies on `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry`)
2. **Environment variable**: set `OTEL_ENABLED=true`

When enabled, the gateway exports traces, metrics, and logs via OTLP. Metrics include request count, request duration, and classification outcomes per category/tier.

**Status**: experimental. The OTel integration is feature-gated and not enabled by default.

## FAQ

**Q: Why am I getting 401 responses?**

All proxy routes (`/v1/*`) require a valid bearer token set via the `PROXY_API_BEARER_TOKEN` environment variable. Dashboard routes (`/dashboard/*`) require HTTP basic auth with credentials from `DASHBOARD_BASIC_USER` and `DASHBOARD_BASIC_PASSWORD`. Ensure all three variables are set before starting the server.

**Q: Can I use a YAML configuration file?**

Yes. The config overlay path (set via `CONFIG_PATH`) accepts both `.toml` and `.yaml` files. The format is auto-detected from the file extension.

**Q: Do I need a database to run Cerebrum?**

No. The gateway runs with an in-memory persistence backend out of the box — no database required. For production use, configure PostgreSQL or SQLite.

**Q: How do I reset the in-memory database?**

The in-memory backend is ephemeral — simply restart the server. For SQLite, delete the `cerebrum.db` file (or the path configured in `config.toml`).

**Q: How do I add a new intent category?**

Add a `[categories.NEW_CATEGORY]` section and a matching `[routing.NEW_CATEGORY]` section in `config.toml`, then restart. No code changes needed. Run `cargo run -- --validate` to check your configuration.

**Q: How do I enable streaming?**

Set `"stream": true` in the `POST /v1/chat/completions` request body. The gateway will proxy the upstream response as Server-Sent Events.

**Q: Does Cerebrum store my prompts?**

Only a 200-character snippet of the last user message is persisted. The full prompt is never stored. The character count of the full message is recorded separately for cost estimation.

**Q: Can I run Cerebrum in a Docker container?**

Yes. Build the binary with `cargo build --release` and copy `target/release/cerebrum` into a minimal runtime image (e.g., `debian:stable-slim`). No runtime dependencies beyond the system's C library are required.

**Q: How do I update the few-shot classifier's training data?**

Submit feedback via `POST /v1/feedback` with the original prompt text and the correct category. The few-shot classifier incorporates this feedback for future classifications. Bootstrap data is stored in `data/fewshot_bootstrap.yaml`.

**Q: What happens if the upstream API is unreachable?**

The gateway returns a `502` response with an `upstream_error` JSON body that includes the upstream's status code and message. For streaming requests, the error is emitted as an SSE `event: error` event before the stream closes.

## License

Cerebrum is distributed under the terms of the MIT license.
