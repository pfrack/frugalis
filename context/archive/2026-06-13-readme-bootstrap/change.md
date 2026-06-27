---
change_id: readme-bootstrap
title: Bootstrap README.md for the Cerebrum repository
status: archived
created: 2026-06-13
updated: 2026-06-27

archived_at: 2026-06-27T00:00:00Z
---

## Notes

The repository had no top-level `README.md`. The user requested one that covers
operator + developer needs comprehensively (~400–600 lines).

Inputs synthesized:
- `Cargo.toml`, `src/` modules, `config.toml`, `render.yaml`, `.github/workflows/deploy.yml`
- `openapi/completions.yaml`, `migrations/`, `scripts/manual_tests.sh`
- `templates/base.html` + `templates/dashboard/*`, `static/dashboard.css`
- `data/fewshot_bootstrap.yaml`
- `context/foundation/{prd,tech-stack,lessons}.md` and recent changes
- `AGENTS.md` (conventions), `context/changes/**/plan.md` (architecture rationale)

Deliverables:
1. `README.md` at repo root (the user-facing artifact)
2. `context/changes/readme-bootstrap/research.md` (per `/10x-research` protocol)
