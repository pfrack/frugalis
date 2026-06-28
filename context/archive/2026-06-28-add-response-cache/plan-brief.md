# Response Cache — Plan Brief

> Full plan: `context/changes/add-response-cache/plan.md`

## What & Why

Add an in-memory response cache to Frugalis that stores upstream LLM responses keyed by SHA-256 hash of the request body. Identical prompts within the cache TTL window skip both classification and the upstream round-trip — reducing latency from ~500ms-5s to <1ms and eliminating redundant upstream API costs.

## Starting Point

Frugalis currently proxies every request through classify → upstream with no cross-request caching. The only early-return optimization is `try_optimize_request` (`src/main.rs:781`) which catches 4 trivial probe patterns (hello/hi/test/hey). `DashMap` and `sha2` are already in `Cargo.toml`. The `AppState` struct, config system, and dashboard page registry are all ready to accept a new feature without architectural changes.

## Desired End State

Identical non-streaming requests served from cache with <1ms latency. Configurable TTL (default 5 min) and max entries (default 1000) via `[cache]` section in `config.toml`. Cache bypass via `X-Frugalis-No-Cache` header. Dashboard page at `/dashboard/cache` showing hits, misses, hit rate, and entry count. Streaming requests and error responses are never cached. Cache disabled by default — requires explicit `[cache]` config to activate.

## Key Decisions Made

| Decision                       | Choice                                      | Why (1 sentence)                                                              | Source |
| ------------------------------ | ------------------------------------------- | ----------------------------------------------------------------------------- | ------ |
| Cache scope                    | Full response body                          | Maximum latency savings — skips both classify + upstream                      | Plan   |
| Cache key                      | SHA-256 of raw request body                 | Handles all parameters automatically, matches existing probe optimizer's body-matching approach | Plan   |
| TTL                            | Configurable, default 300s                  | Operators tune per deployment; fits existing config pattern                   | Plan   |
| Protocol coverage              | Both /v1/chat/completions and /v1/messages  | Claude Code and OpenAI clients both benefit                                   | Plan   |
| Cache bypass                   | `X-Frugalis-No-Cache` header                | Client control for freshness without operator intervention                    | Plan   |
| Cache size                     | Configurable, default 1000 entries          | ~10 MB memory at average response size; configurable per deployment           | Plan   |
| Eviction                       | TTL expiry + moka capacity eviction         | moka handles both automatically — no custom eviction code needed              | Plan   |
| Error caching                  | 2xx only                                    | Transient errors (429, 503) must not be replayed                              | Plan   |
| Cache timing                   | After probe optimization, before classify   | Probe catches trivials for free; cache catches repeated substantive prompts   | Plan   |
| Observability                  | Dashboard page + debug logs                 | Operators verify cache health without extra infrastructure                    | Plan   |
| Storage backend                | `moka::sync::Cache` (in-memory)             | TTL + capacity bounded, lock-free reads, well-maintained crate                | Plan   |

## Scope

**In scope:**
- In-memory cache with TTL and capacity bounds via `moka`
- `[cache]` config section (ttl_secs, max_entries)
- Cache check/insertion in both proxy handlers
- `X-Frugalis-No-Cache` bypass header
- `/dashboard/cache` stats page
- Unit and integration tests

**Out of scope:**
- Distributed cache (Redis, etc.)
- Persistent cache across restarts
- Caching streaming/SSE responses
- Classification-only caching tier
- Per-model cache namespaces
- Cache-Control response header negotiation

## Architecture / Approach

```
Request → validate body → collect headers → probe optimize → CACHE CHECK → classify → upstream → CACHE INSERT → respond
                                                    ↑                    ↑
                                             (free, catches         (SHA-256 hash,
                                             4 trivial patterns)    moka in-memory)
```

- **`src/cache.rs`** — `ResponseCache` wrapping `moka::sync::Cache<String, CachedEntry>` with atomic hit/miss counters
- **`src/config.rs`** — `CacheConfig` struct, `load_cache_config_from_value`, `[cache]` section in `ConfigRoot`
- **`src/main.rs`** — `response_cache: Option<Arc<ResponseCache>>` in `AppState`; cache check + insertion in both handlers
- **`src/dashboard.rs`** — `CacheTemplate` struct, cache handler, route, nav entry

## Phases at a Glance

| Phase                          | What it delivers                                 | Key risk                                      |
| ------------------------------ | ------------------------------------------------ | --------------------------------------------- |
| 1. Cache Infrastructure        | Module, config, AppState wiring, moka dependency | moka version compatibility                    |
| 2. Wire Into Proxy Handlers    | Cache check + insertion in both handlers         | Regression in existing handler tests          |
| 3. Dashboard Cache Stats Page  | `/dashboard/cache` page with hit/miss stats      | Template Askama compilation errors            |

**Prerequisites:** None — `sha2` and `dashmap` already in deps
**Estimated effort:** ~1-2 sessions across 3 phases

## Open Risks & Assumptions

- `moka` v0.12 sync API is assumed to work with the current Tokio runtime without conflicts (moka's sync cache uses its own internal threads for housekeeping but is otherwise agnostic)
- Cache key = full body hash means even trivial whitespace diffs cause cache misses — acceptable given the 5-minute TTL window
- Dashboard page assumes basic auth middleware already in place (it is) — no new auth concerns
- Memory usage is bounded by max_entries × avg response size (~10 MB at defaults); fine for Render's free tier

## Success Criteria (Summary)

- Identical non-streaming requests served from cache with <1ms latency (vs ~500ms-5s upstream)
- Cache bypass via `X-Frugalis-No-Cache` header works
- Streaming and error responses are never cached
- Dashboard page at `/dashboard/cache` shows accurate hit/miss stats
- All existing tests pass with no regressions
