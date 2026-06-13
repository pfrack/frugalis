---
date: 2026-06-13T12:04:15+02:00
researcher: kiro
git_commit: 4f99e7b
branch: fewshots-classifier
repository: cerebrum
topic: "Better config UX — self-reliant, easy to use without explanation"
tags: [research, codebase, config, ux, cli, onboarding]
status: complete
last_updated: 2026-06-13
last_updated_by: kiro
---

# Research: Better Config UX — Self-Reliant, Easy to Use

**Date**: 2026-06-13T12:04:15+02:00
**Researcher**: kiro
**Git Commit**: 4f99e7b
**Branch**: fewshots-classifier
**Repository**: cerebrum

## Research Question

How to make Cerebrum's configuration as easy as possible for users — self-reliant and easy to use without explanation or help.

## Summary

The current config system is *architecturally sound* (embedded defaults + overlay merge + validation) but has **5 critical UX gaps** that make it hostile to new users:

1. **No discoverability** — no `--help`, no `--init`, no `--config` flag; user must know the `CONFIG_PATH` env var exists
2. **Full-replacement merge for routing/categories** — overlay replaces the ENTIRE section, so specifying one route silently drops all others
3. **Broken examples** — `routing_examples/` use obsolete flat format that won't work as `CONFIG_PATH` overlays
4. **No minimal template** — the 180-line `config.toml` mixes power-user concerns (regex patterns) with basic setup (which model to route to)
5. **Silent degradation** — no warning when a routing overlay drops category routes

## Detailed Findings

### 1. Current Config Loading Architecture

The system works in three layers (`src/main.rs:92-112`):

```
Embedded config.toml (include_str!) → Parse as ConfigRoot
                                         ↓
                              CONFIG_PATH overlay → merge_configs()
                                         ↓
                              Final ConfigRoot → extract per-section configs
```

**Key functions:**
- `load_config_from_path()` — `src/config.rs:696-703` — detects TOML/YAML by extension
- `merge_configs()` — `src/config.rs:815-870` — merges overlay into base
- Overlay semantics: struct fields merge field-by-field, but `routing`, `categories`, `negative_patterns`, `auth_providers`, `model_costs` are **full-replacement**

### 2. The Full-Replacement Merge Footgun

`src/config.rs:845-858`:
```rust
if let Some(v) = overlay.categories { base.categories = Some(v); }
if let Some(v) = overlay.routing { base.routing = Some(v); }
if let Some(v) = overlay.negative_patterns { base.negative_patterns = Some(v); }
```

**Impact**: A user who creates a config with only `[routing.COMPLEX_REASONING]` gets:
- COMPLEX_REASONING → their specified model ✓
- FILE_READING, SYNTAX_FIX, CASUAL → empty RouteEntry (no endpoint, no model) ✗
- DEFAULT fallback → bare DEFAULT_MODEL with empty endpoint ✗

This is the #1 UX problem. Users expect additive override, get destructive replace.

### 3. CLI Surface — Zero Discoverability

`src/main.rs:62-73` — manual arg parsing:
- `--validate` — only recognized flag
- Anything else → exit code 2
- No `--help`, `--version`, `--init`, `--config`

User must know to set `CONFIG_PATH` env var. No startup message hints at this.

### 4. routing_examples/ Are Broken

All 4 files use the legacy flat format:
```toml
[FILE_READING]    ← should be [routing.FILE_READING]
[FALLBACK]        ← should be [routing.DEFAULT]
```

These won't deserialize into `ConfigRoot` correctly — the keys don't match any section. They silently produce zero routing overrides. `ROUTING_CONFIG_LEGACY` (`src/config.rs:15`) exists but is `#[cfg(test)]` only.

### 5. What a Minimal Config Actually Looks Like

A user who just wants to route to OpenRouter needs **only the routing section**:

```toml
[routing.DEFAULT]
model = "openai/gpt-4o-mini"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENROUTER_API_KEY"

[routing.COMPLEX_REASONING]
model = "anthropic/claude-3.5-sonnet"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENROUTER_API_KEY"
```

But due to the full-replacement merge, they MUST also specify FILE_READING, SYNTAX_FIX, and CASUAL routes or those categories get broken routing.

### 6. RouteEntry Required Fields

`src/routing.rs` — `RouteEntry` struct:
- `model: String` — required
- `endpoint: String` — required
- `provider_type: String` — required
- `cost_per_1m_input_tokens: Option<f64>` — optional
- `api_key_env: Option<String>` — optional (not needed for ollama/local)

## Proposed Solutions

### A. `--init` Command (LOW effort, HIGH impact)

Add a `--init [path]` flag that writes a minimal starter config:

```rust
"--init" => {
    let minimal = include_str!("../init_template.toml");
    // write to path or stdout
}
```

**`init_template.toml`** — ~20 lines, routing-only, heavily commented:
```toml
# Cerebrum Configuration — Quickstart
# Usage: CONFIG_PATH=this-file.toml cerebrum
# Only override what you need. Everything else uses built-in defaults.
# Full reference: https://github.com/.../config.toml

# Route intents to models. ALL categories must be listed (full replacement).
[routing.FILE_READING]
model = "your-model"
endpoint = "https://your-provider/v1/chat/completions"
provider_type = "openai_compatible"  # or: nvidia_nim, anthropic, ollama
api_key_env = "YOUR_API_KEY"

[routing.SYNTAX_FIX]
model = "your-cheap-model"
endpoint = "https://your-provider/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "YOUR_API_KEY"

[routing.COMPLEX_REASONING]
model = "your-smart-model"
endpoint = "https://your-provider/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "YOUR_API_KEY"

[routing.CASUAL]
model = "your-cheap-model"
endpoint = "https://your-provider/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "YOUR_API_KEY"

[routing.DEFAULT]
model = "your-cheap-model"
endpoint = "https://your-provider/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "YOUR_API_KEY"

# Required env vars:
#   PROXY_API_BEARER_TOKEN   — bearer token for proxy routes
#   DASHBOARD_BASIC_USER     — dashboard login
#   DASHBOARD_BASIC_PASSWORD — dashboard password
#   YOUR_API_KEY             — the key referenced above
```

### B. `--quickstart` Interactive Wizard (MEDIUM effort)

Prompts for provider → fills template. No new deps (raw stdin). Generates a ready-to-use file.

### C. Fix the Merge Semantics for Routing (MEDIUM effort, HIGH impact)

**Option 1**: Change `merge_configs` to do per-key merge for routing:
```rust
// Instead of full replacement:
if let Some(overlay_routing) = overlay.routing {
    let base_routing = base.routing.get_or_insert_with(HashMap::new);
    for (key, entry) in overlay_routing {
        base_routing.insert(key, entry);  // per-route override
    }
}
```

This lets users specify ONLY the routes they want to change. Much friendlier.

**Option 2**: Keep full-replacement but warn at startup when routing overlay drops known categories.

### D. Fix routing_examples/ (LOW effort)

Rewrite all 4 files to use `[routing.CATEGORY]` format with `[routing.DEFAULT]` instead of `[FALLBACK]`.

### E. Startup Messaging (LOW effort)

After config loads, log:
```
INFO No CONFIG_PATH set — using embedded defaults. Run `cerebrum --init` to generate a starter config.
INFO Routes loaded: FILE_READING, SYNTAX_FIX, COMPLEX_REASONING, CASUAL, DEFAULT
```

### F. `--help` Flag (LOW effort)

```
cerebrum — intent-aware routing gateway

USAGE:
    cerebrum [OPTIONS]

OPTIONS:
    --validate    Validate configuration and exit
    --init [PATH] Generate a starter config (default: stdout)
    --help        Show this help

ENVIRONMENT:
    CONFIG_PATH              Path to config overlay (TOML or YAML)
    PROXY_API_BEARER_TOKEN   Required for proxy routes
    DASHBOARD_BASIC_USER     Required for dashboard access
    DASHBOARD_BASIC_PASSWORD Required for dashboard access
```

## Priority Ranking

| # | Change | Effort | Impact | Recommendation |
|---|--------|--------|--------|----------------|
| 1 | `--help` flag | 10 min | High | Do first |
| 2 | `--init` command + template | 30 min | High | Do second |
| 3 | Fix routing_examples/ | 15 min | Medium | Quick fix |
| 4 | Startup messaging | 10 min | Medium | Quick fix |
| 5 | Per-key merge for routing | 1-2 hr | Very High | Core UX fix |
| 6 | `--quickstart` wizard | 1 hr | Medium | Nice-to-have |

## Code References

- `src/main.rs:62-73` — CLI arg parsing (only --validate)
- `src/main.rs:76` — `CONFIG_PATH` env var read
- `src/main.rs:92-112` — Config loading + merge
- `src/config.rs:696-703` — `load_config_from_path()` format detection
- `src/config.rs:815-870` — `merge_configs()` — overlay merge logic
- `src/config.rs:845` — categories full-replacement
- `src/config.rs:857` — routing full-replacement
- `src/config.rs:862-900` — `ConfigRoot` struct definition
- `src/routing.rs` — `RouteEntry` struct + `DEFAULT_MODEL` constant
- `routing_examples/*.toml` — legacy flat format (broken for CONFIG_PATH)
- `config.toml` — embedded defaults (180+ lines)

## Architecture Insights

1. **The config system is over-engineered for power users, under-engineered for first-time users.** All the pieces are there (formats, validation, merge) but zero onboarding surface.
2. **Full-replacement merge was a conscious design choice** (see `context/archive/2026-06-09-in-memory-config-filesystem/`) — it prevents stale inherited state. But for routing it creates a cliff: "specify all or specify none."
3. **The fix is layered**: make routing merge per-key (additive), but keep categories/negative_patterns as full-replacement (since partial category sets are actually dangerous — you'd get mismatched routing/classification).

## Historical Context

- `context/archive/2026-06-09-in-memory-config-filesystem/` — introduced `include_str!` + overlay merge
- `context/archive/2026-06-10-move-all-config-to-file/` — moved all env vars to config.toml
- `context/archive/2026-06-11-config-format-upgrade/` — added YAML support + external patterns
- `context/archive/2026-06-07-shared-category-config/` — extracted categories to config

## Open Questions

1. Should routing merge be per-key (additive) or stay full-replacement with better docs/warnings?
2. Should `--init` output TOML or YAML by default? (TOML matches existing `config.toml`)
3. Should there be provider-specific templates (e.g., `cerebrum --init --provider openrouter`)?
4. Is a `--quickstart` wizard worth the complexity given the simple `--init` + edit flow?
