# scripts/

Integration test suite for Frugalis.

## Usage

```bash
./scripts/test.sh              # full automated suite (default)
./scripts/test.sh --basic      # quick smoke: health, auth, classify, shutdown
./scripts/test.sh --cache      # cache tests: TTL, bypass, streaming, dashboard
./scripts/test.sh --interactive # manual testing (server must be running)
./scripts/test.sh --anthropic  # anthropic pass-through interactive
./scripts/test.sh --fewshot    # few-shot classifier interactive
```

## Files

| File | Purpose |
|------|---------|
| `test.sh` | Unified test runner (all modes) |
| `lib.sh` | Shared infrastructure: server lifecycle, colors, `classify` helper |
| `TEST_SCENARIOS.md` | Detailed documentation of each test scenario |

## Prerequisites

- Rust toolchain (`cargo build --release`)
- Python 3 (JSON parsing in mock servers)
- `curl`
- Port 10000 available

## Environment

| Variable | Default | Purpose |
|----------|---------|---------|
| `PROXY_API_BEARER_TOKEN` | `test-token-123` | Auth token for proxy routes |
| `HOST` | `127.0.0.1:10000` | Server address |
| `CACHE_TTL` | `5` | TTL seconds for cache tests |

## What's Tested

- **Classification** — hardcoded defaults, config overrides, partial categories, negative suppression
- **Config formats** — TOML, YAML, external pattern files
- **Validation CLI** — `--validate` detects invalid regex, schema errors, unknown flags
- **Anthropic pass-through** — `/v1/messages` auth, content-type, format acceptance
- **OpenAI→Anthropic translation** — non-streaming, streaming SSE, error forwarding
- **Claude Code compat** — `/v1/models` unauthenticated, header forwarding, cache_control passthrough
- **Response cache** — enable/disable, hit/miss, TTL expiry, bypass header, streaming exclusion
