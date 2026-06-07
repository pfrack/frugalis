# Per-Intent Latency Summary — Plan Brief

> Full plan: `context/changes/per-intent-latency-summary/plan.md`

## What & Why

Add a per-intent latency summary to the Cerebrum dashboard. The operator can see average and P99 latency grouped by intent category (COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL) with a configurable time window. This fulfills the PRD's secondary success criterion: "Dashboard includes a per-intent latency summary for recent traffic."

## Starting Point

The dashboard already has an index page, an inference log table (`/dashboard/inferences`), auth gates, template rendering via Askama, and a PostgreSQL `inferences` table with `category`, `duration_ms`, and `created_at` columns. All prerequisites (F-01 through F-03, S-01, S-02) are implemented. What's missing is the aggregation query that GROUP BYs on category and the UI to display it.

## Desired End State

Two delivery points:
1. **Dashboard index** — a summary card below the welcome section showing total inferences and per-category average latency in the last 24 hours, with a link to the full breakdown
2. **`/dashboard/latency`** — a dedicated page with a configurable time-window form (`?hours=`, default 24), a table showing Category, Count, Avg Duration, and P99 Duration per category, plus an "unclassified records excluded" footnote

Both views degrade to static/error state when persistence is unavailable.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Time window | Configurable via `?hours=` URL param, default 24, range 1–720 | Operator can inspect different periods; simple integer parameter with bounds validation. | Plan |
| Placement | Summary card on index + full table on `/dashboard/latency` | Index gives at-a-glance; dedicated page gives full detail. | Plan |
| NULL category handling | Exclude from table, show count in footnote | Clean summary + operator awareness of unclassified traffic via a separate COUNT query. | Plan |
| Statistics computed | AVG + exact P99 via `PERCENTILE_CONT(0.99)` | PostgreSQL handles both in a single GROUP BY query; P99 exposes outliers that averages hide. | Plan |
| Empty state | Match `inferences.html` empty-state pattern | Consistent UX across dashboard pages using existing CSS classes. | Plan |

## Scope

**In scope:**
- New `LatencySummary` struct + `fetch_latency_summary(hours)` method on `PersistenceConfig`
- Summary card on `/dashboard` index (24h fixed)
- Dedicated `/dashboard/latency` page with time-window form and full table
- Navigation link "Latency" on all dashboard pages
- Unit tests (auth gates, parameter parsing, empty state) and DB integration tests

**Out of scope:**
- Charts or graphical visualization (pure table)
- Per-model latency breakdown (category only)
- Date-range picker (simple hours integer)
- Real-time updates (page-refresh based)
- Export/CSV/download

## Architecture / Approach

```
PersistenceConfig::fetch_latency_summary(hours)
  ├── GROUP BY category: COUNT, AVG(duration_ms), PERCENTILE_CONT(0.99)
  └── Separate COUNT for category IS NULL

dashboard() handler ──→ fetch_latency_summary(24) ──→ DashboardIndex template
latency() handler  ──→ fetch_latency_summary(hours) ──→ LatencyTemplate

Both handlers degrade to error/empty template when persistence is None.
```

One new SQL method serves both views. The index handler calls it with fixed `hours=24`; the latency handler parses the `?hours=` query param. The data struct (`LatencySummary`) is the single contract between the persistence layer and templates.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Aggregation Query | `fetch_latency_summary(hours)` method on PersistenceConfig | PERCENTILE_CONT returns NULL when all duration_ms are NULL — must handle Option |
| 2. Dashboard Index Card | Summary card on `/dashboard` showing 24h latency overview | Handler signature change from no-State to with-State must not break existing tests |
| 3. Full Latency Page | `/dashboard/latency` with time-window form, full table, footnote | Template must correctly display nullable AVG/P99 values when no data exists |

**Prerequisites:** F-03 (Askama templates), S-02 (inference log inspection working), a running database with inference data.

**Estimated effort:** ~1 session across 3 phases.

## Open Risks & Assumptions

- `PERCENTILE_CONT(0.99)` requires at least 1 non-NULL `duration_ms` value per group — returns NULL otherwise. Template must handle the `Option<i32>`.
- The dashboard index page now queries the DB on every load — adds ~1-2ms latency. Acceptable at MVP scale.
- If the `inferences` table grows very large (100K+ rows), the aggregation query may benefit from a composite index on `(created_at, category)` — defer to post-MVP tuning.

## Success Criteria (Summary)

- Operator sees a latency summary card on the dashboard landing page when data exists
- Operator can navigate to `/dashboard/latency`, adjust the time window, and see AVG + P99 per category
- Unclassified (NULL-category) records are counted in a footnote, not silently dropped
- Both pages handle missing database gracefully (no crashes, no 500s)
