-- Migration 005: Add token-usage and Claude Code attribution columns.
-- All columns are nullable so existing rows and the memory backend stay valid
-- (existing rows simply have NULL token/attribution data). Additive — zero
-- downtime, no backfill.
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS input_tokens INTEGER;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS output_tokens INTEGER;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS cache_read_tokens INTEGER;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS cache_creation_tokens INTEGER;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS client_session_id TEXT;
