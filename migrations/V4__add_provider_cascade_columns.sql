-- Migration 004: Add provider fallback/cascade columns for observability
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS provider_attempts SMALLINT DEFAULT 1;
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS final_provider TEXT;
