# Bootstrap README.md — Plan Brief

> Full plan: `context/changes/readme-bootstrap/plan.md`
> Research: `context/changes/readme-bootstrap/research.md`

## What & Why

Cerebrum has no top-level README. A new visitor must grep source files to understand what it is, how to run it, or how to contribute. We're writing a comprehensive ~400–600 line README.md that covers operator and developer needs — from quick start to architecture to deployment.

## Starting Point

An empty repo root (no README.md). Research has exhaustively mapped every source file, config surface, API route, test layout, and deployment artifact. The PRD, tech-stack doc, and lessons.md provide additional context.

## Desired End State

A professional-reference README.md at the repo root that lets any reader go from "what is this?" to running the gateway in under 5 minutes, and serves as a complete reference for configuration, API, architecture, and operations.

## Key Decisions Made

| Decision | Choice | Why |
|---|---|---|
| Primary audience | Operator-first | Matches PRD solo-developer/operator persona — most readers need to run it, not extend it |
| Section coverage | Full coverage (all major topics) | Self-contained reference — reader rarely needs to check source files |
| API reference depth | Tabular summary | Quick reference matching OpenAPI spec; defers detail to YAML file |
| Tone | Professional reference | Neutral, precise — matches Rust/Axum ecosystem norms |
| Quick start | Full with copy-paste commands | Concrete shell commands so reader is running in < 5 min |
| OTel positioning | Opt-in experimental | Honest about maturity — requires Cargo feature + env var |

## Scope

**In scope:** Overview, quick start, config reference, architecture, API routes table, dashboard walkthrough, deployment (Render/CI/CD), testing guide, development guide, OTel (experimental)

**Out of scope:** Full API examples (deferred to OpenAPI spec), architecture diagrams, tutorials, contribution guidelines, changelog

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Title, Badges, Overview & TOC | Project identity, feature list, navigation | Getting the elevator pitch right is subjective |
| 2. Quick Start & Configuration | Copy-paste commands, env vars, persistence | Commands must be verified correct |
| 3. Architecture & Core Concepts | Classifier chain, request flow, privacy | Complex topic — accuracy vs readability tension |
| 4. API Reference & Dashboard | Routes table, auth, dashboard pages | Route table must match spec exactly |
| 5. Operations & Development | Deployment, CI/CD, testing, dev guide, OTel | Multiple sub-topics risk sprawl |
| 6. Polish & Final Review | FAQ, cross-reference check, line count, proofread | Last phase catches all residual issues |

**Estimated effort:** 1 session, 6 sequential phases (each adds to the running document)

## Open Risks & Assumptions

- README will stay in sync as the codebase evolves (no automated sync mechanism)
- Accuracy depends on careful cross-referencing against source at Phase 6

## Success Criteria (Summary)

- README.md exists at repo root, 400–600 lines
- Quick Start commands are copy-paste executable
- All env var names, route paths, and config keys match source
- Consistent professional-reference tone throughout
