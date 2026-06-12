# Move All Config to File — Implementation Plan

## Overview

This plan implements the research document's recommendation: a hybrid configuration system that supports **both YAML and TOML formats** via serde derives, and **externalizes regex patterns** into simple pattern files with zero escaping requirements. The goal is to improve user experience for non-Rust DevOps engineers while maintaining strict regex correctness.

**Key deliverables:**
- Phase 1: Replace manual TOML parsing (1559 lines) with `#[derive(Deserialize)]` structs
- Phase 2: Add YAML support via `serde-saphyr` using format detection by extension
- Phase 3: Add `patterns_file` field support and external pattern file loader
- Phase 4: Add `--validate` CLI flag to validate config schema + regex patterns

## Current State Analysis

The current `config.rs` (1559 lines) manually parses `toml::Value` trees using `.as_table()`, `.get()`, `.as_str()`, etc. This makes it:
- Monolithic and hard to maintain
- Tied strictly to TOML format
- Difficult to add new config fields (easy to miss error handling)
- Error-prone for future format changes

The `config.toml` contains:
- ~180 lines total
- 40+ regex patterns with weights across 4 categories
- Routing tables (category → model/endpoint/provider)
- Auth providers, negative patterns, model costs
- All non-secret configuration

**Problem:** Regex patterns must be escaped for TOML (single-quoted strings) which is brittle and user-hostile. YAML would require double-escaping (`\\b`) which is even worse. Research evaluated 8 formats and concluded the real UX win is **externalizing patterns**.

### Key Discoveries

- src/config.rs:config.rs is the sole configuration loader — all components (main.rs, persistence.rs, intent_classifier.rs) depend on it
- src/main.rs loads config at startup in a fixed sequence, merges overlay from CONFIG_PATH
- All config loaders follow the same pattern: `load_*_from_value(root: &toml::Value) -> Struct`
- Config structs already exist but lack `Deserialize` derives; fields are manually extracted
- Tests are extensive: unit tests in config.rs (lines 1053-1559), integration tests in main.rs
- No existing CLI argument parsing — only env vars and config file
- Pattern compilation happens in `RegexClassifier::from_env` (intent_classifier.rs:491) via `RegexSet::new(&patterns)`

## Desired End State

After this plan, the configuration system will:

1. **Multi-format support**: Accept `.toml` or `.yaml`/`.yml` config files with identical semantics. Same serde structs serve both formats.
2. **External pattern files**: Categories can reference `patterns_file: "patterns/file_reading.patterns"` instead of inline `patterns: [...]`.
3. **Zero-escaping pattern format**: Simple `weight | regex` lines, no string literal escaping issues.
4. **Validation**: `cerebrum --validate` checks config schema correctness (required fields, type constraints, structural validity) and compiles all regex patterns, reporting file:line on errors.
5. **Full backwards compatibility**: Existing `config.toml` with inline patterns continues to work indefinitely.

### Success Criteria

- All existing unit and integration tests pass unchanged
- New YAML config with inline patterns loads identically to TOML
- External pattern files compile correctly and integrate into classification scoring
- `--validate` exits 0 on success (schema + patterns valid), non-zero on any config/pattern error
- Documentation updated (research already covers format rationale)

## Implementation Approach

**High-level strategy:** Systematic serde refactor followed by feature additions.

1. **Serde Derive Refactor (Phase 1)**
   - Add `serde` derive macros to all config structs
   - Rename fields with `#[serde(rename_all = "snake_case")]` to match TOML keys
   - Replace each manual `load_*_from_value` with a single `Deserialize` implementation
   - Update tests to use `toml::from_str`/`serde_saphyr::from_str` instead of manual functions
   - Keep old loader functions as thin wrappers during transition, then remove

2. **YAML Support (Phase 2)**
   - Add `serde-saphyr` dependency (pure Rust, no unsafe, active)
   - Create `load_config_from_path(path: &str) -> Result<ConfigRoot, String>` that:
     - Detects format by extension: `.toml` → `toml::from_str`, `.yaml`/`.yml` → `serde_saphyr::from_str`
     - Returns unified `ConfigRoot` struct
   - Update `main.rs` to use `load_config_from_path` instead of embedded `config.toml` parsing
   - Add tests for YAML parsing with identical semantics

3. **External Pattern Files (Phase 3)**
   - Add `patterns_file: Option<String>` field to `CategoryConfig` (renamed from `patterns: Vec<PatternEntry>` to `patterns_source: PatternsSource`)
   - Define `enum PatternsSource { Inline(Vec<PatternEntry>), External(PathBuf)) }` with custom `Deserialize` that accepts either `patterns` or `patterns_file`
   - Add `patterns_dir: PathBuf` top-level config (defaults to `./patterns/`)
   - Implement `load_patterns_from_file(path: &str) -> Result<Vec<PatternEntry>, String>`: lines starting with `#` are comments, else split on first ` | `, parse weight, keep regex verbatim
   - In `RegexClassifier::from_env`, resolve all categories' pattern sources: if Inline use directly, if External read file and parse
   - Validate each regex compiles with `Regex::new`, report file:line if fails
   - Keep backward compatibility: Inline patterns remain the default if neither field is set

4. **Validation CLI (Phase 4)**
- Add `std::env::args().collect()` check at top of `main()`
- If `--validate` present:
- Load config (using overlay if CONFIG_PATH set)
- Validate config schema (required fields, type constraints, cross-references)
- Compile all patterns (including external files)
- Print success or all errors, exit with code 0 or 1
- If no flags, proceed with normal server startup

## Critical Implementation Details

### 1. Serde Renaming Strategy

The current TOML uses snake_case keys (e.g., `client_timeout_secs`). Since serde expects snake_case by default, we can use `#[serde(rename_all = "snake_case")]` on structs to auto-map. For fields with different names (e.g., `pattern_regex` → `regex`), use `#[serde(rename = "regex")]`.

### 2. Two-Tier Config Root

We need a unified `ConfigRoot` struct that contains all top-level sections:
```rust
#[derive(Deserialize)]
struct ConfigRoot {
    server: ServerConfig,
    http: HttpConfig,
    cors: Option<CorsConfig>,
    database: Option<DatabaseConfig>,
    persistence: Option<PersistenceSettings>,
    classifiers: Option<ClassifiersConfig>,
    regex_classifier: Option<RegexClassifierConfig>,
    llm_classifier: Option<LlmClassifierConfig>,
    categories: HashMap<String, CategoryConfig>,
    negative_patterns: Option<Vec<NegativePatternConfig>>,
    routing: Option<HashMap<String, RouteEntry>>,
    auth_providers: Option<Vec<AuthProviderConfig>>,
    model_costs: Option<HashMap<String, f64>>,
    baseline_model: Option<String>,
    classify_db_log: Option<bool>,
    dashboard: Option<DashboardConfig>,
}
```

The `load_config_from_path` function will deserialize directly into this struct.

### 3. Format Detection

Simple extension check:
```rust
fn detect_format(path: &str) -> Format {
    match Path::new(path).extension().and_then(|s| s.to_str()) {
        Some("yaml" | "yml") => Format::Yaml,
        Some("toml") => Format::Toml,
        _ => Format::Toml, // default to TOML for backward compat
    }
}
```

### 4. Pattern File Parsing

Each line format: `<weight> | <regex>` (strict).
- Skip empty lines and lines starting with `#` (full-line comments only)
- `split_once(" | ")` — the exact delimiter with single spaces; no tolerance for variations
- Parse weight as `u8`, regex as the remainder (verbatim, no escaping)
- On error: return `Err("line N: invalid format, expected '<weight> | <regex>'")`

Example valid line: `3 | (?i)\b(?:read|show)\s+file\b`

This format is immune to YAML/TOML escaping issues because the pattern file is a custom plain-text format.

### 5. Migration Tool Workflow

`cerebrum --migrate-config --input config.toml --output config.yaml --extract-patterns ./patterns/` should:

1. Load input config in original format (TOML or YAML)
2. Write YAML output:
   - All top-level sections copied
   - Categories section: for each category, replace `patterns: [...]` with `patterns_file: "patterns/<category>.patterns"`
   - Ensure YAML is readable ( Askama/serde_yaml formatting)
3. Write pattern files:
   - Iterate categories in defined order (prioritized)
   - For each `PatternEntry { regex, weight }`, write `{weight} | {regex}\n`
4. Preserve comments? The research suggests comments may be lost; that's acceptable. The resulting pattern files have self-documenting `# Format: weight | regex (verbatim)` header.

### 6. Validation Timing

Validate patterns at startup **before** launching the server or executing migration. This is the recommended approach because:
- Users get immediate feedback
- Prevents runtime compilation errors
- Simpler than lazy compilation

In `main.rs`, after constructing `RegexClassifier::from_env`, if it returns `Err`, log the error and exit with code 1 (or return error in validation mode). For validation mode, we should attempt to construct the classifier chain and report any regex errors.

### 7. Test Strategy

**Phase 1 (serde refactor):** All existing tests must pass unchanged. The loader functions can delegate to serde deserialization to preserve the same error semantics.

**Phase 2 (YAML):** Add new tests:
- `test_yaml_config_equals_toml()`: Load same config as YAML and TOML, compare structs
- `test_yaml_patterns_parse_identically()`

**Phase 3 (external patterns):** Add tests:
- `test_load_patterns_from_file_success()`
- `test_load_patterns_from_file_invalid_weight()`
- `test_load_patterns_from_file_with_comments()`
- `test_external_patterns_compile()`
- Integration test: category with `patterns_file` loads and matches correctly

**Phase 4 (CLI):** Add tests:
- `test_validate_success_exits_0()`
- `test_validate_failure_exits_1()`
- `test_migrate_config_creates_yaml_and_pattern_files()`

We'll run `cargo test` after each phase to ensure no regressions.

## Testing Strategy

Follow existing test patterns in the codebase:

### Unit Tests
- Located in same file as code (`#[cfg(test)] mod tests`)
- Use temporary files with `std::env::temp_dir()` (config.rs:1118-1120)
- Use `serial_test` for tests modifying env vars or shared static state
- Test error cases: missing file, invalid format, missing required fields, invalid regex weights
- Use `assert!(result.is_ok())` and `assert!(result.is_err())` patterns

### Integration Tests
- Located in `src/main.rs` (e.g., `persistence_integration_*`)
- Use `test_app()` helper to construct minimal AppState
- Test full config loading pipeline: file → structs → classifier construction
- Skip with `eprintln!("SKIP ...: DATABASE_URL not set")` when dependencies missing

### Manual Verification
1. Start server with `RUST_LOG=info cargo run` and config from config.toml — ensure same behavior
2. Create `test.yaml` equivalent of config.toml — start with `CONFIG_PATH=test.yaml` — verify identical routing
3. Test `--validate` on both TOML and YAML configs
4. Run `--migrate-config` on config.toml, then start with migrated `config.yaml` — ensure functionality unchanged
5. Verify pattern file escaping: copy regex from regex101 directly into pattern file, no modifications needed

## Implementation Phases

### Phase 1: Serde Derive Refactor

**Goal:** Replace manual `toml::Value` tree-walking with `#[derive(Deserialize)]` across all config structs.

#### Changes Required:

1. **src/config.rs** (multiple locations)
   - Add `use serde::Deserialize;`
   - Add `#[derive(Clone, Debug, Deserialize)]` to all config structs: `DashboardConfig`, `CorsConfig`, `ServerConfig`, `HttpConfig`, `DatabaseConfig`, `PersistenceSettings`, `AuthProviderConfig`, `ClassifiersConfig`, `RegexClassifierConfig`, `LlmClassifierConfig`
   - Add `#[serde(rename_all = "snake_case")]` to structs where field names match snake_case TOML keys
   - Add custom renames where needed (e.g., `type_` → `"type"`)
   - Remove all manual extraction logic in `load_*_from_value` functions; replace with:
     ```rust
     #[derive(Deserialize)]
     pub(crate) struct ServerConfig {
         pub port: u16,
         pub log_level: String,
         pub log_format: String,
     }
     // Remove entire load_server_config_from_value function body
     ```
   - Keep the function signatures for backward compatibility to other modules, but internals just call `toml::from_str` (or `serde_saphyr::from_str` later)
   - `routing_from_value` and `hardcoded_routing` remain unchanged (they build from value trees, not serde structs directly)

2. **src/config.rs:344-366** (`DatabaseConfig`), **src/config.rs:368-382** (`PersistenceSettings`), **src/config.rs:422-459** (`parse_env_int` — serde not applicable), etc.

3. **src/config.rs:664-757** (`load_categories_from_value`) — biggest change:
   - Replace hand-parsed loops with `#[derive(Deserialize)]` for `CategoryConfig`
   - `CategoryConfig` needs:
     ```rust
     #[derive(Clone, Debug, Deserialize)]
     pub(crate) struct CategoryConfig {
         pub name: String,
         pub description: String,
         pub threshold: u32,
         pub priority: u8,
         pub patterns: Option<Vec<PatternEntry>>, // may become patterns_source later
         pub dual_threshold: Option<DualThreshold>,
     }
     ```
   - `load_categories_from_value` becomes:
     ```rust
     pub(crate) fn load_categories_from_value(root: &toml::Value) -> Result<Vec<CategoryConfig>, String> {
         let root: ConfigRoot = Deserialize::deserialize(root.clone()).map_err(|e| e.to_string())?;
         // Convert HashMap values to Vec sorted by priority?
         // Or keep as HashMap and let consumer sort
         Ok(root.categories.into_values().collect())
     }
     ```
   - Wait: `ConfigRoot` contains `categories: HashMap<String, CategoryConfig>`. The original function returns `Vec<CategoryConfig>`. We need to decide order preservation. The original app likely iterates in arbitrary order (DB insertion order of hashmap). But tests may assume a particular order. Let's preserve order by sorting by `priority` field asc. Add `#[derive(Deserialize)]` and then in `load_categories_from_value`, collect into vec and sort by priority.

4. **src/config.rs:806-825** (`build_model_costs`) — unchanged; uses `toml::Value` directly, but after refactor we'll get `ConfigRoot.model_costs` as `Option<HashMap<String, f64>>`. Replace manual extraction.

5. **Update tests:**
   - Replace `load_categories_from_value` tests to use real TOML strings with `let root: toml::Value = toml::from_str(content)?;` then call new deserialization. Or better, update function to accept any type implementing `Deserialize`.
   - Keep all test values identical; ensure error messages remain similar.

6. **Update src/main.rs:**
   - Replace embedded config loading at lines 54-61 with:
     ```rust
     let config_path_option = std::env::var("CONFIG_PATH").ok();
     let mut config_root = if let Some(path) = &config_path_option {
         match std::fs::read_to_string(path) {
             Ok(content) => match detect_format(path) {
                 Format::Toml => toml::from_str(&content).unwrap_or_else(|_| toml::Value::Table(Default::default())),
                 Format::Yaml => serde_saphyr::from_str(&content).unwrap_or_else(|_| toml::Value::Table(Default::default())),
             },
             Err(_) => {
                 eprintln!("failed to read config file at {path}; using embedded defaults");
                 toml::Value::Table(Default::default())
             }
         }
     } else {
         // Use embedded defaults as TOML string
         let default_content = include_str!("../config.toml");
         toml::from_str(default_content).unwrap_or_else(|_| toml::Value::Table(Default::default()))
     };
     ```
   - Actually, we want unified config root struct. Better: keep using `toml::Value` for now as abstraction layer. We'll do full migration in Phase 2.

**Phase 1 Focus:** Only serde derives, keep TOML manual parsing paths intact but refactored to use serde internally. Do NOT change format support yet. The goal is to get to ~250 lines from 1559.

#### Success Criteria for Phase 1:
- `cargo test` passes (all existing tests unchanged semantics)
- `config.rs` line count reduced by ~75% (manual extraction removed)
- No functional changes; error messages slightly different due to serde errors (acceptable)
- Type safety improved (compile errors if struct fields mismatch)

### Phase 2: Multi-Format Support

**Goal:** Allow config files in `.toml` or `.yaml`/`.yml` formats using `serde-saphyr`.

#### Changes Required:

1. **Add dependency:**
   - Cargo.toml: `serde-saphyr = "0.4"` (latest stable)
   - `cargo update`

2. **Define unified config loader:**
   - Create `config/loader.rs` module (or add to config.rs):
     ```rust
     pub enum ConfigFormat {
         Toml,
         Yaml,
     }
     
     pub fn load_config_from_path(path: &str) -> Result<ConfigRoot, String> {
         let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
         let format = detect_format(path);
         match format {
             ConfigFormat::Toml => toml::from_str(&content).map_err(|e| e.to_string()),
             ConfigFormat::Yaml => serde_saphyr::from_str(&content).map_err(|e| e.to_string()),
         }
     }
     
     fn detect_format(path: &str) -> ConfigFormat {
         match Path::new(path).extension().and_then(|s| s.to_str()) {
             Some("yaml" | "yml") => ConfigFormat::Yaml,
             _ => ConfigFormat::Toml,
         }
     }
     ```

3. **Update ConfigRoot:**
   - Ensure `ConfigRoot` struct represents all config keys. Use `#[serde(default)]` on optional fields to allow missing sections.
   - Renaming: for `auth_provider` array in TOML, we need `#[serde(rename = "auth_provider")]` on a field `auth_providers: Vec<AuthProviderConfig>`. Double-check all renames.

4. **Refactor loading functions to use ConfigRoot:**
   - Replace `load_server_config_from_value(&toml::Value)` with `load_server_config(&ConfigRoot) -> ServerConfig` that returns `config.server.clone().unwrap_or_default()`
   - Or even better: pass `&ConfigRoot` directly to components instead of individual configs. But this is a bigger refactor. Let's take incremental:
   - `main.rs` will load `config_root: ConfigRoot` from config file(s), then extract individual configs via functions that take `&ConfigRoot`.
   - Update `load_*_from_value` to `load_*(config_root: &ConfigRoot) -> Type` and remove the `&toml::Value` parameter.

5. **Update src/main.rs:**
   - At lines 53-100, replace `toml::Value` merging with direct `ConfigRoot` loading:
     ```rust
     let mut config_root: ConfigRoot = if let Some(path) = &config_path_option {
         match load_config_from_path(path) {
             Ok(overlay) => overlay,
             Err(e) => {
                 eprintln!("failed to parse config at {path}: {e}; using embedded defaults");
                 // Fall back to embedded defaults parsed as TOML, but deserialize into ConfigRoot
                 let default_toml = include_str!("../config.toml");
                 toml::from_str(default_toml).map_err(|e| {
                     eprintln!("embedded config invalid: {e}");
                     ConfigRoot::default()
                 })?
             }
         }
     } else {
         // Just use embedded defaults
         let default_toml = include_str!("../config.toml");
         toml::from_str(default_toml).map_err(|e| {
             eprintln!("embedded config invalid: {e}");
             ConfigRoot::default()
         })?
     };
     ```
   - However, we need CONFIG_ROOT to be `Deserialize` with `Default`.
   - Then replace all `config::load_*_from_value(&config_root)` calls with new functions taking `&ConfigRoot`.

6. **Default implementation:**
   - `ConfigRoot` needs `Default` that yields all fields as `None` or default values sensibly.
   - Alternatively, always load embedded defaults and overlay; never use pure default empty struct.

7. **Tests:**
   - Add YAML test fixtures in `config.rs` tests:
     ```rust
     let yaml_content = r#"
     server:
       port: 10000
       log_level: info
     categories:
       CASUAL:
         description: "Simple"
         threshold: 1
         priority: 4
     "#;
     let config: ConfigRoot = serde_saphyr::from_str(yaml_content).unwrap();
     ```
   - Test that YAML and TOML parse to same `ConfigRoot`

#### Success Criteria for Phase 2:
- `cargo test` passes
- New YAML test files parse correctly
- `CONFIG_PATH=config.yaml` works identically to `config.toml`
- `--validate` flag still not added yet

### Phase 3: External Pattern Files

**Goal:** Support `patterns_file` in categories and simple line-based pattern format.

#### Changes Required:

1. **Update `CategoryConfig`:**
   ```rust
   #[derive(Clone, Debug, Deserialize)]
   pub(crate) struct CategoryConfig {
       pub name: String,
       pub description: String,
       pub threshold: u32,
       pub priority: u8,
       #[serde(flatten)]
       pub patterns_source: PatternsSource,
       pub dual_threshold: Option<DualThreshold>,
   }
   
   #[derive(Clone, Debug, Deserialize)]
   pub(crate) enum PatternsSource {
       Inline(Vec<PatternEntry>),
       #[serde(rename = "patterns_file")]
       External(String),
   }
   ```
   - The `flatten` ensures either `patterns: [...]` or `patterns_file: "path"` deserializes into the enum.
   - Need custom `Deserialize` to handle both forms. serde supports this with `flatten` and `untagged` enum:
     ```rust
     #[derive(Clone, Debug, Deserialize)]
     #[serde(untagged)]
     pub(crate) enum PatternsSource {
         Inline { patterns: Vec<PatternEntry> },
         External { patterns_file: String },
     }
     ```
   - But TOML doesn't support union types cleanly. Better: keep `patterns: Option<Vec<PatternEntry>>` and `patterns_file: Option<String>` separately, then combine in post-processing.

   Actually, simplest: keep both fields as `Option`:
   ```rust
   #[derive(Clone, Debug, Deserialize)]
   pub(crate) struct CategoryConfig {
       pub name: String,
       pub description: String,
       pub threshold: u32,
       pub priority: u8,
       pub patterns: Option<Vec<PatternEntry>>,
       #[serde(rename = "patterns_file")]
       pub patterns_file: Option<String>,
       pub dual_threshold: Option<DualThreshold>,
   }
   ```
   Then in loading, resolve to a single `Vec<PatternEntry>` by:
   - If `patterns_file` is Some(path), read and parse file
   - Else use `patterns.unwrap_or_default()`
   Store as a new field `resolved_patterns: Vec<PatternEntry>` after loading.

   Or we could have a separate `resolve_patterns` function that takes `CategoryConfig` + base patterns dir and returns `Vec<PatternEntry>`.

2. **Add top-level `patterns_dir` config:**
   - Field in `ConfigRoot`: `patterns_dir: Option<PathBuf>` (default: `"./patterns"`)
   - `#[serde(default = "default_patterns_dir")] fn default_patterns_dir() -> PathBuf { "./patterns".into() }`

3. **Implement pattern file loader:**
   ```rust
   pub(crate) fn load_patterns_from_file(
       path: &str,
       base_dir: &Path,
   ) -> Result<Vec<PatternEntry>, String> {
       let full_path = base_dir.join(path);
       let content = std::fs::read_to_string(&full_path)
           .map_err(|e| format!("cannot read pattern file {}: {}", full_path.display(), e))?;
       
       let mut entries = Vec::new();
       for (line_num, line) in content.lines().enumerate() {
           let line = line.trim();
           if line.is_empty() || line.starts_with('#') {
               continue;
           }
           let (weight_str, regex) = line.split_once(" | ")
               .ok_or_else(|| format!("{}: invalid format (missing ' | ' delimiter)", line_num + 1))?;
           let weight = weight_str.trim().parse::<u8>()
               .map_err(|e| format!("{}: invalid weight: {}", line_num + 1, e))?;
           entries.push(PatternEntry {
               regex: regex.to_string(),
               weight,
           });
       }
       Ok(entries)
   }
   ```

4. **Integrate into `RegexClassifier::from_env`:**
   - Change signature to: `from_env(routing, fallback, short_prompt_len, categories: &[CategoryConfig], negative_patterns: &[NegativePatternConfig], patterns_dir: &Path) -> Result<Self, String>`
   - Before building patterns, resolve each category's patterns:
     ```rust
     let mut resolved_categories = Vec::new();
     for cat in categories {
         let patterns = if let Some(ref path) = cat.patterns_file {
             load_patterns_from_file(path, patterns_dir)?
         } else {
             cat.patterns.clone().unwrap_or_default()
         };
         resolved_categories.push(CategoryConfig {
             name: cat.name.clone(),
             description: cat.description.clone(),
             threshold: cat.threshold,
             priority: cat.priority,
             patterns, // But CategoryConfig still has patterns field; better: change CategoryConfig::patterns to always be the resolved vec
             dual_threshold: cat.dual_threshold.clone(),
         });
     }
     ```
   - Actually, we can restructure: after loading ConfigRoot, before passing to `RegexClassifier::from_env`, do pattern resolution and construct a new `Vec<CategoryConfig>` with patterns filled in from either source.

5. **Update config loading in main.rs:**
   - After `load_config_from_path`, get `config_root: ConfigRoot`
   - Resolve patterns directory: `config_root.patterns_dir.clone().unwrap_or_else(|| PathBuf::from("./patterns"))`
   - Iterate all categories, for each `patterns_file`, call `load_patterns_from_file`, replace `patterns` field
   - Validate each regex with `Regex::new(pattern)` and collect errors (all errors aggregated)
   - If any pattern fails, log all failures and abort startup (or return error for validation mode)

6. **Validation mode:**
   - `--validate` flag should:
   - Load config (with CONFIG_PATH overlay)
   - Validate config schema (required fields, type constraints, cross-references)
   - Resolve patterns (external files)
   - Compile every regex with `Regex::new`
   - Report all errors (schema + regex) aggregated
   - Exit 0 success, 1 failure

7. **No migration tool:** Users who want to switch to YAML + external patterns do so manually. The `--validate` flag helps them verify the result.

**Backward compatibility:**
- If neither `patterns` nor `patterns_file` is present, use empty vec (current behavior)
- Existing `config.toml` with inline `patterns` continues to work unchanged
- Default `patterns_dir` = `./patterns` is harmless if not used

#### Success Criteria for Phase 3:
- Category with `patterns_file` loads correctly and patterns compile
- Category without `patterns_file` uses inline patterns as before
- Invalid pattern weight or regex produces clear error with file:line (for external) or category name (for inline)
- `--validate` detects schema errors, inline pattern errors, and external pattern errors
- All existing tests pass; new tests cover external patterns

### Phase 4: Validation CLI

**Goal:** Add `--validate` flag to the existing binary that checks both config schema correctness and regex pattern validity. No migration tool — users manually convert configs if desired.

#### Changes Required:

1. **Main entry point modification:**
   - At the very start of `main()`, before any heavy lifting:
   ```rust
   let args: Vec<String> = std::env::args().collect();
   let mut enable_validate = false;

   let mut i = 1;
   while i < args.len() {
       match args[i].as_str() {
           "--validate" => {
               enable_validate = true;
               i += 1;
           }
           _ => {
               eprintln!("unknown argument: {}", args[i]);
               std::process::exit(2);
           }
       }
   }
   ```

2. **Validation path:**
   - If `enable_validate`:
     - Load config (respect CONFIG_PATH if set, else embedded defaults)
     - Build `ConfigRoot` — any deserialization error means schema invalid
     - Validate config schema beyond serde: required sections present, valid port range, non-empty strings where needed, valid `provider_type` values, etc.
     - Resolve patterns (including external files)
     - Compile all regexes with `Regex::new`; collect errors
     - If any errors: print all to stderr with file:line context, exit 1
     - Else: println!("Configuration valid"); exit 0
     - Do not start server

3. **Schema validation checks (beyond serde deserialization):**
   - `server.port` is in valid range (1–65535)
   - `server.log_level` is one of: trace, debug, info, warn, error
   - `server.log_format` is one of: compact, full, json, pretty
   - `http.client_timeout_secs` > 0
   - Routing entries reference categories that exist in `categories`
   - `auth_providers[].type` is one of the known provider types
   - Category `threshold` > 0
   - Category `priority` > 0
   - `patterns_file` paths are readable (if specified)
   - `patterns_dir` exists (if specified)
   - Each `model_costs` value > 0.0
   - Collect all schema errors before reporting (don't fail on first)

4. **Regex pattern validation:**
   - For each category (inline or external patterns), attempt `Regex::new` on every pattern
   - For external patterns: report file:line (e.g., `patterns/file_reading.patterns:5: invalid regex ...`)
   - For inline patterns: report category name and pattern index
   - Collect all regex errors before reporting

5. **Refactor for testability:**
   - Create `fn run_validation(config_path: Option<&str>) -> Result<(), Vec<String>>` that can be unit tested
   - `main` calls `run_validation` and exits with appropriate code

6. **Error handling:**
   - Use `eprintln!` for errors
   - Exit code 0 = success, 1 = validation errors, 2 = argument errors
   - Print all errors, not just the first one

#### Success Criteria for Phase 4:
- `cerebrum --validate` verifies config schema + compiles all patterns, exits 0 on success
- Schema errors (invalid port, unknown provider type, missing required field) are reported
- Regex errors in both inline and external patterns are reported with file:line context
- Helpful error message for invalid flags
- No regression in normal server startup (no flags)

---

## Testing Strategy

### Phase 1 (Serde Refactor)

**Unit tests (config.rs) — adapt existing:**
- All 2000+ lines of existing tests must still compile and pass
- The loader functions will change internals but public signatures remain
- We'll need to adjust tests that construct `toml::Value` manually and call loaders; those should still work because we'll internally deserialize. However, some tests may need updates to match new error messages.

**Integration tests (main.rs):**
- `test_app()` and friends construct AppState via current config loading pipeline; should continue to pass unchanged

**Key tests to retain:**
- `load_routing_from_file_success`, `load_routing_behavior`, `hardcoded_routing_produces_expected_defaults`
- Category loading tests (various scenarios)
- Negative pattern loading tests
- All `parse_env_int` tests

### Phase 2 (YAML Support)

**New tests in config.rs:**
```rust
#[test]
fn yaml_config_matches_toml() {
    let toml = r#"
    server: { port: 10000 }
    http: { client_timeout_secs: 120 }
    categories:
      CASUAL:
        description: "Simple"
        threshold: 1
        priority: 4
    "#;
    let yaml = r#"
    server:
      port: 10000
    http:
      client_timeout_secs: 120
    categories:
      CASUAL:
        description: Simple
        threshold: 1
        priority: 4
    "#;
    let toml_config: ConfigRoot = toml::from_str(toml).unwrap();
    let yaml_config: ConfigRoot = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(toml_config.server.port, yaml_config.server.port);
    assert_eq!(toml_config.categories["CASUAL"].threshold, yaml_config.categories["CASUAL"].threshold);
}
```

**Integration test:**
- Temporarily create `test.yaml` duplicating `config.toml`
- `std::env::set_var("CONFIG_PATH", "test.yaml");`
- Build `AppState` and assert equal values to TOML path

### Phase 3 (External Patterns)

**New tests:**
```rust
#[test]
fn load_patterns_from_file_basic() {
    let tmp_dir = temp_dir();
    let pattern_file = tmp_dir.join("test.patterns");
    std::fs::write(&pattern_file, "3 | hello\n2 | world\n").unwrap();
    
    let patterns = load_patterns_from_file(pattern_file.to_str().unwrap(), &tmp_dir).unwrap();
    assert_eq!(patterns.len(), 2);
    assert_eq!(patterns[0].regex, "hello");
    assert_eq!(patterns[0].weight, 3);
}

#[test]
fn load_patterns_from_file_with_comments() {
    // comments and blank lines ignored
}

#[test]
fn load_patterns_from_file_invalid_weight() {
    // error reported with line number
}

#[test]
fn external_patterns_compile_success() {
    // category with patterns_file; RegexClassifier::from_env succeeds
}
```

**Integration test:**
- Category config with `patterns_file: "test.patterns"` that defines weight 3 pattern `(?i)\bhello\b`
- Construct classifier, classify `"Hello world"` → matches category

**Validation flag test:**
- Run with `--validate` on a config containing an invalid regex in an external file — expect exit code 1

### Phase 4 (CLI Validation)

**Tests in main.rs (or dedicated tests module):**

- `test_validate_success()`: valid config + patterns → Ok
- `test_validate_invalid_regex()`: config with bad regex → Err with file:line
- `test_validate_schema_error_invalid_port()`: port out of range → Err
- `test_validate_schema_error_unknown_provider()`: bad provider_type → Err
- `test_validate_schema_error_missing_required()`: missing required field → Err
- `test_validate_collects_all_errors()`: multiple errors reported, not just first
- `test_validate_external_pattern_file_not_found()`: missing patterns_file → Err with path

Refactor: create `fn run_validation(config_path: Option<&str>) -> Result<(), Vec<String>>` and test that directly. Then `main` just calls it and exits.

**Manual testing:**
- `cargo run -- --validate` on current repo's `config.toml` should succeed
- `cargo run -- --badflag` prints error and exits 2

## Migration Notes

**For existing users:**

Existing `config.toml` (with inline `patterns`) will continue to work unchanged. No action required. The multi-format support and external patterns are **opt-in**.

**Manual migration path (if desired):**

1. Create a YAML version of `config.toml` (by hand or with any TOML→YAML converter)
2. Extract category patterns into `patterns/*.patterns` files using the `weight | regex` line format
3. Replace `patterns: [...]` with `patterns_file: "patterns/<category>.patterns"` in each category
4. Run `cerebrum --validate` to verify the new config
5. Start with: `CONFIG_PATH=config.yaml cargo run`
6. Delete old `config.toml` once satisfied

**Rollback:** Switch back to `config.toml` or keep using TOML forever.

**Breaking changes:** None intended. All changes are additive with backward compatibility.

## Performance Considerations

- Serde deserialization is fast; no performance regression expected
- External pattern files add one file read per category on startup (cached). Pattern files are small (<10KB each); total I/O < 100KB.
- Validation time: regex compilation is already done at startup; external file I/O adds ~10-50ms.
- No runtime cost for pattern resolution (happens once at startup).

## Open Risks & Assumptions

1. **Assumption:** `serde-saphyr` fully supports all TOML features used in current config (including inline tables, arrays, nested tables). This is likely; but we must test thoroughly.
2. **Assumption:** YAML and TOML can be deserialized to the same `ConfigRoot` without special renames. Conflicts may arise: YAML uses `?` for complex keys; TOML uses `[[array]]` for arrays of tables. Our config uses `[[auth_provider]]` and `[[negative_patterns]]`. These map to `Vec<AuthProviderConfig>` and `Vec<NegativePatternConfig>` in serde. Should work identically!
3. **Risk:** Changing error messages from custom `String::from("No [categories] section found")` to serde's `missing field` errors may break tests or user expectations. We can provide custom error messages with `#[serde(skip_deserializing)]` and manual validation after deserialization. But simpler: accept new error messages as they're still informative.
4. **Risk:** The `routing` config uses inline tables like `[routing.FILE_READING]`. This is a table of tables. In YAML, that is a nested map. serde should handle it.
5. **Risk:** `config::merge_toml_values` currently does deep merging with overrides. We need to replicate that merging logic for YAML overlay. The current `override_keys` (categories, routing, auth_provider, etc.) trigger complete section replacement. We'll implement `merge_configs(base: ConfigRoot, overlay: ConfigRoot)` that replaces override-key sections wholesale and shallowly merges the rest (server, http, etc.).
6. **Risk:** Users may place pattern files in non-standard locations. We should document that `patterns_file` is relative to `patterns_dir` (or to the config file's directory if not absolute).
7. **Assumption:** Pattern files are small enough to read into memory; no need for streaming.

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append `— <commit sha>` after a step lands. Do not rename step titles.

### Phase 1: Serde Derive Refactor

#### Automated
- [x] 1.1 Add `serde` to Cargo.toml
- [x] 1.2 Annotate all config structs with `#[derive(Deserialize)]` and `#[serde(rename_all = "snake_case")]`
- [x] 1.3 Refactor `load_*_from_value` functions to deserialize directly from `ConfigRoot` or `toml::Value` with serde
- [x] 1.4 Update `CategoryConfig` and `load_categories_from_value` to use serde
- [x] 1.5 Update tests to work with new deserialization paths (may require adjusting expected error messages)
- [x] 1.6 Run full test suite and verify all tests pass

#### Manual
- [ ] 1.7 Verify config loading still works with embedded `config.toml`
- [ ] 1.8 Check that error messages remain informative

---

### Phase 2: Multi-Format Support

#### Automated
- [x] 2.1 Add `serde_yaml` dependency (adapted from plan's `serde-saphyr`)
- [x] 2.2 Implement `load_config_from_path` with format detection
- [x] 2.3 Update `main.rs` config loading to use `ConfigRoot` deserialization (with CONFIG_PATH overlay merging)
- [x] 2.4 Implement `merge_configs` to replace `merge_toml_values` for overlay
- [x] 2.5 Update all `load_*` calls to use `&ConfigRoot` instead of `&toml::Value`
- [x] 2.6 Add YAML-specific test cases
- [x] 2.7 Test both TOML and YAML configs produce identical `ConfigRoot`

#### Manual
- [ ] 2.8 Create a sample YAML config and start with `CONFIG_PATH=sample.yaml`
- [ ] 2.9 Verify all functionality identical to TOML

---

### Phase 3: External Pattern Files

#### Automated
- [ ] 3.1 Extend `CategoryConfig` with `patterns_file: Option<String>` (keep `patterns: Option<Vec<PatternEntry>>`)
- [ ] 3.2 Add `patterns_dir: Option<PathBuf>` to `ConfigRoot` with default `"./patterns"`
- [ ] 3.3 Implement `load_patterns_from_file(path, base_dir)`
- [ ] 3.4 In `main.rs`, after loading `ConfigRoot`, resolve all category patterns (fill a new `resolved_categories: Vec<CategoryConfig>`)
- [ ] 3.5 Compile all patterns (inline + external) and report errors with file:line for external files
- [ ] 3.6 Modify `RegexClassifier::from_env` to accept resolved categories (no external references) — or perform resolution before calling it
- [ ] 3.7 Add validation in startup to compile all regexes and abort on error
- [ ] 3.8 Add tests: pattern file loading, external patterns integration, validation errors

#### Manual
- [ ] 3.10 Create sample pattern file and verify correct parsing
- [ ] 3.11 Test startup with a config using `patterns_file`

---

### Phase 4: Validation CLI

#### Automated
- [ ] 4.1 Extend argument parser to handle `--validate`
- [ ] 4.2 Implement `run_validation(config_path: Option<&str>) -> Result<(), Vec<String>>`: load config, validate schema, resolve patterns, compile all regexes, collect all errors
- [ ] 4.3 Add schema validation checks (port range, log level, provider types, required fields, cross-references)
- [ ] 4.4 Add regex validation with file:line context for external patterns and category+index for inline
- [ ] 4.5 Add tests: validate success, schema errors, regex errors, multiple errors collected, external file not found
- [ ] 4.6 Wire `--validate` flag in `main()` to call `run_validation` and exit with appropriate code

#### Manual
- [ ] 4.7 Run `--validate` on existing `config.toml` — should succeed
- [ ] 4.8 Verify helpful error output on intentionally broken config

---

## References

- Research: `context/changes/move-all-config-to-file/research-config-format.md`
- Current config: `config.toml`
- Config module: `src/config.rs`
- Main startup: `src/main.rs:50-2841`
- Intent classifier: `src/intent_classifier.rs`
- Tests: `src/config.rs:1053-1559`, `src/main.rs` (multiple `#[tokio::test]` functions)
- Lessons: `context/foundation/lessons.md`