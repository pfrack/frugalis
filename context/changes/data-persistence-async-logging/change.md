---
change_id: data-persistence-async-logging
title: Data Persistence Async Logging Pipeline
status: planned
created: 2026-05-26
updated: 2026-05-26
archived_at: null
---

## Notes

Foundation F-02 from roadmap: add Supabase PostgreSQL persistence for inference metadata as a non-blocking side path.

Planning decisions captured:
- Schema: minimal contract plus request_id and status.
- Privacy: prompt snippet only, fixed-length trim policy.
- Reliability: one short retry in background; on final failure drop record and emit structured error log.
- Migrations: versioned SQL files committed in repository.
- Trigger: emit logging job after request handling completes, never on the synchronous response path.
- Test strategy: unit tests + selective DB integration tests (enabled when DATABASE_URL is available).
