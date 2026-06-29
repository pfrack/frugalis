# Lessons Learned

> Append-only register of recurring rules and patterns. Re-read at start by /10x-frame, /10x-research, /10x-plan, /10x-plan-review, /10x-implement, /10x-impl-review.

## Use OpenAPI Generator for Endpoints

- **Context**: API endpoint implementation in backend services
- **Problem**: it is easier to take care of api architecture with swagger file
- **Rule**: Use OpenAPI Generator when creating endpoints.
- **Applies to**: plan,implement,impl-review

## Re-run review after a follow-up change touches the same handler

- **Context**: src/main.rs `completion_handler` (and other modules where a prior review's fixes live).
- **Problem**: Two subsequent changes (`f19fc07` Dashboard rewrite, `9fb9ce3` SSE streaming proxy) regressed all 4 of the previous review's fixes (F1-F4) by rewriting the same `completion_handler`. The original fixes were correctly in place at HEAD after the review, but were lost when later changes touched the same file.
- **Rule**: When a follow-up change rewrites a handler or module that has been reviewed and approved, the author of the follow-up must either (a) preserve the prior review's fixes verbatim (grep for `F1`, `F2`, ... markers in plan + code), or (b) document the deviation in the new change's plan and have it re-reviewed. When /10x-impl-review runs on a change whose files were touched by an intervening commit, treat the file state as the source of truth and verify the previous review's fixes are still present.
- **Applies to**: plan, implement, impl-review

## Handle upstream error bodies without full buffering where possible

- **Context**: src/main.rs:347-361 (streaming error path), src/main.rs:436-449 (non-streaming path)
- **Problem**: Upstream error responses are read fully into memory (capped at 10 MB) before constructing the error response. This introduces unnecessary latency and memory pressure on large error payloads.
- **Rule**: Prefer streaming upstream error bodies directly to the client (chunked transfer) or truncating early, rather than buffering the entire body before responding. Keep error-path latency and memory bounded.
- **Applies to**: implement, impl-review

## Document guard points with self-describing comments, not review cross-references

- **Context**: Review follow-ups that touch handlers or modules with prior review findings (src/main.rs `completion_handler`, `classify_and_log`, and similar system-boundary functions).
- **Problem**: Adding opaque review cross-references (e.g. `// F1`, `// F2`) to guard points creates cryptic markers that future developers reading the code cannot understand without digging through old review reports. The markers become noise, not documentation.
- **Rule**: When documenting a guard point at a system boundary (input validation, auth, error handling), write a self-describing comment that explains WHAT invariant is protected and WHY — not a review ID. If a prior review finding is relevant, reference it by its rule in lessons.md, not by its finding number.
- **Applies to**: implement, impl-review

## Favor dynamic WHERE clause building over duplicated SQL branches

- **Context**: src/persistence.rs:135-224 (`fetch_inferences` method)
- **Problem**: Four separate SQL query strings and bind/fetch blocks for different filter combinations were duplicated (~80 lines). Each new filter combination would multiply the branches, increasing maintenance burden and review surface.
- **Rule**: Use dynamic WHERE clause building with a bind_count tracker and reusable bind/fetch logic instead of duplicating SQL queries and bind/fetch blocks for every filter combination. Keep the number of SQL statement variants proportional to the number of independent dimensions, not their product.
- **Applies to**: implement, impl-review

## Log operational failures before falling back

- **Context**: File I/O operations with fallback paths (config file reading, template loading, environment variables)
- **Problem**: `unwrap_or_else` silently swallows errors and falls back, making it difficult to diagnose why alternate code paths are being used. Example: `std::fs::read_to_string(path).unwrap_or_else(|_| default_value)` masks permission errors, file corruption, or misconfiguration.
- **Rule**: When using a fallback for failed operations, log the failure before falling back so operators can diagnose configuration or environmental issues. Use `warn!` for user-configurable paths, `debug!` for internal defaults.
- **Applies to**: implement, impl-review

## Delete dead code rather than suppressing warnings

- **Context**: Any `#[cfg(test)]` helper, utility function, or module path that triggers `dead_code` warnings
- **Problem**: Dead test helpers accumulate when written speculatively ("we might need this later") but never called. Suppressing with `#[allow(dead_code)]` hides the rot — the code stays, confuses future readers, and eventually someone has to figure out whether it's safe to remove or if something depends on it.
- **Rule**: When a `dead_code` warning fires on code with zero callers, delete it. Tests are the best documentation for how helpers work — if there's no test, there's no value. YAGNI: if you aren't using it now, you don't need it.
- **Applies to**: implement, impl-review

## Squash merges must not bundle unrelated in-flight changes into one PR

- **Context**: PR merges on the rollout branch where the feature branch has been carrying other in-flight changes' work (other change folders' planning artifacts, sibling changes' production code, formatting sweeps). Triggers when a feature branch was used as a staging area for parallel work and the squash lands it all at once.
- **Problem**: PR scope stops matching change scope. The reviewing skill (/10x-impl-review) cannot meaningfully verify the bundled work against the originating plan, because no plan covers it. Reverting the bundled work is destructive (some of it is load-bearing for the implemented sibling changes), so the drift stays in main indefinitely and the next plan-vs-PR review surfaces the same finding with no clean fix. Concrete instance: Tests #12 PR (commit 35906ce) bundled ~1,200 lines of artifacts from the `readme-bootstrap` change folder (README.md + research + change.md), the `opentelemetry-integration` plan addenda and review report, OTel F6 production code in `src/telemetry.rs`, and a `cargo fmt` sweep on `src/config.rs` / `src/dashboard.rs` / `src/routing.rs` — none of which were in the testing-critical-path-regression-guards plan's "Changes Required".
- **Rule**: Before opening a PR, rebase the feature branch onto the current main and confirm the diff against main is exactly the work for the change's plan. If a feature branch has been staging other changes' work, either (a) split that work off into its own branch and PR first, or (b) explicitly call out the bundled scope in the PR description and get reviewer sign-off that the drift is acceptable. Do not let a squash merge hide a multi-PR scope creep inside a single change folder.
- **Applies to**: frame

## Organize src/ into domain subdirectories, not flat

- **Context**: Any phase that creates new source modules or adds substantial logic to `src/`. Especially when a new subsystem has multiple concerns.
- **Problem**: Without domain grouping, `src/` grows into a flat namespace with a monolithic main.rs (was 8,460 lines across 13 files) — hard to navigate, merge conflicts on every touch, no clear boundaries between subsystems.
- **Rule**: Always group source files into domain-named subdirectories (`proxy/`, `classification/`, `config/`, etc.) when a module exceeds 2-3 files or crosses subsystem boundaries. Never add new top-level `.rs` files without a directory home.
- **Co-location by maintainer decision**: The 2-3-file threshold is the default trigger, not the only justification. A maintainer may co-locate related flat files into a domain subdirectory even when no single file exceeds the threshold, provided: (a) the files share a domain concept named by the folder, (b) the extraction is documented in the change's plan with rationale overriding the YAGNI default, and (c) AGENTS.md is amended to reflect the new structure. Examples in this repo: `dashboard/` (nav/templates/handlers split for discoverability), `routing/` (auth + routes co-located as request-pipeline middleware), `app/` (composition root + cli + quickstart co-located as entry-point glue). Such extractions are explicit exceptions — future files below the threshold continue to default to flat unless a maintainer makes the same documented override.
- **Applies to**: plan, implement, impl-review
