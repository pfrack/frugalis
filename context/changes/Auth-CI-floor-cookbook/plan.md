# CI Floor + Auth Guard — Implementation Plan

## Overview

Wire `fmt --check`, `slow_tests`, and a grep-based constant-time-compare guard into both CI workflows (`ci.yml` for PRs, `deploy.yml` for push-to-main). Create a `Makefile` as the single source of truth for CI gate sequences. Make `constant_time_eq_str` `pub(crate)` and add a direct 3-case unit test. Update `test-plan.md` §6.5 with the grep-guard pattern.

## Current State Analysis

**CI workflows:**
- `ci.yml` (PR): lint (clippy) → typecheck (cargo check) → auth tests → persistence tests → build release. Missing: `fmt --check`, `slow_tests`, grep guard.
- `deploy.yml` (push-to-main): auth tests → persistence tests → build release. Missing: lint, typecheck, `fmt --check`, `slow_tests`, grep guard.

**Justfile:** `justfile:188` defines `ci: fmt-check lint-strict test test-slow build-release` — the full gate sequence. Not invoked from CI. Local `just gates` runs `test test-slow lint-strict fmt-check`.

**Auth guard:** `constant_time_eq_str` at `src/routing/auth.rs:169` is a private function called at `:34, :43, :44`. No direct unit test. No automated guard against `==` reversion. The `subtle::ConstantTimeEq` + HMAC-based comparison is correct at all 3 call sites today.

**Risk #7 from test-plan.md:** "Auth constant-time compare regresses — cleanup change reverts `constant_time_eq_str` to `==` on any auth path."

### Key Discoveries:

- `justfile:188` `ci` recipe already defines the exact gate sequence this plan needs — the gap is YAML wiring, not tooling
- `deploy.yml` is missing lint + typecheck that `ci.yml` has — both workflows need to be brought to parity on the new gates
- `constant_time_eq_str` uses HMAC-SHA256 + `subtle::ConstantTimeEq` (`auth.rs:170-184`) — the function is well-implemented but private and untested directly
- No false-positive `==` comparisons against secret variable names exist in current `src/` (verified via grep)
- `make` is pre-installed on GitHub Actions ubuntu-latest; `just` is not — Makefile is the natural CI target

## Desired End State

- Every PR and every push-to-main runs `fmt --check`, `slow_tests`, and an auth grep guard before building
- A future cleanup that reverts `constant_time_eq_str` to `==` in any auth comparison context fails CI within seconds
- `constant_time_eq_str` has a direct unit test proving equal/unequal/different-length behavior
- `Makefile` is the single source of truth for CI gate sequences; `justfile` stays for local dev

### What We're NOT Doing

- Coverage threshold (separate change — needs baseline data per frame)
- Cookbook backfill for §6.1–§6.4 (separate change — orthogonal to CI wiring)
- `justfile` modifications (stays as-is for local dev)
- `src/classification/llm.rs:80` non-CT gap (low-risk, local env only — per frame verdict)
- Installing `just` in CI (Makefile approach chosen instead)

## Implementation Approach

Three phases: (1) create Makefile + guard script as standalone files, (2) wire both CI workflows to use them, (3) make `constant_time_eq_str` testable and add the direct unit test. Each phase is independently verifiable. Phases 1 and 3 have no ordering dependency; Phase 2 depends on Phase 1.

## Phase 1: Makefile + Guard Script

### Overview

Create the Makefile with CI gate targets and the grep-based auth guard script. These are standalone files with no compilation dependency on existing code.

### Changes Required:

#### 1. Makefile

**File**: `Makefile` (new, repo root)

**Intent**: Define `make ci` (PR gate sequence) and `make ci-deploy` (deploy gate sequence) as the canonical targets that both workflows will invoke. Also define individual gate targets for local debugging.

**Contract**: The `ci` target runs: `fmt-check lint-strict test test-slow guard-auth build-release`. The `ci-deploy` target runs: `fmt-check test test-slow guard-auth build-release` (no lint/typecheck — redundant with upstream PR CI). Individual targets: `fmt-check`, `lint-strict`, `test`, `test-slow`, `guard-auth`, `build-release`. The `guard-auth` target runs `bash .github/scripts/guard-auth-compare.sh`.

#### 2. Guard script

**File**: `.github/scripts/guard-auth-compare.sh` (new)

**Intent**: Fail CI if any auth comparison in `src/routing/auth*` files uses `==` on secret-derived strings instead of `constant_time_eq_str`. Protects Risk #7 directly.

**Contract**: The script performs two checks:
1. **Presence check**: `grep -rn 'constant_time_eq_str' src/routing/auth*` must find at least 1 match (confirms the function is still used).
2. **Forbidden-pattern check**: `grep -rn '==' src/routing/auth*` on non-test, non-definition lines — if `==` appears in a line that also references a secret field name (`proxy_api_bearer_token`, `dashboard_basic_user`, `dashboard_basic_password`) or appears in a credential-comparison context, the script exits non-zero with a diagnostic message pointing to the offending line.

The script must exclude: the function definition line (`fn constant_time_eq_str`), test module lines (`#[cfg(test)]`, `mod tests`), and comment lines. Exit 0 on clean; exit 1 with file:line on violation.

### Success Criteria:

#### Automated Verification:

- `make ci` target exists and runs without error (when all gates pass): `make ci`
- `make ci-deploy` target exists and runs without error: `make ci-deploy`
- Guard script is executable: `test -x .github/scripts/guard-auth-compare.sh`
- Guard script passes on current codebase: `bash .github/scripts/guard-auth-compare.sh`
- Guard script fails when `constant_time_eq_str` call is replaced with `==` (manual verification via test branch)

#### Manual Verification:

- `make ci` output shows each gate running in sequence
- Intentionally replacing one `constant_time_eq_str` call with `==` causes `make guard-auth` to fail with a clear diagnostic

---

## Phase 2: CI Workflow Wiring

### Overview

Update both `ci.yml` and `deploy.yml` to invoke the Makefile targets. Replace the existing multi-step test/build sequence with a single `make` invocation per workflow.

### Changes Required:

#### 1. ci.yml

**File**: `.github/workflows/ci.yml`

**Intent**: Replace the current multi-step sequence (lint → typecheck → auth tests → persistence tests → build) with `make ci`, which runs the full gate sequence including the new fmt-check, slow_tests, and guard-auth steps.

**Contract**: The `test` job's steps become: (1) Check out code, (2) Install Rust toolchain, (3) `make ci`. The `SQLX_OFFLINE: "true"` env var is set at the job level (not per-step) since `make ci` passes it through. The persistence test conditional (`if [ -n "$DATABASE_URL" ]`) moves into the Makefile's `test` target or stays as a separate step — whichever preserves the current conditional behavior.

#### 2. deploy.yml

**File**: `.github/workflows/deploy.yml`

**Intent**: Replace the current multi-step sequence (auth tests → persistence tests → build) with `make ci-deploy`, which runs fmt-check → test → test-slow → guard-auth → build-release (no lint/typecheck).

**Contract**: The `test` job's steps become: (1) Check out code, (2) Install Rust toolchain, (3) `make ci-deploy`. The `deploy` job remains unchanged. Same `SQLX_OFFLINE` handling as ci.yml.

### Success Criteria:

#### Automated Verification:

- `ci.yml` contains `make ci` step: `grep -q 'make ci' .github/workflows/ci.yml`
- `deploy.yml` contains `make ci-deploy` step: `grep -q 'make ci-deploy' .github/workflows/deploy.yml`
- `ci.yml` no longer has individual `cargo clippy`, `cargo check`, `cargo test auth`, `cargo build` steps (they're now in the Makefile)
- `deploy.yml` no longer has individual `cargo test`, `cargo build` steps

#### Manual Verification:

- Push a test branch with a trivial change; verify GitHub Actions shows `make ci` running all gates
- Verify the deploy workflow shows `make ci-deploy` running the reduced gate set

---

## Phase 3: Auth Unit Test + Visibility

### Overview

Make `constant_time_eq_str` accessible for direct testing and add a unit test that locks down its contract. Update `test-plan.md` §6.5 with the grep-guard pattern.

### Changes Required:

#### 1. constant_time_eq_str visibility

**File**: `src/routing/auth.rs`

**Intent**: Change `constant_time_eq_str` from `fn` (private) to `pub(crate) fn` so it can be called directly from the test module without going through the `AuthConfig` validation methods.

**Contract**: Line 169 changes from `fn constant_time_eq_str(left: &str, right: &str) -> bool` to `pub(crate) fn constant_time_eq_str(left: &str, right: &str) -> bool`. No other signature changes.

#### 2. Direct unit test

**File**: `src/routing/auth.rs` (in `mod tests` block, after existing tests)

**Intent**: Add a test that directly exercises `constant_time_eq_str` with three cases: (a) equal strings return true, (b) unequal strings of identical length return false, (c) strings of different lengths return false. This locks the function's contract independently of the `AuthConfig` validation methods that currently test it only indirectly.

**Contract**: Test function named `auth_constant_time_eq_str_compares_correctly`. Three assertions:
- `constant_time_eq_str("same", "same")` → `true`
- `constant_time_eq_str("same", "diff")` → `false` (same length, different content)
- `constant_time_eq_str("short", "longer")` → `false` (different lengths)

#### 3. test-plan.md §6.5 update

**File**: `context/foundation/test-plan.md`

**Intent**: Replace the TBD placeholder at §6.5 with a pointer to the grep-guard script and a description of the pattern.

**Contract**: §6.5 changes from "TBD — see §3 Phase 4 for constant-time-compare guard pattern." to a short paragraph describing: (a) the guard script at `.github/scripts/guard-auth-compare.sh`, (b) how it works (grep for `==` in auth files, exclude definition/test lines), (c) how to add a new auth comparison site (add field name to the guard's allowlist). The pattern is documented, not just pointed to.

### Success Criteria:

#### Automated Verification:

- `cargo test auth` passes with the new `constant_time_eq_str` test: `cargo test auth`
- `make guard-auth` passes: `make guard-auth`
- `test-plan.md` §6.5 no longer contains "TBD": `! grep -q 'TBD' context/foundation/test-plan.md` (or check §6.5 specifically)

#### Manual Verification:

- Read the new test and confirm it covers equal, unequal-same-length, and unequal-diff-length cases
- Read §6.5 and confirm it describes the guard pattern clearly enough for a future contributor to add a new auth site

---

## Testing Strategy

### Unit Tests:

- `auth_constant_time_eq_str_compares_correctly` — direct 3-case test on `constant_time_eq_str`
- All existing `auth_*` tests continue to pass (no regression from visibility change)

### Integration Tests:

- `make ci` runs the full gate sequence end-to-end locally
- `make ci-deploy` runs the reduced gate sequence end-to-end locally
- Guard script exits 0 on clean codebase, exits 1 on injected `==` violation

### Manual Testing Steps:

1. Run `make ci` locally — verify all gates pass in sequence
2. Intentionally replace `constant_time_eq_str(token, &self.proxy_api_bearer_token)` with `token == &self.proxy_api_bearer_token` at `auth.rs:34`; run `make guard-auth` — verify it fails with diagnostic
3. Revert the intentional break; push to a test branch; verify GitHub Actions runs `make ci` successfully
4. Verify `cargo test auth` passes with the new direct test

## Performance Considerations

- `make ci` adds `fmt --check` (~1s), `slow_tests` (~30-60s), and `guard-auth` (~1s) to PR CI. Total added: ~30-60s.
- `make ci-deploy` adds the same gates to deploy.yml. Total added: ~30-60s per main push.
- The Makefile uses `.PHONY` targets — no incremental build benefit, but each gate is independently re-runnable.

## Migration Notes

- Existing `ci.yml` and `deploy.yml` steps are replaced wholesale by `make` invocations. No gradual migration — the Makefile subsumes all existing steps.
- The `justfile` is unchanged. Local `just ci` and `just gates` continue to work. The Makefile mirrors the justfile `ci` recipe but adds `guard-auth` as a new gate.
- If a future change adds gates to the justfile `ci` recipe, the Makefile must be updated manually to stay in sync.

## References

- Frame brief: `context/changes/Auth-CI-floor-cookbook/frame.md`
- Test plan Phase 4 row: `context/foundation/test-plan.md:72`
- Risk #7 (constant-time compare): `context/foundation/test-plan.md:48`
- Quality gates: `context/foundation/test-plan.md:109-119`
- Justfile `ci` recipe: `justfile:188`
- Justfile `gates` recipe: `justfile:194`
- Current ci.yml: `.github/workflows/ci.yml:28-56`
- Current deploy.yml: `.github/workflows/deploy.yml:25-46`
- Auth call sites: `src/routing/auth.rs:34,43,44`
- `constant_time_eq_str` definition: `src/routing/auth.rs:169`
- Existing auth tests: `src/routing/auth.rs:222-283`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Makefile + Guard Script

#### Automated

- [ ] 1.1 `make ci` target exists and runs without error
- [ ] 1.2 `make ci-deploy` target exists and runs without error
- [ ] 1.3 Guard script is executable
- [ ] 1.4 Guard script passes on current codebase

#### Manual

- [ ] 1.5 `make ci` output shows each gate running in sequence
- [ ] 1.6 Intentionally replacing one `constant_time_eq_str` call with `==` causes `make guard-auth` to fail with a clear diagnostic

### Phase 2: CI Workflow Wiring

#### Automated

- [ ] 2.1 `ci.yml` contains `make ci` step
- [ ] 2.2 `deploy.yml` contains `make ci-deploy` step
- [ ] 2.3 `ci.yml` no longer has individual cargo steps (they're in Makefile)
- [ ] 2.4 `deploy.yml` no longer has individual cargo steps (they're in Makefile)

#### Manual

- [ ] 2.5 Push a test branch; verify GitHub Actions shows `make ci` running all gates
- [ ] 2.6 Verify deploy workflow shows `make ci-deploy` running the reduced gate set

### Phase 3: Auth Unit Test + Visibility

#### Automated

- [ ] 3.1 `cargo test auth` passes with the new `constant_time_eq_str` test
- [ ] 3.2 `make guard-auth` passes
- [ ] 3.3 `test-plan.md` §6.5 no longer contains TBD

#### Manual

- [ ] 3.4 Read the new test and confirm it covers equal, unequal-same-length, and unequal-diff-length cases
- [ ] 3.5 Read §6.5 and confirm it describes the guard pattern clearly enough for a future contributor
