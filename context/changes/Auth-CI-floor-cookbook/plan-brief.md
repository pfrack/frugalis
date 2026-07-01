# CI Floor + Auth Guard — Plan Brief

> Full plan: `context/changes/Auth-CI-floor-cookbook/plan.md`
> Frame brief: `context/changes/Auth-CI-floor-cookbook/frame.md`

## What & Why

Phase 4 bundles 3 loosely-coupled concerns (auth guard, CI wiring, cookbook) under one change, and within CI wiring, the coverage threshold carries a substantially different implementation scope (1–2 hours, data-dependent) than the other 3 gates (pure wiring). This plan addresses the first concern: wire `fmt --check`, `slow_tests`, and a grep-based constant-time-compare guard into both CI workflows, and lock the `constant_time_eq_str` invariant with a direct unit test.

## Starting Point

`ci.yml` has lint + typecheck + auth tests + persistence + build. `deploy.yml` has auth tests + persistence + build — missing lint and typecheck entirely. Neither workflow runs `fmt --check`, `slow_tests`, or the auth grep guard. The `justfile:188` `ci` recipe already defines the full gate sequence (`fmt-check lint-strict test test-slow build-release`) but is not invoked from CI. `constant_time_eq_str` (`src/routing/auth.rs:169`) is private with no direct unit test; existing tests cover it only indirectly through `validate_proxy_bearer_header` and `validate_dashboard_basic_header`.

## Desired End State

Every PR and every push-to-main runs `fmt --check`, `slow_tests`, and an auth grep guard before building. A future cleanup that reverts `constant_time_eq_str` to `==` in any auth comparison context will fail CI within seconds. The `constant_time_eq_str` function has a direct unit test proving equal/unequal/different-length behavior. The Makefile is the single source of truth for CI gate sequences.

## Key Decisions Made

| Decision | Choice | Why | Source |
|---|---|---|---|
| CI gate invocation | Makefile recipes (`make ci` / `make ci-deploy`) | `make` pre-installed on ubuntu-latest; single source of truth; no tool-install step | Plan |
| Auth guard location | `.github/scripts/guard-auth-compare.sh` | Dedicated CI infrastructure dir; single script referenced by both workflows | Plan |
| Guard scope | Narrowly scoped to `src/routing/auth.*` and the 3 known call sites | Low false-positive rate; protects exactly the invariant Risk #7 describes | Plan |
| slow_tests in deploy.yml | PR only (`ci.yml`) | Keeps deploy.yml fast; local + PR both enforce; slow_tests too timing-sensitive for the deploy gate | Plan |
| Deploy.yml backfill | Add fmt-check + slow_tests + guard; do NOT backfill lint/typecheck | deploy.yml catches up on new gates; lint/typecheck are redundant with upstream PR CI | Plan |
| CT function visibility | Make `constant_time_eq_str` `pub(crate)` | Enables direct unit test; minimal visibility expansion | Plan |
| CT unit test contract | Equal → true, unequal-same-length → false, unequal-diff-length → false | Locks function behavior without testing constant-time property (not the identified risk) | Plan |

## Scope

**In scope:**
- `Makefile` with `ci`, `ci-deploy`, `fmt-check`, `test-slow`, `guard-auth` targets
- `.github/scripts/guard-auth-compare.sh` — grep-based guard script
- `ci.yml` — add fmt-check, slow_tests, guard steps (or `make ci`)
- `deploy.yml` — add fmt-check, guard steps (or `make ci-deploy`)
- `src/routing/auth.rs` — make `constant_time_eq_str` `pub(crate)`, add direct unit test
- `test-plan.md` §6.5 — update TBD with grep-guard pattern (one-line pointer to the guard script)

**Out of scope:**
- Coverage threshold (separate change — needs baseline data)
- Cookbook backfill for §6.1–§6.4 (separate change — orthogonal to CI wiring)
- `justfile` changes (stays as-is for local dev; CI uses Makefile)
- `src/classification/llm.rs:80` non-CT gap (low-risk, local env only — per frame verdict)

## Architecture / Approach

A new `Makefile` at the repo root defines `ci` (PR gate: fmt-check → lint-strict → test → test-slow → guard-auth → build-release) and `ci-deploy` (deploy gate: fmt-check → test → test-slow → guard-auth → build-release — no lint/typecheck). Both workflows install Rust, then call `make ci` or `make ci-deploy`. The guard script at `.github/scripts/guard-auth-compare.sh` uses `grep -rn 'constant_time_eq_str'` to find call sites and `grep -P '==\s'` scoped to `src/routing/auth*` files to detect forbidden direct comparisons, excluding the function definition line itself.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Makefile + guard script | Makefile with CI targets + `.github/scripts/guard-auth-compare.sh` | Guard regex false positives on non-auth `==` in auth files |
| 2. CI workflow wiring | `ci.yml` and `deploy.yml` invoke `make ci` / `make ci-deploy` | Step ordering; justfile recipes diverge from Makefile |
| 3. Auth unit test + visibility | `constant_time_eq_str` pub(crate) + direct 3-case test | None — straightforward |

**Prerequisites:** None — all work is local file edits + CI YAML.
**Estimated effort:** ~1 session across 3 phases.

## Open Risks & Assumptions

- `make` is pre-installed on GitHub Actions ubuntu-latest runners (true as of 2026; verify if runner image changes)
- The guard script's regex must exclude the function definition line (`fn constant_time_eq_str`) — if it doesn't, the guard will false-positive on its own definition
- `justfile` stays as-is for local dev; if contributors expect `just ci` to match CI exactly, the Makefile must be kept in sync (manual sync, no automation)

## Success Criteria (Summary)

- `make ci` passes locally with all gates green
- A test branch that reverts one `constant_time_eq_str` call to `==` fails CI at the guard step
- Both `ci.yml` and `deploy.yml` show the new gate steps in GitHub Actions
- `cargo test auth` passes with the new direct `constant_time_eq_str` test
