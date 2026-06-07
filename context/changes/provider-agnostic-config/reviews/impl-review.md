<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Provider-Agnostic Routing Configuration

- **Plan**: context/changes/provider-agnostic-config/plan.md
- **Scope**: All 4 phases
- **Date**: 2026-06-07
- **Verdict**: REJECTED (1 critical, 1 warning pattern, 1 warning plan drift, 1 observation)
- **Findings**: 1 critical, 2 warnings, 1 observation

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | FAIL ❌ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | FAIL ❌ |
| Architecture | WARNING ⚠️ |
| Pattern Consistency | WARNING ⚠️ |
| Success Criteria | FAIL ❌ |

**► Overall: REJECTED**

---

## Findings

### F1 — Panic on non-object request body

```
Severity: ❌ CRITICAL
Impact:    🔬 HIGH — causes immediate service crash; architectural stakes
Dimension: Safety & Quality
Location:  src/main.rs:365
```

**Detail**:
`completion_handler` parses the request body as JSON and then unconditionally executes `req_body["model"] = ...`. If the JSON body is valid but not an object (e.g., an array or string), this index assignment panics, leading to a crash and denial of service.

**Fix**:
```rust
// Replace the direct assignment with:
if let serde_json::Value::Object(map) = &mut req_body {
    map.insert("model".to_string(), serde_json::Value::String(classification.model.clone()));
} else {
    return json_response(
        StatusCode::BAD_REQUEST,
        r#"{"error":"bad_request","message":"request body must be a JSON object"}"#.to_string(),
    );
}
```

**Decision**: FIXED (applied)

---

### F2 — Phase 3 Handler Enrichment not delivered

```
Severity:  ⚠️ WARNING
Impact:    🔬 HIGH — architectural stakes; think carefully before deciding
Dimension: Plan Adherence
Location:  N/A (plan vs. current behavior)
```

**Detail**:
The plan specifies that `POST /v1/chat/completions` should return enriched JSON containing `status, category, model, tier, endpoint, provider_type, api_key`. The current implementation either proxies the request upstream or returns minimal JSON (status, category, model, tier). Enriched fields are never present. This is a major drift from the original intent, and the Phase 3 manual verification items cannot be performed.

**Fix Options**:

1. ⭐ **Recommended**: Accept the current upstream-proxying behavior as the de facto standard and update the plan to reflect it. This acknowledges that Change 2 (reqwest-upstream-routing) and Change 4 (SSE streaming) changed the architectural direction, and the enriched response is no longer needed.
   - Strength: Aligns documentation with reality; avoids unnecessary rework.
   - Tradeoff: Original design goals are abandoned; stakeholders must be notified.
   - Confidence: HIGH — the upstream proxy is the north star (S-01).
   - Blind spot: None significant.

2. **Revert** to enriched JSON response and remove upstream proxying from `completion_handler`. This would undo significant work and likely break the S-01 slice.
   - Strength: Strict adherence to the original plan.
   - Tradeoff: Large regression; upstream proxying would need to be re-added later.
   - Confidence: LOW — contradicts the current architecture.
   - Blind spot: Unknown impact on downstream consumers.

**Decision**: A (Accept drift) — plan updated with implementation note documenting the deviation.

---

### F3 — Missing tests for `auth_headers_for`

```
Severity:  ⚠️ WARNING
Impact:    🏃 LOW — quick decision; fix is obvious and narrowly scoped
Dimension: Pattern Consistency
Location:  src/intent_classificator.rs:344-355
```

**Detail**:
The public function `auth_headers_for` is not covered by unit tests. In reference modules (`auth.rs`, `persistence.rs`), all public functions have direct test coverage. This creates a verification gap for header generation logic.

**Fix**: Added unit tests covering each provider type (`openai_compatible`, `anthropic`, `ollama`, `local`, unknown) and empty provider default.

**Decision**: FIXED (tests added)

---

### F4 — Silent defaults in TOML parsing

```
Severity:  OBSERVATION
Impact:    🏃 LOW — obvious improvement, easy to add
Dimension: Reliability
Location:  src/intent_classificator.rs:424-459
```

**Detail**:
When parsing `routing.toml`, missing fields (`model`, `provider_type`, `api_key_env`) are silently defaulted without any warning log, making configuration debugging harder.

**Fix**: Added `warn!` logs when falling back to environment defaults, indicating the category and field name.

**Decision**: FIXED (warnings added)

---

## Summary

- **Fixed**: F1 (panic fix), F3 (tests), F4 (warnings)
- **Plan updated**: F2 (documented deviation)
- **Verdict**: REJECTED due to critical panic (F1) and significant plan drift (F2). The code now crashes on malformed non-object JSON bodies; fix applied but verification needed. The enriched response requirement was abandoned; plan updated accordingly.
