-- Migration 003: Add prompt_char_count column for cost estimation
ALTER TABLE inferences ADD COLUMN IF NOT EXISTS prompt_char_count INTEGER;
