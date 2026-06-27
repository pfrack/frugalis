# Rename Cerebrum → Frugalis — Implementation Plan

## Overview

Rename the project from "Cerebrum" to "Frugalis" across all source code, configuration, infrastructure, templates, scripts, and documentation. Archives (`context/archive/`) are left untouched as historical records. GitHub repo rename is handled separately after this lands.

## Current State Analysis

- 123 files contain "cerebrum" (648 total mentions)
- ~55 mentions in 7 source files, ~13 in config/infra, ~3 in templates, ~67 in scripts, ~21 in README
- ~300+ in context/archive/ (left untouched)
- Public API headers: `X-Cerebrum-Category` / `X-Cerebrum-Model` — renamed to `X-Frugalis-*`
- No external clients depend on the old headers (solo-dev project)
- GitHub workflows have zero "cerebrum" references — safe

### Key Discoveries:

- `src/main.rs:2135-2147` + `src/main.rs:2864-2876`: Header parsing uses lowercase string `"x-cerebrum-category"` / `"x-cerebrum-model"` — two handler sites
- `src/telemetry.rs:114-130`: OTel metric names use `cerebrum.` prefix
- `src/config.rs:63` + `src/persistence.rs:69`: Default SQLite path `./cerebrum.db` and in-memory URI `sqlite:file:cerebrum?mode=memory&cache=shared`
- `src/auth.rs:216`: Basic auth realm string `"cerebrum-dashboard"`
- `src/main.rs:799,845`: Synthetic model name `"cerebrum-optimized"`
- `justfile:205`: Cleanup globs reference `cerebrum-config-*`, `cerebrum-test-*`, `cerebrum.db`

## Desired End State

Every user-facing and developer-facing reference says "Frugalis" (or "frugalis" for identifiers). The binary is `frugalis`, the crate is `frugalis`, the dashboard says "Frugalis", headers are `X-Frugalis-*`, OTel metrics are `frugalis.*`, and `cargo build` produces `target/release/frugalis`. Archives remain unchanged.

Verification: `grep -ri cerebrum src/ templates/ Cargo.toml docker-compose.yml render.yaml Dockerfile justfile config.toml init_template.toml README.md AGENTS.md manual-test/ scripts/` returns zero matches.

## What We're NOT Doing

- Renaming GitHub repo (separate step after merge)
- Touching `context/archive/**` files
- Touching `context/changes/bootstrap-verification/`
- Renaming the local filesystem directory (`~/code/cerebrum`) — that's a user preference
- Database migration (the SQLite filename change means a fresh DB; old `cerebrum.db` still works if user points `sqlite_path` at it)

## Implementation Approach

Mechanical find-and-replace organized by layer. Each phase is independently verifiable via `cargo build` / `cargo test` (phase 1) or grep (phases 2-3). No code logic changes — only string replacements.

### Addendum (post-review): `src/test_util.rs`

During Phase 1, the duplicate inline `EnvGuard` test structs in `src/main.rs` were extracted into a shared `src/test_util.rs` module. This was a benign DRY refactoring that was not in the original plan. It is kept; this addendum documents it retroactively.

## Phase 1: Source Code

### Overview

Rename all "cerebrum" references in `.rs` files. This is the critical phase — it affects compilation, runtime behavior, and the API contract.

### Changes Required:

#### 1. Crate name

**File**: `Cargo.toml`

**Intent**: Change the crate/binary name from `cerebrum` to `frugalis`.

**Contract**: `name = "frugalis"` (line 2)

#### 2. Main application strings and headers

**File**: `src/main.rs`

**Intent**: Replace all "cerebrum"/"Cerebrum" string literals — CLI help text, log messages, header names, synthetic model name, panic message, OTel service name, test assertions.

**Contract**: Case-sensitive replacements:
- `"cerebrum"` → `"frugalis"` (lowercase: CLI usage, OTel init, header lookups, log messages)
- `"Cerebrum"` → `"Frugalis"` (capitalized: panic message, test assertions)
- `"cerebrum-optimized"` → `"frugalis-optimized"` (model name in synthetic responses)
- `"x-cerebrum-category"` → `"x-frugalis-category"` (header parsing, both handler sites)
- `"x-cerebrum-model"` → `"x-frugalis-model"` (header parsing, both handler sites)
- `"X-Cerebrum-Category"` → `"X-Frugalis-Category"` (log messages, doc comments)
- `"X-Cerebrum-Model"` → `"X-Frugalis-Model"` (doc comments)

#### 3. Quickstart wizard

**File**: `src/quickstart.rs`

**Intent**: Rename welcome message, default config path, and start command references.

**Contract**:
- `"cerebrum quickstart"` → `"frugalis quickstart"`
- `"./cerebrum-config.toml"` → `"./frugalis-config.toml"`
- `"cerebrum --quickstart"` → `"frugalis --quickstart"` (doc comment)
- `"cerebrum-quickstart-test"` → `"frugalis-quickstart-test"` (test temp dir prefix)
- Start command output: `CONFIG_PATH={} cerebrum` → `CONFIG_PATH={} frugalis`

#### 4. Telemetry metrics

**File**: `src/telemetry.rs`

**Intent**: Rename OTel metric name prefix from `cerebrum.*` to `frugalis.*`.

**Contract**: Replace `"cerebrum.` with `"frugalis.` in all 4 metric definitions (lines 114, 119, 125, 130).

#### 5. Intent classifier comments

**File**: `src/intent_classifier.rs`

**Intent**: Update comments referencing the old name/header.

**Contract**:
- `x-cerebrum-category` → `x-frugalis-category` (doc comment)
- `"Cerebrum"` → `"Frugalis"` (code comments, 2 occurrences)

#### 6. Config defaults

**File**: `src/config.rs`

**Intent**: Rename default SQLite path from `cerebrum.db` to `frugalis.db`.

**Contract**: `"./cerebrum.db"` → `"./frugalis.db"` (line 63 + line 318 in test)

#### 7. Persistence

**File**: `src/persistence.rs`

**Intent**: Rename SQLite path references and in-memory URI.

**Contract**:
- Doc comment: `./cerebrum.db` → `./frugalis.db`
- In-memory shared-cache URI: `"sqlite:file:cerebrum?mode=memory&cache=shared"` → `"sqlite:file:frugalis?mode=memory&cache=shared"`

#### 8. Auth realm

**File**: `src/auth.rs`

**Intent**: Rename the Basic auth realm.

**Contract**: `"cerebrum-dashboard"` → `"frugalis-dashboard"` (line 216)

### Success Criteria:

#### Automated Verification:

- Build succeeds: `cargo build --release`
- All tests pass: `cargo test`
- Binary name is correct: `ls target/release/frugalis`
- No cerebrum references in src: `grep -ri cerebrum src/` returns nothing

#### Manual Verification:

- Run `./target/release/frugalis --help` — output says "frugalis"
- Dashboard shows "Frugalis Dashboard" in browser title

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Config & Infrastructure

### Overview

Rename in all config, docker, and deployment files.

### Changes Required:

#### 1. Docker Compose

**File**: `docker-compose.yml`

**Intent**: Rename service and volume from `cerebrum` to `frugalis`.

**Contract**:
- Service name: `cerebrum:` → `frugalis:`
- Volume name: `cerebrum-postgres-data` → `frugalis-postgres-data`

#### 2. Render deployment

**File**: `render.yaml`

**Intent**: Rename service name and start command.

**Contract**:
- `name: cerebrum` → `name: frugalis`
- `startCommand: ./target/release/cerebrum` → `startCommand: ./target/release/frugalis`

#### 3. Dockerfile

**File**: `Dockerfile`

**Intent**: Rename binary references in build and runtime stages.

**Contract**:
- `--bin cerebrum` → `--bin frugalis`
- `/app/target/release/cerebrum` → `/app/target/release/frugalis`
- `/usr/local/bin/cerebrum` → `/usr/local/bin/frugalis` (COPY dest + ENTRYPOINT)

#### 4. Config file

**File**: `config.toml`

**Intent**: Rename header comment and commented-out SQLite path.

**Contract**:
- `# Cerebrum Configuration` → `# Frugalis Configuration`
- `"./cerebrum.db"` → `"./frugalis.db"` (commented example)

#### 5. Init template

**File**: `init_template.toml`

**Intent**: Rename all 5 references in comments.

**Contract**: Replace `cerebrum` → `frugalis` and `Cerebrum` → `Frugalis` in comment lines (lines 1, 5, 7, 15, 23).

#### 6. Justfile

**File**: `justfile`

**Intent**: Rename recipe comments and temp file cleanup globs.

**Contract**:
- Comments: `# Run cerebrum with...` → `# Run frugalis with...` (4 occurrences)
- Cleanup: `cerebrum-config-*` → `frugalis-config-*`, `cerebrum-test-*` → `frugalis-test-*`, `cerebrum_test_*` → `frugalis_test_*`, `cerebrum.db` → `frugalis.db`

### Success Criteria:

#### Automated Verification:

- No cerebrum in config files: `grep -ri cerebrum Cargo.toml docker-compose.yml render.yaml Dockerfile config.toml init_template.toml justfile` returns nothing
- Docker builds: `docker build -t frugalis .` (if Docker available)

#### Manual Verification:

- `docker-compose config` shows valid YAML with `frugalis` service name

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Templates, Scripts & Docs

### Overview

Rename in HTML templates, test scripts, and documentation.

### Changes Required:

#### 1. Base template

**File**: `templates/base.html`

**Intent**: Rename page title and navigation brand text.

**Contract**:
- `<title>Cerebrum Dashboard</title>` → `<title>Frugalis Dashboard</title>`
- Nav text: `Cerebrum` → `Frugalis`

#### 2. Dashboard index

**File**: `templates/dashboard/index.html`

**Intent**: Rename welcome message.

**Contract**: `Welcome to Cerebrum` → `Welcome to Frugalis`

#### 3. Manual test library

**File**: `manual-test/lib.sh`

**Intent**: Rename binary path, log/config file prefixes.

**Contract**:
- Comment: `Cerebrum manual` → `Frugalis manual`
- `BINARY="./target/release/cerebrum"` → `BINARY="./target/release/frugalis"`
- `/tmp/cerebrum-test-` → `/tmp/frugalis-test-`
- `/tmp/cerebrum-config-` → `/tmp/frugalis-config-`

#### 4. Manual test runner

**File**: `manual-test/run.sh`

**Intent**: Replace all `cerebrum` references (56 occurrences — mostly header names in curl commands and comments).

**Contract**: Global case-sensitive replacements:
- `x-cerebrum-category` → `x-frugalis-category`
- `x-cerebrum-model` → `x-frugalis-model`
- `X-Cerebrum-Category` → `X-Frugalis-Category`
- `X-Cerebrum-Model` → `X-Frugalis-Model`
- `cerebrum` → `frugalis` (remaining: binary name, config paths, comments)
- `Cerebrum` → `Frugalis` (capitalized occurrences in comments)

#### 5. Scripts

**File**: `scripts/manual_tests.sh`

**Intent**: Rename the single reference.

**Contract**: `cerebrum` → `frugalis`

#### 6. README

**File**: `README.md`

**Intent**: Rename all 21 occurrences — title, badges, description, commands, paths.

**Contract**: Global replacements:
- `Cerebrum` → `Frugalis` (project name in prose)
- `cerebrum` → `frugalis` (binary name, paths, URLs, code blocks)
- Badge URLs: `pfrack/cerebrum` → `pfrack/frugalis` (will redirect after repo rename, but correct them now)
- Clone URL: `github.com/pfrack/cerebrum.git` → `github.com/pfrack/frugalis.git`

#### 7. AGENTS.md

**File**: `AGENTS.md`

**Intent**: Rename the single project description reference.

**Contract**: `Cerebrum is a Rust/Axum gateway...` → `Frugalis is a Rust/Axum gateway...`

#### 8. Foundation docs

**File**: `context/foundation/roadmap.md`

**Intent**: Rename project references in active roadmap (this is a living document, not archive).

**Contract**: Replace `Cerebrum` → `Frugalis` and `cerebrum` → `frugalis` throughout.

#### 9. Foundation infra doc

**File**: `context/foundation/infrastructure.md`

**Intent**: Rename in active infrastructure documentation.

**Contract**: Replace `Cerebrum` → `Frugalis` and `cerebrum` → `frugalis` throughout.

### Success Criteria:

#### Automated Verification:

- Zero matches: `grep -ri cerebrum templates/ manual-test/ scripts/ README.md AGENTS.md context/foundation/` returns nothing
- Full project check: `grep -ri cerebrum src/ templates/ Cargo.toml docker-compose.yml render.yaml Dockerfile justfile config.toml init_template.toml README.md AGENTS.md manual-test/ scripts/ context/foundation/` returns nothing

#### Manual Verification:

- README reads coherently with "Frugalis" throughout
- `manual-test/run.sh` executes against running server (headers accepted)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests:

- `cargo test` — all existing tests pass (header names updated in test assertions)
- Key tests to watch: skip-classify header tests in main.rs (lines ~6241, ~7623)

### Integration Tests:

- Manual test suite: `./manual-test/run.sh` completes without failures
- Docker build: `docker build -t frugalis .` succeeds

### Manual Testing Steps:

1. `cargo build --release && ./target/release/frugalis --help` — shows "frugalis" in usage
2. Start server, visit `/dashboard/` — title bar shows "Frugalis Dashboard"
3. Send request with `X-Frugalis-Category: SYNTAX_FIX` + `X-Frugalis-Model: gpt-4o-mini` — verify skip-classify works
4. Check OTel metrics (if enabled) use `frugalis.*` prefix

## Migration Notes

- **SQLite users**: The default path changes from `./cerebrum.db` to `./frugalis.db`. Existing databases are NOT migrated — users should either rename the file or set `sqlite_path` explicitly in config.
- **Docker volumes**: `cerebrum-postgres-data` volume name changes. Existing Docker volumes with the old name must be manually renamed or data re-imported.
- **Render**: Service rename on Render may require a fresh deploy or service recreation.

## References

- Source files: `src/main.rs:2135-2147`, `src/main.rs:2864-2876` (header parsing)
- API contract: `X-Cerebrum-Category`/`X-Cerebrum-Model` → `X-Frugalis-Category`/`X-Frugalis-Model`
- Framing discussion: Session conversation (2026-06-27) — name chosen for uniqueness + cost-optimization semantics

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles.

### Phase 1: Source Code

#### Automated

- [x] 1.1 Build succeeds: `cargo build --release`
- [x] 1.2 All tests pass: `cargo test`
- [x] 1.3 Binary name correct: `ls target/release/frugalis`
- [x] 1.4 No cerebrum in src: `grep -ri cerebrum src/` returns nothing

#### Manual

- [x] 1.5 `./target/release/frugalis --help` output says "frugalis"
- [x] 1.6 Dashboard shows "Frugalis Dashboard"

### Phase 2: Config & Infrastructure

#### Automated

- [x] 2.1 No cerebrum in config files: `grep -ri cerebrum Cargo.toml docker-compose.yml render.yaml Dockerfile config.toml init_template.toml justfile` returns nothing
- [x] 2.2 Docker builds: `docker build -t frugalis .`

#### Manual

- [x] 2.3 `docker-compose config` valid YAML with frugalis service

### Phase 3: Templates, Scripts & Docs

#### Automated

- [x] 3.1 Zero matches in templates/scripts/docs: `grep -ri cerebrum templates/ manual-test/ scripts/ README.md AGENTS.md context/foundation/` returns nothing
- [x] 3.2 Full project check passes (zero matches in all non-archive files)

#### Manual

- [x] 3.3 README reads coherently
- [x] 3.4 Manual test suite runs against server with new header names (17/17 core tests pass; CC3 mock file infra issue pre-exists)
