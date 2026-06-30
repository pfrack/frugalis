<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Proxy Translation Contract Tests

- **Plan**: `context/changes/testing-proxy-translation-contracts/plan.md`
- **Scope**: Phase 1 + Phase 2 of 3 (commit d3a347e)
- **Date**: 2026-06-30
- **Verdict**: APPROVED (all findings resolved)
- **Findings**: 0 critical  2 warnings  5 observations
- **Final commits**:
  - `894681a` — Revert plan Progress 3.1–3.16 to pending (F1)
  - `24b6f43` — Add proper SHAs to Phase 1 + reverted Phase 3 items (F1 follow-up)
  - `7c0b362` — Rename test to match plan 2.5 contract (F2)
  - `ac865f6` — Refactor harnesses + env-var isolation + body helper (F3, F4, F5)

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS (after F1+F2 fixes) — was WARNING pre-triage |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS — `cargo test`: 439 passed |

## Findings

### F1 — Plan Progress section falsely marks Phase 3 as done

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: `context/changes/testing-proxy-translation-contracts/plan.md:482-497` (pre-fix)
- **Detail**: Plan checked off Phase 3.1–3.16 as `[x]` completed in commit d3a347e, but `git show d3a347e` confirmed only Phase 1 + Phase 2 landed. Phase 3 source (12 tests in `responses_handler.rs`, 4 in `handlers.rs` for nvidia_nim/ollama) lived only as uncommitted working-tree changes (`git status` showed `M src/proxy/handlers.rs` +165, `M src/proxy/responses_handler.rs` +171).
- **Fix A ⭐ Recommended** (chosen): Revert plan Progress 3.1–3.16 to `- [ ]` now; Phase 3 lands in a separate commit, then check items off.
- **Decision**: FIXED via Fix A
  - `894681a`: reverted plan Progress 3.1–3.16 to `- [ ]`; updated `change.md` `last_updated_note`.
  - `24b6f43`: appended `— d3a347e` to Phase 1.1–1.3 (lacked SHA annotation per repo convention); appended `— reverted by 894681a (was falsely checked off in d3a347e)` to Phase 3.1–3.16 so the revert commit is traceable per item.
  - Phase 3 source remains as uncommitted working-tree changes; will be checked off when it lands.

### F2 — 2.5 deviation: extended existing test instead of new test

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: `src/proxy/handlers.rs:3284` (pre-fix)
- **Detail**: Plan 2.5 contract called for a NEW test `test_messages_handler_openai_translation_buffered`. Implementation instead extended the existing `test_messages_handler_openai_translation_non_streaming`. All required assertions present (type, role, content[], usage.input_tokens, usage.output_tokens, stop_reason, field-leak guards for object/choices). Test name no longer matched the plan's intent.
- **Fix A** (chosen): Rename the extended test to `test_messages_handler_openai_translation_buffered` to match the plan.
- **Decision**: FIXED via Fix A
  - `7c0b362`: renamed test in `src/proxy/handlers.rs:3284`.
  - Verification: `cargo test test_messages_handler_openai_translation` → 5 passed.

### F3 — Harness boilerplate duplication (Observation)

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — out of scope
- **Detail**: `test_app_with_nim_http_client` and `test_app_with_ollama_http_client` are ~99% identical, differing only in the `provider_type` string. Matches the established duplication pattern across all existing harnesses; refactor would touch every existing harness.
- **Decision**: FIXED via `ac865f6`
  - Introduced `test_app_with_provider(env_var_name, max_upstream_body_bytes, endpoint_path, provider_type)` helper in `src/app/test_helpers.rs`.
  - Reduced the 5 plain harnesses (http_client, anthropic, nim, ollama, openai_responses) to one-line wrappers around the helper.
  - `test_app_with_cache` left alone (different return shape + response_cache).
  - Eliminated ~280 lines of near-identical boilerplate while preserving all public signatures.

### F4 — Inconsistent env-var isolation on no-set tests (Observation)

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — provably safe
- **Detail**: `test_classify_no_enriched_fields` and `test_completion_no_enriched_fields_with_missing_env` reference `TEST_API_KEY` / `MISSING_KEY_XYZ` but lack `#[serial]`/`EnvGuard`. Risk is moot because both go through `test_app_with_enriched_classifier` with `http_client: None` and return before reaching the env-var branch.
- **Decision**: FIXED via `ac865f6`
  - `test_classify_no_enriched_fields`: added `#[serial]` + `EnvGuard("TEST_API_KEY")` + `set_var("TEST_API_KEY", "sk-test-value-123")` (mirrors sibling `test_completion_does_not_include_enriched_fields`).
  - `test_completion_no_enriched_fields_with_missing_env`: added `#[serial]` + `EnvGuard("MISSING_KEY_XYZ")` + `remove_var("MISSING_KEY_XYZ")` (guarantees absence for the duration of the test).

### F5 — Inline `to_bytes(...).unwrap()` in SSE tests (Observation)

- **Severity**: OBSERVATION
- **Impact**: 🏃 LOW — established pattern
- **Detail**: New SSE tests use `to_bytes(...).unwrap()` directly instead of `parse_json_body`. Justified: SSE bodies are not JSON; `parse_json_body` would panic on `serde_json::from_slice`. Matches existing SSE tests in the file.
- **Decision**: FIXED via `ac865f6`
  - Added `body_to_string(response) -> String` helper in `src/app/test_helpers.rs` next to `parse_json_body` for SSE / text / non-JSON bodies.
  - Migrated the 2 inline `to_bytes + serde_json::from_slice` call sites in new Phase 2 tests to use the existing `parse_json_body` helper (those tests assert on JSON shape).
  - The new `body_to_string` helper is available for future SSE test sites that pre-date this commit.

### F6 — Phase 1 harnesses mirror `test_app_with_anthropic_http_client` exactly (Observation, positive)

- **Severity**: OBSERVATION
- **Impact**: n/a
- **Detail**: New harnesses (`test_app_with_nim_http_client`, `test_app_with_ollama_http_client`) follow `test_app_with_anthropic_http_client` exactly in AppState construction, auth config, httpmock setup, and routing entry shape. Provider type strings correct.
- **Decision**: SUPERSEDED by F3 — the 5 plain harnesses (including these) were collapsed into one-line wrappers around `test_app_with_provider`, which guarantees the same body across providers by construction. The positive finding holds structurally.

### F7 — Phase 2 tests use `parse_json_body` consistently (Observation, positive)

- **Severity**: OBSERVATION
- **Impact**: n/a
- **Detail**: All new JSON-shape assertions use `parse_json_body`. Field-leak guards present in both translation directions (no Anthropic-only fields in O→A output; no OpenAI-only fields in A→O output).
- **Decision**: ACCEPTED — compliance verification (positive). Strengthened by F5 migration which reduced 2 inline call sites to use `parse_json_body` as well.

## Verification Evidence

- `cargo test --no-fail-fast` → **439 passed (1 suite, 10.74s)** — pre-fix
- `cargo test --no-fail-fast test_messages_handler_openai_translation` → **5 passed** — post-fix (after `7c0b362` rename)
- `git show --stat d3a347e` → confirmed scope: `src/app/test_helpers.rs` +146, `src/proxy/handlers.rs` +230
- `git status` (pre-fix) → confirmed `M src/proxy/handlers.rs` +165, `M src/proxy/responses_handler.rs` +171 as uncommitted Phase 3 work

## Triage Outcome

- **Fixed**: F1 (commits `894681a` + `24b6f43`), F2 (commit `7c0b362`), F3 (commit `ac865f6`), F4 (commit `ac865f6`), F5 (commit `ac865f6`)
- **Accepted**: F7 (positive compliance verification)
- **Superseded**: F6 (structural guarantee now comes from F3's helper)
- **Skipped**: none
- **Dismissed**: none

## Final Verdict

**APPROVED** — all findings resolved. Phase 1 + Phase 2 work in commit d3a347e is sound. Phase 3 work remains as uncommitted working-tree changes and is the subject of a separate follow-up commit (see `follow-ups/review-fixes.md`).
