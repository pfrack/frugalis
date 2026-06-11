# Move All Config to File — Implementation Plan

## Overview

This plan implements the research document's recommendation: a hybrid configuration system that supports **both YAML and TOML formats** via serde derives, and **externalizes regex patterns** into simple pattern files with zero escaping requirements. The goal is to improve user experience for non-Rust DevOps engineers while maintaining strict regex correctness.

**Key deliverables:**
- Phase 1: Replace manual TOML parsing (1559 lines) with `#[derive(Deserialize)]` structs
- Phase 2: Add YAML support via `serde-saphyr` using format detection by extension
- Phase 3: Add `patterns_file` field support and external pattern file loader
- Phase 4: Add `--validate` and `--migrate-config` CLI flags to existing binary

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
4. **Validation**: `cerebrum --validate` compiles all regex patterns and checks config schema, reporting file:line on errors.
5. **Migration tool**: `cerebrum --migrate-config --input config.toml --output config.yaml --extract-patterns ./patterns/` converts existing configs.
6. **Full backwards compatibility**: Existing `config.toml` with inline patterns continues to work indefinitely.

### Success Criteria

- All existing unit and integration tests pass unchanged
- New YAML config with inline patterns loads identically to TOML
- External pattern files compile correctly and integrate into classification scoring
- `--validate` exits 0 on success, non-zero on any config/pattern error
- Migration tool produces valid YAML + pattern files that classify identically to original TOML
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

4. **CLI Flags (Phase 4)**
   - Add `std::env::args().collect()` check at top of `main()`
   - If `--validate` present:
     - Load config (using overlay if CONFIG_PATH set)
     - Compile all patterns (including external files)
     - Print success or all errors, exit with code 0 or 1
   - If `--migrate-config` present with required `--input` and `--output`:
     - Load input config (any format) from `--input <path>`
     - Create output directory if needed
     - Write config in YAML format to `--output`
     - Extract all category patterns to `--extract-patterns <dir>`:
       - For each category, create `<dir>/<category_name>.patterns`
       - Write weight and regex lines
     - Print summary and success message
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
     - Resolve patterns (external files)
     - Compile every regex with `Regex::new`
     - Report first error or all errors aggregated
     - Exit 0 success, 1 failure

7. **Migration tool:** (see Phase 4)

**Backward compatibility:**
- If neither `patterns` nor `patterns_file` is present, use empty vec (current behavior)
- Existing `config.toml` with inline `patterns` continues to work unchanged
- Default `patterns_dir` = `./patterns` is harmless if not used

#### Success Criteria for Phase 3:
- Category with `patterns_file` loads correctly and patterns compile
- Category without `patterns_file` uses inline patterns as before
- Invalid pattern weight or regex produces clear error with file:line (for external) or category name (for inline)
- `--validate` detects both inline and external pattern errors
- All existing tests pass; new tests cover external patterns

### Phase 4: CLI Commands

**Goal:** Add `--validate` and `--migrate-config` options to the existing binary (no new binary, no clap dependency — simple arg matching).

#### Changes Required:

1. **Main entry point modification:**
   - At the very start of `main()`, before any heavy lifting:
     ```rust
     let args: Vec<String> = std::env::args().collect();
     let mut enable_validate = false;
     let mut enable_migrate = false;
     let mut migrate_input = None;
     let mut migrate_output = None;
     let mut migrate_extract_patterns = None;
     
     let mut i = 1;
     while i < args.len() {
         match args[i].as_str() {
             "--validate" => {
                 enable_validate = true;
                 i += 1;
             }
             "--migrate-config" => {
                 enable_migrate = true;
                 i += 1;
             }
             "--input" if enable_migrate && i + 1 < args.len() => {
                 migrate_input = Some(args[i+1].clone());
                 i += 2;
             }
             "--output" if enable_migrate && i + 1 < args.len() => {
                 migrate_output = Some(args[i+1].clone());
                 i += 2;
             }
             "--extract-patterns" if enable_migrate && i + 1 < args.len() => {
                 migrate_extract_patterns = Some(args[i+1].clone());
                 i += 2;
             }
             _ => {
                 eprintln!("unknown argument: {}", args[i]);
                 std::process::exit(2);
             }
         }
     }
     
     if enable_migrate && (migrate_input.is_none() || migrate_output.is_none() || migrate_extract_patterns.is_none()) {
         eprintln!("--migrate-config requires --input, --output, and --extract-patterns");
         std::process::exit(2);
     }
     ```

2. **Validation path:**
   - If `enable_validate`:
     - Load config (respect CONFIG_PATH if set, else embedded defaults)
     - Build `ConfigRoot`
     - Resolve patterns (including external)
     - Compile all regexes; collect errors
     - If errors: print to stderr, exit 1
     - Else: println!("Configuration valid"); exit 0
   - Do not start server

3. **Migration path:**
   - If `enable_migrate`:
     - Load input config from `migrate_input.unwrap()` using `load_config_from_path` (must succeed)
     - Convert to `ConfigRoot`
     - For each category in `categories` (sorted by priority? or alphabetical for consistency):
       - Replace `patterns` with `patterns_file: "patterns/<category_name>.patterns"`
       - Ensure order preservation? YAML maps are unordered but serde_yaml writes in insertion order if we iterate sorted keys.
     - Write `ConfigRoot` to `migrate_output.unwrap()` in YAML format using `serde_yaml` (maybe `serde_saphyr` provides serialization). **Check:** `serde-saphyr` supports `Serialize` as well.
       ```rust
       let yaml = serde_saphyr::to_string(&config_root).map_err(|e| ...)?;
       std::fs::write(&output_path, yaml)?;
       ```
     - Ensure output file has `.yaml` or `.yml` extension (recommend)
     - Extract patterns: create `migrate_extract_patterns.unwrap()` directory (mkdir -p)
       - For each category (use same order as in YAML keys), create `<category>.patterns` file
       - If category has inline patterns, write `weight | regex` lines
       - If category already has `patterns_file` (unlikely), copy that file? Or warn.
     - Print success message: "Migrated config to <output> with patterns in <dir>"
     - Exit 0

4. **Pattern file naming:**
   - Use category name in snake_case? The research example uses `file_reading.patterns`. We can use the category name as-is: `FILE_READING.patterns` or convert to snake case. Simpler: use the category name exactly (e.g., `FILE_READING.patterns`). But research used lowercase with underscores. We can follow config ordering: the `ConfigRoot.categories` is a HashMap; we can iterate in sorted order (by priority) and use `category.name` directly. Users can rename files if they want.

5. **YAML serialization:**
   - Use `serde_saphyr::to_string`. It supports ordered maps? `serde_saphyr::Serializer` does preserve order on serialization when using `serde::ser::SerializeMap`? But our `ConfigRoot` has `HashMap` for categories. We want deterministic output. For migration, we can sort categories by `priority` ascending and build an ordered map.
   - Create a helper: `fn sorted_config_root(config: ConfigRoot) -> BTreeMap<String, CategoryConfig>` to output sorted categories.
   - Or: Implement manual YAML writing: iterate `config.categories` sorted by priority, write YAML fragments with `serde_yaml::to_string` for each, and compose. But easier: convert `HashMap<String, CategoryConfig>` to `BTreeMap<String, CategoryConfig>` sorted by category priority (store priority separately). For migration tool, we don't need perfect round-trip; we just need a readable YAML.

   Actually simpler: The `--migrate-config` tool is a one-shot operation. We can use a temporary struct with ordered fields. Let's design:

   - Create a `MigrateConfig` struct that mirrors `ConfigRoot` but with `categories: Vec<CategoryConfig>` instead of `HashMap`. We'll sort categories by `priority asc` and then `name` to break ties.
   - Load `ConfigRoot` from source.
   - Build `MigrateConfig` by copying all fields; for `categories`, take `config_root.categories.into_values().collect()` sort by (priority, name).
   - Serialize `MigrateConfig` to YAML; order of fields in struct is the order we define.

6. **Error handling:**
   - Use `eprintln!` for errors
   - Exit code 0 success, 1 for validation errors or migration failures, 2 for argument errors

7. **Documentation output:**
   - After migration, print instructions: "Edit the generated YAML config. Pattern files are in `patterns/`. Run with `CONFIG_PATH=config.yaml`."

#### Success Criteria for Phase 4:
- `cerebrum --validate` verifies config and patterns, exits 0 on success
- `cerebrum --migrate-config --input config.toml --output config.yaml --extract-patterns ./patterns/` succeeds
- Migrated YAML config + pattern files produce identical classification results
- Helpful error messages for invalid flags
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

### Phase 4 (CLI)

**Tests in main.rs (or dedicated tests module):**

- `test_validate_success()`: mock config file, call `main` with `--validate` via `std::process::Command`? That's hard to test in unit tests. Better: factor validation logic into a public function `validate_config() -> Result<(), String>` and test that directly. Then `main` just calls it and exits.

  Refactor: create `fn run_validation() -> Result<(), String>` and `fn run_migration(args) -> Result<(), String>` that can be unit tested. Then `main` becomes a dispatcher.

  ```rust
  #[test]
  fn validate_success() {
      // setup temp config and pattern files
      let config_path = "...";
      let result = run_validation_with_path(config_path);
      assert!(result.is_ok());
  }
  
  #[test]
  fn validate_invalid_pattern() {
      // create config with bad regex
      let result = run_validation();
      assert!(result.is_err());
  }
  ```

- `test_migrate_config_creates_files()`: 
  - Input: TOML config with inline patterns
  - Output: assert YAML file created, pattern directory with files
  - Compare that pattern file contents match expected `weight | regex`

**Manual testing:**
- `cargo run -- --validate` on current repo's `config.toml` should succeed
- `cargo run -- --migrate-config --input config.toml --output migrated.yaml --extract-patterns ./migrated_patterns/`
  - Check `migrated.yaml` is valid YAML
  - Check `migrated_patterns/FILE_READING.patterns` etc. exist
  - Run with `CONFIG_PATH=migrated.yaml` and same `patterns` dir → same behavior
- `cerebrum --badflag` prints error and exits 2

## Migration Notes

**For existing users:**

Existing `config.toml` (with inline `patterns`) will continue to work unchanged. No action required. The multi-format support and external patterns are **opt-in**.

**Opt-in migration path:**

1. Run: `cerebrum --migrate-config --input config.toml --output config.yaml --extract-patterns ./patterns/`
2. Review generated `config.yaml` and `patterns/*.patterns` files
3. Optionally rename pattern files or edit YAML to use different paths
4. Start with: `CONFIG_PATH=config.yaml cargo run`
5. Delete old `config.toml` once satisfied

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
5. **Risk:** `config::merge_toml_values` currently does deep merging with overrides. We need to replicate that merging logic for YAML overlay. Our `load_config_from_path` only loads one format; the merge must happen before deserialization. Current approach: read overlay and merge as `toml::Value` then deserialize. This works with `toml::Value` intermediate. For YAML overlay on TOML base, we could:
   - Deserialize base TOML to `ConfigRoot`
   - Deserialize overlay YAML to `ConfigRoot`
   - Merge fields manually (complex)
   - Or: convert base to a generic `toml::Value` representation (still TOML), overlay YAML to `toml::Value`? Not directly.

   Simpler: Keep the merging as `toml::Value` but read YAML overlay and convert to TOML-like structure? Not possible. We need a unified merge. We could:
   - Read embedded defaults as `ConfigRoot`
   - Read overlay (any format) as `ConfigRoot`
   - Use a `merge_configs(base: ConfigRoot, overlay: ConfigRoot) -> ConfigRoot` that recursively overlays fields (with same override_keys logic). This is doable with serde's `merge` crate or manual. Since config is not deeply nested (mostly flat within sections), we can implement manual overlay:
     - For each top-level field in overlay, if it's `Some(...)`, replace base's field; else keep base's.
     - Special case: `categories` merge by key: overlay category overrides base category of same name; but also allow new categories to be added. The current `merge_toml_values` handles nested tables, which means overlay categories are merged with base categories (table insertion). Actually currently, if you have a base category and overlay defines same category key, the entire category table is replaced (because `override_keys` includes `"categories"`). So we can do the same: replace whole `categories` map when any category is overridden? But the current code says `override_keys` are the ones that get complete replacement, and it includes `categories`, `routing`, `auth_provider`, etc. That means if overlay contains `categories`, the whole section is replaced. So we can just replace the whole `HashMap` when present.

   So merge becomes: for each field (server, http, cors, database, persistence, classifiers, regex_classifier, llm_classifier, categories, negative_patterns, routing, auth_providers, model_costs, baseline_model, classify_db_log, dashboard), if overlay has `Some(value)`, use it; else use base's value.

   That is simple. We'll implement `fn merge_configs(base: ConfigRoot, overlay: ConfigRoot, override_keys: &[&str]) -> ConfigRoot`. Actually the current `merge_toml_values` does a recursive merge except for `override_keys` which replace completely. To match behavior, we need to replicate that at the `ConfigRoot` level.

   But note: `override_keys` from main.rs: classifiers, regex_classifier, llm_classifier, categories, auth_provider, model_costs, routing, negative_patterns. That's almost every user-configurable section. Effectively, overlay completely replaces those sections, while server, http, database, etc. are merged shallowly. So our merge can be:

   ```rust
   let mut merged = base.clone();
   if let Some(overlay_server) = overlay.server { merged.server = Some(overlay_server); }
   if let Some(overlay_http) = overlay.http { merged.http = Some(overlay_http); }
   // ... for each field
   if override_keys.contains("categories") && overlay.categories.is_some() {
       merged.categories = overlay.categories;
   }
   // Similarly for routing, auth_providers, etc.
   ```

   We'll implement in main.rs merging. That's Phase 2 work.

6. **Risk:** Users may place pattern files in non-standard locations. We should document that `patterns_file` is relative to the config file's directory (or to `patterns_dir`). The `load_patterns_from_file` function should interpret the path relative to the config file's directory if it's not absolute. For migration, we write to `patterns/` relative to config dir.
7. **Assumption:** Pattern files are small enough to read into memory; no need for streaming.

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append `— <commit sha>` after a step lands. Do not rename step titles.

### Phase 1: Serde Derive Refactor

#### Automated
- [ ] 1.1 Add `serde` to Cargo.toml
- [ ] 1.2 Annotate all config structs with `#[derive(Deserialize)]` and `#[serde(rename_all = "snake_case")]`
- [ ] 1.3 Refactor `load_*_from_value` functions to deserialize directly from `ConfigRoot` or `toml::Value` with serde
- [ ] 1.4 Update `CategoryConfig` and `load_categories_from_value` to use serde
- [ ] 1.5 Update tests to work with new deserialization paths (may require adjusting expected error messages)
- [ ] 1.6 Run full test suite and verify all tests pass

#### Manual
- [ ] 1.7 Verify config loading still works with embedded `config.toml`
- [ ] 1.8 Check that error messages remain informative

---

### Phase 2: Multi-Format Support

#### Automated
- [ ] 2.1 Add `serde-saphyr` dependency
- [ ] 2.2 Implement `load_config_from_path` with format detection
- [ ] 2.3 Update `main.rs` config loading to use `ConfigRoot` deserialization (with CONFIG_PATH overlay merging)
- [ ] 2.4 Implement `merge_configs` to replace `merge_toml_values` for overlay
- [ ] 2.5 Update all `load_*` calls to use `&ConfigRoot` instead of `&toml::value`
- [ ] 2.6 Add YAML-specific test cases
- [ ] 2.7 Test both TOML and YAML configs produce identical `ConfigRoot`

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
- [ ] 3.8 Add `--validate` flag logic (lightweight: just load config and compile patterns, then exit)
- [ ] 3.9 Add tests: pattern file loading, external patterns integration, validation errors

#### Manual
- [ ] 3.10 Create sample pattern file and verify correct parsing
- [ ] 3.11 Test startup with a config using `patterns_file`

---

### Phase 4: Migration Tool

#### Automated
- [ ] 4.1 Extend argument parser to handle `--migrate-config --input <p> --output <p> --extract-patterns <dir>`
- [ ] 4.2 Implement `run_migration(input_path, output_path, patterns_dir)`:
   - Load input config
   - Sort categories by priority ascending
   - Replace each category's `patterns` with `patterns_file: "patterns/<category>.patterns"`
   - Serialize to YAML (use `serde_saphyr` with `Serializer` configured for sorted keys)
   - Write to output file
   - Create patterns directory
   - For each category (sorted), write pattern file with `weight | regex` lines
- [ ] 4.3 Add tests: migration produces valid YAML and pattern files; migrated config loads successfully

#### Manual
- [ ] 4.4 Run migration on existing `config.toml`; inspect output
- [ ] 4.5 Start server with migrated YAML and extracted patterns; verify identical behavior
- [ ] 4.6 Verify pattern files can be edited independently

---

## References

- Research: `context/changes/move-all-config-to-file/research-config-format.md`
- Current config: `config.toml`
- Config module: `src/config.rs`
- Main startup: `src/main.rs:50-2841`
- Intent classifier: `src/intent_classifier.rs`
- Tests: `src/config.rs:1053-1559`, `src/main.rs` (multiple `#[tokio::test]` functions)
- Lessons: `context/foundation/lessons.md`