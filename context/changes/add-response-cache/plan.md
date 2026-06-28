# Response Cache Implementation Plan

## Overview

Add an in-memory response cache to the Frugalis gateway that stores upstream LLM responses keyed by SHA-256 hash of the request body. Identical prompts served from cache skip classification AND the upstream round-trip entirely, reducing latency and upstream costs. `moka` provides TTL-based expiry + capacity-bounded eviction. A dashboard page surfaces hit/miss stats.

## Current State Analysis

Frugalis proxies every request through classify → resolve providers → upstream → buffer/stream with no cross-request caching. The only early-return path is `try_optimize_request` (`src/main.rs:781`) which catches 4 trivial probe patterns (hello/hi/test/hey) and empty messages. `DashMap` is already in `Cargo.toml` (used by fewshot_classifier for ML feature vectors, not caching). Both proxy handlers (`completion_handler` at `src/main.rs:2090`, `messages_handler` at `src/main.rs:2820`) share the same structure: validate body → probe optimize → classify → iterate providers → buffer/stream. Streaming responses use SSE with `Cache-Control: no-cache` and are inherently uncacheable. The `sha2` crate for SHA-256 hashing is already a dependency.

### Key Discoveries:

- `try_optimize_request` at `src/main.rs:781` is the only existing early-return path — cache check should slot in after it but before classification
- `handle_buffered_response` at `src/main.rs:1350` and `translate_anthropic_buffered_response` at `src/main.rs:1806` are the non-streaming response buffers where cache insertion logically belongs
- `AppState` at `src/main.rs:86` has no cache field — adding one follows the existing pattern of optional feature fields (e.g., `persistence: Option<...>`)
- Dashboard pages follow a strict pattern: `NavPage` in `PAGES` (`src/dashboard.rs:44`), `dashboard_page!` macro (`src/dashboard.rs:81`), handler function, route in `routes()`
- Config follows `[section]` → `ConfigRoot` optional field → `load_*_from_value` → `merge_configs` pattern (e.g., `[dashboard]`)
- `sha2` crate is already a dependency for SHA-256 hashing

## Desired End State

Identical non-streaming LLM requests served from an in-memory cache within the TTL window, bypassing both classification and upstream calls. Cache respects `X-Frugalis-No-Cache` bypass header. Operators can configure TTL and max entries via `config.toml` `[cache]` section, monitor hit/miss rates via `/dashboard/cache`, and disable the cache entirely by omitting the config section. Streaming requests and error responses are never cached.

## What We're NOT Doing

- Distributed/shared cache (Redis, etc.) — in-memory only
- Persistent cache across restarts
- Caching streaming/SSE responses
- Caching classification results separately from full responses
- Cache warming or pre-population
- Per-model or per-provider cache namespaces — single unified cache
- Cache-Control response header negotiation — cache is transparent to clients

## Implementation Approach

Use the `moka` crate (`sync` feature) for a thread-safe, TTL-aware, capacity-bounded cache. Wrap it in a `ResponseCache` struct in a new `src/cache.rs` module that tracks hit/miss counts via atomics. Add a `[cache]` config section. Insert the cache check after probe optimization but before classification in both proxy handlers. Insert cache entries after successful non-streaming upstream responses.

## Critical Implementation Details

- **Timing & lifecycle**: Cache check MUST come after `try_optimize_request` but before classification. Reversing this order would make the cache check run on every probe (unnecessary) or skip the probe check entirely (regression). The probe optimizer catches trivial cases with near-zero cost; the cache catches repeated substantive prompts with a SHA-256 hash cost.
- **Performance constraints**: SHA-256 hashing of request bodies is O(n) on body size. For typical LLM request bodies (1-100 KB), this is negligible (<1ms). The `moka` cache uses lock-free reads so cache hits add near-zero latency to the response path.

## Phase 1: Cache Infrastructure

### Overview

Create the cache module, config schema, and wire it into `AppState`. Cache is disabled by default (no `[cache]` section in embedded `config.toml`).

### Changes Required:

#### 1. New cache module

**File**: `src/cache.rs` (new)

**Intent**: Provide a thread-safe, TTL-bounded, capacity-bounded response cache with hit/miss tracking. Wraps `moka::sync::Cache` and exposes `get`, `put`, and `stats` operations.

**Contract**:
- `ResponseCache` struct with `new(ttl_secs: u64, max_entries: u64) -> Self`
- `get(&self, key: &str) -> Option<CachedEntry>` — checks moka cache, increments `hits` or `misses` atomically
- `put(&self, key: String, entry: CachedEntry)` — inserts into moka cache (moka handles TTL + capacity eviction automatically)
- `stats(&self) -> CacheStats` — returns `CacheStats { hit_count, miss_count, entry_count, max_entries, ttl_secs }`
- `CachedEntry { body: String, content_type: String, status: u16 }` — the stored response
- Cache key is caller-provided (hex-encoded SHA-256); the module does not compute hashes
- Uses `std::sync::atomic::AtomicU64` for hit/miss counters (lock-free)
- `moka::sync::Cache` internally handles concurrent access without locks

#### 2. Cache configuration

**File**: `src/config.rs`

**Intent**: Add a `[cache]` config section so operators can tune TTL and capacity or disable the cache.

**Contract**:
- Add `CacheConfig` struct with fields: `ttl_secs: u64` (default 300), `max_entries: u64` (default 1000)
- Add `load_cache_config_from_value(root: &ConfigRoot) -> Option<CacheConfig>` — returns `None` when section is absent or `max_entries == 0` (cache disabled)
- Add `cache: Option<CacheConfig>` field to `ConfigRoot`
- Add cache line to `merge_configs`: overlay replaces base (simple override, not field-by-field merge — the section is small enough that full replacement is fine)

#### 3. Add dependency

**File**: `Cargo.toml`

**Intent**: Add `moka` crate for the cache backend.

**Contract**: Add `moka = { version = "0.12", default-features = false, features = ["sync"] }` to `[dependencies]`

#### 4. Add cache to AppState

**File**: `src/main.rs`

**Intent**: Hold the cache in shared application state so both handlers can access it.

**Contract**:
- Add field `response_cache: Option<Arc<cache::ResponseCache>>` to `AppState` struct
- Add `mod cache;` declaration
- In `main()`, construct the cache after config loading: call `load_cache_config_from_value`, if `Some(cfg)` create `Arc::new(cache::ResponseCache::new(cfg.ttl_secs, cfg.max_entries))`, otherwise `None`
- Log at info level when cache is enabled: `info!("Response cache enabled: ttl={}s max_entries={}", ttl, max)`

### Success Criteria:

#### Automated Verification:

- Project compiles: `cargo build`
- All existing tests pass: `cargo test`
- Unit tests for `ResponseCache` pass: `cargo test cache` (new tests in `src/cache.rs`)
- Config parsing works: `cargo test config` (existing tests + new cache config test)

#### Manual Verification:

- Cache field is `None` in `AppState` when `[cache]` section is absent from `config.toml` (current state — no regression)
- Cache is constructed with correct TTL and max_entries when `[cache]` section is present in a custom config overlay via `CONFIG_PATH`

---

## Phase 2: Wire Cache Into Proxy Handlers

### Overview

Add cache check (after probe optimization, before classification) and cache insertion (after successful non-streaming upstream response) to both `completion_handler` and `messages_handler`.

### Changes Required:

#### 1. Cache check in completion_handler

**File**: `src/main.rs` (function `completion_handler`)

**Intent**: After probe optimization and `X-Frugalis-No-Cache` bypass check, look up the request body hash in the cache. On hit, return the cached response immediately — skipping classification and upstream call entirely.

**Contract**:
- After `try_optimize_request` returns `None` (line ~2137) and before classification (line ~2158), insert the cache check block
- The block must:
  1. Check `X-Frugalis-No-Cache` header — if present and non-empty, skip cache (debug log)
  2. If `state.response_cache.is_some()` and not bypassed: compute SHA-256 of raw body bytes (`&body`), hex-encode the digest as the cache key
  3. Call `cache.get(&key)` — if `Some(entry)`, log `debug!("Cache hit for completion request")` and return `json_response` with `entry.status`, `entry.body`, and `entry.content_type`
  4. On miss, log `debug!("Cache miss")` and continue to classification
- Use `sha2::Sha256` + `sha2::Digest` (already in deps) for hashing

#### 2. Cache insertion in completion_handler

**File**: `src/main.rs` (function `completion_handler`)

**Intent**: After a successful non-streaming upstream response (2xx status, not streaming), store the response in the cache before returning to the client.

**Contract**:
- All non-streaming success-path returns in `completion_handler` that produce a `(status, response_body)` pair must insert into the cache before calling `json_response(status, response_body)`
- Insertion guard: only insert when `state.response_cache.is_some()`, `status == 200`, and the response was not streaming
- Insertion call: `state.response_cache.as_ref().unwrap().put(cache_key.clone(), CachedEntry { body: response_body.clone(), content_type: "application/json".to_string(), status: status.as_u16() })`
- The insertion points are:
  - After `translate_anthropic_buffered_response` success (line ~2470 area)
  - After `handle_buffered_response` success (line ~2694 area)
- The `cache_key` must be captured before entering the provider loop (same key as computed during cache check). If the cache check block computed the key, reuse it; otherwise (cache disabled at check time, or bypass header), the key is not needed for insertion either — skip insertion

#### 3. Cache check in messages_handler

**File**: `src/main.rs` (function `messages_handler`)

**Intent**: Same as completion_handler but for the Anthropic Messages endpoint. Uses the same cache namespace (body hash) — OpenAI and Anthropic request bodies are structurally different so there's no collision risk.

**Contract**:
- Same structure as completion_handler cache check: after probe optimization (line ~2868) and `X-Frugalis-No-Cache` check, before classification (line ~2886)
- On cache hit, return cached response using `json_response` with the stored status, body, and content type

#### 4. Cache insertion in messages_handler

**File**: `src/main.rs` (function `messages_handler`)

**Intent**: Same insertion pattern as completion_handler.

**Contract**:
- Insert after each successful non-streaming 2xx response path in `messages_handler`
- The stored content_type may be `"application/json"` (for translated OpenAI→Anthropic responses or passthrough Anthropic responses)

### Success Criteria:

#### Automated Verification:

- All existing tests pass: `cargo test`
- New cache integration tests pass: verify cache hit returns identical body, cache miss proceeds to upstream, bypass header skips cache, streaming requests are not cached, error responses are not cached
- Auth tests still pass: `cargo test auth`
- All existing proxy tests still pass (no regressions in completion/messages handler behavior)

#### Manual Verification:

- Send identical non-streaming POST to `/v1/chat/completions` twice within TTL: second response is served from cache (lower latency, no upstream call)
- Send with `X-Frugalis-No-Cache: true`: always hits upstream
- Send streaming request (`"stream": true`): never served from cache
- Send request that gets a 5xx upstream error: error not cached, retry hits upstream again

---

## Phase 3: Dashboard Cache Stats Page

### Overview

Add a `/dashboard/cache` page showing cache hit/miss statistics, entry count, and configuration.

### Changes Required:

#### 1. Dashboard page template

**File**: `templates/dashboard/cache.html` (new)

**Intent**: Render cache statistics in the dashboard UI.

**Contract**:
- Extends `base.html` with `{% block content %}`
- Displays: whether cache is enabled, hit count, miss count, hit rate percentage, current entries, max entries, TTL
- When cache is disabled, show a message: "Response cache is not configured. Add a [cache] section to config.toml to enable."

#### 2. Register page and template struct

**File**: `src/dashboard.rs`

**Intent**: Add the cache page to the dashboard navigation and define its template struct.

**Contract**:
- Add `NavPage` entry to `PAGES` array: `{ path: "cache", label: "Cache", icon: ... }` — use a simple database/cache icon SVG
- Define template struct using `dashboard_page!` macro:

```
dashboard_page! {
    struct CacheTemplate for "dashboard/cache.html" {
        enabled: bool,
        hit_count: u64,
        miss_count: u64,
        hit_rate: f64,
        entry_count: u64,
        max_entries: u64,
        ttl_secs: u64,
    }
}
```

#### 3. Cache stats handler

**File**: `src/dashboard.rs`

**Intent**: Query the cache for current stats and render the template.

**Contract**:
- Handler function `async fn cache_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse`
- If `state.response_cache.is_none()`: render `CacheTemplate` with `enabled: false` and zero stats
- If cache is present: call `state.response_cache.stats()`, compute `hit_rate = hits / (hits + misses)` (guard against division by zero), render template
- Log at debug level: `debug!("Cache stats: hits={} misses={} entries={}", ...)`

#### 4. Register route

**File**: `src/dashboard.rs`

**Intent**: Add the cache page route to the dashboard router.

**Contract**: Add `.route("/cache", get(cache_handler))` to the router chain in `routes()`

### Success Criteria:

#### Automated Verification:

- Dashboard tests pass: `cargo test dashboard`
- New cache dashboard test verifies: page returns 200 with basic auth, shows "disabled" when no cache configured, shows stats when cache is enabled
- Template compiles (Askama checks at compile time)

#### Manual Verification:

- Navigate to `/dashboard/cache` with valid basic auth credentials: page renders with cache stats
- After sending several identical requests: hit count increases, miss count stays stable
- After TTL expires: entry count drops as entries expire, new request produces a miss

---

## Testing Strategy

### Unit Tests:

- `src/cache.rs`: `test_cache_get_put` (insert and retrieve), `test_cache_hit_miss_counters`, `test_cache_ttl_expiry` (entry expires after TTL), `test_cache_max_capacity` (LRU eviction when full), `test_cache_stats`
- `src/config.rs`: `test_cache_config_defaults`, `test_cache_config_disabled_when_absent`, `test_cache_config_disabled_when_max_entries_zero`

### Integration Tests:

- `test_cache_hit_returns_cached_response` — send same request twice, verify second response body matches first, and second response has lower latency
- `test_cache_miss_proceeds_to_upstream` — send unique request, verify upstream is called
- `test_cache_bypass_header_skips_cache` — send with `X-Frugalis-No-Cache: true`, verify upstream is called even for cached request
- `test_cache_streaming_not_cached` — send streaming request, verify response is not inserted into cache
- `test_cache_error_not_cached` — send request to dead endpoint, verify error response is not cached
- `test_cache_disabled_when_no_config` — verify that when `AppState.response_cache` is `None`, handlers proceed normally (no cache lookups, no panics)
- `test_cache_dashboard_requires_auth` — verify `/dashboard/cache` returns 401 without credentials
- `test_cache_dashboard_authenticated` — verify `/dashboard/cache` returns 200 with valid credentials

### Manual Testing Steps:

1. Start Frugalis with a `CONFIG_PATH` overlay that includes `[cache]` section
2. Send a non-streaming request to `/v1/chat/completions` — note the response
3. Send the identical request again — verify same response, lower latency
4. Check `/dashboard/cache` — verify hit_count = 1, miss_count = 0 (or 1 depending on first request)
5. Send with `X-Frugalis-No-Cache: true` — verify upstream is called
6. Wait for TTL to expire, send request again — verify cache miss and new upstream call
7. Send a streaming request — verify it's never cached
8. Remove `[cache]` section, restart — verify handlers work normally (no panics, no cache behavior)

## Performance Considerations

- SHA-256 hashing is O(n) on body size; for typical LLM request bodies (1-100 KB), this is <1ms on modern hardware
- `moka::sync::Cache` uses concurrent hash maps with lock-free reads — cache hits add negligible latency
- With default max_entries=1000 and average response ~10 KB, memory overhead is ~10 MB for cached responses
- TTL eviction is handled by moka's internal housekeeping (background thread, configurable) — no request-path overhead

## Migration Notes

- No data migration needed — cache is purely in-memory, ephemeral
- Existing `config.toml` does not need changes — cache is disabled by default when `[cache]` section is absent
- No breaking changes to API contracts or response formats

## References

- Existing probe optimizer: `src/main.rs:781` (`try_optimize_request`)
- Existing buffered response handler: `src/main.rs:1350` (`handle_buffered_response`)
- Dashboard page pattern: `src/dashboard.rs:44` (PAGES), `src/dashboard.rs:81` (macro), `src/dashboard.rs:347` (routes)
- Config pattern: `src/config.rs:133-139` (`load_dashboard_config_from_value`)
- AppState: `src/main.rs:86`
- `sha2` crate: already in `Cargo.toml:40`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Cache Infrastructure

#### Automated

- [x] 1.1 Project compiles: `cargo build` — 1ab45c1
- [x] 1.2 All existing tests pass: `cargo test` — 1ab45c1
- [x] 1.3 Cache unit tests pass: `cargo test cache` — 1ab45c1
- [x] 1.4 Cache config tests pass (included in `cargo test config`) — 1ab45c1

#### Manual

- [x] 1.5 Cache field is `None` in AppState when `[cache]` section is absent
- [x] 1.6 Cache is constructed with correct TTL and max_entries from `CONFIG_PATH` overlay

### Phase 2: Wire Cache Into Proxy Handlers

#### Automated

- [x] 2.1 All existing tests pass: `cargo test` — 0ec63c3
- [x] 2.2 Cache integration tests pass (hit, miss, bypass, streaming, error, disabled) — 0ec63c3
- [x] 2.3 Auth tests pass: `cargo test auth` — 0ec63c3

#### Manual

- [x] 2.4 Identical non-streaming request served from cache on second call
- [x] 2.5 `X-Frugalis-No-Cache: true` bypasses cache
- [x] 2.6 Streaming requests are never cached
- [x] 2.7 Error responses (5xx) are not cached

### Phase 3: Dashboard Cache Stats Page

#### Automated

- [x] 3.1 Dashboard tests pass: `cargo test dashboard`
- [x] 3.2 Cache dashboard test: 200 with auth, disabled state, stats state
- [x] 3.3 Template compiles (Askama)

#### Manual

- [x] 3.4 `/dashboard/cache` renders with stats after authenticated requests
- [x] 3.5 Hit count increases after repeated identical requests
- [x] 3.6 Entry count drops after TTL expiry
