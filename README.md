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
