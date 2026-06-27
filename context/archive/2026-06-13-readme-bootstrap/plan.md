# Bootstrap README.md Implementation Plan

## Overview

Create a comprehensive (~400–600 line) `README.md` at the repo root that lets a new reader understand what Cerebrum is, run it, configure it, deploy it, and contribute to it — without having to read every source file. The tone is professional reference (neutral, precise, technical), operator-first in priority order, with full coverage of all major topics.

## Current State Analysis

The repository has no top-level `README.md`. The codebase is well-documented in source comments, `AGENTS.md`, and `context/foundation/` artifacts, but a new visitor must grep source files to understand the project. Research (`context/changes/readme-bootstrap/research.md`) exhaustively mapped source layout, config system, API surface, persistence, classification chain, deployment, test layout, and conventions.

### Key Discoveries:

- Three required env vars at runtime (`PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`); `DATABASE_URL` is optional.
- Three persistence backends: PostgreSQL, SQLite, in-memory — selected by env var and config.
- Pluggable classifier chain (`regex` → `fewshot` → `llm`); first non-`Fallback` wins.
- OpenAI-compatible API surface with three authenticated proxy routes + four dashboard pages.
- SSE streaming with keepalive and 2 KB upstream-error body cap.
- Privacy by construction: 200-char prompt snippet, never full prompt.
- Config is layered: embedded defaults → optional overlay file → env vars for secrets.
- OTel is opt-in experimental (requires `otel` Cargo feature + `OTEL_ENABLED=true`).

## Desired End State

A ~400–600 line `README.md` at the repo root that covers, in order: project overview and features, quick start with copy-paste commands, configuration reference, architecture overview, API routes table, dashboard walkthrough, deployment guide, development guide, and testing reference. The reader can go from zero to running the gateway in under 5 minutes by following the Quick Start section, and can find any reference topic without reading source files.

## What We're NOT Doing

- No API request/response examples beyond the routes table (deferred to `openapi/completions.yaml`)
- No in-depth tutorial or walkthrough (just reference docs)
- No architecture diagrams beyond text-based description
- No migration or changelog section
- No contributing guidelines beyond brief development guide
- No video or animated content

## Implementation Approach

Write the README sequentially from top to bottom across six phases, each phase producing a contiguous section of the final file. Each phase's output is appended to the running document. Phase 6 does a holistic polish pass over the assembled file.

File will be written directly to `README.md` at the repo root (`/home/pawel/code/cerebrum/README.md`).

---

## Phase 1: Title, Badges, Overview & Table of Contents

### Overview

Establish the top of the README: project name, build/version/license badges, a one-paragraph elevator pitch, a bullet-list of key features, and a table of contents.

### Changes Required:

**File**: `README.md`

**Intent**: Write lines 1–~60 covering the identity and first impression of the project.

**Contract**:
- H1 heading: `# Cerebrum`
- Badges row: GitHub Actions build status (from `/.github/workflows/deploy.yml`), Rust version (from `Cargo.toml`), license (from `Cargo.toml`)
- Elevator pitch (2–3 sentences): intent-aware LLM gateway, single Rust binary, classifies prompts and routes to cheapest acceptable upstream model
- Feature bullets (~8–12 items): intent-aware routing, pluggable classifier chain, OpenAI-compatible API, SSE streaming with keepalive, three persistence backends, operator dashboard, privacy-by-construction snippets, basic-auth/bearer-auth, config-driven routing, CI/CD → Render deploy, opt-in OTel
- Table of contents linking to all major sections

### Success Criteria:

#### Automated Verification:
- README.md exists at repo root
- File starts with `# Cerebrum`
- Table of contents entries link to valid section anchors in the plan
- All badge URLs reference real services/actions

#### Manual Verification:
- Elevator pitch accurately conveys the project's purpose within 3 sentences
- Feature list covers all major capabilities without misleading claims
- TOC structure feels natural to scan

---

## Phase 2: Quick Start & Configuration

### Overview

Write the getting-started section that gets a new user from zero to running the gateway. Prerequisites, environment variables, clone/build/run commands, configuration reference.

### Changes Required:

**File**: `README.md`

**Intent**: Append lines ~61–~200 covering Prerequisites, Quick Start, Environment Variables, Configuration, and Persistence Backend selection.

**Contract**:
- **Prerequisites**: Rust toolchain (min version from `Cargo.toml`), no other runtime deps
- **Quick Start**: Concrete shell commands — `git clone`, `cd cerebrum`, `PROXY_API_BEARER_TOKEN=... DASHBOARD_BASIC_USER=... DASHBOARD_BASIC_PASSWORD=... cargo run`, visit `http://localhost:8080/health`
- **Environment Variables** table: Variable, Required, Description, Default — covering all three required vars plus `DATABASE_URL`, `CONFIG_PATH`, `OTEL_ENABLED`
- **Configuration** subsection: layered config model (embedded defaults → `config.toml` overlay → env vars), `--validate` mode, YAML also accepted
- **Persistence**: three backends with selection order (DATABASE_URL → Postgres, sqlite config → SQLite, else in-memory)

### Success Criteria:

#### Automated Verification:
- Quick Start commands use correct env var names from `src/auth.rs` and `src/main.rs`
- `DATABASE_URL` is correctly marked as optional
- Persistence selection order matches `src/main.rs:343-412`

#### Manual Verification:
- A new user can copy-paste the Quick Start block and have the gateway running
- Configuration explanation accurately reflects the layered model
- Persistence backend descriptions are accurate and complete

---

## Phase 3: Architecture & Core Concepts

### Overview

Write the conceptual overview section: how Cerebrum routes requests, the classifier chain, the request flow, and the privacy/security invariants.

### Changes Required:

**File**: `README.md`

**Intent**: Append lines ~201–~340 covering Architecture, Classification, Request Flow, and Privacy & Security.

**Contract**:
- **Architecture**: Single-binary Axum gateway, one Tokio async runtime, persistence as the only side effect. Fire-and-forget `log_inference` with bounded semaphore
- **Intent-aware routing**: ClassificationTier enum (Regex | FewShot | Fallback), ClassifierChain iterates backends in order, first non-Fallback wins. Routing is data-driven from `config.toml`
- **Classifier backends**: Regex (built-in, default), FewShot (TF-IDF cosine similarity, feedback learning), LLM (opt-in)
- **Request flow**: Receive → authenticate (bearer/basic) → classify → select route → proxy upstream (SSE or buffered) → log inference → respond
- **SSE streaming**: Keepalive every 15s (configurable), 2 KB upstream-error body cap, JSON-escaped error events
- **Privacy**: 200-char `prompt_snippet` cap, `prompt_char_count` for cost math, never stores full prompt
- **Security**: Constant-time credential comparison, bearer for proxy routes, basic auth for dashboard

### Success Criteria:

#### Automated Verification:
- Classifier chain ordering matches `src/intent_classifier.rs:158-175`
- SSE invariants match `src/main.rs` streaming paths
- Privacy section correctly cites 200-char snippet cap from `src/persistence.rs:1118-1121`

#### Manual Verification:
- Architecture section is understandable without reading source
- Request flow description matches actual code paths
- Privacy guarantees are accurately described (no over-promising)

---

## Phase 4: API Reference & Dashboard

### Overview

Write the API surface table and the dashboard walkthrough.

### Changes Required:

**File**: `README.md`

**Intent**: Append lines ~341–~430 covering the API routes table, authentication model, and dashboard pages.

**Contract**:
- **Routes table**: Method, Path, Auth, Purpose — 9 rows matching the research findings (GET /health, POST /v1/chat/completions, POST /v1/classify, POST /v1/feedback, GET /dashboard/*, GET /dashboard/inferences, GET /dashboard/latency, GET /dashboard/savings, GET /dashboard/static/*)
- **Auth model**: Bearer token for proxy routes (set `PROXY_API_BEARER_TOKEN`), Basic auth for dashboard routes (set `DASHBOARD_BASIC_USER` + `DASHBOARD_BASIC_PASSWORD`)
- **Dashboard**: 4 pages — Overview, Inference Logs (paginated/filterable), Latency (avg + p99 per category), Savings (vs baseline model). Nav auto-generated from `PAGES` registry

### Success Criteria:

#### Automated Verification:
- Route table matches `openapi/completions.yaml` and `src/main.rs:1155-1194`
- Dashboard page count and names match `src/dashboard.rs:44-65`
- Auth model correctly describes bearer vs basic auth separation

#### Manual Verification:
- Routes table is scannable and accurate
- Dashboard description accurately reflects the current UI

---

## Phase 5: Operations & Development

### Overview

Write deployment, testing, development, and OTel sections.

### Changes Required:

**File**: `README.md`

**Intent**: Append lines ~431–~560 covering Deployment, CI/CD, Testing, Development, and OTel sections.

**Contract**:
- **Deployment**: `cargo build --release` → `target/release/cerebrum`. Render via `render.yaml` (web service, `cargo build --release` start command). Env vars for secrets (sync: false, set in dashboard)
- **CI/CD**: GitHub Actions on push to main — test auth, test routes_auth, test persistence, build release, POST Render deploy webhook
- **Testing**: `cargo test` for fast unit/integration tests, `cargo test slow_tests` for delay tests, `cargo test auth` / `cargo test routes_auth` / `cargo test persistence` for targeted test groups. Manual test harness at `scripts/manual_tests.sh`
- **Development**: Source layout overview (`src/` files and their roles), conventions (constant-time comparison, dashboard page recipe, config as data)
- **OTel**: Opt-in experimental — requires `otel` Cargo feature + `OTEL_ENABLED=true` env var. Behind `telemetry.rs`

### Success Criteria:

#### Automated Verification:
- Render deployment description matches `render.yaml`
- CI pipeline description matches `.github/workflows/deploy.yml`
- Test commands match `AGENTS.md` and `Cargo.toml`
- OTel is correctly described as opt-in experimental (feature-gated)

#### Manual Verification:
- Deployment section has enough detail for a solo operator to deploy
- Test section clearly explains the test hierarchy
- OTel's experimental status is clear and not misleading

---

## Phase 6: Polish & Final Review

### Overview

Complete the README with any remaining sections (FAQ, troubleshooting if needed), verify the line count is in the target range (~400–600), cross-reference all claims against source files, proofread for consistency, and finalize.

### Changes Required:

**File**: `README.md`

**Intent**: Write remaining ~20–40 lines for FAQ/troubleshooting/tail sections. Then do a full pass over the assembled file.

**Contract**:
- Document reaches ~400–600 lines
- All code references, env var names, route paths, config keys are fact-checked against source
- Consistent formatting (headings, tables, code blocks, lists)
- No broken TOC links
- No placeholder text or TODOs
- Professional reference tone throughout

### Success Criteria:

#### Automated Verification:
- Line count is between 350–650 (soft bounds)
- Markdown is valid (no broken formatting)
- All TOC internal links resolve to existing headings

#### Manual Verification:
- User reads the assembled README and confirms it covers what they need
- No factual errors in any section
- Tone is consistent across all sections
- README is scannable and well-organized for both operators and developers

---

## Testing Strategy

### Review Steps:
- Read the assembled README end-to-end after Phase 6
- Verify every factual claim against its source (env var names, route paths, config keys)
- Check for consistency in tense, terminology, and formatting
- Verify all external links (badges) are live

## Performance Considerations

No performance implications — documentation only. README should render instantly in any markdown viewer.

## Migration Notes

N/A — no existing README to migrate from.

## References

- Research: `context/changes/readme-bootstrap/research.md`
- PRD: `context/foundation/prd.md`
- Tech stack: `context/foundation/tech-stack.md`
- Lessons: `context/foundation/lessons.md`
- OpenAPI spec: `openapi/completions.yaml`
- Render config: `render.yaml`
- CI pipeline: `.github/workflows/deploy.yml`
- Source: `src/auth.rs`, `src/main.rs`, `src/config.rs`, `src/intent_classifier.rs`, `src/persistence.rs`, `src/dashboard.rs`, `src/routing.rs`, `src/telemetry.rs`

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Title, Badges, Overview & Table of Contents

#### Automated

- [x] 1.1 README.md created with H1 heading and badges — 77ce1a7
- [x] 1.2 Feature bullets and elevator pitch written — 77ce1a7
- [x] 1.3 Table of contents with valid section anchors — 77ce1a7

#### Manual

- [ ] 1.4 Elevator pitch and feature list reviewed for accuracy

### Phase 2: Quick Start & Configuration

#### Automated

- [x] 2.1 Quick Start section with copy-paste commands — ad3174b
- [x] 2.2 Environment variables table written — ad3174b
- [x] 2.3 Configuration and persistence sections written — ad3174b

#### Manual

- [ ] 2.4 Quick Start commands verified executable
- [ ] 2.5 Config/persistence descriptions reviewed for accuracy

### Phase 3: Architecture & Core Concepts

#### Automated

- [x] 3.1 Architecture overview section — 6ecb9fb
- [x] 3.2 Classifier chain and routing description — 6ecb9fb
- [x] 3.3 Request flow, SSE, privacy, and security sections — 6ecb9fb

#### Manual

- [ ] 3.4 Architecture section reviewed for technical accuracy

### Phase 4: API Reference & Dashboard

#### Automated

- [x] 4.1 Routes table written — 5ea3565
- [x] 4.2 Auth model section — 5ea3565
- [x] 4.3 Dashboard pages walkthrough — 5ea3565

#### Manual

- [ ] 4.4 Routes table and dashboard descriptions reviewed

### Phase 5: Operations & Development

#### Automated

- [x] 5.1 Deployment and CI/CD sections — 423ffd1
- [x] 5.2 Testing section with command examples — 423ffd1
- [x] 5.3 Development guide and OTel section — 423ffd1

#### Manual

- [ ] 5.4 Operations sections reviewed for accuracy

### Phase 6: Polish & Final Review

#### Automated

- [x] 6.1 FAQ/troubleshooting (if needed)
- [x] 6.2 Line count verified within target range
- [x] 6.3 Cross-reference check against source files

#### Manual

- [ ] 6.4 Full README end-to-end review and final approval
