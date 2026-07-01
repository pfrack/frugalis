<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Persistence + Snippet Guardrails

- **Plan**: context/changes/persistence-snippet-guardrails/plan.md
- **Scope**: All 3 phases (PII guardrails, failure observability, cross-backend identity)
- **Date**: 2026-07-01
- **Verdict**: NEEDS ATTENTION
- **Findings**: 1 critical 2 warnings 5 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | WARNING ⚠️ (1 CRITICAL, 2 WARNING) |
| Architecture | PASS ✅ |
| Pattern Consistency | WARNING ⚠️ (1 finding) |
| Success Criteria | PASS ✅ |

## Findings

### F1 — ReDoS vulnerability in credit card regex

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes; wide blast radius on input processing
- **Dimension**: Safety & Quality
- **Location**: src/proxy/util.rs:324

  Detail:
  The credit card pattern `\b(?:\d[ -]*?){13,19}\b` contains a nested quantifier: `[ -]*?` (lazy star) inside `{13,19}` (quantified group). This creates catastrophic backtracking potential on crafted digit sequences. An attacker sending a long digit string with partial separators can cause the regex engine to explore exponentially many partitionings before failing. Since `redact_pii()` runs on every proxied request body, this is a production Denial-of-Service vector.

  Fix: Replace `[ -]*?` with `[ -]?` — single optional separator per digit, no nested quantifier.
    Strength: Eliminates the backtracking class entirely; one-character change.
    Tradeoff: Slightly narrower pattern (won't match `1234  5678` with double spaces), but that's acceptable for PII redaction purposes.
    Confidence: HIGH — the plan itself acknowledges this exact pattern and proposes `[ -]*?`, but the nested quantifier is a well-known ReDoS vector.
    Blind spot: None significant.
- **Decision**: FIXED — replaced `[ -]*?` with `[ -]?` in src/proxy/util.rs:324

### F2 — SQLite schema divergence risk

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:190-232

  Detail:
  `init_sqlite_schema()` (line 190) contains a raw SQL `CREATE TABLE` statement that duplicates the V1 migration schema. The doc comment says "Keep this DDL in sync" but there's no enforcement mechanism. If future migrations add columns, in-memory tests will pass while production (refinery migrations) may fail — or vice versa.

  Fix A ⭐ Recommended: Extract the DDL into a shared constant or function used by both `init_sqlite_schema()` and the migration runner.
    Strength: Single source of truth; compiler catches divergence.
    Tradeoff: Requires refactoring existing schema code.
    Confidence: HIGH — consistent with the repo's "favor dynamic WHERE clause building" lesson (lessons.md:33).
    Blind spot: Migration files themselves can't easily reference a Rust constant; the constant would need to be written to the migration file at build time or the migration file read at runtime.

  Fix B: Add a test that compares the in-memory schema against the migration file
    Strength: Minimal code change; detects divergence without restructuring.
    Tradeoff: Doesn't prevent it — only catches it after the fact.
    Confidence: MED — adds a regression test but doesn't eliminate the root cause.
    Blind spot: Migration files are SQL strings, not parseable by the test easily.
- **Decision**: FIXED — extracted DDL into `INFERENCES_TABLE_DDL` and `INFERENCES_INDEX_DDL` constants; added `test_schema_constant_matches_migrations` test

### F3 — SQLite in-memory test uses `uuid::Uuid::new_v4()` in URI without `rand` feature

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/sql_backend.rs:67

  Detail:
  The in-memory SQLite URI includes `uuid::Uuid::new_v4()` which is fine since `uuid` is already a dependency with `v4` feature. However, the URI format `sqlite:file:sql_backend_test_{uuid}?mode=memory&cache=shared` relies on SQLite's shared-cache mode for in-memory databases. If the `cache=shared` parameter is dropped or misordered, each connection gets a separate in-memory database and tests will fail silently.

  This is not a bug — it's a pattern consistency observation. The code is correct.

  Fix: None required — marking as OBSERVATION demoted to WARNING due to test fragility.

### F4 — OTel guard test duplicates ~100 lines of fixture setup

- **Severity**: ⚠️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/persistence/mod.rs:688-803

  Detail:
  The `test_otel_no_prompt_body_in_spans` test manually constructs the entire `AppState` instead of using `build_app_with_persistence_backend()`. This duplicates ~100 lines of fixture setup and increases maintenance burden. The plan didn't specify which fixture approach to use.

    Fix: Refactor to use `build_app_with_persistence_backend()` and thread OTel-specific concerns separately.
- **Decision**: FIXED — added `build_app_with_persistence_backend_custom()` with auth_token and api_key_env params; OTel test now uses it

### F5 — `reference_inference_record()` prompt_snippet has no PII

- **Severity**: ⚠️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence/mod.rs:73

  Detail:
  The `reference_inference_record()` helper sets `prompt_snippet: "reference record for cross-backend identity test"` — a plain text snippet with no PII. This is correct for cross-backend identity comparison, but it means the cross-backend tests never exercise PII redaction in the persistence layer. The redaction happens in `enqueue_inference_record` (proxy/util.rs), not in the backends themselves, so this is not a correctness issue — just a gap in test coverage of the redaction → persistence pipeline.

    Fix: Add a separate cross-backend test that inserts a record with PII-containing prompt_snippet and verifies it persists identically across all three backends.
- **Decision**: FIXED — added `test_cross_backend_identity_with_pii_redaction`

### F6 — Unreachable-backend test uses MemoryBackend instead of dead-host SqlBackend

- **Severity**: ⚠️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: src/persistence/mod.rs:443-534

  Detail:
  The plan specified testing an unreachable SQL backend (dead host) to prove error logging. The implementation uses `MemoryBackend.fail_next` injection instead. This is a benign drift — the plan's own "Current State Analysis" identified `MemoryBackend.fail_next` as the existing injection point, and the effect (error log fires + 200 response) is equivalent. The mechanism differs but the observability goal is met.

    Fix: None required — the plan acknowledged this pattern as sufficient.
- **Decision**: SKIPPED — no fix needed

### F7 — Email test regex lacks `(?i)` flag

- **Severity**: ⚠️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/persistence/mod.rs:394

  Detail:
  The `proptest_snippet_free_of_email` test declares its own regex `(?i)[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}` with the `(?i)` flag, but other test regexes (SSN, phone, credit card) are declared inline without `(?i)`. The production `redact_pii` uses `PII_PATTERNS` which has `(?i)` on the email pattern. The test regex does include `(?i)` — so this is actually fine. Marking as minor observation about regex duplication.

    Fix: Extract `PII_PATTERNS` to a shared test-accessible module so tests verify against the same patterns the production code uses.
- **Decision**: FIXED — made `PII_PATTERNS` `pub(crate)` and updated proptest module to use shared patterns

### F8 — `redact_pii` runs 4 regex passes per request

- **Severity**: ⚠️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Performance
- **Location**: src/proxy/util.rs:330-336

  Detail:
  `redact_pii` iterates through all 4 PII patterns for every call, even when the input doesn't contain any PII. The plan's performance section acknowledges this is "< 1ms" and "no measurable impact" — which is reasonable for current throughput. This is a future optimization concern.

    Fix: Consider a fast-path check (e.g., `if !prompt.contains('@') && !prompt.contains('-') { return prompt.to_string(); }`) if profiling shows measurable overhead at scale.
- **Decision**: FIXED — added fast-path: skips regex when input has no `@` and no digits
