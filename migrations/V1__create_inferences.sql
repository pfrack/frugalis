-- Migration 001: Create inferences table for async inference metadata logging
CREATE TABLE IF NOT EXISTS inferences (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id    UUID        NOT NULL,
    status        TEXT        NOT NULL,
    category      TEXT,
    upstream_model TEXT,
    duration_ms   INTEGER,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    prompt_snippet TEXT
);

-- Optimised for recent-record reads (dashboard) and request_id lookup
CREATE INDEX IF NOT EXISTS inferences_created_at_idx  ON inferences (created_at DESC);
CREATE INDEX IF NOT EXISTS inferences_request_id_idx  ON inferences (request_id);

-- TODO: Add retention policy to prevent unbounded growth.
-- Proposed: DELETE FROM inferences WHERE created_at < NOW() - INTERVAL '90 days'
-- Implementation options:
--   1. PostgreSQL pg_cron extension: schedule automatic cleanup
--   2. Application-level cleanup job on a separate schedule
--   3. Supabase automatic cleanup with partition TTL (if supported)
-- To be implemented in v1.1 when ops infrastructure is ready to monitor cleanup.
-- At 100 req/sec, table grows ~8.6M rows/year; cleanup prevents storage exhaustion.
