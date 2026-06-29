-- Migration 001: Create inferences table for async inference metadata logging
CREATE TABLE IF NOT EXISTS inferences (
    id            TEXT        PRIMARY KEY,
    request_id    TEXT        NOT NULL UNIQUE,
    status        TEXT        NOT NULL,
    category      TEXT,
    upstream_model TEXT,
    duration_ms   INTEGER,
    created_at    TEXT        NOT NULL DEFAULT (datetime('now')),
    prompt_snippet TEXT
);

-- Optimised for recent-record reads (dashboard) and request_id lookup
CREATE INDEX IF NOT EXISTS inferences_created_at_idx  ON inferences (created_at DESC);
CREATE INDEX IF NOT EXISTS inferences_request_id_idx  ON inferences (request_id);
