# Change: dashboard-template-scaffold

**ID:** dashboard-template-scaffold
**Status:** archived
**Created:** 2026-06-01
**Updated:** 2026-06-06
**Archived:** 2026-06-06T22:30:33Z

## Summary

Wire Askama HTML templating into the Axum dashboard route, replacing the stub
`Html("<h1>Dashboard route is protected</h1>")` handler with a real template
pipeline using base/child template inheritance. The `/dashboard` endpoint
continues to be protected by existing HTTP Basic auth from F-01.

## Roadmap slot

F-03 — prerequisite for S-02 (inference log inspection) and S-03 (latency summaries).

## PRD refs

FR-006 (dashboard views), dashboard NFR (private operator access).
