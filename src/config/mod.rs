use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

pub(crate) mod loader;
pub(crate) mod routing;
pub(crate) mod types;

pub(crate) use routing::RouteEntry;
pub(crate) use types::{
    AuthProviderConfig, CacheConfig, CategoryConfig, ClassifiersConfig, CorsConfig,
    DashboardConfig, DatabaseConfig, FewShotConfig, HttpConfig, LlmClassifierConfig,
    NegativePatternConfig, PatternEntry, PersistenceSettings, RegexClassifierConfig, ServerConfig,
};

/// Top-level configuration root that mirrors every section in `config.toml` (or
/// `config.yaml`). Loaded once at startup by [`load_config_from_path`] and then
/// projected into typed sub-configs via the `load_*_from_value` helpers in
/// [`loader`].
///
/// Every field is `Option<T>` so an absent TOML section deserialises to `None`
/// and the projection helper falls back to a safe default rather than failing.
/// Semantic validation (bad regex, unknown log levels, etc.) is deferred to
/// [`run_validation`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ConfigRoot {
    #[serde(default)]
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub http: Option<HttpConfig>,
    #[serde(default)]
    pub cors: Option<CorsConfig>,
    #[serde(default)]
    pub database: Option<DatabaseConfig>,
    #[serde(default)]
    pub persistence: Option<PersistenceSettings>,
    #[serde(default)]
    pub classifiers: Option<ClassifiersConfig>,
    #[serde(default)]
    pub regex_classifier: Option<RegexClassifierConfig>,
    #[serde(default)]
    pub llm_classifier: Option<LlmClassifierConfig>,
    #[serde(default)]
    pub fewshot_classifier: Option<FewShotConfig>,
    #[serde(default)]
    pub categories: Option<HashMap<String, CategoryConfig>>,
    #[serde(default)]
    pub patterns_dir: Option<PathBuf>,
    #[serde(default)]
    pub negative_patterns: Option<Vec<NegativePatternConfig>>,
    #[serde(default)]
    pub routing: Option<HashMap<String, RouteEntry>>,
    #[serde(default, rename = "auth_provider")]
    pub auth_providers: Option<Vec<AuthProviderConfig>>,
    #[serde(default)]
    pub model_costs: Option<HashMap<String, f64>>,
    #[serde(default)]
    pub baseline_model: Option<String>,
    #[serde(default)]
    pub classify_db_log: Option<bool>,
    #[serde(default)]
    pub dashboard: Option<DashboardConfig>,
    #[serde(default)]
    pub cache: Option<CacheConfig>,
}

/// Read a config file from `path` and deserialise it into [`ConfigRoot`].
///
/// The format is inferred from the file extension: `.yaml` / `.yml` use
/// `serde_yaml`; everything else is treated as TOML. Returns an error string
/// if the file cannot be read or fails to parse.
pub(crate) fn load_config_from_path(path: &str) -> Result<ConfigRoot, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
    match detect_format(path) {
        ConfigFormat::Toml => toml::from_str(&content).map_err(|e| format!("{}: {}", path, e)),
        ConfigFormat::Yaml => {
            serde_yaml::from_str(&content).map_err(|e| format!("{}: {}", path, e))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ConfigFormat {
    Toml,
    Yaml,
}

/// Infer the config file format from the file extension.
/// `.yaml` / `.yml` → [`ConfigFormat::Yaml`]; anything else → [`ConfigFormat::Toml`].
fn detect_format(path: &str) -> ConfigFormat {
    match std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
    {
        Some("yaml" | "yml") => ConfigFormat::Yaml,
        _ => ConfigFormat::Toml,
    }
}

/// Validate config schema and compile all regex patterns eagerly.
///
/// When `config_path` is `Some`, the file at that path is loaded and validated.
/// When `None`, the config embedded at compile time (`config.toml`) is used.
///
/// Checks include: port sanity, log-level / log-format enumeration, HTTP
/// timeout > 0, all category thresholds and priorities > 0, routing keys
/// matching known categories, auth provider completeness, model costs > 0,
/// `patterns_dir` being a directory if it exists on disk, and successful
/// compilation of every regex pattern (inline or loaded from a patterns file).
///
/// All errors are collected before returning so the caller sees the full list
/// rather than stopping at the first problem.
pub(crate) fn run_validation(config_path: Option<&str>) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    // Load config
    let config_root: ConfigRoot = match config_path {
        Some(path) => match load_config_from_path(path) {
            Ok(root) => root,
            Err(e) => {
                errors.push(format!("config error: {e}"));
                return Err(errors);
            }
        },
        None => {
            let default_content = include_str!("../../config.toml");
            match toml::from_str(default_content) {
                Ok(root) => root,
                Err(e) => {
                    errors.push(format!("embedded config error: {e}"));
                    return Err(errors);
                }
            }
        }
    };

    // ── Server section validation ──
    if let Some(ref server) = config_root.server {
        if server.port == 0 {
            errors.push(format!("server.port: invalid port {}", server.port));
        }
        match server.log_level.as_str() {
            "trace" | "debug" | "info" | "warn" | "error" => {}
            _ => errors.push(format!(
                "server.log_level: unknown level '{}'",
                server.log_level
            )),
        }
        match server.log_format.as_str() {
            "compact" | "full" | "json" | "pretty" => {}
            _ => errors.push(format!(
                "server.log_format: unknown format '{}'",
                server.log_format
            )),
        }
    }

    // ── HTTP section validation ──
    if let Some(ref http) = config_root.http {
        if http.client_timeout_secs == 0 {
            errors.push("http.client_timeout_secs: must be > 0".to_string());
        }
    }

    // ── Categories validation ──
    if let Some(ref cats) = config_root.categories {
        for (name, cat) in cats {
            if cat.threshold == 0 {
                errors.push(format!("categories.{}.threshold: must be > 0", name));
            }
            if cat.priority == 0 {
                errors.push(format!("categories.{}.priority: must be > 0", name));
            }
        }
    } else {
        errors.push("missing [categories] section".to_string());
    }

    // ── Routing validation: ensure all category references exist ──
    if let Some(ref routing) = config_root.routing {
        if let Some(ref cats) = config_root.categories {
            for route_key in routing.keys() {
                if route_key != "DEFAULT" && !cats.contains_key(route_key.as_str()) {
                    errors.push(format!(
                        "routing.{}: references unknown category '{}'",
                        route_key, route_key
                    ));
                }
            }
        }
    }

    // ── Auth provider validation ──
    if let Some(ref providers) = config_root.auth_providers {
        for (i, p) in providers.iter().enumerate() {
            if p.type_.is_empty() {
                errors.push(format!("auth_provider[{}]: missing type", i));
            }
        }
    }

    // ── Model costs validation ──
    if let Some(ref costs) = config_root.model_costs {
        for (name, cost) in costs {
            if *cost <= 0.0 {
                errors.push(format!("model_costs.{}: must be > 0.0", name));
            }
        }
    }

    // ── Patterns directory validation ──
    let patterns_dir = config_root
        .patterns_dir
        .unwrap_or_else(|| PathBuf::from("./patterns"));
    if patterns_dir.exists() && !patterns_dir.is_dir() {
        errors.push(format!(
            "patterns_dir '{}': exists but is not a directory",
            patterns_dir.display()
        ));
    }

    // ── Pattern file resolution & regex validation ──
    if let Some(ref cats) = config_root.categories {
        for (name, cat) in cats {
            // Resolve patterns from external file or inline
            let patterns: Vec<PatternEntry> = if let Some(ref pf) = cat.patterns_file {
                match loader::load_patterns_from_file(pf, &patterns_dir) {
                    Ok(entries) => {
                        // Validate each compiled regex with file:line context
                        let mut has_error = false;
                        for (idx, entry) in entries.iter().enumerate() {
                            if let Err(e) = regex::Regex::new(&entry.regex) {
                                errors.push(format!(
                                    "{}:{}: pattern {}: {}",
                                    pf,
                                    idx + 1,
                                    entry.regex,
                                    e
                                ));
                                has_error = true;
                            }
                        }
                        if has_error {
                            continue; // already recorded per-pattern errors
                        }
                        entries
                    }
                    Err(e) => {
                        errors.push(format!("categories.{}.patterns_file '{}': {}", name, pf, e));
                        continue;
                    }
                }
            } else {
                cat.patterns.clone()
            };

            // Validate inline patterns with category context
            for (idx, entry) in patterns.iter().enumerate() {
                if let Err(e) = regex::Regex::new(&entry.regex) {
                    errors.push(format!(
                        "categories.{}.patterns[{}]: {}: {}",
                        name, idx, entry.regex, e
                    ));
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Merge `overlay` into `base` using a two-tier strategy:
///
/// - **Field-level merge** (`server`, `http`, `database`, `persistence`,
///   `dashboard`): each field of the overlay wins individually, leaving
///   untouched base fields intact.
/// - **Full replacement** (`cors`, `cache`, `classifiers`, `regex_classifier`,
///   `llm_classifier`, `fewshot_classifier`, `categories`, `auth_providers`,
///   `model_costs`, `negative_patterns`, `patterns_dir`, scalar fields): the
///   entire section is replaced when the overlay provides it.
/// - **Key-level merge** (`routing`): overlay entries are upserted into the
///   base routing table rather than wholesale replacing it.
///
/// This allows an `init_template.toml` or environment-specific overlay to
/// selectively override parts of the base `config.toml` without repeating
/// unchanged sections.
pub(crate) fn merge_configs(base: &mut ConfigRoot, overlay: ConfigRoot) {
    if let Some(s) = overlay.server {
        if let Some(ref mut b) = base.server {
            b.port = s.port;
            b.log_level = s.log_level;
            b.log_format = s.log_format;
        } else {
            base.server = Some(s);
        }
    }
    if let Some(s) = overlay.http {
        if let Some(ref mut b) = base.http {
            b.max_upstream_body_bytes = s.max_upstream_body_bytes;
            b.keepalive_interval_secs = s.keepalive_interval_secs;
            b.request_body_limit_bytes = s.request_body_limit_bytes;
            b.client_timeout_secs = s.client_timeout_secs;
            b.client_connect_timeout_secs = s.client_connect_timeout_secs;
            b.streaming_channel_capacity = s.streaming_channel_capacity;
        } else {
            base.http = Some(s);
        }
    }
    if let Some(s) = overlay.cors {
        base.cors = Some(s);
    }
    if let Some(s) = overlay.database {
        if let Some(ref mut b) = base.database {
            b.connection_retries = s.connection_retries;
            b.retry_base_ms = s.retry_base_ms;
            b.max_connections = s.max_connections;
            b.acquire_timeout_secs = s.acquire_timeout_secs;
            b.idle_timeout_secs = s.idle_timeout_secs;
            b.log_concurrency_limit = s.log_concurrency_limit;
        } else {
            base.database = Some(s);
        }
    }
    if let Some(s) = overlay.persistence {
        if let Some(ref mut b) = base.persistence {
            b.backend = s.backend;
            b.sqlite_path = s.sqlite_path;
        } else {
            base.persistence = Some(s);
        }
    }
    if let Some(s) = overlay.dashboard {
        if let Some(ref mut b) = base.dashboard {
            b.default_hours = s.default_hours;
            b.hours_min = s.hours_min;
            b.hours_max = s.hours_max;
            b.page_limit = s.page_limit;
            b.page_limit_max = s.page_limit_max;
            b.recent_count = s.recent_count;
        } else {
            base.dashboard = Some(s);
        }
    }
    if let Some(s) = overlay.cache {
        base.cache = Some(s);
    }
    if let Some(v) = overlay.baseline_model {
        base.baseline_model = Some(v);
    }
    if let Some(v) = overlay.classify_db_log {
        base.classify_db_log = Some(v);
    }
    if let Some(v) = overlay.classifiers {
        base.classifiers = Some(v);
    }
    if let Some(v) = overlay.regex_classifier {
        base.regex_classifier = Some(v);
    }
    if let Some(v) = overlay.llm_classifier {
        base.llm_classifier = Some(v);
    }
    if let Some(v) = overlay.fewshot_classifier {
        base.fewshot_classifier = Some(v);
    }
    if let Some(v) = overlay.categories {
        base.categories = Some(v);
    }
    if let Some(v) = overlay.auth_providers {
        base.auth_providers = Some(v);
    }
    if let Some(v) = overlay.model_costs {
        base.model_costs = Some(v);
    }
    if let Some(overlay_routing) = overlay.routing {
        let base_routing = base.routing.get_or_insert_with(HashMap::new);
        for (key, entry) in overlay_routing {
            base_routing.insert(key, entry);
        }
    }
    if let Some(v) = overlay.patterns_dir {
        base.patterns_dir = Some(v);
    }
    if let Some(v) = overlay.negative_patterns {
        base.negative_patterns = Some(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validate_success_on_embedded_config() {
        // Validates the embedded config.toml (should always be valid)
        let result = run_validation(None);
        assert!(
            result.is_ok(),
            "embedded config should be valid: {:?}",
            result.err()
        );
    }

    #[test]
    fn validate_invalid_regex_inline() {
        let toml = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
patterns = [{ regex = "[invalid", weight = 1 }]
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_bad_regex.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        assert!(all.contains("invalid"), "should report regex error: {all}");
        assert!(
            all.contains("patterns[0]"),
            "should include pattern index: {all}"
        );
    }

    #[test]
    fn validate_schema_error_invalid_port() {
        let toml = r#"
[server]
port = 0
log_level = "info"
log_format = "compact"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_bad_port.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        assert!(all.contains("port"), "should report port error: {all}");
    }

    #[test]
    fn validate_schema_error_invalid_log_level() {
        let toml = r#"
[server]
port = 10000
log_level = "bogus"
log_format = "compact"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_bad_loglevel.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        assert!(
            all.contains("log_level"),
            "should report log_level error: {all}"
        );
    }

    #[test]
    fn validate_schema_error_missing_categories() {
        let toml = r#"
[server]
port = 10000
log_level = "info"
log_format = "compact"
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_no_cats.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        assert!(
            all.contains("categories"),
            "should report missing categories: {all}"
        );
    }

    #[test]
    fn validate_schema_error_zero_threshold() {
        let toml = r#"
[categories.CASUAL]
description = "Simple"
threshold = 0
priority = 4
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_zero_thresh.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        assert!(
            all.contains("threshold"),
            "should report threshold error: {all}"
        );
    }

    #[test]
    fn validate_collects_multiple_errors() {
        let toml = r#"
[server]
port = 0
log_level = "nope"
log_format = "compact"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let tmp_dir = std::env::temp_dir();
        let file_path = tmp_dir.join("test_validate_multi.toml");
        std::fs::write(&file_path, toml).unwrap();

        let result = run_validation(Some(file_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        // Should have at least 2 errors: port + log_level
        assert!(
            errors.len() >= 2,
            "should collect multiple errors, got {}: {:?}",
            errors.len(),
            errors
        );
    }

    #[test]
    fn validate_external_pattern_file_not_found() {
        use std::io::Write;
        let tmp_dir = std::env::temp_dir();
        let config_path = tmp_dir.join("test_validate_missing_patterns.toml");
        let mut file = std::fs::File::create(&config_path).unwrap();
        write!(
            file,
            r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
patterns_file = "nonexistent.patterns"
"#
        )
        .unwrap();
        drop(file);

        let result = run_validation(Some(config_path.to_str().unwrap()));
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let all = errors.join(" ");
        // Should mention the missing file
        assert!(
            all.contains("nonexistent.patterns") || all.contains("cannot read"),
            "should report missing pattern file: {all}"
        );
    }

    // ── Phase 5: Routing example parse tests ──
    // Each routing example in routing_examples/ must parse as a valid
    // ConfigRoot (with a [routing.*] table). Validates the rewrite from
    // the legacy flat format ([CATEGORY]) to the nested format
    // ([routing.CATEGORY]) compatible with CONFIG_PATH overlays.

    #[test]
    fn routing_example_openrouter_parses_as_config_root() {
        let content = include_str!("../../routing_examples/routing-openrouter.toml");
        let root: ConfigRoot = toml::from_str(content).expect("openrouter example should parse");
        let routing = root.routing.expect("routing section should be present");
        // The 5 expected route categories
        for key in [
            "FILE_READING",
            "SYNTAX_FIX",
            "COMPLEX_REASONING",
            "CASUAL",
            "DEFAULT",
        ] {
            let entry = routing
                .get(key)
                .unwrap_or_else(|| panic!("missing route key {key} in openrouter example"));
            assert!(
                !entry.primary().model.is_empty(),
                "{key} model should be set"
            );
            assert!(
                !entry.primary().endpoint.is_empty(),
                "{key} endpoint should be set"
            );
            assert!(
                !entry.primary().provider_type.is_empty(),
                "{key} provider_type should be set"
            );
        }
    }

    #[test]
    fn routing_example_nvidia_nim_parses_as_config_root() {
        let content = include_str!("../../routing_examples/routing-nvidia-nim.toml");
        let root: ConfigRoot = toml::from_str(content).expect("nvidia-nim example should parse");
        let routing = root.routing.expect("routing section should be present");
        // Endpoints must be present in every entry — the legacy file omitted
        // them, producing empty-string endpoints. Verify the rewrite fixed it.
        for key in [
            "FILE_READING",
            "SYNTAX_FIX",
            "COMPLEX_REASONING",
            "CASUAL",
            "DEFAULT",
        ] {
            let entry = routing
                .get(key)
                .unwrap_or_else(|| panic!("missing route key {key} in nvidia-nim example"));
            assert!(
                !entry.primary().endpoint.is_empty(),
                "{key} endpoint should be present (legacy version omitted it): {entry:?}"
            );
        }
    }

    #[test]
    fn routing_example_manual_tests_parses_as_config_root() {
        let content = include_str!("../../routing_examples/routing-manual-tests.toml");
        let root: ConfigRoot = toml::from_str(content).expect("manual-tests example should parse");
        let routing = root.routing.expect("routing section should be present");
        assert!(routing.contains_key("DEFAULT"));
        // FALLBACK (legacy key) must be gone
        assert!(!routing.contains_key("FALLBACK"));
    }

    #[test]
    fn routing_example_unreachable_parses_as_config_root() {
        let content = include_str!("../../routing_examples/routing_unreachable.toml");
        let root: ConfigRoot = toml::from_str(content).expect("unreachable example should parse");
        let routing = root.routing.expect("routing section should be present");
        assert!(!routing.contains_key("FALLBACK"));
        assert!(routing.contains_key("DEFAULT"));
    }

    #[test]
    fn yaml_config_roundtrip() {
        let toml = r#"
[server]
port = 9999
log_level = "debug"

[http]
client_timeout_secs = 30

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let yaml = r#"
server:
  port: 9999
  log_level: debug
http:
  client_timeout_secs: 30
categories:
  CASUAL:
    description: Simple
    threshold: 1
    priority: 4
"#;
        let toml_root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        let yaml_root: ConfigRoot = serde_yaml::from_str(yaml).expect("valid YAML");
        assert_eq!(
            toml_root.server.as_ref().map(|s| s.port),
            yaml_root.server.as_ref().map(|s| s.port)
        );
        assert_eq!(
            toml_root.server.as_ref().map(|s| s.log_level.as_str()),
            yaml_root.server.as_ref().map(|s| s.log_level.as_str())
        );
        assert_eq!(
            toml_root.http.as_ref().map(|h| h.client_timeout_secs),
            yaml_root.http.as_ref().map(|h| h.client_timeout_secs)
        );
        assert_eq!(
            toml_root
                .categories
                .as_ref()
                .and_then(|c| c.get("CASUAL"))
                .map(|c| c.threshold),
            yaml_root
                .categories
                .as_ref()
                .and_then(|c| c.get("CASUAL"))
                .map(|c| c.threshold)
        );
    }

    #[test]
    fn yaml_config_with_auth_providers() {
        let yaml = r#"
server:
  port: 10000
auth_provider:
  - type: openai_compatible
    header: authorization
    value_template: "Bearer {api_key}"
  - type: anthropic
    header: x-api-key
    value_template: "{api_key}"
categories:
  CASUAL:
    description: Simple
    threshold: 1
    priority: 4
"#;
        let root: ConfigRoot = serde_yaml::from_str(yaml).expect("valid YAML");
        let providers = root
            .auth_providers
            .expect("auth_providers should be present");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].type_, "openai_compatible");
        assert_eq!(providers[1].type_, "anthropic");
    }

    #[test]
    fn load_config_from_path_toml() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_load_config.toml");
        let mut file = std::fs::File::create(&file_path).expect("create temp file");
        write!(file, "[server]\nport = 8888\n").expect("write");
        drop(file);

        let result = load_config_from_path(file_path.to_str().unwrap());
        assert!(
            result.is_ok(),
            "TOML load should succeed: {:?}",
            result.err()
        );
        let root = result.unwrap();
        assert_eq!(root.server.unwrap().port, 8888);
    }

    #[test]
    fn load_config_from_path_yaml() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_load_config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create temp file");
        write!(file, "server:\n  port: 7777\n").expect("write");
        drop(file);

        let result = load_config_from_path(file_path.to_str().unwrap());
        assert!(
            result.is_ok(),
            "YAML load should succeed: {:?}",
            result.err()
        );
        let root = result.unwrap();
        assert_eq!(root.server.unwrap().port, 7777);
    }

    #[test]
    fn load_config_from_path_unknown_extension_defaults_to_toml() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_load_config.conf");
        let mut file = std::fs::File::create(&file_path).expect("create temp file");
        write!(file, "[server]\nport = 6666\n").expect("write");
        drop(file);

        let result = load_config_from_path(file_path.to_str().unwrap());
        assert!(result.is_ok(), "unknown ext should be treated as TOML");
    }

    #[test]
    fn load_config_from_path_missing_file() {
        let result = load_config_from_path("/nonexistent/path/config.toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot read"));
    }

    #[test]
    fn merge_configs_overrides_categories() {
        let mut base: ConfigRoot = toml::from_str(
            r#"
[categories.CASUAL]
description = "Original"
threshold = 1
priority = 4
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[categories.FILE_READING]
description = "Override"
threshold = 3
priority = 1
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        // Categories is an override key → complete replacement
        let cats = base.categories.unwrap();
        assert!(!cats.contains_key("CASUAL"));
        assert_eq!(cats.get("FILE_READING").unwrap().description, "Override");
    }

    #[test]
    fn merge_configs_shallow_merge_server() {
        let mut base: ConfigRoot = toml::from_str(
            r#"
[server]
port = 10000
log_level = "info"
log_format = "compact"
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[server]
port = 20000
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        let server = base.server.unwrap();
        // port overridden, log_level and log_format preserved
        assert_eq!(server.port, 20000);
        assert_eq!(server.log_level, "info");
        assert_eq!(server.log_format, "compact");
    }

    #[test]
    fn merge_configs_routing_per_key_merge() {
        // Base has DEFAULT + FILE_READING. Overlay has only FILE_READING with
        // a different model. After merge: DEFAULT is preserved (untouched),
        // FILE_READING uses the overlay's model.
        let mut base: ConfigRoot = toml::from_str(
            r#"
[routing.DEFAULT]
model = "base-default"
endpoint = "https://base.example/v1/chat/completions"
provider_type = "openai_compatible"

[routing.FILE_READING]
model = "base-file-reading"
endpoint = "https://base.example/v1/chat/completions"
provider_type = "openai_compatible"
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[routing.FILE_READING]
model = "overlay-file-reading"
endpoint = "https://overlay.example/v1/chat/completions"
provider_type = "openai_compatible"
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        let routing = base.routing.expect("routing should be present after merge");
        // DEFAULT preserved from base
        let default = routing.get("DEFAULT").expect("DEFAULT preserved from base");
        assert_eq!(default.primary().model, "base-default");
        assert_eq!(
            default.primary().endpoint,
            "https://base.example/v1/chat/completions"
        );
        // FILE_READING replaced by overlay
        let file_reading = routing
            .get("FILE_READING")
            .expect("FILE_READING present after merge");
        assert_eq!(file_reading.primary().model, "overlay-file-reading");
        assert_eq!(
            file_reading.primary().endpoint,
            "https://overlay.example/v1/chat/completions"
        );
    }

    #[test]
    fn merge_configs_routing_full_overlay() {
        // Overlay specifies every route in the base — verify all are replaced
        // (per-key merge still produces the same end state as full-replacement
        // when the overlay covers every key).
        let mut base: ConfigRoot = toml::from_str(
            r#"
[routing.DEFAULT]
model = "base-default"
endpoint = "https://base.example/v1/chat/completions"
provider_type = "openai_compatible"

[routing.FILE_READING]
model = "base-fr"
endpoint = "https://base.example/v1/chat/completions"
provider_type = "openai_compatible"
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[routing.DEFAULT]
model = "new-default"
endpoint = "https://new.example/v1/chat/completions"
provider_type = "openai_compatible"

[routing.FILE_READING]
model = "new-fr"
endpoint = "https://new.example/v1/chat/completions"
provider_type = "openai_compatible"
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        let routing = base.routing.expect("routing should be present after merge");
        assert_eq!(
            routing.get("DEFAULT").unwrap().primary().model,
            "new-default"
        );
        assert_eq!(
            routing.get("FILE_READING").unwrap().primary().model,
            "new-fr"
        );
    }

    #[test]
    fn merge_configs_routing_initialize_from_none() {
        // Base has no routing. Overlay has one route. After merge: base routing
        // exists with the overlay's entry (the None base case for get_or_insert).
        let mut base: ConfigRoot = toml::from_str(
            r#"
[server]
port = 10000
log_level = "info"
log_format = "compact"
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[routing.SYNTAX_FIX]
model = "sf-model"
endpoint = "https://sf.example/v1/chat/completions"
provider_type = "openai_compatible"
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        let routing = base.routing.expect("routing initialized from None");
        assert_eq!(routing.len(), 1);
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().model,
            "sf-model"
        );
    }

    #[test]
    fn config_root_with_patterns_dir() {
        let toml = r#"
patterns_dir = "./custom_patterns"
[server]
port = 9999
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        assert_eq!(
            root.patterns_dir.as_ref().map(|p: &PathBuf| p.to_str()),
            Some(Some("./custom_patterns"))
        );
    }

    #[test]
    fn config_root_with_patterns_file() {
        let toml = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
patterns_file = "casual.patterns"
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        let cat = root.categories.as_ref().unwrap().get("CASUAL").unwrap();
        assert_eq!(cat.patterns_file.as_deref(), Some("casual.patterns"));
    }

    #[test]
    fn merge_configs_cache_override() {
        let mut base: ConfigRoot = toml::from_str(
            r#"
[cache]
ttl_secs = 300
max_entries = 1000
"#,
        )
        .expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(
            r#"
[cache]
ttl_secs = 60
max_entries = 100
"#,
        )
        .expect("valid TOML");

        merge_configs(&mut base, overlay);
        let cache = base.cache.expect("cache should be present after merge");
        assert_eq!(cache.ttl_secs, 60);
        assert_eq!(cache.max_entries, 100);
    }
}
