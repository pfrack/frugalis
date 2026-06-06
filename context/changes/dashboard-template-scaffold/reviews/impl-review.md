<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Dashboard Template Scaffold

- **Plan**: context/changes/dashboard-template-scaffold/plan.md
- **Scope**: Phase 1 & 2 of 2 (full implementation)
- **Date**: 2026-06-07
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical, 2 warnings, 3 observations

> **Note**: A prior review on 2026-06-01 returned APPROVED against commit da9f084. Since then, subsequent changes (Dashboard rewrite f19fc07, inference log inspection, cost savings metric, per-intent latency summary, dashboard router refactor) have heavily expanded the implementation. This review assesses current HEAD against the original plan. Many "drift" items are the result of those later, intentional changes — not implementation errors in this plan.

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING |
| Scope Discipline | PASS (at implementation time; plan now stale vs. HEAD) |
| Safety & Quality | WARNING |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Success Criteria Verification

All automated criteria pass at current HEAD.

- **2.1** `cargo build --release` — **PASS** (warnings only, no errors)
- **2.2** `cargo test` includes `test_dashboard_authenticated_returns_html` — **PASS** (95/95)
- **2.3** `cargo test auth` — **PASS** (18/18)

Manual criteria (2.4 browser, 2.5 curl 401) were confirmed in the prior review.

## Findings

### WARNING FINDINGS

### F1 — Template filter ordering: escape before truncate can split HTML entities
- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: templates/dashboard/index.html:131 (and similar patterns in sibling templates)
- **Detail**: `{{ record.prompt_snippet|e|truncate(60) }}` applies Askama's `|e` (HTML-escape) BEFORE `|truncate(60)`. If the raw string contains `<`, `>`, `&`, or `'` near position 60, the escape filter expands them to `&lt;`, `&gt;`, `&amp;`, `&#x27;` — and truncate then cuts the entity in half, producing garbled display like `&lt` or `&am`. Not an XSS vector (escaping still happens), but creates a user-visible display bug on truncated prompts containing special characters.
- **Fix**: Swap filter order to `|truncate(60)|e` — truncate the raw string first (60 chars of UTF-8), then escape the result. This guarantees truncation happens on character boundaries before entity expansion.

### F2 — Naming inconsistency: DashboardIndex vs XxxTemplate siblings
- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/dashboard.rs:75
- **Detail**: The index page struct is named `DashboardIndex` while all sibling page structs use the `XxxTemplate` suffix (`InferencesTemplate`, `LatencyTemplate`, `SavingsTemplate` — see dashboard.rs:89, 97, 105). Both conventions are defensible, but mixing them within 30 lines of the same file creates confusion about which convention governs.
- **Fix**: Rename to `DashboardTemplate` to match siblings, or rename siblings to drop the suffix (pick one and apply consistently). The template path `dashboard/index.html` is defined independently so the rename is purely a code-level change.

### OBSERVATIONS

### F3 — Plan is superseded; does not reflect current multi-page architecture
- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Plan Adherence
- **Location**: N/A (plan document)
- **Detail**: The original plan was implemented correctly at da9f084 (zero-field `DashboardIndex` struct in `src/main.rs`, single handler, single test, "coming soon" template). Subsequent changes (f19fc07 Dashboard rewrite, inference log inspection, cost savings, latency summary, dashboard router refactor) moved the struct/handler/routes into a new `src/dashboard.rs` module, added 3 additional pages with a full nav router (violating the plan's "No multiple dashboard sub-pages or navigation router" guardrail), injected JS theme toggle (violating "No JavaScript / client-side interactivity"), and added DB queries + metrics display (violating "No querying or displaying inference records" and "No latency summaries or metrics"). The plan's "What We're NOT Doing" section and "Desired End State" now contradict HEAD. The test coverage was also substantially expanded (13 additional tests beyond the one planned). All of these expansions were intentional and part of later roadmap slices — they are not implementation mistakes. The plan document simply wasn't updated to reflect the cumulative scope.
- **Fix**: Either archive this plan (since F-03 is done and the scaffold served its purpose as a prerequisite for S-02/S-03) or add an epilogue section documenting how later slices built on the scaffold.

### F4 — /static served without authentication
- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:628
- **Detail**: The dashboard CSS (`static/dashboard.css`) is served via `ServeDir` on `/static` outside the dashboard auth layer. Currently harmless — CSS contains no secrets. However, any static asset added to `static/` in the future would also be publicly accessible by default, which could accidentally expose internal assets.
- **Fix**: Consider nesting `ServeDir` inside the authenticated dashboard router (e.g., at `/dashboard/static/`) or documenting `/static` as intentionally public.

### F5 — |safe filter on nav icons is safe now but creates future risk
- **Severity**: ℹ️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: templates/base.html:30
- **Detail**: `{{ item.icon|safe }}` bypasses HTML escaping for sidebar navigation icons. Currently safe because icons are compile-time `&str` constants defined in `src/dashboard.rs:31-34`. If `NavPage.icon` ever becomes user-configurable or sourced from a database, this becomes an XSS injection point.
- **Fix**: Add a `// Safety` doc comment on the `NavPage` struct documenting that `icon` must be a trusted compile-time constant. Alternatively, consider a newtype wrapper that enforces the trust boundary at the type level.

## Summary

The original scaffold plan was implemented faithfully at da9f084. Current HEAD reflects cumulative work from at least 5 subsequent changes that built on the scaffold to create the full multi-page dashboard visible today. 

Two actionable warnings: the truncate-before-escape filter ordering (F1, produces garbled display on edge cases) and the naming inconsistency (F2, quick rename). Three observations flag plan staleness, the unauthenticated `/static` route, and the `|safe` icon pattern — all low-impact and addressable at leisure.

All 95 tests pass, auth coverage is complete, and Askama's compile-time template checking eliminates the class of runtime rendering errors.
