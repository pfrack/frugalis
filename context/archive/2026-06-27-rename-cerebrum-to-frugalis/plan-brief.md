# Rename Cerebrum → Frugalis — Plan Brief

> Full plan: `context/changes/rename-cerebrum-to-frugalis/plan.md`

## What & Why

Rename the entire project from "Cerebrum" to "Frugalis" — a Latin word meaning "frugal" that directly communicates the tool's cost-optimization purpose. The current name collides with 7+ other "cerebrum" projects on GitHub (IdM framework, decentralized identity org, ML toolkits, neural network libs) making it hard to discover and distinguish.

## Starting Point

The project currently ships as a binary called `cerebrum`, with HTTP headers `X-Cerebrum-Category`/`X-Cerebrum-Model`, OTel metrics `cerebrum.*`, and all user-facing text referencing the old name. 123 files, 648 mentions total — but only ~70 in actively-maintained non-archive files.

## Desired End State

Every user-facing and developer-facing reference says "Frugalis"/"frugalis". The binary is `frugalis`, headers are `X-Frugalis-*`, metrics are `frugalis.*`, dashboard says "Frugalis". A grep across non-archive files returns zero "cerebrum" matches.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
| --- | --- | --- | --- |
| HTTP headers | Rename X-Cerebrum-* → X-Frugalis-* (clean break) | No external clients; clean break avoids legacy cruft | Plan |
| Archive files | Leave untouched | Historical documents; rewriting them has no operational value | Plan |
| Model name | Rename cerebrum-optimized → frugalis-optimized | Consistent branding in API responses | Plan |
| GitHub repo rename | Separately, after code lands | Keeps this plan focused on code; GitHub auto-redirects old URL | Plan |
| New name choice | "Frugalis" | Unique on crates.io + GitHub, communicates cost-optimization, 8 chars, Latin origin (timeless) | Frame (session) |

## Scope

**In scope:**
- All `.rs` source files (7 files, 55 mentions)
- Config/infra: Cargo.toml, docker-compose.yml, render.yaml, Dockerfile, config.toml, init_template.toml, justfile
- Templates: base.html, dashboard/index.html
- Scripts: manual-test/run.sh, manual-test/lib.sh, scripts/manual_tests.sh
- Docs: README.md, AGENTS.md, context/foundation/{roadmap,infrastructure}.md

**Out of scope:**
- `context/archive/**` (historical)
- GitHub repo rename (separate step)
- Local directory rename (`~/code/cerebrum`)
- Database migration (users rename file or set explicit path)

## Architecture / Approach

Purely mechanical find-and-replace. No logic changes. Three phases ordered by risk: source code first (compilation + tests validate immediately), then config/infra, then docs/scripts. Each phase is independently verifiable.

## Phases at a Glance

| Phase | What it delivers | Key risk |
| --- | --- | --- |
| 1. Source Code | Binary builds as `frugalis`, headers/metrics renamed | Test failures if a replacement is missed in assertions |
| 2. Config & Infrastructure | Docker/Render/compose all reference `frugalis` | Docker volume name change loses local dev data |
| 3. Templates, Scripts & Docs | All user-facing text says "Frugalis" | 56 replacements in run.sh — easy to miss one |

**Prerequisites:** None — straightforward text replacement on current main.
**Estimated effort:** ~1 session, single phase per commit.

## Open Risks & Assumptions

- Docker volume rename means local dev Postgres data won't auto-attach (recreate with `docker-compose up`)
- Badge URLs in README point to `pfrack/frugalis` which won't resolve until repo is renamed on GitHub
- Render service name change may require manual action in the Render dashboard

## Success Criteria (Summary)

- `grep -ri cerebrum` across all non-archive files returns zero matches
- `cargo test` passes with zero failures
- `./target/release/frugalis --help` prints correct usage
