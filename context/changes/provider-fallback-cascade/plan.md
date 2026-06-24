# Provider Fallback / Cascade — Implementation Plan

## Overview

When an upstream provider fails (5xx, connection timeout, 429 rate-limit), the proxy automatically retries on the next configured provider in priority order. Each routing category defines an ordered list of providers; the first healthy one wins. Cross-protocol fallback is supported (e.g., primary Anthropic, fallback OpenAI-compatible). Streaming requests retry only before the first byte reaches the client.

## Current State Analysis

- `RouteEntry` (`src/routing.rs:7-13`) holds a single provider: `model`, `endpoint`, `provider_type`, `api_key_env`, `cost_per_1m_input_tokens`
- `ClassificationResult` (`src/intent_classifier.rs:88-95`) carries a single provider's fields copied from the matched `RouteEntry`
- `routing_from_value` (`src/config.rs:503-570`) parses a flat `[routing]` TOML table where each category maps to one `RouteEntry`
- `build_upstream_request` (`src/main.rs:1023-1058`) builds a single reqwest request from the classification result
- On failure: `upstream_req.send().await` error → immediate 502 return (`src/main.rs:2100-2115`)
- On 5xx: forwarded directly to client with no retry (`src/main.rs:2134-2145`)
- Three forwarding paths: OpenAI pass-through, Anthropic pass-through (messages_handler), OpenAI→Anthropic translation
- `AppState.routing` is `Arc<RwLock<HashMap<String, RouteEntry>>>` — single entry per category

### Key Discoveries:

- `src/main.rs:2064-2076` — NIM sanitization happens per-request, already demonstrates per-provider body mutation
- `src/main.rs:1880-2042` — Anthropic translation path builds a completely different request body — fallback across protocols means re-entering the translation branch
- `lessons.md` — "Log operational failures before falling back" and "Handle upstream error bodies without full buffering"
- `config.rs:974-1012` — `ConfigRoot` uses `Option<HashMap<String, RouteEntry>>` for routing

## Desired End State

Each routing category can specify an ordered list of providers. When a request fails with a retryable error (5xx, timeout, 429), the proxy logs the failure at `warn!` level and retries on the next provider in the list. The inference log records which provider ultimately served the request and how many attempts were made. Streaming requests that fail before the first byte cascades transparently; mid-stream failures are not retried.

Verification: Configure a category with 2 providers where the primary is down → request succeeds via fallback; dashboard shows the fallback provider and attempt count; latency is bounded to N × per-provider timeout.

## What We're NOT Doing

- Retry on the same provider (exponential backoff) — that's rate-limit courtesy, not cascade
- Mid-stream fallback — once SSE bytes flow to the client, we're committed
- Circuit breaker / health checks — future enhancement; this is per-request cascade only
- Weighted load balancing — this is priority-ordered failover, not round-robin
- Retry-After header parsing for 429 — we cascade immediately to next provider

## Implementation Approach

Three phases, each independently testable:
1. **Data model** — Extend `RouteEntry` to support multiple providers; update config parsing; backward-compatible (single provider = list of one)
2. **Retry loop** — Extract the forwarding logic into a retry loop that walks the provider list; log failures per lessons.md
3. **Observability** — Record attempt count and final provider in inference log; expose in dashboard

## Phase 1: Config & Data Model

### Overview

Extend the routing config to support an ordered array of providers per category. Maintain backward compatibility: existing configs with a single provider per category continue to work unchanged.

### Changes Required:

#### 1. Provider entry struct

**File**: `src/routing.rs`

**Intent**: Define a `ProviderEntry` struct representing a single upstream provider. The existing `RouteEntry` becomes a wrapper holding a `Vec<ProviderEntry>` (ordered by priority) plus the category-level cost field.

**Contract**:
```rust
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderEntry {
    pub model: String,
    pub endpoint: String,
    pub provider_type: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(from = "RouteEntryRaw")]
pub struct RouteEntry {
    pub providers: Vec<ProviderEntry>,
    pub cost_per_1m_input_tokens: Option<f64>,
}
```

The `#[serde(from = "RouteEntryRaw")]` custom deserialization handles both formats:
- **Legacy** (flat): `model = "x", endpoint = "y", provider_type = "z"` → single-element `providers` vec
- **New** (array): `providers = [{model, endpoint, provider_type, api_key_env, timeout_ms}, ...]`

#### 2. Raw deserialization helper

**File**: `src/routing.rs`

**Intent**: Add a `RouteEntryRaw` intermediary struct that serde deserializes first, then converts to the canonical `RouteEntry`. This enables backward-compatible config parsing without changing the TOML structure for single-provider categories.

**Contract**: `RouteEntryRaw` has all fields optional; if `providers` array is absent, constructs a single-element vec from the flat fields. Implements `From<RouteEntryRaw> for RouteEntry`.

#### 3. Config parser update

**File**: `src/config.rs`

**Intent**: Update `routing_from_value` to work with the new `RouteEntry` shape. Since serde handles the `from` conversion, the function body changes minimally — just update field access patterns where `model`/`endpoint` are referenced directly.

**Contract**: `routing_from_value` returns the same `(HashMap<String, RouteEntry>, RouteEntry)` signature. Callers that access `entry.model` now access `entry.providers[0].model` (or via a helper method `entry.primary()`).

#### 4. Convenience accessor on RouteEntry

**File**: `src/routing.rs`

**Intent**: Add `fn primary(&self) -> &ProviderEntry` that returns `&self.providers[0]` — keeps call sites clean during migration.

**Contract**: Panics if `providers` is empty (which the deserializer prevents).

#### 5. Update ClassificationResult

**File**: `src/intent_classifier.rs`

**Intent**: `ClassificationResult` currently copies a single provider's fields. Add a `providers: Vec<ProviderEntry>` field that carries the full fallback list. The existing `model`/`endpoint`/`provider_type`/`api_key_env` fields become the primary (first in list) for backward compatibility with logging and dashboard.

**Contract**: `ClassificationResult` gains `pub providers: Vec<ProviderEntry>`. The `route_match` and `route_fallback` functions populate it from `RouteEntry.providers`.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes — existing tests work with single-provider configs
- `cargo clippy` clean
- New unit test: parse config with `providers = [...]` array → `RouteEntry.providers.len() > 1`
- New unit test: parse legacy flat config → `RouteEntry.providers.len() == 1`

#### Manual Verification:

- Existing `config.toml` loads without changes (backward compatible)
- New config with `providers` array loads correctly

---

## Phase 2: Retry Loop

### Overview

Extract the forwarding logic into a loop that iterates through the provider list. On retryable failure, log at `warn!` and advance to the next provider. Applies to all three forwarding paths (OpenAI pass-through, Anthropic pass-through, OpenAI→Anthropic translation).

### Changes Required:

#### 1. Retryable error detection helper

**File**: `src/main.rs`

**Intent**: Add a function that inspects a `reqwest::Response` status (or send error) and returns whether to retry. Covers: connection errors (from `send().await`), 5xx status codes, and 429 status.

**Contract**: `fn is_retryable_error(result: &Result<reqwest::Response, reqwest::Error>) -> bool` — returns `true` for connection failures, timeouts, 5xx, and 429.

#### 2. Forwarding retry loop for OpenAI pass-through path

**File**: `src/main.rs`

**Intent**: Wrap the existing `build_upstream_request` → `send` → check response section in a loop over `classification.providers`. Each iteration builds the request for that provider (with its own model, endpoint, provider_type, api_key_env, timeout). On retryable failure, `warn!` log the error with provider details and continue to next. On success or non-retryable error, break. If all providers exhaust, return the last error.

**Contract**: The loop replaces the single-shot forwarding in `completion_handler`. The per-provider timeout is set via `reqwest::RequestBuilder::timeout()` if `provider.timeout_ms` is set, otherwise uses the client's default.

#### 3. Forwarding retry loop for Anthropic translation path

**File**: `src/main.rs`

**Intent**: Same retry loop pattern for the Anthropic branch (where OpenAI→Anthropic translation occurs). Each provider iteration re-translates if the provider_type differs from the previous attempt.

**Contract**: Before each attempt, check `provider.provider_type`: if `"anthropic"`, translate body to Anthropic format; otherwise use OpenAI format. This enables cross-protocol fallback (Anthropic primary → OpenAI fallback or vice versa).

#### 4. Forwarding retry loop for messages_handler

**File**: `src/main.rs`

**Intent**: Same pattern in the Anthropic Messages pass-through handler. Here the incoming format is Anthropic; if a fallback provider speaks OpenAI, the body must be translated from Anthropic→OpenAI.

**Contract**: Mirror the retry logic from completion_handler, with the translation direction inverted when crossing protocols.

#### 5. Per-provider timeout support

**File**: `src/main.rs`

**Intent**: When building the upstream request, apply the provider's `timeout_ms` if set. This uses `reqwest::RequestBuilder::timeout()` which overrides the client-level timeout for that single request.

**Contract**: `if let Some(ms) = provider.timeout_ms { req = req.timeout(Duration::from_millis(ms)); }`

### Success Criteria:

#### Automated Verification:

- `cargo test` passes
- `cargo clippy` clean
- New unit test: `is_retryable_error` returns correct booleans for 200, 429, 500, 503, connection error
- New integration test: mock two providers, first returns 503, second returns 200 → handler returns 200

#### Manual Verification:

- Configure a category with primary pointing to an unreachable endpoint and fallback to a real provider → request succeeds
- Check logs: `warn!` message appears for the failed primary attempt
- Streaming request with failed primary → falls back before first byte, client receives stream from fallback

---

## Phase 3: Observability

### Overview

Record fallback information in the inference log so operators can see which provider ultimately served each request, how many attempts were made, and which providers failed.

### Changes Required:

#### 1. Inference record schema extension

**File**: `src/persistence.rs`

**Intent**: Add `provider_attempts` (u8) and `final_provider` (String) fields to the inference log record. These capture how many providers were tried and which one succeeded.

**Contract**: Add columns `provider_attempts SMALLINT DEFAULT 1` and `final_provider TEXT` to the inference table. The SQLite and Postgres insert statements include these fields. Migration adds the columns with defaults so existing rows remain valid.

#### 2. Pass attempt metadata through logging

**File**: `src/main.rs`

**Intent**: After the retry loop resolves, pass the attempt count and final provider name to `log_classification` (or the persistence call). The information is available from the loop index and the provider that succeeded.

**Contract**: Extend the existing `log_classification` helper (or the persistence record struct) to accept `attempts: u8` and `final_provider: &str`.

#### 3. Dashboard display

**File**: `src/dashboard.rs` (templates)

**Intent**: Show `provider_attempts` and `final_provider` in the inference log table when available. Highlight rows where `provider_attempts > 1` to make fallback events visible.

**Contract**: Add columns to the inference log template; conditionally style rows with multiple attempts.

### Success Criteria:

#### Automated Verification:

- `cargo test` passes
- `cargo clippy` clean
- Migration applies cleanly (sqlite + postgres)
- Unit test: inference record with `provider_attempts = 2` persists and reads back correctly

#### Manual Verification:

- Trigger a fallback scenario → check dashboard shows attempt count > 1 and correct final provider
- Normal (no-fallback) requests show attempt count = 1

---

## Testing Strategy

### Unit Tests:

- Config parsing: legacy flat format → single provider
- Config parsing: new array format → multiple providers
- `is_retryable_error`: 200 → false, 429 → true, 500 → true, 503 → true, connection error → true, 400 → false
- `RouteEntry::primary()` returns first provider
- NIM sanitization still works through the retry loop

### Integration Tests:

- Mock server returning 503 on first call, 200 on second → verify fallback works
- Mock server returning 429 → verify cascade to second provider
- All providers fail → verify last error returned to client
- Streaming: first provider returns 503 header → fallback serves the stream

### Manual Testing Steps:

1. Configure a category with 2 providers, stop the primary → confirm fallback
2. Configure per-provider timeout (e.g., 2s) on a slow provider → confirm timeout triggers cascade
3. Verify logs show warn! for each failed attempt
4. Verify dashboard shows attempt count and final provider
5. Verify backward compatibility: existing single-provider config works unchanged

## Performance Considerations

- Zero overhead on happy path — single provider succeeds, no retry loop iteration cost
- Retry adds latency proportional to failed provider timeouts (bounded by `timeout_ms` per provider)
- Body re-translation for cross-protocol fallback: ~1ms CPU for JSON parsing + transformation
- No additional allocations when providers list has one entry (common case)

## References

- Existing forwarding logic: `src/main.rs:2064-2160`
- RouteEntry: `src/routing.rs:7-13`
- ClassificationResult: `src/intent_classifier.rs:88-95`
- Config parsing: `src/config.rs:503-570`
- Anthropic translation: `src/main.rs:1880-2042`
- Lessons: `context/foundation/lessons.md` (log before fallback, buffer errors minimally)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Config & Data Model

#### Automated

- [x] 1.1 cargo test passes with existing single-provider configs
- [x] 1.2 cargo clippy clean
- [x] 1.3 Unit test: providers array config parses correctly
- [x] 1.4 Unit test: legacy flat config parses to single-element providers vec

#### Manual

- [ ] 1.5 Existing config.toml loads without changes

### Phase 2: Retry Loop

#### Automated

- [ ] 2.1 cargo test passes
- [ ] 2.2 cargo clippy clean
- [ ] 2.3 Unit test: is_retryable_error correctness
- [ ] 2.4 Integration test: 503 primary → 200 fallback

#### Manual

- [ ] 2.5 Unreachable primary cascades to fallback provider
- [ ] 2.6 Streaming request cascades before first byte
- [ ] 2.7 warn! log appears for each failed attempt

### Phase 3: Observability

#### Automated

- [ ] 3.1 cargo test passes
- [ ] 3.2 cargo clippy clean
- [ ] 3.3 Migration applies cleanly
- [ ] 3.4 Unit test: inference record with provider_attempts > 1

#### Manual

- [ ] 3.5 Dashboard shows attempt count and final provider for fallback scenarios
