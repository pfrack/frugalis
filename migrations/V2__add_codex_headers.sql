-- Add Responses API and Codex CLI header fields to the inferences table.
-- All new columns are nullable so existing rows remain valid.
-- Note: SQLite does not support IF NOT EXISTS on ALTER TABLE, and this
-- migration only runs once per database (tracked by refinery), so plain
-- ADD COLUMN is safe on both Postgres and SQLite.
ALTER TABLE inferences ADD COLUMN previous_response_id TEXT;
ALTER TABLE inferences ADD COLUMN codex_installation_id TEXT;
ALTER TABLE inferences ADD COLUMN codex_turn_state TEXT;
ALTER TABLE inferences ADD COLUMN codex_window_id TEXT;
ALTER TABLE inferences ADD COLUMN codex_turn_metadata TEXT;
