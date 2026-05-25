---
bootstrapped_at: 2026-05-25T00:00:00Z
starter_id: axum
starter_name: "Axum (Rust web framework)"
project_name: cerebrum
language_family: rust
package_manager: cargo
cwd_strategy: subdir-then-move
bootstrapper_confidence: first-class
phase_3_status: ok
audit_command: "cargo audit --json"
---

## Hand-off

```yaml
starter_id: axum
package_manager: cargo
project_name: cerebrum
hints:
  language_family: rust
  team_size: solo
  deployment_target: self-host
  ci_provider: github-actions
  ci_default_flow: auto-deploy-on-merge
  bootstrapper_confidence: first-class
  path_taken: standard
  quality_override: false
  self_check_answers: null
  has_auth: false
  has_payments: false
  has_realtime: true
  has_ai: true
  has_background_jobs: false
```

### Why this stack

This project is an API-first gateway with a short solo timeline, so the Rust API default is the fastest fit with low decision overhead. Axum matches the product shape directly, keeps the stack strongly typed and convention-friendly, and supports the two key workload signals from your requirements: AI-oriented routing logic and continuous response delivery behavior. You selected the quick path, so we kept deployment and CI choices simple and aligned with the starter defaults: self-host plus GitHub Actions with automatic deployment on merge. Scaffolding confidence is first-class, which means setup is expected to be mostly smooth with occasional manual touches rather than high-friction bootstrap work.

## Pre-scaffold verification

| Signal      | Value                                         | Severity | Notes                                                                                      |
| ----------- | --------------------------------------------- | -------- | ------------------------------------------------------------------------------------------ |
| npm package | not run                                       | —        | rust starter — no npm package to check                                                     |
| GitHub repo | not run                                       | —        | docs_url points to docs.rs (not GitHub); no GitHub recency signal available for this card  |

No recency signals available for this starter. Cargo ecosystem starters do not expose a meaningful npm or GitHub recency check via the bootstrapper-config signals. The starter was selected by tech-stack-selector from its registry entry dated 2026-04-20.

## Scaffold log

**Resolved invocation**: `cargo new .bootstrap-scaffold --name cerebrum --bin --edition 2024 && cd .bootstrap-scaffold && cargo add axum tokio --features tokio/full`

> Note: `--name cerebrum` was added at runtime because `cargo new` disallows `.`-prefixed directory names as package names; the package name override preserves the intended project name.

**Strategy**: scaffold into a temporary directory, then move files up (subdir-then-move)
**Exit code**: 0
**Files moved**: 3 (`Cargo.toml`, `Cargo.lock`, `src/main.rs`)
**Conflicts (.scaffold siblings)**: none
**.gitignore handling**: absent in scaffold
**.bootstrap-scaffold cleanup**: deleted

### Packages added by `cargo add`

- `axum` v0.8.9 (features: form, http1, json, matched-path, original-uri, query, tokio, tower-log, tracing)
- `tokio` v1.52.3 (features: bytes, fs, full, io-std, io-util, libc, macros, net, parking_lot, process, rt, rt-multi-thread, signal, signal-hook-registry, socket2, sync, time, tokio-macros)

**Locked dependencies**: 62 packages (Rust 1.94.1 compatible)

## Post-scaffold audit

**Tool**: `cargo audit --json`
**Advisory database**: 1,098 advisories (last updated 2026-05-23)
**Summary**: 0 CRITICAL, 0 HIGH, 0 MODERATE, 0 LOW
**Direct vs transitive**: not distinguished by cargo-audit in this output (0 total)

Audit result: clean tree across all 62 locked dependencies.

## Hints recorded but not acted on

| Hint                    | Value              |
| ----------------------- | ------------------ |
| bootstrapper_confidence | first-class        |
| quality_override        | false              |
| path_taken              | standard           |
| self_check_answers      | null               |
| team_size               | solo               |
| deployment_target       | self-host          |
| ci_provider             | github-actions     |
| ci_default_flow         | auto-deploy-on-merge |
| has_auth                | false              |
| has_payments            | false              |
| has_realtime            | true               |
| has_ai                  | true               |
| has_background_jobs     | false              |

`has_realtime: true` and `has_ai: true` were surfaced in the Step 0 summary. No automated scaffold modification was made based on these flags in v1; these will be actioned by a future M1L4 skill or manual configuration.

## Next steps

Next: a future skill will set up agent context (CLAUDE.md, AGENTS.md). For now, your project is scaffolded and verified — happy hacking.

Useful manual steps in the meantime:
- `git init` (if you have not already) to start your own repo history.
- Review any `.scaffold` siblings the conflict policy created and decide which version of each file to keep. (None were created this run.)
- Address audit findings per your project's risk tolerance — 0 findings this run.
