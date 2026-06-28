-- Migration 002: Enforce request_id uniqueness on inferences
-- Prevents duplicate rows if upstream retry logic logs the same request twice.
ALTER TABLE inferences ADD CONSTRAINT inferences_request_id_unique UNIQUE (request_id);
