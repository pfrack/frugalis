<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Config Format Upgrade (Multi-Format + External Patterns)

- **Plan**: context/changes/config-format-upgrade/plan.md
- **Scope**: All 4 Phases (Automated completion = all [x] in Progress)
- **Date**: 2026-06-12
- **Verdict**: APPROVED
- **Findings**: 0 critical, 1 warning, 2 observations (all triaged and fixed)

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS ✅ |
| Scope Discipline | PASS ✅ |
| Safety & Quality | PASS ✅ |
| Architecture | PASS ✅ |
| Pattern Consistency | PASS ✅ |
| Success Criteria | PASS ✅ |

## Overall

✅ **APPROVED** — All phases implemented as planned, tests pass (171/171), all 3 triage findings resolved.

---

## Findings (Previous Review)

### F1 (prev) — TOML table scoping nuance for `patterns_dir`

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW
- **Decision**: SKIPPED (documentation note, no code change)

---

## Findings (This Review)

### F1 — Path traversal in external pattern file loading

**Decision**: FIXED

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:574
- **Fix applied**: Added `canonicalize()` on resolved path and prefix check against base_dir to prevent path traversal.
- **Change**: `base_dir.join(path)` → canonicalize both paths, verify `full_path.starts_with(&base_dir)`.

### F2 — `#[serde(rename_all = "snake_case")]` not explicitly present

**Decision**: FIXED

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Plan Adherence
- **Location**: src/config.rs:852
- **Fix applied**: Added `#[serde(rename_all = "snake_case")]` to ConfigRoot struct.

### F3 — `patterns_dir` typed as `Option<String>` not `Option<PathBuf>`

**Decision**: FIXED

- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW
- **Dimension**: Plan Adherence
- **Location**: src/config.rs:873
- **Fix applied**: Changed field type from `Option<String>` to `Option<PathBuf>`, updated all usages.

---

## Supporting Evidence

- **Automated tests**: 171 passed, 0 failed (`cargo test --release`)
- **`--validate` flag**: Exits 0 on valid config, exits 2 on unknown flags
- **Integration tests** (`manual-test/run.sh --auto`): 51/51 passed

---

## Notes

- Plan variation: `serde-saphyr` → `serde_yaml` (acceptable, more common)
- Pre-existing HTTP header injection at `intent_classifier.rs:413` (not introduced by this change)

---

**Recommendation**: Change already marked `impl_reviewed`. Fixes applied successfully.
