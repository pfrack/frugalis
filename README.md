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
