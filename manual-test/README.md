# Manual Tests for Shared Category Configuration (S-07b)

This directory contains test scripts for validating the `shared-category-config` change.

## Files

- `run.sh` – Main test script with two modes:
  - **Interactive mode** (default): Manual testing via `/v1/chat/completions` endpoint
  - **Automated mode** (`--auto`): Full integration test suite with server lifecycle management
- `TEST.md` – Detailed documentation of all manual test scenarios

## Quick Start

### Interactive Manual Testing (default)

```bash
# Start the server in one terminal
RUST_LOG=info cargo run

# In another terminal, run manual tests
export PROXY_API_BEARER_TOKEN="your-token"
./run.sh
```

This mode sends requests to an already-running server and shows detailed results for each test case. It's useful for ad-hoc validation, debugging, or exploring behavior with different prompts.

### Automated Integration Tests

```bash
# Run all automated tests (server auto-started per scenario)
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

The automated suite (`--auto`) includes 7 test scenarios covering:

1. **Hardcoded defaults** – No config file, all 4 categories work
2. **Threshold override** – Category threshold from `config.toml` is respected
3. **Partial categories** – Only 2 of 4 categories configured, missing ones fall back
4. **Legacy routing.toml** – Backward compatibility with old config format
5. **Combined config** – Full `config.toml` with `[[categories]]` and `[FALLBACK]`
6. **Field integrity** – Extreme values (threshold=100) work correctly
7. **Negative suppression** – NEGATIVE_META penalty mechanism intact

**Total: 29 assertions, all passing.**

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
- For interactive mode, token must match what server expects

## Reference

- Implementation plan: `../../context/changes/shared-category-config/plan.md`
- Research: `../../context/changes/shared-category-config/research.md`
- Change log: `../../context/changes/shared-category-config/change.md`
