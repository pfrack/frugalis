# Automated Integration Tests

## Overview

`run.sh --auto` runs the full integration test suite. For each test scenario it:
- Creates an appropriate `config.toml` (or uses none for hardcoded defaults)
- Starts the server in the background
- Validates classification via `/v1/classify` or `/v1/messages`
- Stops the server and cleans up

## Prerequisites

- Rust toolchain (`cargo`)
- `PROXY_API_BEARER_TOKEN` env var (any non-empty value)
- Port 10000 available (or set `HOST=host:port`)

## Usage

```bash
cd manual-test
./run.sh --auto
```

Exit code 0 on success, non-zero on failure.

## Test Scenarios

### 1. Hardcoded Defaults (no config file)
No configuration file present. Verifies all four categories classify correctly:
- `"please read the file src/main.rs"` → `FILE_READING`
- `"fix this bug please"` → `SYNTAX_FIX`
- `"architect a distributed rate limiter"` → `COMPLEX_REASONING`
- `"hello"` → `CASUAL`

### 2. Threshold Override
`FILE_READING` threshold set to 100 (unreachable). Verifies that `"please read the file src/main.rs"` no longer matches `FILE_READING` and falls back to `CASUAL`.

### 3. Partial Categories
Only `FILE_READING` and `CASUAL` configured. Verifies those two work and missing categories fall back gracefully.

### 4. Legacy routing.toml
Tests backward compatibility — server starts with no `config.toml` and uses hardcoded defaults.

### 5. Combined Config
Full `config.toml` with all four categories plus `[FALLBACK]` routing. All categories route to expected models.

### 6. Field Integrity
Extreme values (threshold=100, custom `model_env_var`). Verifies overrides work and no crashes.

### 7. Negative Suppression
Regression test: `"read the architecture document"` does **not** classify as `COMPLEX_REASONING` because `"read"` triggers `FILE_READING` suppression.

### 8. Embedded Config
Config embedded in routing file (no separate config path). Verifies server accepts it.

### 9. Validation CLI
`--validate-config` flag parses and validates config without starting the server.

### 10. YAML Support
`.yaml` config file accepted alongside `.toml`.

### 11. External Patterns
Pattern files loaded from disk for regex classifier customization.

### 12–13. Anthropic Passthrough
`/v1/messages` endpoint forwards to Anthropic with correct headers, model routing, and streaming support.

### 14. Legacy Fallback Routing
Old `[FALLBACK]` config format still works for backward compatibility.

## How It Works

1. **Builds** the binary once with `cargo build --release`
2. For each test:
   - Writes a `config.toml` to `/tmp/frugalis-config-*.toml`
   - Launches the server with that config
   - Polls `http://localhost:10000/health` until ready
   - Sends classification requests and validates responses
   - Kills the server
3. Prints pass/fail summary

## Troubleshooting

### "Server failed to start"
- Port 10000 in use? Check `lsof -i :10000`
- Build failed? Run `cargo build --release` manually
- Check `/tmp/frugalis-test-*.log` for server errors

### Classification mismatches
- Look for `loaded N categories from config.toml` in server logs
- Verify prompts match expected patterns (see unit tests in `src/intent_classifier.rs`)
- Check config syntax is valid TOML

### Authentication errors
- `PROXY_API_BEARER_TOKEN` must be set (any non-empty value)
- Defaults to `test-token-123` if not explicitly set

## Cleanup

The script uses `trap` for cleanup. Manual cleanup if needed:

```bash
pkill -f frugalis 2>/dev/null || true
rm -f /tmp/frugalis-config-*.toml /tmp/frugalis-test-*.log
```
