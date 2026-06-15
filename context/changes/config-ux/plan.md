# Config UX Improvement — Implementation Plan

## Overview

Make Cerebrum's configuration self-reliant and easy to use without explanation. Add CLI discoverability (`--help`, `--init`, `--quickstart`), fix the routing merge footgun with per-key additive semantics, fix broken routing examples, and add startup messaging that guides new users.

## Current State Analysis

The config system is architecturally sound (embedded defaults → overlay merge → validation) but has zero onboarding surface:

- CLI only recognizes `--validate`; unknown args → exit 2 (`src/main.rs:105-119`)
- `CONFIG_PATH` env var is the only way to load custom config — undiscoverable
- `merge_configs()` does full-replacement for routing — specifying one route silently drops all others (`src/config.rs:845-858`)
- All 4 routing examples use legacy flat `[CATEGORY]` format that can't work as `CONFIG_PATH` overlays
- No startup message hints at how to configure the system

### Key Discoveries:

- Manual arg parsing (no clap/structopt) — adding flags is straightforward
- `tracing` crate for logging — use `info!`/`warn!` macros for startup messages
- `routing: Option<HashMap<String, RouteEntry>>` — per-key merge is a natural fit
- Only 2 `merge_configs` tests exist — we'll add a new test for routing per-key merge
- Routing examples use `FALLBACK` key — needs to become `DEFAULT` to match runtime expectations
- `routing-nvidia-nim.toml` omits `endpoint` field — `RouteEntry.endpoint` is required (`String`), so these silently produce empty-string endpoints

## Desired End State

After this plan is complete:
1. `cerebrum --help` prints usage with all flags and env vars
2. `cerebrum --init [path]` writes a commented starter config (TOML, placeholder values) to stdout or file
3. `cerebrum --quickstart` walks the user through provider selection (OpenRouter, Anthropic, NVIDIA NIM, Ollama, Custom) and generates a working config
4. A user's overlay config with `[routing.COMPLEX_REASONING]` only overrides that one route — all others inherit from embedded defaults
5. `routing_examples/` contains valid `CONFIG_PATH`-compatible overlay files
6. On startup without `CONFIG_PATH`, a single info log points users to `--init`

Verification: run `cargo build`, `cargo test`, then manually test `--help`, `--init`, `--quickstart`, and an overlay config with a single route override.

## What We're NOT Doing

- Adding a CLI framework (clap/structopt) — manual parsing is sufficient for 4 flags
- Changing merge semantics for categories/negative_patterns — full-replacement stays (partial category sets are dangerous)
- Provider-specific `--init` templates (e.g., `--init --provider openrouter`) — the wizard covers this
- YAML output from `--init`/`--quickstart` — TOML only, matching embedded config
- Config validation beyond what `--validate` already does

## Implementation Approach

Work bottom-up: CLI parsing first (unblocks all commands), then `--init` (simplest command), then `--quickstart` (builds on `--init` template knowledge), then the merge fix (deepest change), then examples + messaging (cleanup). Each phase is independently testable.

## Critical Implementation Details

**Timing & lifecycle** — The `--help`, `--init`, and `--quickstart` flags must exit before tracing is initialized (they write to stdout/stderr directly). Only `--validate` and normal server startup proceed past the arg-parsing block. This means these commands use `eprintln!`/`println!` — not `info!`.

---

## Phase 1: CLI Foundation — `--help` and Arg Parsing Refactor

### Overview

Restructure the manual arg parsing in `main.rs` to support `--help`, `--init [PATH]`, and `--quickstart`. Add `--help` output. The new flags are "early exit" — they print and terminate before any config loading or server startup.

### Changes Required:

#### 1. CLI arg parsing

**File**: `src/main.rs`

**Intent**: Expand the `while` loop at lines 105-119 to recognize `--help`, `--init`, and `--quickstart`. Each new flag sets a mode enum/variable and breaks out of the loop. `--init` optionally consumes the next positional arg as a path.

**Contract**: After parsing, the code dispatches on the mode: `Help` → print help and exit 0; `Init(Option<String>)` → handled in Phase 2; `Quickstart` → handled in Phase 3; `Validate` → existing behavior; `Run` (default) → existing server startup.

#### 2. `--help` output

**File**: `src/main.rs`

**Intent**: Print a concise usage block covering all flags and environment variables, then exit 0.

**Contract**: Output matches this structure:
```
cerebrum — intent-aware routing gateway

USAGE:
    cerebrum [OPTIONS]

OPTIONS:
    --help         Show this help
    --init [PATH]  Generate a starter config (default: stdout)
    --quickstart   Interactive setup wizard
    --validate     Validate configuration and exit

ENVIRONMENT:
    CONFIG_PATH              Path to config overlay (TOML or YAML)
    PROXY_API_BEARER_TOKEN   Required for proxy routes
    DASHBOARD_BASIC_USER     Required for dashboard access
    DASHBOARD_BASIC_PASSWORD Required for dashboard access
```

### Success Criteria:

#### Automated Verification:

- Build succeeds: `cargo build`
- `cargo test` passes (no regressions)
- `./target/debug/cerebrum --help` prints usage and exits 0
- Unknown args still exit with code 2

#### Manual Verification:

- Help text is readable and accurate

---

## Phase 2: `--init` Command + Template

### Overview

Create an embedded init template with placeholder routing values and heavy comments. Implement `--init [path]` that writes it to stdout (default) or to a file path.

### Changes Required:

#### 1. Init template file

**File**: `init_template.toml` (new, project root — will be `include_str!`'d)

**Intent**: Provide a minimal, heavily-commented starter config that covers only routing (the most common customization). All 5 categories listed with placeholder values. Comments explain the full-replacement caveat is gone (per-key merge), what provider_type options exist, and which env vars are needed.

**Contract**: ~30 lines of TOML. Sections: `[routing.DEFAULT]`, `[routing.FILE_READING]`, `[routing.SYNTAX_FIX]`, `[routing.COMPLEX_REASONING]`, `[routing.CASUAL]`. Each entry has `model`, `endpoint`, `provider_type`, `api_key_env` with placeholder values. Header comment explains usage: `CONFIG_PATH=this-file.toml cerebrum`.

#### 2. `--init` implementation

**File**: `src/main.rs`

**Intent**: When mode is `Init(path)`, load the embedded template and either write it to the given path or print to stdout. Exit 0 on success, exit 1 with error message on file write failure.

**Contract**: `const INIT_TEMPLATE: &str = include_str!("../init_template.toml");` — write to path if provided (create parent dirs, refuse to overwrite existing file without `--force`), otherwise `print!("{}", INIT_TEMPLATE)`.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes
- `./target/debug/cerebrum --init` outputs valid TOML to stdout
- `./target/debug/cerebrum --init /tmp/test-cerebrum.toml` creates the file
- Running `--init` on an existing file exits 1 with error (no overwrite)

#### Manual Verification:

- Template comments are clear and actionable
- A user can fill in values and use the file as `CONFIG_PATH`

---

## Phase 3: `--quickstart` Interactive Wizard

### Overview

Add a stdin-driven setup wizard that prompts the user to select a provider, enter model names and API key env var, then generates a complete routing config and writes it to a file.

### Changes Required:

#### 1. Wizard module

**File**: `src/quickstart.rs` (new)

**Intent**: Encapsulate the interactive wizard logic: print prompts, read stdin lines, build a TOML string from user answers, write to file. Supports providers: OpenRouter, Anthropic, NVIDIA NIM, Ollama, Custom.

**Contract**: Public function `pub fn run_quickstart() -> Result<(), String>`. Flow:
1. Print provider menu (numbered list: 1=OpenRouter, 2=Anthropic, 3=NVIDIA NIM, 4=Ollama, 5=Custom)
2. Read choice → set endpoint/provider_type defaults for that provider
3. Prompt for model name(s) — offer "use same model for all?" shortcut or per-category models
4. Prompt for API key env var name (skip for Ollama)
5. Prompt for output file path (default: `./cerebrum-config.toml`)
6. Generate TOML with all 5 routing categories filled in
7. Write file (refuse overwrite unless confirmed)

#### 2. Wire into main

**File**: `src/main.rs`

**Intent**: Add `mod quickstart;` and call `quickstart::run_quickstart()` when mode is `Quickstart`.

**Contract**: On error, print message to stderr and exit 1. On success, print the path and a hint: `Config written to {path}. Start with: CONFIG_PATH={path} cerebrum`.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes
- Module compiles without warnings

#### Manual Verification:

- Run `./target/debug/cerebrum --quickstart` interactively
- Select each provider type and verify generated TOML is valid
- Verify generated config works with `CONFIG_PATH=<generated> cerebrum --validate`
- Verify Custom provider path works with arbitrary endpoint/model/provider_type

---

## Phase 4: Per-Key Additive Merge for Routing

### Overview

Change `merge_configs()` so that `routing` merges per-key (overlay entries override matching keys; unmentioned keys inherit from base) instead of full-replacement. This is the core UX fix — users can now specify only the routes they want to change.

### Changes Required:

#### 1. Merge logic change

**File**: `src/config.rs`

**Intent**: Replace the full-replacement line for routing with per-key insertion. An overlay with `[routing.COMPLEX_REASONING]` only overrides that one entry; the other 4 routes from embedded defaults remain.

**Contract**: Change the routing merge block (currently `if let Some(v) = overlay.routing { base.routing = Some(v); }`) to iterate overlay entries and insert them into the base routing map individually. If base routing is `None`, initialize it first.

```rust
if let Some(overlay_routing) = overlay.routing {
    let base_routing = base.routing.get_or_insert_with(HashMap::new);
    for (key, entry) in overlay_routing {
        base_routing.insert(key, entry);
    }
}
```

#### 2. Test for per-key merge

**File**: `src/config.rs` (test module)

**Intent**: Add a test proving that a routing overlay with one entry preserves the other base routes.

**Contract**: Test function `merge_configs_routing_per_key_merge` — base has DEFAULT + FILE_READING routes, overlay has only FILE_READING with different model. After merge, DEFAULT is unchanged and FILE_READING has the overlay's model.

#### 3. Update init template comment

**File**: `init_template.toml`

**Intent**: Update the header comment to reflect that users only need to specify routes they want to override — unmentioned routes inherit from built-in defaults.

**Contract**: Replace "ALL categories must be listed" with "Only override what you need — unmentioned routes use built-in defaults."

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes (including new routing merge test)
- Existing `merge_configs_overrides_categories` test still passes (categories stays full-replacement)
- Existing `merge_configs_shallow_merge_server` test still passes

#### Manual Verification:

- Create an overlay with only `[routing.COMPLEX_REASONING]` and verify other routes still work via `--validate`

---

## Phase 5: Fix routing_examples/ + Startup Messaging

### Overview

Rewrite all 4 routing example files to use the correct `[routing.CATEGORY]` nested format compatible with `CONFIG_PATH` overlays. Add a startup info log when no `CONFIG_PATH` is set. Rename `FALLBACK` → `DEFAULT` in all examples.

### Changes Required:

#### 1. Rewrite routing examples

**Files**: `routing_examples/routing_unreachable.toml`, `routing_examples/routing-manual-tests.toml`, `routing_examples/routing-openrouter.toml`, `routing_examples/routing-nvidia-nim.toml`

**Intent**: Convert from legacy flat `[CATEGORY]` format to `[routing.CATEGORY]` format. Rename `FALLBACK` to `DEFAULT`. Add a header comment explaining usage as `CONFIG_PATH` overlay. Add missing `endpoint` fields where absent (the nvidia-nim file omits them).

**Contract**: Each file becomes a valid `ConfigRoot` partial overlay. Table headers become `[routing.FILE_READING]`, `[routing.COMPLEX_REASONING]`, `[routing.SYNTAX_FIX]`, `[routing.CASUAL]`, `[routing.DEFAULT]`. All entries have `model`, `endpoint`, `provider_type`; `api_key_env` where applicable.

#### 2. Startup info log

**File**: `src/main.rs`

**Intent**: After config loading completes (post-merge), if `CONFIG_PATH` was not set, emit an info-level log hinting at `--init`.

**Contract**: `info!("No CONFIG_PATH set — using embedded defaults. Run `cerebrum --init` to generate a starter config.");` — placed after tracing is initialized and after the config merge block.

#### 3. Route summary log

**File**: `src/main.rs`

**Intent**: After routing config is extracted, log which routes are active so users can verify their overlay took effect.

**Contract**: `info!("Routes active: {}", routes.keys().sorted().join(", "));` — requires collecting keys from the routing HashMap. Place near existing classifier init logs.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo test` passes
- Each routing example parses as valid `ConfigRoot`: `toml::from_str::<ConfigRoot>(include_str!("../routing_examples/..."))` in a test or manual check
- `cargo build` with `CONFIG_PATH=routing_examples/routing-openrouter.toml` doesn't panic on validate

#### Manual Verification:

- Start cerebrum without CONFIG_PATH → see info log about `--init`
- Start with `CONFIG_PATH=routing_examples/routing-openrouter.toml` → see correct routes active log
- Verify route summary shows all 5 expected category names

---

## Testing Strategy

### Unit Tests:

- `merge_configs_routing_per_key_merge` — overlay one route, verify others preserved
- `merge_configs_routing_full_overlay` — overlay all routes, verify all replaced
- Verify init template is valid TOML: `toml::from_str::<ConfigRoot>(INIT_TEMPLATE)` should NOT parse (placeholders aren't valid) — but `toml::from_str::<toml::Value>(INIT_TEMPLATE)` should parse as valid TOML syntax

### Integration Tests:

- `--help` exits 0 with expected output
- `--init` produces output that is syntactically valid TOML
- A single-route overlay + embedded defaults → all 5 routes present after merge

### Manual Testing Steps:

1. Run `cerebrum --help` and verify output
2. Run `cerebrum --init > /tmp/test.toml` and inspect
3. Run `cerebrum --quickstart`, select each provider, verify generated file
4. Create minimal overlay with one route, start with `CONFIG_PATH`, verify all routes work
5. Verify startup log message when no CONFIG_PATH set

## Performance Considerations

None — all changes are in the startup path (runs once) or the merge function (runs once at startup). Zero runtime performance impact on request handling.

## References

- Research: `context/changes/config-ux/research.md`
- Config merge logic: `src/config.rs:815-870`
- CLI parsing: `src/main.rs:105-119`
- Config loading: `src/main.rs:139-160`
- Routing struct: `src/routing.rs:7-13`
- Lessons (relevant): "Log operational failures before falling back" — applies to startup messaging

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: CLI Foundation — --help and Arg Parsing Refactor

#### Automated

- [x] 1.1 Build succeeds: `cargo build`
- [x] 1.2 Tests pass: `cargo test`
- [x] 1.3 `--help` prints usage and exits 0
- [x] 1.4 Unknown args still exit with code 2

#### Manual

- [x] 1.5 Help text is readable and accurate

### Phase 2: --init Command + Template

#### Automated

- [ ] 2.1 Build succeeds: `cargo build`
- [ ] 2.2 Tests pass: `cargo test`
- [ ] 2.3 `--init` outputs valid TOML to stdout
- [ ] 2.4 `--init /tmp/test.toml` creates the file
- [ ] 2.5 `--init` on existing file exits 1

#### Manual

- [ ] 2.6 Template comments are clear and actionable
- [ ] 2.7 Filled template works as CONFIG_PATH

### Phase 3: --quickstart Interactive Wizard

#### Automated

- [ ] 3.1 Build succeeds: `cargo build`
- [ ] 3.2 Tests pass: `cargo test`
- [ ] 3.3 Module compiles without warnings

#### Manual

- [ ] 3.4 Wizard works for each provider type
- [ ] 3.5 Generated config works with --validate
- [ ] 3.6 Custom provider path works

### Phase 4: Per-Key Additive Merge for Routing

#### Automated

- [ ] 4.1 Build succeeds: `cargo build`
- [ ] 4.2 Tests pass including new merge test: `cargo test`
- [ ] 4.3 Existing merge tests still pass
- [ ] 4.4 New `merge_configs_routing_per_key_merge` test passes

#### Manual

- [ ] 4.5 Single-route overlay preserves other routes via --validate

### Phase 5: Fix routing_examples/ + Startup Messaging

#### Automated

- [ ] 5.1 Build succeeds: `cargo build`
- [ ] 5.2 Tests pass: `cargo test`
- [ ] 5.3 Each routing example parses as valid ConfigRoot
- [ ] 5.4 CONFIG_PATH with example file doesn't panic

#### Manual

- [ ] 5.5 No-CONFIG_PATH startup shows init hint log
- [ ] 5.6 CONFIG_PATH startup shows routes active log
- [ ] 5.7 Route summary shows all 5 category names
