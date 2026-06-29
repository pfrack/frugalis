---
id: replace-sqlx
title: Replace sqlx with a better database library
status: impl_reviewed
created: 2026-06-28
updated: 2026-06-29
---

# Replace sqlx

Research and migrate from sqlx 0.8 to a modern Rust database library that eliminates
code duplication between Postgres and SQLite backends, provides compile-time safety,
and reduces boilerplate.
