# Manual Testing Guide: Shared Category Configuration (S-07b)

## Overview

This guide provides manual test procedures to verify that the `CategoryConfig` refactor works correctly. These tests complement the automated test suite and verify:

1. Classification behavior remains unchanged with hardcoded defaults
2. `config.toml` overrides are respected at runtime
3. Partial category configurations work correctly
4. Legacy `routing.toml` backward compatibility
5. Combined `config.toml` with both categories and routing

## Prerequisites

- Server built: `cargo build --release`
- Server can be started: `RUST_LOG=info cargo run`
- `PROXY_API_BEARER_TOKEN` environment variable set
- `NVIDIA_API_KEY` environment variable set (if using NVIDIA NIM routing)
- Target server running on `localhost:10000` (or set `HOST` env var)

## Quick Start

Run all manual tests:

```bash
cd /home/pawel/code/cerebrum
bash manual-test-shared-category-config.sh
```

The script will automatically:
- Wait for the server to be ready
- Run all test scenarios
- Report pass/fail for each
- Exit with code 0 on success, 1 on any failure

## Test Scenarios

### 1. Hardcoded Defaults (No Config File)

**Setup**: Ensure no `config.toml` or `routing.toml` in the working directory.

**What it verifies**:
- Server starts with hardcoded `CategoryConfig` values
- All four categories (`FILE_READING`, `SYNTAX_FIX`, `COMPLEX_REASONING`, `CASUAL`) classify correctly
- Classification matches expected patterns

**Expected results**:
- `"read the content of src/main.rs"` → `FILE_READING`
- `"fix this bug"` → `SYNTAX_FIX`
- `"design a distributed database schema"` → `COMPLEX_REASONING`
- `"hello"` → `CASUAL`

---

### 2. Threshold Override

**Setup**: Create `config.toml` with `SYNTAX_FIX` threshold set to 5.

```toml
[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs and errors"
threshold = 5  # Raised from default 3
priority = 2
model_env_var = "DEFAULT_MODEL"
```

**What it verifies**:
- Category threshold overrides are applied at runtime
- Prompts that previously matched `SYNTAX_FIX` now fall back when threshold not met

**Expected results**:
- `"fix this bug"` pattern scores 3–4, but threshold is now 5 → falls back to `CASUAL`
- Other categories unaffected

---

### 3. Partial Categories

**Setup**: Create `config.toml` with only `FILE_READING` and `CASUAL`.

```toml
[[categories]]
name = "FILE_READING"
description = "Reading files"
threshold = 3
priority = 1
model_env_var = "DEFAULT_MODEL_READING"

[[categories]]
name = "CASUAL"
description = "Simple questions"
threshold = 1
priority = 4
model_env_var = "DEFAULT_MODEL"
```

**What it verifies**:
- Routing table contains only the configured categories
- Missing categories (`SYNTAX_FIX`, `COMPLEX_REASONING`) do not cause crashes
- Classification may still produce those categories internally, but routing falls back to `CASUAL`

**Expected results**:
- `"hello"` → routes to `CASUAL` model
- `"read file"` → routes to `FILE_READING` model
- `"fix bug"` → falls back to `CASUAL` (no `SYNTAX_FIX` route)
- No panics or errors

---

### 4. Legacy routing.toml Backward Compatibility

**Setup**: Remove `config.toml` if present, copy `routing_examples/routing-manual-tests.toml` to `routing.toml`.

```bash
rm -f config.toml
cp routing_examples/routing-manual-tests.toml routing.toml
```

**What it verifies**:
- Server loads `routing.toml` when `config.toml` is absent
- Info log message indicates legacy file usage
- Routing works with the legacy format (no `[[categories]]` sections)

**Expected results**:
- Server starts successfully with info log: "Using legacy routing.toml; consider renaming to config.toml"
- All four categories route correctly via the `[CATEGORY]` sections in `routing.toml`
- Classification behavior unchanged

---

### 5. Combined Config (Categories + Routing)

**Setup**: Create `config.toml` with `[[categories]]` sections **and** `[FALLBACK]` routing.

```toml
[[categories]]
name = "FILE_READING"
description = "Reading files"
threshold = 3
priority = 1
model_env_var = "DEFAULT_MODEL_READING"

[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs"
threshold = 3
priority = 2
model_env_var = "DEFAULT_MODEL"

[[categories]]
name = "COMPLEX_REASONING"
description = "Multi-step reasoning"
threshold = 3
priority = 3
model_env_var = "DEFAULT_MODEL_COMPLEX"

[[categories]]
name = "CASUAL"
description = "Simple questions"
threshold = 1
priority = 4
model_env_var = "DEFAULT_MODEL"

[FALLBACK]
model = "nvidia/nemotron-3-nano-30b-a3b"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
```

**What it verifies**:
- Both categories and routing load from the same file
- `model_env_var` fields control which environment variable provides the model default
- All categories present in routing table
- FALLBACK route is set

**Expected results**:
- All four category prompts route to expected models:
  - `FILE_READING` → `DEFAULT_MODEL_READING` (or override via env var)
  - `SYNTAX_FIX` → `DEFAULT_MODEL`
  - `COMPLEX_REASONING` → `DEFAULT_MODEL_COMPLEX`
  - `CASUAL` → `DEFAULT_MODEL`
- No errors in server logs

---

### 6. Field Value Integrity

**Setup**: Use config with extreme values to verify each field is wired correctly.

```toml
[[categories]]
name = "FILE_READING"
description = "Test"
threshold = 100      # Unreachable threshold
priority = 1
model_env_var = "CUSTOM_MODEL"
```

**What it verifies**:
- `threshold` field controls matching
- `priority` field controls ranking when multiple categories match (though SF dual-threshold is special)
- `model_env_var` field determines which env var provides the default model
- `description` and `name` are stored correctly

**Expected results**:
- `"read file"` prompts fall back to `CASUAL` (threshold 100 not met)
- Model used for `FILE_READING` route comes from `CUSTOM_MODEL` env var (if set) or falls back
- No crashes

---

## Cross-Check: Verify Classification Output

For each test scenario, you can also check the `/v1/chat/completions` response for the `X-Frugalis-Category` header (if enabled) or infer from the model selected.

The `manual-test/run.sh` script demonstrates this: it checks which model was used to infer the classification.

## Log Inspection

When running tests, monitor the server logs (run with `RUST_LOG=info cargo run`) for:

- `INFO config::load_categories: loaded N categories from config.toml`
- `INFO config::load_routing: using legacy routing.toml`
- `WARN intent_classifier::route_match: category not in routing table` (expected for missing categories in partial config)
- `WARN config::load_categories: ...; using hardcoded category defaults` (when config invalid)

## Cleanup

After running tests, clean up test config files:

```bash
rm -f config.toml routing.toml
```

If you backed up original files, restore them.

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|--------------|-----|
| All classifs go to CASUAL | `config.toml` has only CASUAL or all other categories missing | Add all required categories to `config.toml` |
| 401 Unauthorized | `PROXY_API_BEARER_TOKEN` not set or wrong | Set token env var |
| Server returns 500 | Invalid TOML syntax in `config.toml` | Validate with `toml-cli` or `python3 -c 'import toml; toml.load("config.toml")'` |
| No change after config edit | Server not reloading; needs restart | Restart `cargo run` instance |
| Missing category in routing | Category name typo or not in `[[categories]]` | Ensure exact uppercase name `[A-Z_]+` |

## Success Criteria

- [ ] All 6 test scenarios pass
- [ ] No panics or 5xx errors during testing
- [ ] Server logs show expected info/warn messages
- [ ] Classification behavior identical to pre-refactor for default case
- [ ] Threshold overrides visibly change classification outcomes
- [ ] Legacy routing.toml still works without modifications

---

## Reference

- **Implementation Plan**: `context/changes/shared-category-config/plan.md`
- **Research**: `context/changes/shared-category-config/research.md`
- **Code**: `src/intent_classifier.rs` (CategoryConfig, build_all_patterns, classify)
- **Config**: `src/config.rs` (load_categories, hardcoded_routing)
