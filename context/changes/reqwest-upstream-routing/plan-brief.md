# reqwest-upstream-routing — Plan Brief

> Full plan: `context/changes/reqwest-upstream-routing/plan.md`
> Research: `context/changes/reqwest-upstream-routing/research.md`

## What & Why

Add upstream HTTP routing to `POST /v1/chat/completions`. After classification, the handler forwards the request body to the upstream model API, collects the response, and returns it to the client. This delivers FR-003 (routing) — the classification checkpoint is done, and actual model proxying is the next step toward the S-01 north star.

## Starting Point

`completion_handler` at `src/main.rs:197-228` classifies the prompt and returns a synthetic `{"status":"classified",...}` JSON. The `classification.endpoint`, `classification.api_key_env`, and `classification.provider_type` fields are all populated by the classifier but never consumed by the handler. `auth_headers_for()` at `intent_classificator.rs:278-289` exists but has no call site. `reqwest` is absent from the dependency tree.

## Desired End State

A single `reqwest` client (300s timeout, shared via `AppState`) powers upstream calls from `completion_handler`. The handler classifies, resolves the API key from the env var named by `classification.api_key_env`, builds auth headers via `auth_headers_for()`, overrides the `model` field to the classified model, forwards to `classification.endpoint`, and returns the buffered response with `Content-Type: application/json`. When no key or endpoint is configured, the handler degrades to classification JSON (current behavior). Upstream errors are wrapped in a structured Cerebrum error envelope. `X-Cerebrum-Category`/`X-Cerebrum-Model` headers skip re-classification.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| API key source | Per-entry `api_key_env` from routing config | `RouteEntry` already carries `api_key_env` and `provider_type`; `auth_headers_for()` already handles provider mapping — no new env var needed. | Plan |
| Model override | Deserialize body, swap `model` field | Ensures the upstream model matches routing config — the point of intent-based routing. | Plan |
| http_client optionality | `Option<reqwest::Client>` | Tests work without a client; production always has one; degradation path preserves existing test behavior. | Plan |
| Upstream timeout | 300s (5 minutes) | Generous enough for complex model completions; consistent with the SSE streaming change's read timeout. | Plan |
| Upstream error handling | Wrap in Cerebrum envelope, keep status | Consistent error format across all gateway responses; matches the 502 format for missing endpoint. | Plan |
| Response headers | Forward Content-Type only | Minimal surface; client gets JSON MIME type without leaking internal/upstream headers. | Plan |
| Test strategy | `httpmock` crate | Real HTTP over localhost tests actual reqwest behavior; no port conflicts with `0.0.0.0:0` binding. | Plan |
| Skip-classify headers | `X-Cerebrum-Category` + `X-Cerebrum-Model` | Clients that pre-classify via `/v1/classify` can skip re-classification; both headers must be present. | Research |

## Scope

**In scope:**
- `reqwest` dependency + `httpmock` dev-dependency
- `http_client: Option<reqwest::Client>` in `AppState`, built in `main()` with 300s timeout
- `completion_handler` rewrite: classify → resolve key → build auth → override model → forward → return
- X-Cerebrum skip-classify header support
- Structured Cerebrum error envelope for upstream failures
- 4 new `httpmock`-based tests
- `openapi/completions.yaml` schema update

**Out of scope:**
- SSE streaming (Change 4)
- Multi-provider configuration tooling (Change 3)
- Retry logic, circuit breaking, request hedging
- Modifications to `classify_handler`, `classify_and_log`, `persistence.rs`, `auth.rs`, or `intent_classificator.rs`

## Architecture / Approach

```
Client → POST /v1/chat/completions
  ├─ Check X-Cerebrum-Category / X-Cerebrum-Model headers (skip classify)
  ├─ Classify prompt → ClassificationResult (category, model, endpoint, api_key_env, provider_type)
  ├─ Resolve API key from std::env::var(api_key_env) — degrade to JSON if absent
  ├─ Override body.model → classification.model
  ├─ Build auth headers via auth_headers_for(provider_type, key)
  ├─ POST to classification.endpoint via reqwest (300s timeout)
  ├─ On error → Cerebrum envelope {"error":"upstream_error","status":<n>,"message":"..."}
  └─ Return buffered response + Content-Type: application/json
```

`AppState.http_client` is `Option<reqwest::Client>`. When `None`, the handler degrades to classification JSON (test path). In production, `main()` always builds the client. The gate for upstream calls is per-request: is `api_key_env` set and resolvable?

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Dependencies & State | Cargo.toml changes, AppState field, client build, test helper updates | Compilation failure if reqwest feature flags are wrong |
| 2. Handler Rewrite | Upstream routing in completion_handler with model override, error handling, skip-classify | Handler signature change (-> impl IntoResponse) breaks existing test expectations |
| 3. Tests | 4 new httpmock-based tests, existing test compatibility | Port conflicts between concurrent httpmock servers |
| 4. OpenAPI Spec | Updated completions.yaml | None — documentation-only |

**Prerequisites:** `classify-endpoint` change (Change 1) must be complete — `RouteEntry.api_key_env` and `RouteEntry.provider_type` fields are already live on main.
**Estimated effort:** ~1 session across 4 phases

## Open Risks & Assumptions

- **Model override requires valid JSON body**: If the client sends malformed JSON, the deserialize step fails. The handler should return 400 in this case rather than 500.
- **`reqwest` 0.12 TLS**: The `rustls-tls` feature uses rustls, not native-tls. This avoids OpenSSL linking issues on Render but means no system CA bundle. rustls uses `webpki-roots` by default.
- **`impl IntoResponse` return type**: Changing from `(StatusCode, String)` to `impl IntoResponse` or a concrete response type may require adapting test assertions that currently destructure the tuple.
- **httpmock port binding**: Tests using `MockServer::start()` on `0.0.0.0:0` should not conflict, but CI environments with aggressive port scanning could theoretically interfere.

## Success Criteria (Summary)

- `cargo test` passes all existing + 4 new upstream routing tests
- Sending a real request with a valid upstream key and routing config returns actual model output (not classification JSON)
- Degradation path returns classification JSON when no key is configured (backwards-compatible)
- Upstream 4xx/5xx are wrapped in the Cerebrum error envelope with the correct status code
