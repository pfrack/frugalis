# Cerebrum

[![CI](https://github.com/pfrack/cerebrum/actions/workflows/deploy.yml/badge.svg)](https://github.com/pfrack/cerebrum/actions/workflows/deploy.yml)
![Rust Edition](https://img.shields.io/badge/edition-2021-blue)

**Intent-aware LLM request routing gateway.** Cerebrum is a single Rust binary that sits between your agents and upstream model providers. It classifies each incoming prompt into an intent category, selects the cheapest acceptable model for that category, and proxies the request with full SSE streaming support.

## Features

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
1. Loads configuration (embedded defaults + optional overlay)
2. Initializes the classifier chain from the configured backends
3. Resolves the persistence backend (Postgres / SQLite / in-memory)
4. Builds the Axum router with middleware and starts the HTTP server

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
3. **Classify** — the classifier chain evaluates the last user message against all configured backends
4. **Select route** — the winning category maps to a `[routing.*]` entry with model, endpoint, and provider configuration
5. **Proxy upstream** — the request is forwarded to the upstream provider. If `stream: true` in the request body, the response is streamed as Server-Sent Events (SSE); otherwise it is buffered and returned as JSON
6. **Log inference** — a fire-and-forget background task persists the inference metadata (snippet, category, duration, model, token usage)
7. **Respond** — the upstream response (or classification-only result) is returned to the client

### SSE Streaming

When a request includes `"stream": true`, the gateway proxies the upstream response as Server-Sent Events:

- Upstream data chunks are forwarded directly as SSE `data:` events
- A `: keepalive` comment is injected after every `keepalive_interval_secs` (default: 15s) of silence on the stream
- Upstream error responses emit an `event: error` SSE event with a JSON-escaped body capped at **2 KB**
- The streaming channel capacity is configurable (default: 32 messages)

### Privacy & Security

- **Prompt snippet** — only the first 200 characters of the last user message are stored (`extract_snippet`). The full prompt is never persisted
- **Character count** — `prompt_char_count` records the true message length for cost estimation without exposing content
- **Constant-time comparison** — all credential comparisons (bearer token, basic auth user/password) use HMAC-SHA256 via the `subtle` crate to prevent timing attacks
- **Auth separation** — bearer token authentication for proxy routes (`/v1/*`), HTTP basic authentication for dashboard routes (`/dashboard/*`), each with its own middleware layer

## API Reference

All proxy API routes are mounted under `/v1/` and require the bearer token set via `PROXY_API_BEARER_TOKEN`.

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

The operator dashboard is served under `/dashboard/` with HTTP basic authentication (credentials from `DASHBOARD_BASIC_USER` and `DASHBOARD_BASIC_PASSWORD`). It has four pages with an auto-generated sidebar navigation driven from a single `PAGES` registry:

- **Dashboard** (`/dashboard/`) — overview page with server status indicator, quick stats (total inferences, classified count), and recent inference records
- **Inference Logs** (`/dashboard/inferences`) — paginated table of inference records with search/filter by category, model, and date range. Each row shows the prompt snippet, category, model, duration, and timestamp
- **Latency** (`/dashboard/latency`) — per-category latency breakdown showing average and p99 response times over a configurable time window (default: 24 hours)
- **Savings** (`/dashboard/savings`) — estimated cost savings compared to the configured `baseline_model`, based on actual model costs and per-category routing decisions
