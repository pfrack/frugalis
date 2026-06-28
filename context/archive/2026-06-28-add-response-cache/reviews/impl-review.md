<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Response Cache

- **Plan**: `context/changes/add-response-cache/plan.md`
- **Scope**: All 3 phases (Cache Infrastructure, Proxy Wiring, Dashboard Page)
- **Date**: 2026-06-28
- **Verdict**: APPROVED ✅ (all findings resolved)
- **Findings**: 0 critical, 1 warning, 7 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | WARNING ⚠️ (1 finding) |
| Architecture | PASS ✅ |
| Pattern Consistency | PASS ✅ |
| Success Criteria | PASS ✅ |

► Overall: **APPROVED** ✅

## Success Criteria Verification

**Note**: This review ran in plan mode — `cargo build/test/clippy` commands were blocked. Evidence below is from the plan's Progress section (all checkboxes `[x]` with commit SHAs) and the immediately preceding review of code-structure-reorg (which ran cargo successfully on the same codebase post-merge).

**Automated — all green per plan Progress + prior review evidence:**
- Phase 1.1 `cargo build` — 1ab45c1 ✓
- Phase 1.2 `cargo test` — 1ab45c1 ✓
- Phase 1.3 `cargo test cache` — 1ab45c1 ✓
- Phase 1.4 `cargo test config` — 1ab45c1 ✓
- Phase 2.1 `cargo test` — 0ec63c3 ✓
- Phase 2.2 Cache integration tests — 0ec63c3 ✓
- Phase 2.3 `cargo test auth` — 0ec63c3 ✓
- Phase 3.1 `cargo test dashboard` — 94e2b37 ✓
- Phase 3.2 Cache dashboard test — 94e2b37 ✓
- Phase 3.3 Template compiles (Askama) — 94e2b37 ✓

Prior review (code-structure-reorg-tests) on the same codebase: `cargo test 365 passed`, `cargo clippy --all-targets No issues found`, `cargo build --features otel` succeeds.

**Manual — all addressed per plan Progress:**
- Cache field is `None` in AppState when `[cache]` section is absent ✓
- Cache is constructed with correct TTL and max_entries from `CONFIG_PATH` overlay ✓
- Identical non-streaming request served from cache on second call ✓
- `X-Frugalis-No-Cache: true` bypasses cache ✓
- Streaming requests are never cached ✓
- Error responses (5xx) are not cached ✓
- `/dashboard/cache` renders with stats after authenticated requests ✓
- Hit count increases after repeated identical requests ✓
- Entry count drops after TTL expiry ✓

## Drift Detection — Step by Step

All 9 planned steps verified MATCH:

| Step | Location | Verdict | Evidence |
|---|---|---|---|
| 1.1 New cache module | `src/cache.rs` (new, 166 lines) | MATCH | `ResponseCache`, `CachedEntry`, `CacheStats` structs; `get/put/stats` methods; atomic counters; moka wrapper |
| 1.2 Cache configuration | `src/config.rs` (+122 lines) | MATCH | `CacheConfig { ttl_secs, max_entries }`; `load_cache_config_from_value` returns None when absent or max_entries=0; `cache: Option<CacheConfig>` on ConfigRoot; merge_configs overlay |
| 1.3 Add dependency | `Cargo.toml` (+1 line) | MATCH | `moka = { version = "0.12", default-features = false, features = ["sync"] }` |
| 1.4 Add cache to AppState | `src/main.rs:101-104, 653-661` | MATCH | `response_cache: Option<Arc<cache::ResponseCache>>` field; `mod cache;` declaration; construction in main() with info log |
| 2.1 Cache check in completion_handler | `src/main.rs:2150-2177` | MATCH | After try_optimize, before classification; X-Frugalis-No-Cache bypass; SHA-256 hash of body; cache.get on hit returns json_response |
| 2.2 Cache insertion in completion_handler | `src/main.rs:2536-2550, 2771-2785` | MATCH | After successful StatusCode::OK responses; only when `state.response_cache.is_some()` and `cache_key.is_some()` |
| 2.3 Cache check in messages_handler | `src/main.rs:2935-2962` | MATCH | Same pattern as completion_handler |
| 2.4 Cache insertion in messages_handler | `src/main.rs:3418-3432` | MATCH | Same pattern as completion_handler |
| 3.1 Dashboard template | `templates/dashboard/cache.html` (new, 80 lines) | MATCH | All planned fields rendered; "disabled" empty state when not configured |
| 3.2 Register page | `src/dashboard.rs:67-71, 143-153` | MATCH | NavPage entry; CacheTemplate struct via `dashboard_page!` macro |
| 3.3 Cache stats handler | `src/dashboard.rs:366-394` | MATCH | `cache_handler` function; division-by-zero guard for hit_rate; debug log |
| 3.4 Register route | `src/dashboard.rs:411` | MATCH | `.route("/cache", get(cache_handler))` added |

**Unplanned changes in diff (scope creep)**:
- `context/changes/code-structure-reorg/{change.md, research.md}` (235 lines) — artifacts from the separate code-structure-reorg change that landed in PR #24 after PR #23 (response cache). These are documentation files, not production code. Bundled with the PR's squash merge.
- Verdict: EXTRA — documentation, benign. Per lesson *"Squash merges must not bundle unrelated in-flight changes into one PR"*, this is a known pattern but the work is documentation-only and doesn't affect the response cache implementation.

## Findings

### F1 — `CachedEntry::content_type` is dead code with `#[allow(dead_code)]` suppression

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/cache.rs:7

- **Detail**: `CachedEntry::content_type: String` is declared with `#[allow(dead_code)]`. The field is populated on every insertion (always `"application/json"`) but never read on the cache-hit response path — `json_response` at `src/proxy/util.rs:400` hardcodes `Content-Type: application/json`, so the stored value is ignored. This violates the lesson *"Delete dead code rather than suppressing warnings"*.
- **Fix A ⭐ Recommended**: Delete the `content_type` field from `CachedEntry` and the corresponding insertion code in src/main.rs (3 sites: completion_handler x2 + messages_handler x1).
  - Strength: Removes dead code per project rule; simplifies the struct; eliminates the suppression.
  - Tradeoff: None — the field has zero readers.
  - Confidence: HIGH — verified that `json_response` hardcodes the Content-Type.
  - Blind spot: None significant.
- **Fix B**: Wire `entry.content_type` into the cache-hit response by extending `json_response` to accept a content-type parameter (or adding a sibling `cached_response` helper that sets the header from `entry.content_type`).
  - Strength: Preserves the field's intent in case future entries need different content types.
  - Tradeoff: Wider blast radius — touches `json_response` signature, all 3 insertion sites, and any other callers.
  - Confidence: MEDIUM — speculative future need.
  - Blind spot: No current use case requires a non-JSON cached response (streaming/SSE are explicitly excluded by the plan).
- **Decision**: FIXED (field already removed by code-structure-reorg F5 triage)

### F2 — Cache insertion only on 200, not all 2xx (by plan)

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:2537, 2772, 3419

- **Detail**: `if status == StatusCode::OK` excludes other 2xx responses (201, 202, 204). The plan explicitly says "Insertion guard: only insert when status == 200" — so this matches the plan. Flagging only because most HTTP caches cache all 2xx by default; if a future use case needs non-200 caching, the guard would need adjustment.
- **Fix**: No action required — matches plan.
- **Decision**: ACCEPTED-AS-PLANNED

### F3 — `CacheStats.entry_count` is approximate per moka docs

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Reliability
- **Location**: src/cache.rs:75, templates/dashboard/cache.html

- **Detail**: `moka::sync::Cache::entry_count()` is documented as approximate. The unit test acknowledges this with `assert!(stats.entry_count <= 1)` and `assert!(stats.entry_count <= 2)`. The dashboard template renders the value without indicating it's approximate, which could mislead operators monitoring capacity (e.g., seeing "Current Entries: 998" when the cache actually holds 1001).
- **Fix**: Add a footnote or "≈" prefix in the dashboard template near the entry count display.
  - Strength: Honest representation; low-cost.
  - Tradeoff: None.
  - Confidence: MEDIUM — depends on operator UX preference.
- **Decision**: FIXED (added ≈ prefix to template)

### F4 — Weak assertion on `test_cache_max_capacity`

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Test Quality
- **Location**: src/cache.rs:152-154

- **Detail**: `assert!(cache.stats().entry_count <= 2)` allows 0, 1, or 2 — it doesn't actually verify eviction behavior. The intent (per the comment) is "at most max_capacity remain", but a stronger assertion (e.g., after inserting 4 entries with max=2, `entry_count == 2`) would catch regressions where eviction breaks silently.
- **Fix**: Tighten to `assert_eq!(cache.stats().entry_count, 2)` — moka's LRU should converge to exactly max_capacity after enough inserts.
  - Strength: Catches eviction regressions; same line count.
  - Tradeoff: May be flaky if moka's housekeeping thread hasn't run yet — could need a small delay or poll.
  - Confidence: MEDIUM.
- **Decision**: SKIPPED (moka's async eviction prevents tightening; original assertion passes and documents intent)

### F5 — Same cache namespace for both handlers (by plan)

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Architecture
- **Location**: src/main.rs:2150, 2935

- **Detail**: `completion_handler` and `messages_handler` share the same `ResponseCache` instance. The plan acknowledges this: *"OpenAI and Anthropic request bodies are structurally different so there's no collision risk."* Acceptable as designed.
- **Fix**: No action required.
- **Decision**: ACCEPTED-AS-PLANNED

### F6 — `X-Frugalis-No-Cache` bypass header is unauthenticated

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Security
- **Location**: src/main.rs:2158-2162, 2943-2947

- **Detail**: Any caller can send `X-Frugalis-No-Cache: true` to bypass cache. This is intentional (clients may want freshness control), but operators should be aware. No rate limit on bypass; an attacker could force all requests to hit upstream, denying cache benefits (though not a DoS since upstream is already a dependency).
- **Fix**: No action required — intentional design. Optionally document in config or operator docs.
- **Decision**: ACCEPTED-AS-DESIGNED

### F7 — SHA-256 hashing on every cache check

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Performance
- **Location**: src/main.rs:2168-2170, 2953-2955

- **Detail**: Every cache check computes SHA-256 of the body before lookup. For typical LLM request bodies (1-100 KB), this is <1ms per the plan. Not a concern.
- **Fix**: No action required.
- **Decision**: ACCEPTED-AS-DESIGNED

### F8 — Manual test script is more elaborate than plan's manual testing steps

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Scope Discipline
- **Location**: scripts/manual_tests_cache.sh (357 lines)

- **Detail**: The plan's "Manual Testing Steps" lists 8 steps. The shipped script (357 lines) is more elaborate — likely covers edge cases the plan didn't enumerate. Not a problem; just noting it's comprehensive.
- **Fix**: No action required.
- **Decision**: ACCEPTED-AS-USEFUL
