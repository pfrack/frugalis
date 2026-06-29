# scripts/

Single-file integration test suite for Frugalis. Tests the running server from the outside via HTTP.

## Usage

```bash
./scripts/test.sh              # full automated suite (default)
./scripts/test.sh --basic      # quick smoke: health, auth, classify, shutdown
./scripts/test.sh --cache      # cache tests: TTL, bypass, streaming, dashboard
./scripts/test.sh --interactive # manual testing (server must be running)
./scripts/test.sh --anthropic  # anthropic pass-through interactive
./scripts/test.sh --fewshot    # few-shot classifier interactive
```

## Prerequisites

- Rust toolchain (`cargo build --release`)
- Python 3 (JSON parsing in mock servers)
- `curl`
- Port 10000 available (or set `HOST=host:port`)

## Environment

| Variable | Default | Purpose |
|----------|---------|---------|
| `PROXY_API_BEARER_TOKEN` | `test-token-123` | Auth token for proxy routes |
| `HOST` | `127.0.0.1:10000` | Server address |
| `CACHE_TTL` | `5` | TTL seconds for cache tests |

## What's Tested

- **classify:** hardcoded defaults, threshold override, partial categories, combined config, negative suppression, embedded config
- **config:** YAML loading + validation, external pattern files, invalid pattern detection
- **validate:** valid config, invalid regex, schema errors, unknown flags, informative errors
- **anthropic:** format acceptance, array-of-text-blocks, auth gate, content-type gate, error envelope, OpenAPI spec
- **models:** unauthenticated access, Anthropic shape (display_name, type=model)
- **translate:** OAI→Anthropic non-streaming, streaming SSE, error forwarding
- **headers:** anthropic-beta/version/x-claude-code-session-id forwarding
- **cache_control:** Anthropic→Anthropic passthrough, OAI→Anthropic auto-insertion
- **cache:** enable/disable, hit/miss, TTL expiry, bypass header, streaming exclusion

## How It Works

1. Builds the binary once (`cargo build --release`)
2. For each test: writes a temp config → starts server → sends requests → validates → stops server
3. Mock Python HTTP servers simulate Anthropic upstream for translation tests
4. Prints pass/fail summary; exit 0 on all-pass

## Troubleshooting

- Port in use: `lsof -i :10000`
- Server logs: `/tmp/frugalis-test-*.log`
- Manual cleanup: `pkill -f frugalis; rm -f /tmp/frugalis-config-* /tmp/frugalis-test-*`
