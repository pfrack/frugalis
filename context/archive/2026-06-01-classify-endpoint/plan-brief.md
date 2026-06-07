# Classify Endpoint — Plan Brief

> Full plan: `context/changes/upstream-proxy-routing/plan.md`
> Research: `context/changes/upstream-proxy-routing/research.md`

## What & Why

Add a dedicated `POST /v1/classify` endpoint that decouples intent classification from the proxy handler. Currently, `completion_handler` at `src/main.rs:119` both classifies and returns JSON in one function. A separate endpoint establishes a clean API boundary before the proxy handler becomes a routing proxy (Change 2). This is Change 1 of 4 in the upstream proxy routing sequence.

## Starting Point

`completion_handler` does classification + JSON response in one function. The proxy routes sub-router has only one route (`POST /chat/completions`). Classification logic (`classify()` at `src/intent_classificator.rs:456`) is pure, stateless, and callable from any handler. The OpenAPI spec documents only the chat completions endpoint.

## Desired End State

A `POST /v1/classify` with bearer auth and OpenAI-compatible body returns:
```json
{"status":"classified","category":"SYNTAX_FIX","model":"gpt-4o-mini","tier":"Regex"}
```
A lightweight record with `status = "classified"` is logged to the inferences table. Existing `/v1/chat/completions` behavior is unchanged.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Response format | Classification only (category, model, tier) | Matches existing completion_handler JSON, avoids leaking provider config to clients. | Plan |
| Classify logging | Log lightweight record with status "classified" | Dashboard should show classification volume; status field distinguishes classify from proxy events. | Plan |
| OpenAPI scope | Document both /classify and /chat/completions | lessons.md requires OpenAPI for endpoints; both paths share the same spec file. | Plan |
| Auth model | Bearer auth via existing proxy_routes middleware | Route lives under /v1/, inherits require_proxy_bearer for free. | Research |
| Handler extraction | New classify_handler mirrors completion_handler's classification half | Same Content-Type guard, prompt extraction, classify call — no new patterns. | Research |

## Scope

**In scope:**
- New `classify_handler` function (~30 lines) in `src/main.rs`
- Route registration: `POST /v1/classify` in `build_app`
- OpenAPI spec update: add /classify path, fix 401 response format
- Integration test: `test_classify_handler_returns_classification_json`
- Lightweight persistence logging (status = "classified")

**Out of scope:**
- No changes to `completion_handler` (still returns classification JSON)
- No changes to `Cargo.toml`, `src/intent_classificator.rs`, `src/auth.rs`, `src/persistence.rs`
- No `reqwest`, no upstream HTTP calls, no SSE streaming
- No endpoint/provider info in classify response
- Test for `/v1/chat/completions` classification remains (migrates in Change 4)

## Architecture / Approach

```
POST /v1/classify
    │
    ▼
[auth middleware — inherited from proxy_routes]
    │
    ▼
classify_handler (NEW)
    ├── Validate Content-Type: application/json → 415 if wrong
    ├── extract_last_user_message(&body)
    ├── classifier.classify(&prompt) → ClassificationResult
    ├── Build InferenceRecord { status: "classified", ... }
    ├── log_inference() fire-and-forget (skips if persistence is None)
    └── 200 JSON: {status, category, model, tier}
```

No changes to existing routes. The classify handler follows the same pattern as `completion_handler` — the classification half is fork-identical.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Handler + Route + OpenAPI | classify_handler, route registration, updated API spec | Route name collision (none expected — `/v1/classify` is unused) |
| 2. Tests | Integration test for classify endpoint | Existing test `test_completion_handler_returns_classification_json` must still pass (it hits /chat/completions) |

**Prerequisites:** None — foundations F-01 (auth), F-02 (persistence), and the regex classifier are complete.
**Estimated effort:** ~1 session across 2 phases (~50 lines new code + ~10 modified).

## Open Risks & Assumptions

- The `InferenceRecord.status` field is a free-form `String` — "classified" is a new value. Dashboard templates render all status codes the same way (no status-specific logic). No dashboard change needed.
- classify_handler duplicates Content-Type guard and prompt extraction from completion_handler. DRY refactor is deferred — the duplication is 4 lines and the handlers diverge in Change 2 anyway.
- Test coverage for classify auth is already covered by existing `routes_auth_proxy_requires_valid_bearer_token` which tests `/v1/chat/completions` — the same middleware protects both routes.

## Success Criteria (Summary)

- curl POST to `/v1/classify` returns correct category/model/tier JSON
- Classify endpoint requires auth, rejects wrong content-type
- Dashboard shows classify records with category badges
- All 17 existing tests pass with zero changes
