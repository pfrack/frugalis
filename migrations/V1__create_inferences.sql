-- Create inferences table for async inference metadata logging.
-- Cross-dialect: valid on both Postgres and SQLite. The application always
-- supplies request_id and created_at on INSERT, so no DEFAULT is needed.
CREATE TABLE IF NOT EXISTS inferences (
    request_id            TEXT PRIMARY KEY,
    status                TEXT NOT NULL,
    category              TEXT,
    upstream_model        TEXT,
    duration_ms           INTEGER,
    created_at            TEXT NOT NULL,
    prompt_snippet        TEXT,
    prompt_char_count     INTEGER,
    provider_attempts     SMALLINT DEFAULT 1,
    final_provider        TEXT,
    input_tokens          INTEGER,
    output_tokens         INTEGER,
    cache_read_tokens     INTEGER,
    cache_creation_tokens INTEGER,
    client_session_id     TEXT
);

CREATE INDEX IF NOT EXISTS inferences_created_at_idx ON inferences (created_at DESC);
