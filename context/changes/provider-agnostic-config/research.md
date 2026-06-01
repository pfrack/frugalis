---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 7940421e3d801a63974e0f060b8ad4f39f322853
branch: main
repository: cerebrum
topic: "Provider-agnostic routing configuration"
tags: [research, provider-agnostic, routing-config, toml, auth-schemes]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
---

# Research: Provider-Agnostic Routing Configuration

Extracted from the master research doc at `context/changes/upstream-proxy-routing/research.md`.

## Provider Auth Matrix

Two fundamental patterns cover ~90% of LLM providers:

| Provider | Auth Header | Base URL | OpenAI-Compat? | Extra Headers |
|---|---|---|---|---|
| OpenAI | `Authorization: Bearer <key>` | `https://api.openai.com/v1` | N/A (standard) | None |
| OpenRouter | `Authorization: Bearer <key>` | `https://openrouter.ai/api/v1` | Yes | `HTTP-Referer`, `X-Title` (optional) |
| Groq | `Authorization: Bearer <key>` | `https://api.groq.com/openai/v1` | Yes | None |
| DeepSeek | `Authorization: Bearer <key>` | `https://api.deepseek.com/v1` | Yes | None |
| Together AI | `Authorization: Bearer <key>` | `https://api.together.xyz/v1` | Yes | None |
| Mistral | `Authorization: Bearer <key>` | `https://api.mistral.ai/v1` | Yes | None |
| Fireworks | `Authorization: Bearer <key>` | `https://api.fireworks.ai/inference/v1` | Yes | None |
| xAI (Grok) | `Authorization: Bearer <key>` | `https://api.x.ai/v1` | Yes | None |
| **Anthropic** | **`x-api-key: <key>`** | `https://api.anthropic.com` | **No** — different body schema | `anthropic-version: 2023-06-01` |
| Azure OpenAI | `api-key: <key>` | Custom per-resource | Yes (different URL) | None |
| **Ollama** | **None** | `http://localhost:11434/v1` | Yes | None |
| vLLM / TGI | None (configurable) | Variable | Yes | Varies |

**Two provider adapters cover the field:**
1. **`openai_compatible`** — `Authorization: Bearer <key>`, forwards body as-is, works with ~90% of providers
2. **`anthropic`** — `x-api-key: <key>`, translates OpenAI body to Anthropic Messages format (deferred from MVP)

Ollama is `openai_compatible` with no API key. vLLM/TGI are `openai_compatible` with optional auth.

## Current Codebase: Provider-Agnostic at Source Level

**The Rust source is already provider-agnostic.** Zero references to OpenRouter, Anthropic, or any specific provider exist in `src/`. The only provider assumption is in `routing.toml.example` (all endpoints point to `https://openrouter.ai/api/v1/chat/completions`).

## RouteEntry Must Gain Fields for Provider Configuration

**Current** (`src/intent_classificator.rs:10-14`):
```rust
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,
    pub cost_per_1m_input_tokens: Option<f64>,
}
```

**Required additions:**

| Field | Type | Purpose |
|---|---|---|
| `provider_type` | `String` | `"openai_compatible"`, `"anthropic"`, `"ollama"` — determines auth + body translation |
| `api_key_env` | `Option<String>` | Name of env var holding the API key. `None` for no-auth providers like Ollama |

**Optional (deferred):**
- `extra_headers: HashMap<String, String>` — provider-specific headers
- `timeout_secs: Option<u64>`, `max_retries: Option<u32>`

## ClassificationResult Must Carry Auth Info Downstream

`ClassificationResult` (`src/intent_classificator.rs:56-61`) currently carries `category`, `model`, `endpoint`, `tier`. It must also carry `provider_type`, `api_key_env` so the handler can construct the correct upstream request.

## routing.toml Format

**Current format** (flat, all fields inline):
```toml
[COMPLEX_REASONING]
model = "claude-3.5-sonnet"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
```

**Provider-agnostic format** (flat, adding `provider_type` and `api_key_env`):
```toml
[COMPLEX_REASONING]
model = "claude-sonnet-4-20250514"
endpoint = "https://api.anthropic.com/v1/messages"
provider_type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[CASUAL]
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
provider_type = "openai_compatible"
api_key_env = "OPENAI_API_KEY"

[LOCAL]
model = "llama3.2"
endpoint = "http://localhost:11434/v1/chat/completions"
provider_type = "ollama"
```

**Two-level design (providers + routing)** was considered but deferred for MVP to keep parsing simple.

## Auth Header Construction: Lookup Table

```rust
fn auth_headers_for(provider_type: &str, api_key: &str) -> Vec<(String, String)> {
    match provider_type {
        "openai_compatible" | "" =>
            vec![("authorization".into(), format!("Bearer {api_key}"))],
        "anthropic" =>
            vec![("x-api-key".into(), api_key.to_string())],
        "ollama" | "local" =>
            vec![],  // no auth
        _ =>
            vec![("authorization".into(), format!("Bearer {api_key}"))],
    }
}
```

## TOML Parsing Changes Required

**File**: `src/intent_classificator.rs:326-354` (`load_routing_from_file`)

The parser currently reads `model`, `endpoint`, `cost_per_1m_input_tokens`. For provider-agnostic support, additionally read:
- `provider_type` from `value.get("provider_type")` — defaults to `""` (empty = openai_compatible)
- `api_key_env` from `value.get("api_key_env")` — optional, defaults to `None`

## Secret Management Pattern

Rather than loading ALL possible keys at startup, the proxy should **lazily read the key** when a route entry references it via `api_key_env`. This avoids requiring env vars for providers that aren't used. The first time a route uses `api_key_env = "ANTHROPIC_API_KEY"`, the handler reads that env var. If missing, return a 502 with a clear error.

## Integration Points

| File | Line(s) | Change |
|---|---|---|
| `src/intent_classificator.rs:10-14` | `RouteEntry` struct | Add `provider_type: String`, `api_key_env: Option<String>` |
| `src/intent_classificator.rs:56-61` | `ClassificationResult` | Add `provider_type: String`, `api_key_env: Option<String>` |
| `src/intent_classificator.rs:326-354` | `load_routing_from_file()` | Parse new TOML fields |
| `src/intent_classificator.rs:217-257` | `hardcoded_routing()` | Add `provider_type: String::new()` defaults |
| `src/main.rs` | `completion_handler` | Read provider_type + api_key_env, resolve key lazily, construct auth header |
| `routing.toml.example` | Entire file | Replace with provider-agnostic format |

**No changes needed**: `src/persistence.rs`, `src/auth.rs`, `Cargo.toml`

## Open Questions (Resolved in Planning)

1. **Flat vs two-level TOML**: Flat (single-level) for MVP. Two-level `[providers.X]` deferred.
2. **Anthropic body translation**: Deferred. Anthropic has a different body schema. `provider_type = "anthropic"` is defined but routing to Anthropic will error until Change 4 adds the body adapter.
3. **Key validation**: Lazy (read when first used). Errors surface mid-request as 502.
4. **`extra_headers` field**: Deferred from MVP. OpenRouter-specific headers like `HTTP-Referer` won't be configurable in this change.
