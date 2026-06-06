# Change: dashboard-mvp-rewrite

**ID:** dashboard-mvp-rewrite
**Status:** impl_reviewed
**Created:** 2026-06-06
**Updated:** 2026-06-06

## Summary

Comprehensive rewrite of the dashboard from a basic POC scaffold into a full-featured
observability UI. Extracted dashboard logic into a dedicated module (`src/dashboard.rs`),
implemented 4-page navigation system with sidebar, added complete CSS styling, and
integrated all dashboard slices (S-02, S-03, S-04) into a cohesive user experience.

## Before (POC)

The original `dashboard-template-scaffold` (F-03) provided:
- Simple Askama template with "coming soon" placeholder
- Handler in `src/main.rs` returning minimal HTML
- No navigation, styling, or actual data display

## After (MVP)

The rewrite delivers a production-ready dashboard with:

**Architecture:**
- Dedicated `src/dashboard.rs` module (314 lines) with separate handlers
- Navigation registry (`PAGES`) with auto-generated sidebar
- Shared `dashboard_page!` macro for template structs
- Four routes: `/dashboard`, `/dashboard/inferences`, `/dashboard/latency`, `/dashboard/savings`

**UI/UX:**
- Full sidebar navigation with icons and active state highlighting
- Status bar showing gateway/database connection status
- Quick stats cards on homepage (total requests, categories, savings, classification rate)
- Recent activity table with expandable prompt snippets
- Pagination and filtering on inferences page
- Configurable time windows on latency page
- Dark/light theme toggle with persistent preference
- Responsive CSS with modern design (572 lines)

**Integration:**
- Homepage aggregates data from all three dashboard slices
- Real-time status indicators (DB connected, classifier active, baseline model)
- Graceful degradation when database is unavailable
- Error handling with user-friendly banners

## Roadmap slot

This change supersedes the incremental approach of S-02, S-03, S-04 by delivering all
dashboard observability features in a single cohesive rewrite. It builds upon the
foundation of F-03 (dashboard-template-scaffold) but transforms the user experience
from placeholder to MVP-complete.

**Outcome:** Operators can view inference logs, latency summaries, and cost-savings
metrics through a polished, navigable interface.

## PRD refs

FR-006 (dashboard views), dashboard NFR (private operator access).
Also implements Secondary Success Criterion (per-intent latency) and FR-007 nice-to-have
(cost-savings metric).

## Technical notes

The rewrite maintains backward compatibility with existing auth (F-01) and data layer
(F-02) but significantly improves the frontend architecture:
- Uses `askama_web` with `WebTemplate` derive for Axum 0.8 integration
- Parallel query execution with `tokio::join!` for homepage performance
- Consistent error handling pattern across all handlers
- Pagination logic (offset/limit) with safe limits enforcement
- Filter parameters (`filter_category`, `filter_model`) on inferences page
