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
