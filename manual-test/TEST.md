# Automated Integration Tests: Shared Category Configuration (S-07b)

## Overview

This script provides fully automated integration tests for the `shared-category-config` change. It:

- Builds the server binary (release mode)
- For each test scenario:
  - Creates the appropriate `config.toml`
  - Starts the server in the background
  - Waits for the health endpoint
  - Runs classification checks via HTTP API
  - Validates results
  - Stops the server
- Reports a pass/fail summary

## Prerequisites

- Rust toolchain installed
- `cargo` available
- `PROXY_API_BEARER_TOKEN` environment variable set (any non-empty value for tests)
- Server port 10000 available (or set `PORT` env var)

## Usage

```bash
cd /home/pawel/code/cerebrum
./test-shared-category-config.sh
```

The script will:
1. Build `target/release/cerebrum` if needed
2. Run 7 test scenarios sequentially
3. Output colored pass/fail results
4. Exit with code 0 on success, 1 on any failure

## Test Scenarios

### 1. Hardcoded Defaults
No configuration file present. Verifies server starts with hardcoded `CategoryConfig` and all four categories classify correctly using the exact same prompts as unit tests:

- `"please read the file src/main.rs"` → `FILE_READING`
- `"fix this bug please"` → `SYNTAX_FIX`
- `"architect a distributed rate limiter"` → `COMPLEX_REASONING`
- `"hello"` → `CASUAL`

### 2. Threshold Override
Creates `config.toml` with `SYNTAX_FIX` threshold raised from 3 to 5. Verifies that `"fix this bug please"` no longer matches `SYNTAX_FIX` and falls back to `CASUAL`.

### 3. Partial Categories
Creates `config.toml` with only `FILE_READING` and `CASUAL` categories. Verifies:
- Those two categories still work
- Missing categories are not in routing table
- Classification for missing categories falls back to `CASUAL`
- No panics or errors

### 4. Legacy routing.toml
Tests backward compatibility with the old `routing.toml` format. Verifies server starts when no `config.toml` exists and uses hardcoded categories + routing from `routing.toml` if present (or pure hardcoded if not).

### 5. Combined Config
Creates full `config.toml` with all four categories in `[[categories]]` plus `[FALLBACK]` routing. Verifies all four categories route correctly to their expected default models:
- `FILE_READING` → `meta/llama-3.1-70b-instruct` (via `DEFAULT_MODEL_READING`)
- `SYNTAX_FIX` → `meta/llama-3.1-8b-instruct` (via `DEFAULT_MODEL`)
- `COMPLEX_REASONING` → `meta/llama-3.3-70b-instruct` (via `DEFAULT_MODEL_COMPLEX`)
- `CASUAL` → `meta/llama-3.1-8b-instruct` (via `DEFAULT_MODEL`)

### 6. Field Value Integrity
Tests that all `CategoryConfig` fields are respected by setting extreme values:
- `FILE_READING` threshold = 100 (effectively unreachable)
- `FILE_READING` `model_env_var = "CUSTOM_MODEL"`
Verifies threshold override works and no crashes.

### 7. Negative Suppression
Regression test for the `NEGATIVE_META` mechanism. Verifies that `"read the architecture document"` does **not** classify as `COMPLEX_REASONING` (since `"read"` matches `FILE_READING`, which suppresses `COMPLEX_REASONING` per negative metadata).

## How It Works

The script:
1. **Builds** the binary once with `cargo build --release`
2. For each test:
   - Writes a specific `config.toml` to `/tmp/cerebrum-config-test.toml`
   - Sets `CONFIG_PATH` env var to that file
   - Launches the server in the background: `./target/release/cerebrum`
   - Polls `http://localhost:10000/health` until it returns 200
   - Sends HTTP POST requests to `/v1/chat/completions` with test prompts
   - Extracts the `model` field from the JSON response
   - Maps the model name to an expected category (heuristic based on default model constants)
   - Validates the classification matches expectation
   - Kills the server process
   - Cleans up the temp config file
3. Prints a summary table

## Model → Category Mapping

The script infers classification from the model selected (since the API doesn't expose category directly in response). Default mappings:

| Model Pattern | Category |
|--------------|----------|
| `70b` or `reading` | `FILE_READING` |
| `3.3` or `complex` | `COMPLEX_REASONING` |
| `coder` or `qwen` | `SYNTAX_FIX` |
| `8b` or `nano` | `CASUAL` |

This works because the default routing maps categories to specific models.

## Environment Variables

- `PROXY_API_BEARER_TOKEN` - required (any non-empty string for tests)
- `HOST` - optional, defaults to `127.0.0.1:10000`
- `PORT` - optional, server port (default 10000)
- Other server env vars (`NVIDIA_API_KEY`, etc.) are set to dummy values by the script where needed

## Troubleshooting

### "Server failed to start"
Check that:
- Port 10000 is not in use
- You have permission to bind to the port
- Binary exists (`cargo build --release` succeeded)

Look at the log file: `/tmp/cerebrum-test-*.log`

### "ERROR" responses from classify()
Could be:
- Server not started yet (wait failure)
- HTTP error (401, 500, etc.) - check server logs
- Invalid JSON response

### Classification mismatches
If a test fails with unexpected category:
1. Check the server logs for warnings
2. Verify the config was loaded correctly (look for `loaded N categories from config.toml` in logs)
3. Check that the prompt actually matches the category's patterns (see unit tests for exact expected prompts)

## Cleanup

The script uses `trap` to ensure cleanup on exit. Manual cleanup if script aborted:

```bash
pkill -f cerebrum 2>/dev/null || true
rm -f /tmp/cerebrum-config-*.toml
rm -f /tmp/cerebrum-test-*.log
```

## Integration with CI

To run in CI/CD:

```bash
#!/bin/bash
set -euo pipefail

# Build
cargo build --release

# Set dummy token for tests
export PROXY_API_BEARER_TOKEN="ci-test-token"

# Run tests
./test-shared-category-config.sh
```

## Success Criteria

All 7 tests must pass:
- [x] Hardcoded defaults work
- [x] Threshold override respected
- [x] Partial categories handled
- [x] Legacy routing.toml supported
- [x] Combined config works
- [x] Field values wired correctly
- [x] Negative suppression intact

## Reference

- Implementation plan: `context/changes/shared-category-config/plan.md`
- Code: `src/intent_classifier.rs`, `src/config.rs`
- Manual guide: `manual-test-shared-category-config.md` (for interactive testing)
- Automated runner: `test-shared-category-config.sh` (this file)
