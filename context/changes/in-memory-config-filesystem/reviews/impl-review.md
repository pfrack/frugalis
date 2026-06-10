<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: In-Memory Config Filesystem

- **Plan**: context/changes/in-memory-config-filesystem/plan.md
- **Scope**: Phases 1-4 of 6
- **Date**: 2026-06-09
- **Verdict**: NEEDS ATTENTION (all findings resolved during triage)
- **Findings**: 1 critical  7 warnings  0 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | FAIL → PASS (all DRIFT/MISSING fixed) |
| Scope Discipline | PASS |
| Safety & Quality | FAIL → PASS (merge bug fixed) |
| Architecture | PASS |
| Pattern Consistency | WARNING → PASS (dead code removed) |
| Success Criteria | WARNING → PASS (truncation test fixed) |

## Findings

### F1 — merge_toml_values nested table merge is a no-op

- **Severity**: ❌ CRITICAL
- **Impact**: 🔬 HIGH — architectural stakes
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:205-209
- **Detail**: Clone of base_nested was merged into but result discarded.
- **Fix**: Pass mutable reference directly via `ref mut` pattern.
- **Decision**: FIXED

### F2 — Phase 4 startup wiring incomplete

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes
- **Dimension**: Plan Adherence
- **Location**: src/main.rs:121-221
- **Detail**: 6 config values read from env vars/hardcoded instead of TOML.
- **Fix**: Complete Phase 4 wiring — HttpConfig, classify_db_log, baseline_model, dashboard_config, auth_providers, port from TOML.
- **Decision**: FIXED via Fix A

### F3 — auth_headers_for still hardcoded match

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM
- **Dimension**: Plan Adherence
- **Location**: src/intent_classifier.rs:502-508
- **Detail**: Old two-parameter signature unchanged, not data-driven.
- **Fix**: Replace with providery-list-based data-driven function.
- **Decision**: FIXED via Fix A

### F4 — Dashboard hardcoded values not replaced

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Plan Adherence
- **Location**: src/dashboard.rs:157,159,194-197,301,303
- **Detail**: Hardcoded 24, 1..720, 20, 100, 5 not using dashboard_config.
- **Fix**: Replace with state.dashboard_config fields.
- **Decision**: FIXED

### F5 — config.toml missing baseline_model and classify_db_log

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Plan Adherence
- **Location**: config.toml
- **Detail**: Top-level keys from plan absent.
- **Fix**: Added keys (resolved during F2 fix).
- **Decision**: FIXED

### F6 — test_max_upstream_body_bytes_truncation regression

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM
- **Dimension**: Success Criteria
- **Location**: src/main.rs:1004
- **Detail**: Test expected BAD_GATEWAY (502) but got 200 after AppState refactoring.
- **Fix**: make_test_app_state() now reads MAX_UPSTREAM_BODY_BYTES from env var.
- **Decision**: FIXED

### F7 — Dead code accumulated

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Pattern Consistency
- **Location**: src/config.rs:382, src/routing.rs:50
- **Detail**: hardcoded_model_default and DEFAULT_MODEL_READING unused after wiring.
- **Fix**: Deleted dead code.
- **Decision**: FIXED

### F8 — Phase 5 not started

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM
- **Dimension**: Plan Adherence
- **Location**: N/A
- **Detail**: PersistenceConfig still reading env vars, HttpClientConfig still defined.
- **Fix**: Implemented Phase 5 — PersistenceConfig accepts DatabaseConfig, HttpClientConfig removed.
- **Decision**: FIXED
