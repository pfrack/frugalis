# Move All Config to File — Plan Brief

> Full plan: `context/changes/move-all-config-to-file/plan.md`  
> Research: `context/changes/move-all-config-to-file/research-config-format.md`

## What & Why

We are implementing a hybrid configuration system that supports both YAML and TOML formats with external pattern files. The motivation is to improve user experience for DevOps engineers (YAML familiarity) while solving regex escaping issues by moving patterns to simple `weight | regex` files where no escaping is needed. This maintains full backwards compatibility.

## Starting Point

Currently, `config.rs` (1559 lines) manually parses `toml::Value` trees, is TOML-only, and regex patterns are embedded inline in `config.toml` with TOML string quoting which can cause escaping bugs. All config is loaded at startup from an embedded `config.toml` with optional overlay via `CONFIG_PATH`.

## Desired End State

After this plan:
- Config can be in `.toml` or `.yaml/.yml` formats with identical semantics
- Categories can reference external pattern files (`patterns_file: "patterns/file_reading.patterns"`) using a line-based format `weight | regex` with zero escaping
- CLI flags `--validate` and `--migrate-config` are available to check config and convert old configs
- Existing `config.toml` continues to work unchanged
- The codebase is simpler (~250 lines of config.rs) due to serde derives

## Key Decisions Made

| Decision                       | Choice                                    | Why                                                                 | Source           |
|-------------------------------|-------------------------------------------|---------------------------------------------------------------------|------------------|
| Configuration formats         | TOML and YAML via serde (extension detection) | Users pick format; implementation reuses same structs              | Research: p214-223 |
| Pattern storage               | External files with custom `weight \| regex` format | Zero escaping, copy-paste from regex101 works                      | Research: p91-107 |
| YAML library                  | `serde-saphyr`                             | Pure Rust, active, no unsafe, recommended over deprecated forks   | Research: p228-237 |
| Migration approach            | Add flags to existing binary              | Simpler, no extra binary, matches infra                            | Research        |
| Configuration merging         | Top-level replacement for user sections   | Simpler than deep merge; matches current override_keys logic       | Analysis of main.rs     |
| Validation timing             | Startup-time before launching server     | Fail fast, prevents runtime surprises                              | Research        |
| Backward compatibility        | Keep TOML path always working            | Zero breakage for existing users                                   | Research        |

## Scope

**In scope:**
- Serde refactor of all config structs and loaders (Phase 1)
- YAML format detection and loading (Phase 2)
- External pattern file support (Phase 3)
- CLI flags `--validate` and `--migrate-config` (Phase 4)
- Full test coverage for new functionality
- Documentation through updated examples

**Out of scope:**
- Hot-reloading configuration at runtime
- Schema evolution or versioning beyond simple defaults
- UI for editing patterns
- Converting embedded `config.toml` to YAML in the repository

## Architecture / Approach

The implementation proceeds in four phases:

1. **Serde Derive Refactor**: Replace manual `toml::Value` access with `#[derive(Deserialize)]` on all config structs. This reduces code by ~75% and enables multi-format support trivially.

2. **Multi-Format Support**: Add `serde-saphyr` dependency. Introduce `load_config_from_path` that detects extension (`.toml` vs `.yaml/.yml`) and deserializes to a unified `ConfigRoot`. Reimplement config overlay merging using `ConfigRoot` instead of `toml::Value`.

3. **External Patterns**: Extend `CategoryConfig` with optional `patterns_file`. Add top-level `patterns_dir` (default `./patterns`). Implement simple line parser for `weight | regex` format. Resolve patterns during startup; compile all patterns (including external) for validation.

4. **CLI Commands**: Add argument parsing at top of `main()`. `--validate` loads and compiles all patterns then exits. `--migrate-config --input <in> --output <out> --extract-patterns <dir>` converts any config to YAML + pattern files.

## Phases at a Glance

| Phase      | What it delivers                            | Key risk                            |
|------------|---------------------------------------------|-------------------------------------|
| 1. Serde   | Clean, type-safe config deserialization    | Breaking test expectations         |
| 2. YAML    | Dual-format loading without duplication    | Format edge cases (TAI/unsafe)     |
| 3. Patterns| External pattern files and validation      | File path resolution               |
| 4. CLI     | `--validate` and `--migrate-config`        | Argument parsing edge cases        |

**Prerequisites:** None — this is pure code change, no external services required.  
**Estimated effort:** ~2-3 days across 4 phases; phases are somewhat independent but should be executed in order.

## Open Risks & Assumptions

- `serde-saphyr` may have subtle differences from `toml` crate in handling edge cases (inline tables, arrays). Will require thorough testing.
- Pattern file format's strict ` | ` delimiter could be problematic if user uses whitespace variations; we must document exactly. Assumption: this format is simple enough to be obvious.
- Migration tool produces YAML with `HashMap` categories; order may be unpredictable. We will sort by priority for deterministic output, but users may want alphabetical. Assumption: priority order is canonical.
- Overlay merging semantics must exactly match current `merge_toml_values`; any deviation could cause subtle config differences. We'll reimplement field-by-field replacement for override sections.
- No existing CLI parsing means we add minimal arg handling; if we later need more subcommands we may want a library, but for two flags hand-rolled is fine.

## Success Criteria (Summary)

- All existing unit and integration tests pass unchanged
- New YAML config formats load correctly and behave identically to TOML
- External pattern files compile and integrate into classification
- `cerebrum --validate` exits 0 on good config, 1 on errors
- Migration tool converts `config.toml` to `config.yaml` + `patterns/*.patterns` with identical runtime behavior
- Codebase simpler (~250 lines in config.rs vs 1559) with better type safety