# persistence-snippet-guardrails

- **created**: 2026-07-01
- **updated**: 2026-07-01
- **status**: implementing

## Summary

Test plan Phase 3: make async logging failure observable (not silent) and prove snippet
extraction holds across all 3 backends (memory/SQLite/Postgres) with PII guardrails.
Covers risks #5 (silent log_inference failure) and #6 (PII leakage through snippet extraction).
