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
