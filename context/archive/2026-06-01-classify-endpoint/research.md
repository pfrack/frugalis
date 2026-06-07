---
date: 2026-06-01T00:00:00+02:00
researcher: pfrack
git_commit: 7940421e3d801a63974e0f060b8ad4f39f322853
branch: main
repository: cerebrum
topic: "Separate classification endpoint"
tags: [research, upstream-routing, classify-endpoint, classify, api-route]
status: complete
last_updated: 2026-06-01
last_updated_by: pfrack
---

# Research: Classify Endpoint

Extracted from the master research doc at `context/changes/upstream-proxy-routing/research.md`.

## Feasibility of Separate Classify Endpoint

**This is a trivial change.** Adding `POST /v1/classify` is a ~30-line net addition with zero breaking changes.

**Current router** (`src/main.rs:336-360`):
```
proxy_routes (/v1/*, bearer auth):
  POST /chat/completions  →  completion_handler
```

**After adding classify route:**
```
proxy_routes (/v1/*, bearer auth):
  POST /chat/completions  →  completion_handler  (unchanged)
  POST /classify          →  classify_handler    (NEW)
```

**Why it's safe:**
- **Auth is inherited** — the `.layer(require_proxy_bearer)` on `proxy_routes` applies to ALL routes in the sub-router. New route gets auth for free.
- **No existing test breaks** — all tests hit `/v1/chat/completions`, `/health`, or `/dashboard/*`. Adding a route has no side effects on existing route behavior.
- **One test needs relocation** — `test_completion_handler_returns_classification_json` currently hits `/v1/chat/completions` to test classification. After the split, it should hit `/v1/classify` instead. When `completion_handler` later becomes a routing proxy, that test would break anyway (deferred to Change 4).

## Separability of Classification Logic

**`classify()` is a pure, stateless method** at `src/intent_classificator.rs:456-534`. It takes `&self` (the pre-compiled `RegexSet` + routing table) and a `&str` prompt, returns a `ClassificationResult`. No async, no I/O, no side effects. Any handler with `AppState` can call it.

**`extract_last_user_message` is already a shared utility** at `src/persistence.rs:417-447`. No changes needed.

**The classify handler** is literally the first half of `completion_handler` (`src/main.rs:119-147`), minus the logging block.

## Coupling Points Between Handler and Classifier

`completion_handler` currently couples classification and response assembly:

| Lines | Concern | Extractable? |
|---|---|---|
| 124-130 | Content-Type validation | Shared (both handlers need it) |
| 132 | Timer start | Routing-specific (measures upstream latency) |
| 134-135 | Body parse + prompt extraction | Shared (both handlers need prompt text) |
| 137-139 | Classify call | Classification-specific (moves to classify_handler) |
| 141-146 | Build classification JSON | Classification-specific (moves to classify_handler) |
| 150-172 | Fire-and-forget logging | Routing-specific (logs inference events with upstream model) |

## Integration Points

| File | Change |
|---|---|
| `src/main.rs` | Add `classify_handler` function (~25 lines after line 175). Add `.route("/classify", post(classify_handler))` in `build_app` (at line 338). |
| `openapi/completions.yaml` | Add `POST /v1/classify` path alongside existing `/v1/chat/completions`. |

**No changes needed**: `Cargo.toml`, `src/intent_classificator.rs`, `src/auth.rs`, `src/persistence.rs`

## Open Questions

1. **Should the classify endpoint log?** (Resolved in planning: Yes, lightweight record with `status = "classified"`)
2. **Should the classify endpoint return provider info?** (Resolved: No — classification metadata only)
