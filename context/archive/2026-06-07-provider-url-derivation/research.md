---
date: 2026-06-07T13:53:44+02:00
researcher: opencode
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "provider-url-derivation"
tags: [research, codebase, routing, endpoint, provider-type]
status: complete
last_updated: 2026-06-07
last_updated_by: opencode
last_updated_note: "Scope reduced to standalone provider_path() only — not wired into routing flow"
---

# Research: Provider URL Derivation

**Date**: 2026-06-07T13:53:44+02:00
**Researcher**: opencode
**Git Commit**: 792b2618299caf26c5adabf28293e2eb1bafc836
**Branch**: provider-url-derivation
**Repository**: cerebrum

## Research Question

How should the system derive upstream provider URLs? The goal is to separate the base URL (stored in `RouteEntry.endpoint`) from the API path suffix (derived from `provider_type` via a `provider_path()` lookup function), mirroring the existing `auth_headers_for()` pattern.

## Summary

The plan is well-grounded. The codebase already has the canonical pattern to mirror (`auth_headers_for` at `src/intent_classifier.rs:358`), the `provider_type` field already exists on both `RouteEntry` and `ClassificationResult`, and the TOML format is already flat (single-level, per-category entries). The single call site that consumes `endpoint` is `completion_handler:405` via `client.post(&classification.endpoint)`. No unexpected blockers or alternative approaches emerged from the research.

One finding not addressed in the plan: the current codebase has augmented its provider type vocabulary beyond the three originally planned types (`openai_compatible`, `anthropic`, `ollama`). The hardcoded routing and TOML examples now use `nvidia_nim` as the default provider type, and the `auth_headers_for` wildcard fallback arm already handles it (returns Bearer auth). The `provider_path()` function must explicitly handle `"nvidia_nim"` as well.

After analysis of configuration complexity tradeoffs, scope was reduced to adding `provider_path()` as a standalone tested utility only — not wired into any routing, config, or handler flow. See Follow-up Research #2.

## Detailed Findings

### 1. Endpoint Lifecycle — Full Trace

The endpoint flows through four stages:

| Stage | Location | What Happens |
|-------|----------|-------------|
| **Source** | `hardcoded_routing()` (`intent_classifier.rs:297-351`) or `load_routing_from_file()` (`intent_classifier.rs:434-485`) | `RouteEntry.endpoint` is populated from `NVIDIA_ENDPOINT` env var (default: `https://integrate.api.nvidia.com/v1/chat/completions`) or from `routing.toml` `[category].endpoint` field |
| **Storage** | `RegexClassifier.routing: HashMap<String, RouteEntry>` (`intent_classifier.rs:103`) and `AppState.routing: Arc<HashMap<String, RouteEntry>>` (`main.rs:32`) | The routing map is merged from all classifier backends at startup; also used for X-Cerebrum-Category header bypass |
| **Classification** | `route_match()` (`intent_classifier.rs:646-655`) and `route_fallback()` (`intent_classifier.rs:658-667`) | `endpoint: route.endpoint.clone()` — copied verbatim from `RouteEntry` into `ClassificationResult` |
| **Consumption** | `completion_handler` (`main.rs:404-405`) | `client.post(&classification.endpoint)` — used as the full upstream URL. **This is the single call site.** |

The `ClassificationResult::fallback()` at `intent_classifier.rs:515-524` always returns `endpoint: String::new()`, triggering the 502 "no endpoint configured" guard at `main.rs:360`.

### 2. Provider Type Inventory — Every String in the Codebase

**Canonical provider types (in `auth_headers_for` match at `intent_classifier.rs:358`):**

| provider_type | auth_headers_for arm | provider_path would return |
|---|---|---|
| `"openai_compatible"` | `Authorization: Bearer <key>` | `"/v1/chat/completions"` |
| `""` (empty/default) | Same as `openai_compatible` | `"/v1/chat/completions"` |
| `"nvidia_nim"` | Wildcard → `Bearer <key>` | `"/v1/chat/completions"` (NVIDIA NIM is OpenAI-compatible) |
| `"anthropic"` | `x-api-key: <key>` | `"/v1/messages"` |
| `"ollama"` | No auth headers | `"/v1/chat/completions"` |
| `"local"` | No auth headers | `"/v1/chat/completions"` |
| Unknown (wildcard) | Wildcard → `Bearer <key>` | `"/v1/chat/completions"` |

**Where each appears in `RouteEntry` assignments:**

- **`"nvidia_nim"`**: `hardcoded_routing()` all 5 entries (`intent_classifier.rs:303-349`), 3 of 4 TOML examples (`routing-nvidia-nim.toml`, `routing-manual-tests.toml`, `routing_unreachable.toml`)
- **`"openai_compatible"`**: 1 TOML example (`routing-openrouter.toml`), several test fixtures in `main.rs` (lines 1351, 1361, 1398, 1408, 2112)
- **`""` (empty)**: TOML default when `provider_type` field is missing (`intent_classifier.rs:465`), all `test_classifier()` fixtures (`intent_classifier.rs:680-722`), most fallback entries

**Important**: The plan correctly lists the match arms for `provider_path()`, but `"nvidia_nim"` was only implicitly handled (it falls through to the existing wildcard/fallback arm). Since `hardcoded_routing()` uses `"nvidia_nim"` as the sole provider type for all defaults, this should be explicit for clarity.

### 3. RouteEntry Creation Points — All Need to Be Compatible

| Creation Point | File:Line | Current Endpoint Format | Change Impact |
|---|---|---|---|
| `hardcoded_routing()` | `intent_classifier.rs:298-301` | Full URL from `NVIDIA_ENDPOINT` or default `https://integrate.api.nvidia.com/v1/chat/completions` | Default changes to base URL `https://integrate.api.nvidia.com` |
| `load_routing_from_file()` | `intent_classifier.rs:453-457` | Raw TOML value or `DEFAULT_ENDPOINT` (`""`) | Pass through `strip_provider_suffix()` |
| `load_routing()` fallback default | `intent_classifier.rs:500-506` | `String::new()` | Unchanged (empty = no endpoint) |
| `ClassificationResult::fallback()` | `intent_classifier.rs:515-524` | `String::new()` | Unchanged (empty = no endpoint) |
| Test fixtures (all) | Various | `String::new()` or full URLs | Tests with empty endpoints unchanged; tests with full URLs must be updated |

### 4. TOML Examples — Current State

Four routing example files exist at `routing_examples/`:

| File | Endpoint Format | provider_type | Action Required |
|---|---|---|---|
| `routing-openrouter.toml` | Full URL: `https://openrouter.ai/api/v1/chat/completions` | `openai_compatible` | Change to `https://openrouter.ai/api` (keep `/api` prefix; suffix strips `/v1/chat/completions`) |
| `routing-nvidia-nim.toml` | No `endpoint` field (empty, relies on hardcoded) | `nvidia_nim` | No change needed (empty stays empty) |
| `routing-manual-tests.toml` | Full URL: `https://integrate.api.nvidia.com/v1/chat/completions` | `nvidia_nim` | Stripped at parse time to `https://integrate.api.nvidia.com` |
| `routing_unreachable.toml` | Full URL with unreachable hosts | `nvidia_nim` | Stripped at parse time |

All TOML examples that include `endpoint` will have their suffixes auto-stripped by the parser, so no manual updates are strictly required for backward compatibility. However, the plan correctly notes that examples should show the recommended base-URL format.

### 5. Suffix Stripping — Edge Cases Validated

The only two distinct path suffixes in the current provider vocabulary are:
- `"/v1/chat/completions"` (openai_compatible, nvidia_nim, ollama, local, empty, unknown)
- `"/v1/messages"` (anthropic)

These are distinct enough that even naive suffix matching (check `ends_with` first for each) works correctly. No substring conflicts exist (e.g., `"/v1/chat"` is not a suffix of `"/v1/chat/completions"`).

**Edge case — OpenRouter**: OpenRouter's base URL is `https://openrouter.ai/api`, not `https://openrouter.ai`. The `/v1/chat/completions` suffix is appended to form `https://openrouter.ai/api/v1/chat/completions`. After stripping, the base becomes `https://openrouter.ai/api`. The plan correctly notes this.

**Edge case — Custom providers**: If an operator sets `endpoint = "https://custom.api/v1/chat/completions"` with `provider_type = "openai_compatible"`, the suffix is stripped to `"https://custom.api"` and then re-appended — correct, same URL. If they set a non-standard path like `endpoint = "https://custom.api/v2/custom"` with a provider_type whose path is `"/v1/chat/completions"`, the suffix is NOT stripped (no match), so the full URL `https://custom.api/v2/custom/v1/chat/completions` would be composed — which is wrong. The operator must set `endpoint = "https://custom.api/v2/custom"` as a base URL that doesn't match any known suffix. This is documented in the plan's "Open Risks" section.

### 6. Trailing Slash Safety — Confirmed Single Concern

The only trailing slash concern is in `completion_handler` at `main.rs:405`, where `endpoint` (now base URL) is composed with `provider_path()`:

```rust
let upstream_url = format!(
    "{}{}",
    classification.endpoint.trim_end_matches('/'),
    intent_classifier::provider_path(&classification.provider_type)
);
```

The TOML parser already strips trailing slashes from the stored `endpoint` value (per the plan's Phase 1, step 3: `endpoint_raw.trim_end_matches('/')`), so the `trim_end_matches` in the handler is a defense-in-depth measure. No other code path composes endpoint with path.

### 7. Test Impact Assessment

**Tests that construct `RouteEntry` with non-empty endpoints and will need updates:**

| Test / Helper | File:Line | Current Endpoint | Impact |
|---|---|---|---|
| `test_app_with_enriched_classifier()` | `main.rs:864` | `"https://test.endpoint"` | Must change to base URL `"https://test.endpoint"` (no path suffix) — test verifies enriched fields aren't leaked, doesn't exercise upstream path |
| `test_app_with_http_client()` | `main.rs:1349` | `server.url("/v1/chat/completions")` | Server URL includes path; after change, endpoint should be `server.url("")` (just the base), path derived from `provider_type` |
| `test_upstream_skip_classify_via_headers` | `main.rs:1558+` | Uses `test_app_with_http_client()` | Server mock must be updated to match the full URL with derived path |
| `test_upstream_returns_raw_response` | `main.rs:~2040` | Uses `test_app_with_http_client()` | Same |
| `test_upstream_connection_refused_502` | `main.rs:1396` | `"http://127.0.0.1:1/v1/chat/completions"` | Must change to `"http://127.0.0.1:1"` (base only) |
| `test_streaming_keepalive_injected` (slow) | `main.rs:2110` | Dynamic TCP URL with path | Must change to base-only |

**Tests that use empty endpoints and are unaffected:**
- `test_app()` — classifier is None, no routing
- `test_app_with_classifier()` — all endpoints empty (only classification, no proxy)
- All `intent_classifier.rs` tests — `test_classifier()` uses empty endpoints
- All `auth_headers_for_*` tests — independent of endpoint
- All `routes_auth_*` tests — independent of endpoint
- All `persistence_*` tests — independent of endpoint

### 8. Empty Endpoint Behavior — Unchanged

The empty-endpoint check at `main.rs:360-369` returns 502 "no endpoint configured". This guard runs BEFORE the URL composition step in Phase 2. An empty `endpoint` with a non-empty `provider_type` still produces 502 — the system never attempts to construct a URL from an empty base. This is the desired behavior per the plan's decision table: "No implicit default base URLs."

### 9. `ClassificationResult` — No New Fields Needed

The plan correctly decides NOT to add a `provider_path` field to `ClassificationResult`. The path derivation happens at URL-construction time in `completion_handler`, not during classification. This is consistent with the existing pattern: `provider_type` is already a field on `ClassificationResult` but is NOT resolved to auth headers during classification — that also happens in the handler.

## Architecture Insights

### Pattern Consistency with `auth_headers_for`

The `provider_path()` function is the natural sibling of `auth_headers_for()`. Both:
- Are pure functions taking `&str` (provider_type) and returning lookup results
- Use a `match` statement with provider-type arms
- Are placed in `src/intent_classifier.rs`
- Are called once per request in `completion_handler`
- Have a catch-all wildcard arm that returns the openai_compatible default

This consistency is the strongest argument for the design. Future maintainers can add a new provider by adding arms to BOTH functions (or one, if it matches the wildcard).

### Historical Validation

The historical chain that led to this point:
1. **S-01a** (classify-endpoint): Classification was split from proxying — established that classification is pure, stateless, and shareable
2. **S-01b** (reqwest-upstream-routing): `endpoint` field was first consumed in `completion_handler` at line 405
3. **S-01c** (provider-agnostic-config): `provider_type` and `api_key_env` added to `RouteEntry`; `auth_headers_for()` created; TOML format extended
4. **S-01e** (proxy-intent-routing): Final integration; `ClassificationResult` carries endpoint, provider_type, api_key_env through to the handler

Each step added one dimension of provider-awareness. S-08 (this change) completes the pattern by making `provider_type` the source of truth for the URL path as well as auth headers. The architecture is now a full matrix: provider_type → (auth_headers, path_suffix, api_key_env).

## Historical Context (from prior changes)

- `context/archive/2026-06-07-provider-agnostic-config/plan.md` — Introduced `provider_type`, `api_key_env`, and `auth_headers_for()`. The `endpoint` field was always intended to be paired with `provider_type`, but path derivation was deferred.
- `context/archive/2026-06-07-provider-agnostic-config/research.md` — Catalogued 13 providers; recognized clean design would separate base URL from path; identified OpenRouter prefix concern (`/api` in base URL).
- `context/archive/2026-06-07-proxy-intent-routing/research.md` — Established the "fail open, not fail closed" principle for routing (CASUAL fallback), which is preserved by this change.
- `context/archive/2026-06-01-upstream-proxy-routing/research.md` — Lazy key resolution pattern (env var read at request time, not startup) — preserved.
- `context/foundation/roadmap.md` — S-08 recorded as `proposed` with `endpoint URL derivation from provider_type`; prerequisite S-01c is complete.

## Related Research

- `context/archive/2026-06-07-provider-agnostic-config/research.md` — Provider auth matrix and TOML format decisions
- `context/archive/2026-06-01-upstream-proxy-routing/research.md` — Master research covering the original four-change decomposition (S-01a through S-01d)

## Follow-up Research — 2026-06-07T13:55:00+02:00

**Trigger**: User raised concern about fragility of heuristic suffix stripping. Stripping a known path suffix from `endpoint` and re-appending via `provider_path()` can produce wrong URLs when an operator's actual endpoint doesn't match the heuristic assumption (e.g., `endpoint = "https://custom.api/v1/chat/completions"` with `provider_type = "anthropic"` → suffix stripped → `/v1/messages` appended → wrong URL).

**Decision**: Use a new `base_url` field instead of heuristic stripping. When `base_url` is present, compose `base_url + provider_path(provider_type)` in the handler. When absent, fall back to `endpoint` as-is (full backward compatibility).

### New Approach: `base_url` Field

**Struct changes** (2 structs, ~38 test literal sites need `base_url: None`):

| Struct | File:Line | Change |
|--------|-----------|--------|
| `RouteEntry` | `intent_classifier.rs:12` | Add `pub base_url: Option<String>` |
| `ClassificationResult` | `intent_classifier.rs:63` | Add `pub base_url: Option<String>` |

**Constructor sites** that copy `RouteEntry → ClassificationResult` (must propagate `base_url`):

| Site | File:Line |
|------|-----------|
| `route_match()` | `intent_classifier.rs:648` — add `base_url: route.base_url.clone()` |
| `route_fallback()` | `intent_classifier.rs:659` — add `base_url: self.fallback_entry.base_url.clone()` |
| X-Cerebrum bypass | `main.rs:280` — add `base_url: entry.base_url.clone()` |
| `fallback()` | `intent_classifier.rs:516` — add `base_url: None` |
| `load_routing_from_file()` | `intent_classifier.rs:475` — add `base_url` field |
| All test `RouteEntry` literals | Various — add `base_url: None` (or use `#[derive(Default)]`) |

**Handler URL composition** (`main.rs:360-405`):

```rust
fn resolve_endpoint(class: &ClassificationResult) -> String {
    if let Some(ref base_url) = class.base_url {
        format!("{}{}", base_url.trim_end_matches('/'),
                 intent_classifier::provider_path(&class.provider_type))
    } else {
        class.endpoint.clone()
    }
}
```

Used in two places:
1. Line 360: Empty check — `resolve_endpoint(&classification).is_empty()`
2. Line 405: Upstream request — `client.post(&resolve_endpoint(&classification))`

**TOML parser** (`intent_classifier.rs:458`): reads optional `base_url` field:

```rust
let base_url = value.get("base_url").and_then(|v| v.as_str()).map(|s| s.to_string());
```

**Decision rule**: If both `endpoint` and `base_url` are present in a TOML entry, `base_url` takes precedence (handler checks `base_url` first). This is purely handler-side — the TOML parser stores both.

**`provider_path()` function** (new, adjacent to `auth_headers_for` at `intent_classifier.rs:365`):

```rust
pub fn provider_path(provider_type: &str) -> &str {
    match provider_type {
        "anthropic" => "/v1/messages",
        "ollama" | "local" => "/api/chat",
        _ => "/v1/chat/completions",  // openai_compatible, nvidia_nim, empty, unknown
    }
}
```

Note: `"nvidia_nim"` is covered by the wildcard arm (it's OpenAI-compatible). An explicit arm is optional.

### What's Eliminated

- **`strip_provider_suffix()` helper** — no longer needed. Zero heuristic stripping.
- **Risk of wrong URL construction** — operators with unusual endpoints are never affected; they just don't set `base_url`.
- **TOML example churn** — existing `routing.toml` files with full URLs in `endpoint` continue working unchanged. Only new configs opting into `base_url` use the derivation.

### What Stays the Same

- `provider_path()` function — same match arms, same location, same `&'static str` return
- `hardcoded_routing()` default changes — `NVIDIA_ENDPOINT` default becomes `https://integrate.api.nvidia.com` (base URL)
- Handler composes full URL — same pattern, just gated on `base_url.is_some()`
- Empty endpoint → 502 — preserved, now checking the resolved URL

### Updated Test Impact

Tests using `RouteEntry` with non-empty `endpoint` and no `base_url` are **unaffected** — they continue to use `endpoint` as-is (backward compat). Only new tests exercising the `base_url` path need to set `base_url`.

### Suggested Refinements for Plan Phase 1

1. **Drop `strip_provider_suffix` entirely** — replaced by `base_url` gating in the handler
2. **`hardcoded_routing()` uses `base_url` instead of `endpoint`** — sets `base_url: Some(NVIDIA_ENDPOINT)` and `endpoint: String::new()`
3. **`test_app_with_http_client`** — when testing base_url path, set `base_url: Some(server.url(""))` instead of `endpoint: server.url("/v1/chat/completions")`

## Open Questions

### Q1: Should `nvidia_nim` match arm be explicit or wildcard?

The plan's match table for `provider_path()` groups `"nvidia_nim"` with the default fallback arm. Since `hardcoded_routing()` is the production default and uses `"nvidia_nim"`, consider whether it should have an explicit arm for documentation clarity. The wildcard fallback works correctly (NVIDIA NIM's API is OpenAI-compatible), so this is a style choice.

**Recommendation**: Include an explicit `"nvidia_nim"` arm (returning `"/v1/chat/completions"`) for clarity, given it's the production default provider type.

### Q2: Should `strip_provider_suffix` be tested as a unit-testable function?

The plan lists it as a private function with 5 unit tests. Since the test module in `intent_classifier.rs` already has `use super::*`, private functions are accessible in `#[cfg(test)]`. The plan's approach works.

### Q3: What about `routing_examples/routing-manual-tests.toml` — should it be updated?

This file uses full URLs and `provider_type = "nvidia_nim"`. After suffix stripping, the stored endpoint becomes `https://integrate.api.nvidia.com`. The plan says "examples should show base URLs" — this file should be updated to avoid confusion, even though backward-compatible stripping works.

## Follow-up Research #2 — 2026-06-07T14:00:00+02:00

**Trigger**: Scope reduction decision. Adding `base_url` (or any new routing field) introduces configuration complexity (two fields for the same purpose, operator confusion) for marginal benefit at cerebrum's current scale (single provider, 5 routing entries).

**Decision**: Add `provider_path()` as a standalone tested utility function only. Do not wire it into any routing flow. The function exists as a building block — future changes can use it for URL composition, suffix stripping, or config validation if/when multi-provider routing justifies the config complexity.

**What changed from original plan**:
- Dropped: suffix stripping, handler URL composition, `hardcoded_routing()` default change, routing example updates, integration test
- Kept: `provider_path()` function + 7 unit tests, placed after `auth_headers_for`

**Ollama path**: The planned path for ollama/local was `"/v1/chat/completions"` (same as OpenAI). Research found Ollama's actual chat endpoint is `"/api/chat"`. The function uses the correct path.

**No explicit `nvidia_nim` arm**: Since the function isn't wired into routing, there's less need for an explicit arm. `nvidia_nim` is covered by the wildcard `_ → "/v1/chat/completions"` arm. An explicit arm can be added later when the function is wired in.
