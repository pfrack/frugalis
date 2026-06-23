# Manual Tests for Cerebrum

This directory contains integration test scripts for validating Cerebrum's category configuration, routing, persistence, and API behavior.

## Files

- `run.sh` – Main test script with two modes:
  - **Interactive mode** (default): Manual testing via `/v1/chat/completions` endpoint
  - **Automated mode** (`--auto`): Full integration test suite with server lifecycle management
- `lib.sh` – Shared test infrastructure (colors, server lifecycle, classify helper)
- `README.md` – This file
- `TEST.md` – Detailed documentation of test scenarios

## Quick Start

### Interactive Manual Testing (default)

```bash
# Start the server in one terminal
RUST_LOG=info cargo run

# In another terminal, run manual tests
export PROXY_API_BEARER_TOKEN="your-token"
./run.sh
```

This mode sends requests to an already-running server and shows detailed results for each test case.

### Automated Integration Tests

```bash
export PROXY_API_BEARER_TOKEN="test-token"
./run.sh --auto
```

This mode:
- Builds the release binary (if needed)
- Creates various `config.toml` configurations
- Starts and stops the server automatically for each test
- Validates outcomes via `/v1/classify` endpoint
- Reports pass/fail summary
- Exit code 0 on success, non-zero on failure

## Test Coverage

The automated suite (`--auto`) covers:

| # | Scenario | What it validates |
|---|----------|-------------------|
| 1 | Hardcoded defaults | No config file, all 4 categories work |
| 2 | Threshold override | Category threshold from `config.toml` is respected |
| 3 | Partial categories | Only 2 of 4 configured, missing ones fall back |
| 4 | Legacy routing.toml | Backward compatibility with old config format |
| 5 | Combined config | Full `config.toml` with categories + routing |
| 6 | Field integrity | Extreme values (threshold=100) work correctly |
| 7 | Negative suppression | NEGATIVE_META penalty mechanism intact |
| 8 | Embedded config | Config embedded in routing file works |
| 9 | Validation CLI | `--validate-config` flag works |
| 10 | YAML support | `.yaml` config files accepted |
| 11 | External patterns | Pattern files loaded from disk |
| 12–13 | Anthropic passthrough | Messages endpoint, model routing, streaming |
| 14 | Legacy fallback routing | Old `[FALLBACK]` format still works |

**Total: 70 assertions, all passing.**

## Wrapper Script

`scripts/manual_tests.sh` provides a unified entry point:

```bash
./scripts/manual_tests.sh           # interactive (delegates to manual-test/run.sh)
./scripts/manual_tests.sh --auto    # full automated suite
./scripts/manual_tests.sh --basic   # quick smoke tests (health, auth, classify, shutdown)
```

## Prerequisites

- Rust toolchain (`cargo`)
- Python 3 (for JSON parsing in scripts)
- `curl`
- Available port 10000 (or set `HOST=host:port`)
- `PROXY_API_BEARER_TOKEN` environment variable set

## Troubleshooting

### "Server failed to start"
- Check if port 10000 is already in use
- Look at `/tmp/cerebrum-test-*.log` for server errors
- Ensure you have permission to bind to the port

### Classification mismatches
- Verify the server log shows `loaded N categories from config.toml`
- Check that prompts match expected patterns (see unit tests in `src/intent_classifier.rs`)
- Ensure `config.toml` syntax is valid TOML

### Authentication errors
- Set `PROXY_API_BEARER_TOKEN` environment variable
- For automated mode, any non-empty token works (defaults to `test-token-123` if not set)
