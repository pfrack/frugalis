# Frame Brief: Auth + CI floor + cookbook (Phase 4 decomposition)

> Framing step before /10x-plan. This document captures what is *actually*
> at issue, separated from what was initially assumed.

## Reported Observation

The project's CI workflows (`ci.yml` PR, `deploy.yml` push-to-main) lack
4 gates: `cargo fmt --check`, `cargo test slow_tests`, a grep-based auth
constant-time-compare guard, and a coverage threshold. The test plan
cookbook (§6.1–§6.5 in `test-plan.md:132-150`) has TBD entries despite 3
archived shipped phases producing reusable test patterns. There are 3
`constant_time_eq_str` call sites at `src/routing/auth.rs:34,43,44` with
existing unit tests but no automated guard against a future cleanup
reverting them to `==`. A `justfile:188` already defines a `ci` composite
target covering the missing gates that `ci.yml` doesn't invoke.

## Initial Framing (preserved)

- **User's stated cause or approach**: The test plan Phase 4 row
  (`test-plan.md:72`) bundles three concerns as one change — lock the
  constant-time compare invariant, wire CI gates, update the cookbook.
- **User's proposed direction**: Implement Phase 4 as a single change
  "Auth-CI-floor-cookbook."
- **Pre-dispatch narrowing**: The driving signal is the CI health gap (not
  a specific auth concern). The user hasn't separated the three concerns yet
  — the bundle was inherited from the test plan. Gate priority is "all at
  once." The cookbook is deemed needed, not overhead. The user is open to
  decomposing into 2–3 changes.

## Dimension Map

The observation could originate at any of these dimensions:

1. **Auth invariant guard** — The 3 call sites are already correct; the risk is
   future regression. The sole gap (`src/classification/llm.rs:80`, non-CT
   API-key comparison) is low-risk (local env only). A grep guard would
   protect, but no prior regression exists.
2. **CI floor wiring** — `ci.yml` already has lint+typecheck+auth+build.
   Missing: `fmt --check`, `slow_tests`, grep guard, coverage. The
   `justfile:188` already defines a `ci` recipe with all of them. The gap is
   YAML wiring, not tooling. ← initial framing (Phase 4 bundle)
3. **Phase 4 bundling assumption** — Auth guard (risk-specific), CI wiring
   (infra), cookbook (docs) are loosely coupled. The test plan bundled them,
   but each could ship independently. The `justfile` already bridges CI and
   auth tests into one target, so the guard + CI wiring are naturally
   co-located — but cookbook is orthogonal.
4. **Cookbook backfill** — Three archived changes shipped patterns
   (CountingClassifier, test_app_with_provider, format_sse_error_event,
   parse_json_body, keepalive slow-test template). None wrote them up
   centrally. The test plan was refreshed on 2026-06-30, resetting §6 to
   TBD. AGENTS.md and README.md have only basic pattern docs.
5. **Coverage threshold** — No coverage tooling exists. `rust:1.96.0` on
   stable supports `-C instrument-coverage` via `cargo-llvm-cov`, but
   setting it up requires tool installation, baseline run, threshold
   selection, exclusion tuning for `sqlx::query_as!()` and askama macros,
   and `#[coverage(off)]` annotations on test helpers. This is a 1–2 hour
   engineering task with a data dependency (baseline must be measured before
   threshold can be set).

## Hypothesis Investigation

| Hypothesis | Evidence | Verdict |
| --- | --- | --- |
| Auth guard is the core concern | Constant-time comparison is already correct at all 3 call sites (`src/routing/auth.rs:34,43,44`); the sole non-CT gap (`src/classification/llm.rs:80`) is low-risk; no prior regression event exists; no git commit mentions a revert attempt. The guard is preventive, not reactive. | WEAK |
| CI floor gaps are the real driver | `ci.yml:28-56` lacks `fmt --check`, `slow_tests`, any grep guard, and coverage — every PR merges without these gates. The `justfile:188` `ci` target already includes all of them but is not invoked from CI. The deploy workflow (`deploy.yml:25-46`) is also missing `clippy` and `check` that `ci.yml` has. The user identified the CI health gap as the trigger. | STRONG |
| All gates are equal scope | `fmt --check` is a 1-line YAML addition. `slow_tests` is a 1-line YAML addition (tests already exist at `src/proxy/streaming.rs:1019` and `src/persistence/sql_backend.rs:1020`). The auth grep guard needs a short bash script (~20 lines) + YAML wiring. The coverage threshold needs tool installation, baseline data, exclusion tuning, and `#[coverage(off)]` annotations — substantially different scope. Bundling them as "all gates" hides the coverage complexity. | WEAK |
| Cookbook is a separate concern | Cookbook §6.1–§6.5 was reset to TBD on 2026-06-30. Three archived changes shipped patterns that aren't centrally documented. Patterns exist in code: CountingClassifier (`src/classification/chain.rs:84-96`), provider-type harness family (`src/app/test_helpers.rs:117-432`), SSE error-format helper (`src/proxy/util.rs:554-565`), keepalive slow-test template (`src/proxy/streaming.rs:1019`+). No cookbook entry is blocked on CI wiring or auth guard implementation. | STRONG |
| Phase 4 bundle is correct as-is | Auth guard, CI wiring, and cookbook backfill touch different files with no compilation dependency. The `justfile` already bridges test targets. A single PR would touch ~4 files (2 CI workflows, auth.rs test, test-plan.md) — manageable, but the coverage piece is the odd one out because it needs data before threshold can be set. | PARTIAL |

## Narrowing Signals

Decisive observations from Step 4 that narrowed the hypothesis space:

- **CI health gap is the trigger** — the user picked up Phase 4 because
  "every merge bypasses gates the test plan already named," not because of a
  specific auth regression concern.
- **Cookbook is needed** — despite 3 phases of TBD, the user considers the
  cookbook format valuable, not documentation overhead. It's a real deliverable,
  not a checkbox.
- **Open to decomposition** — the user agreed the bundle should be split,
  confirming the Phase 4 row is a deployment grouping, not a development
  dependency.
- **Coverage is scope-heavier** — the pressure test confirmed coverage
  requires baseline measurement before threshold can be set, unlike the
  other 3 gates which are pure wiring.

## Cross-System Convention

The `2026-06-15-cicd-dev-tooling` change explicitly deferred both the
`ci.yml` creation and the coverage threshold to a follow-up
(`research.md:475,478`). The convention established is: CI workflow files
are co-located with the gates they enforce, and gates with data dependencies
ship separately from gates that are pure wiring. This convention supports
splitting the coverage threshold from the auth/CI wiring change.

The `justfile` convention (`justfile:188-194`) is that `ci` runs the full
gate sequence and `gates` runs the local verification subset. This is the
natural target for wiring into both CI workflows.

## Reframed (or Confirmed) Problem Statement

> **The actual problem to plan around is**: Phase 4 bundles 3 loosely-coupled
> concerns (auth guard, CI wiring, cookbook) under one change, and within CI
> wiring, the coverage threshold carries a substantially different
> implementation scope (1–2 hours, data-dependent) than the other 3 gates
> (pure YAML wiring).

Decomposing into 3 independent changes aligns scope with risk, lets the CI
health gap ship first, and unblocks the cookbook backfill which has been
deferred through 3 shipped phases. Each change has its own success criteria
and can be planned, implemented, and verified independently.

## Confidence

- **HIGH** — CI health gap evidence is dominant (user's own trigger + CI vs
  justfile drift confirmed at file:line). Decomposition matches the
  established cicd-dev-tooling convention. Cookbook as separate concern is
  unambiguously orthogonal.

## What Changes for /10x-plan

Instead of one plan for "Phase 4: Auth + CI floor + cookbook," produce three
plans:

1. **CI floor + auth guard** — Wire `fmt --check`, `slow_tests`, and the
   auth grep guard into `ci.yml` and `deploy.yml`. Make
   `constant_time_eq_str` `pub(crate)`, add a direct unit test, and add a
   grep-based CI step that fails if `==` replaces `constant_time_eq_str` in
   auth comparison context. This addresses Risk #7 directly.
2. **Coverage threshold** — Set up `cargo-llvm-cov`, establish baseline
   coverage, pick a floor, tune exclusions, add `#[coverage(off)]`
   annotations, wire to CI. Ships after baseline data is available.
3. **Cookbook backfill** — Populate `test-plan.md` §6.1–§6.5 from the 3
   shipped archived changes: CountingClassifier + chain.test patterns (§6.1),
   provider-type harnesses + body-contract assertion style (§6.2),
   `format_sse_error_event` + streaming assertion patterns (§6.3), property
   test TBD placeholder (§6.4), auth grep guard pattern (§6.5).

## References

- Source files: `src/routing/auth.rs:34,43,44,169` (CT comparison sites);
  `src/classification/llm.rs:80` (low-risk CT gap);
  `.github/workflows/ci.yml:28-56` and `.github/workflows/deploy.yml:25-46`
  (current CI gates); `justfile:188-194` (ci/gates composite targets);
  `src/app/test_helpers.rs:117-432` (test harness patterns);
  `src/proxy/streaming.rs:1019`, `src/persistence/sql_backend.rs:1020` (slow
  test modules)
- Related research: `context/foundation/test-plan.md:72` (Phase 4 row),
  `test-plan.md:109-119` (quality gates), `test-plan.md:132-150` (cookbook
  TBD); `context/archive/2026-06-15-cicd-dev-tooling/research.md:475-478`
  (coverage deferral); `context/archive/2026-06-13-testing-critical-path-regression-guards/plan.md:792-899`
  (Phase 6 cookbook that was overwritten)
- Relevant lessons: `context/foundation/lessons.md:61-66` (domain subdirectory
  convention — not applicable to CI files but relevant if auth guard script
  needs a home)
