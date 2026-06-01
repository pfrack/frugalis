# Intent Classification — Plan Brief

> Full plan: `context/changes/proxy-intent-routing/plan.md`
> Research: `context/changes/proxy-intent-routing/research.md`

## What & Why

Add regex-based intent classification to the Cerebrum proxy gateway. Each POST to `/v1/chat/completions` is classified into one of 4 categories (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) using ~45 compiled regex patterns with weighted scoring. Classification populates the `InferenceRecord` so results appear in the dashboard. This is the core of S-01 — the north-star slice that validates intent-aware triage. Upstream model proxying is deferred to a future change.

## Starting Point

All three foundations (F-01 auth, F-02 persistence, F-03 dashboard) are implemented. `completion_handler` at `src/main.rs:87` receives request bodies, extracts a snippet, builds an `InferenceRecord` with `category: None` and `upstream_model: None`, and logs fire-and-forget. `InferenceRecord` already has the right fields. The dashboard already renders them. The handler returns a static placeholder string — classification is the missing piece.

## Desired End State

A POST to `/v1/chat/completions` with `{"messages":[{"role":"user","content":"fix this bug"}]}` returns:
```json
{"status":"classified","category":"SYNTAX_FIX","model":"gpt-4o-mini","tier":"Regex"}
```
The inference log in the dashboard shows the category badge and model name populated (no longer `—`). If the routing config file is missing or broken, the gateway warns at startup and defaults all requests to CASUAL — never blocks traffic.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Classification scope | Regex-only, no ONNX | Fastest path to working classification; ~140 lines, zero system deps, <50µs latency. | Research |
| Routing config format | `routing.toml` (with TOML crate, `toml::Value` API) | Shape-notes specified static config; TOML is the lightest file option (1 new dep, no serde derive). | Plan |
| Classifier availability | Optional — degrade to CASUAL | Gateway must function without a routing file; matches persistence's warn+None pattern. | Plan |
| Handler response format | JSON with classification metadata | Makes classification observable without proxying; verifiable via curl. | Plan |
| Full prompt extraction | New `extract_prompt_text()` in classifier module | `extract_snippet()` truncates at 200 chars; classifier needs untruncated text for accurate keyword matching. | Plan |
| Regex engine | `regex::RegexSet` | Compiles ~50 patterns once at startup, matches in ~10-50µs; no async overhead. | Research |
| Test depth | Unit tests for classify() + existing handler auth tests | Classification logic self-contained; handler auth contract unchanged. | Plan |

## Scope

**In scope:**
- New `src/intent_classificator.rs` module (~120 lines)
- `regex` and `toml` dependencies
- 45+ regex patterns across 4 categories with weighted scoring and negative suppression
- `routing.toml` config file with hardcoded fallback
- `AppState` integration (7 change sites in `src/main.rs`)
- Handler returns JSON classification metadata
- Unit tests for classification accuracy

**Out of scope:**
- Upstream model proxying (reqwest, SSE streaming, OpenRouter API calls)
- ONNX model inference (Tier 2 fallback)
- Any changes to `persistence.rs`, `auth.rs`, `migrations/`, or `templates/`
- New API endpoints or OpenAPI spec

## Architecture / Approach

```
POST /v1/chat/completions
    │
    ▼
[auth middleware — unchanged]
    │
    ▼
completion_handler
    ├── body: Bytes (unchanged)
    ├── extract_prompt_text(&body) → full last user message ← NEW
    ├── classifier.classify(&prompt) → ClassificationResult ← NEW
    │       ├── RegexSet::matches(sanitized_prompt)
    │       ├── tally weights per category
    │       ├── apply negative suppression
    │       └── resolve or fallback to CASUAL
    ├── build InferenceRecord with category + model ← MODIFIED
    ├── log_inference fire-and-forget ← unchanged
    └── return 200 JSON {status, category, model, tier} ← NEW
```

**Dependencies added**: `regex = "1"`, `toml = "0.8"`

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. New Module Scaffolding | `intent_classificator.rs` with patterns, scoring, TOML loading, prompt extraction | RegexSet compilation failure at startup (handled: warn + fallback) |
| 2. Wire into Handler | AppState field, main() init, handler modification, JSON response | Handler return type change breaks existing tests (mitigated: all auth tests still pass — they check status codes, not body) |
| 3. Tests | Unit tests for classify(), integration test through handler | Test classifier needs `from_values` constructor to avoid TOML file dependency |

**Prerequisites:** None — foundations F-01, F-02, F-03 complete.
**Estimated effort:** ~1 session across 3 phases (140 lines new code + 30 modified).

## Open Risks & Assumptions

- The ~45 regex patterns are derived from analysis of agent coding prompts — field accuracy may differ. The `Regex` tier field in the response makes this observable; tune patterns based on real traffic.
- `routing.toml` must exist at startup or the gateway degrades to CASUAL for all traffic. The WARN log is the only indicator — no dashboard alert for this state yet.
- The `toml` crate v0.8 is pure Rust with no system deps — Render native runtime compatibility is assured.
- Handler return type changes from `(StatusCode, &'static str)` to `(StatusCode, String)`. Any external caller depending on the exact placeholder string `"proxy route is protected"` will break (unlikely — this is a stub that was never meant for consumption).

## Success Criteria (Summary)

- Gateway starts and classifies prompts by category visible in dashboard logs
- curl POST returns JSON with correct category for known prompt patterns
- Gateway starts and functions without `routing.toml` (CASUAL fallback)
- All existing auth and route tests pass with zero regressions
