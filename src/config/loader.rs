use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

use super::routing::*;
use super::types::*;
use super::ConfigRoot;
use crate::config::types::{CategoryConfig, NegativePatternConfig, PatternEntry};

#[cfg(test)]
pub(crate) const CONFIG_DEFAULT: &str = "config.toml";
#[cfg(test)]
pub(crate) const ROUTING_CONFIG_LEGACY: &str = "routing.toml";

/// Extract cache configuration from an already-parsed [`ConfigRoot`].
///
/// Returns `None` — disabling the cache — when the `[cache]` section is
/// absent **or** when `max_entries` is explicitly set to `0`.
pub(crate) fn load_cache_config_from_value(root: &ConfigRoot) -> Option<CacheConfig> {
    match &root.cache {
        Some(cfg) if cfg.max_entries > 0 => Some(cfg.clone()),
        Some(_) => {
            debug!("[cache] max_entries is 0; cache disabled");
            None
        }
        None => None,
    }
}

/// Extract dashboard configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`DashboardConfig::default`] when the `[dashboard]` section
/// is absent.
pub(crate) fn load_dashboard_config_from_value(root: &ConfigRoot) -> DashboardConfig {
    root.dashboard.clone().unwrap_or_else(|| {
        debug!("[dashboard] section not found; using defaults");
        DashboardConfig::default()
    })
}

/// Extract server configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`ServerConfig::default`] (port 10000, info/compact) when the
/// `[server]` section is absent.
pub(crate) fn load_server_config_from_value(root: &ConfigRoot) -> ServerConfig {
    root.server.clone().unwrap_or_else(|| {
        debug!("[server] section not found; using defaults");
        ServerConfig::default()
    })
}

/// Extract HTTP layer configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`HttpConfig::default`] when the `[http]` section is absent.
pub(crate) fn load_http_config_from_value(root: &ConfigRoot) -> HttpConfig {
    root.http.clone().unwrap_or_else(|| {
        debug!("[http] section not found; using defaults");
        HttpConfig::default()
    })
}

/// Extract database pool configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`DatabaseConfig::default`] when the `[database]` section
/// is absent.
pub(crate) fn load_database_config_from_value(root: &ConfigRoot) -> DatabaseConfig {
    root.database.clone().unwrap_or_else(|| {
        debug!("[database] section not found; using defaults");
        DatabaseConfig::default()
    })
}

/// Extract upstream auth provider configs from an already-parsed [`ConfigRoot`].
/// Returns an empty `Vec` when the `[[auth_provider]]` array is absent, meaning
/// no automatic credential injection will occur.
pub(crate) fn load_auth_providers_from_value(root: &ConfigRoot) -> Vec<AuthProviderConfig> {
    root.auth_providers.clone().unwrap_or_else(|| {
        debug!("[auth_provider] section not found; no auth providers configured");
        vec![]
    })
}

/// Extract CORS configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`CorsConfig::default`] (empty allowed-origins, CORS
/// disabled) when the `[cors]` section is absent.
pub(crate) fn load_cors_config_from_value(root: &ConfigRoot) -> CorsConfig {
    root.cors.clone().unwrap_or_else(|| {
        debug!("[cors] section not found; using defaults (empty allowed_origins)");
        CorsConfig::default()
    })
}

/// Extract persistence backend configuration from an already-parsed
/// [`ConfigRoot`]. Falls back to [`PersistenceSettings::default`] (in-memory
/// backend) when the `[persistence]` section is absent.
pub(crate) fn load_persistence_config_from_value(root: &ConfigRoot) -> PersistenceSettings {
    root.persistence.clone().unwrap_or_else(|| {
        debug!("[persistence] section not found; using defaults (memory backend)");
        PersistenceSettings::default()
    })
}

/// Parse an integer from an environment variable with optional range clamping.
///
/// Returns `default` when the variable is unset, empty, non-numeric, or
/// outside `[min, max]`. Emits a `warn!` log for invalid or out-of-range
/// values so misconfigurations are visible in logs without crashing the
/// server.
#[cfg(test)]
pub(crate) fn parse_env_int(var: &str, default: i32, min: Option<i32>, max: Option<i32>) -> i32 {
    let val_str = match std::env::var(var) {
        Ok(s) => s,
        Err(_) => return default,
    };
    if val_str.trim().is_empty() {
        return default;
    }
    let val: i32 = match val_str.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            warn!(
                "Invalid integer value for {}: '{:?}'; using default {}",
                var, val_str, default
            );
            return default;
        }
    };
    if let Some(min) = min {
        if val < min {
            warn!(
                "{} value {} below minimum {}; using default {}",
                var, val, min, default
            );
            return default;
        }
    }
    if let Some(max) = max {
        if val > max {
            warn!(
                "{} value {} above maximum {}; using default {}",
                var, val, max, default
            );
            return default;
        }
    }
    val
}

/// Build an emergency routing table from hardcoded Ollama defaults.
///
/// Used as a last resort when no config file is found on disk. Every
/// provided `categories` entry is wired to `llama3.1` on
/// `http://localhost:11434` (the default Ollama endpoint), and the same
/// model is used as the fallback entry. This lets a fresh local install work
/// without any API key or config file.
pub(crate) fn hardcoded_routing(
    categories: &[CategoryConfig],
) -> (HashMap<String, RouteEntry>, RouteEntry) {
    let endpoint = "http://localhost:11434/v1/chat/completions";
    let mut routing = HashMap::new();

    for cat in categories {
        routing.insert(
            cat.name.clone(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: DEFAULT_MODEL_LOCAL.to_string(),
                    endpoint: endpoint.to_string(),
                    provider_type: "ollama".to_string(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
    }

    let fallback = RouteEntry {
        providers: vec![ProviderEntry {
            model: DEFAULT_MODEL_LOCAL.to_string(),
            endpoint: endpoint.to_string(),
            provider_type: "ollama".to_string(),
            api_key_env: None,
            timeout_ms: None,
        }],
        cost_per_1m_input_tokens: None,
    };
    (routing, fallback)
}

#[cfg(test)]
pub(crate) fn load_routing_from_file(path: &str) -> Result<HashMap<String, RouteEntry>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {}", path, e))?;
    let root: toml::Value =
        toml::from_str(&content).map_err(|e| format!("Invalid TOML in {}: {}", path, e))?;
    let table = root
        .as_table()
        .ok_or_else(|| format!("Root must be a table in {}", path))?;
    let routing_table = match table.get("routing") {
        Some(toml::Value::Table(t)) => t,
        _ => return Err("File must contain a [routing] section".to_string()),
    };
    let mut routing = HashMap::new();
    for (key, value) in routing_table {
        let model = if let Some(m) = value.get("model").and_then(|v| v.as_str()) {
            m.to_string()
        } else {
            warn!(category = %key, "routing.toml missing 'model' for category; using DEFAULT_MODEL");
            DEFAULT_MODEL.to_string()
        };
        let endpoint = value
            .get("endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cost_per_1m_input_tokens = value
            .get("cost_per_1m_input_tokens")
            .and_then(|v| v.as_float());
        let provider_type = if let Some(pt) = value.get("provider_type").and_then(|v| v.as_str()) {
            pt.to_string()
        } else {
            warn!(category = %key, "routing.toml missing 'provider_type' for category; defaulting to openai_compatible (empty)");
            String::new()
        };
        let api_key_env = if let Some(ake) = value.get("api_key_env").and_then(|v| v.as_str()) {
            Some(ake.to_string())
        } else {
            warn!(category = %key, "routing.toml missing 'api_key_env' for category; no API key will be resolved");
            None
        };
        routing.insert(
            key.to_uppercase(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model,
                    endpoint,
                    provider_type,
                    api_key_env,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens,
            },
        );
    }
    Ok(routing)
}

#[cfg(test)]
pub(crate) fn load_routing() -> (HashMap<String, RouteEntry>, RouteEntry) {
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| CONFIG_DEFAULT.to_string());

    // Try config.toml first, then routing.toml for backward compat
    let path = if std::path::Path::new(&config_path).exists() {
        config_path
    } else if std::path::Path::new(ROUTING_CONFIG_LEGACY).exists() {
        tracing::info!("Using legacy routing.toml; consider renaming to config.toml");
        ROUTING_CONFIG_LEGACY.to_string()
    } else {
        tracing::warn!("No config.toml or routing.toml found; using hardcoded routing defaults");
        return hardcoded_routing(&[]);
    };

    let mut routing = match load_routing_from_file(&path) {
        Ok(r) => {
            tracing::info!("Routing: loaded from {path}");
            r
        }
        Err(e) => {
            tracing::warn!("{e}; using hardcoded routing defaults");
            return hardcoded_routing(&[]);
        }
    };
    let fallback_entry = routing.remove("DEFAULT").unwrap_or_else(|| RouteEntry {
        providers: vec![ProviderEntry {
            model: DEFAULT_MODEL.to_string(),
            endpoint: String::new(),
            provider_type: String::new(),
            api_key_env: None,
            timeout_ms: None,
        }],
        cost_per_1m_input_tokens: None,
    });
    (routing, fallback_entry)
}

/// Build the routing dispatch table from an already-parsed [`ConfigRoot`].
///
/// Reads the `[routing]` section and returns `(routing_map, fallback)` where:
/// - All route keys are uppercased before insertion.
/// - The `DEFAULT` key, if present, is removed from the map and returned as
///   the fallback [`RouteEntry`].
/// - Missing `model`, `provider_type`, or `api_key_env` are warned about but
///   do not cause an error.
///
/// Returns an empty map + `DEFAULT_MODEL` fallback when `[routing]` is absent.
pub(crate) fn routing_from_value(
    config_root: &ConfigRoot,
) -> Result<(HashMap<String, RouteEntry>, RouteEntry), String> {
    let routing_table = match &config_root.routing {
        Some(t) => t,
        None => {
            debug!("[routing] section not found; no routing entries configured");
            let fallback = RouteEntry {
                providers: vec![ProviderEntry {
                    model: DEFAULT_MODEL.to_string(),
                    endpoint: String::new(),
                    provider_type: String::new(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            };
            return Ok((HashMap::new(), fallback));
        }
    };

    let default_model = routing_table
        .get("DEFAULT")
        .map(|e| e.primary().model.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();

    let mut routing = HashMap::new();
    for (key, entry) in routing_table {
        let mut providers = entry.providers.clone();
        for provider in &mut providers {
            if provider.model.is_empty() {
                warn!(category = %key, "routing section missing 'model' for category; using DEFAULT model");
                provider.model = default_model.clone();
            }
            if provider.provider_type.is_empty() {
                warn!(category = %key, "routing section missing 'provider_type' for category; defaulting to empty");
            }
            if provider.api_key_env.is_none() {
                warn!(category = %key, "routing section missing 'api_key_env' for category; no API key will be resolved");
            }
        }
        routing.insert(
            key.to_uppercase(),
            RouteEntry {
                providers,
                cost_per_1m_input_tokens: entry.cost_per_1m_input_tokens,
            },
        );
    }
    let fallback = routing.remove("DEFAULT").unwrap_or_else(|| RouteEntry {
        providers: vec![ProviderEntry {
            model: default_model,
            endpoint: String::new(),
            provider_type: String::new(),
            api_key_env: None,
            timeout_ms: None,
        }],
        cost_per_1m_input_tokens: None,
    });
    Ok((routing, fallback))
}

/// Extract and sort intent categories from an already-parsed [`ConfigRoot`].
///
/// Reads the `[categories]` table, assigns each map key as the category
/// `name`, and sorts the resulting `Vec` by `priority` (ascending) so the
/// classifier evaluates higher-priority categories first.
///
/// Returns `Err` when the `[categories]` section is absent or empty.
pub(crate) fn load_categories_from_value(
    config_root: &ConfigRoot,
) -> Result<Vec<CategoryConfig>, String> {
    let cats_map = config_root
        .categories
        .as_ref()
        .ok_or_else(|| "No [categories] section found".to_string())?;

    if cats_map.is_empty() {
        return Err("No categories defined".to_string());
    }

    let mut categories: Vec<CategoryConfig> = cats_map
        .iter()
        .map(|(name, cat)| {
            let mut c = cat.clone();
            c.name = name.clone();
            c
        })
        .collect();

    categories.sort_by_key(|c| c.priority);
    Ok(categories)
}

/// Extract negative suppression patterns from an already-parsed [`ConfigRoot`].
/// Returns an empty `Vec` when the `[negative_patterns]` section is absent.
/// Negative patterns are applied after scoring to veto false-positive
/// category matches.
pub(crate) fn load_negative_patterns_from_value(root: &ConfigRoot) -> Vec<NegativePatternConfig> {
    root.negative_patterns.clone().unwrap_or_else(|| {
        debug!("[negative_patterns] section not found; no negative patterns configured");
        vec![]
    })
}

/// Build a [`ModelCosts`] lookup table from config and routing data.
///
/// Merges two sources with the following priority (higher-priority wins):
/// 1. Top-level `[model_costs]` table entries.
/// 2. Per-route `cost_per_1m_input_tokens` fields — these override the global
///    table for the model used by that route.
///
/// Call this after [`routing_from_value`] so the routing map is available.
pub(crate) fn build_model_costs(
    config_root: &ConfigRoot,
    routing: &HashMap<String, RouteEntry>,
) -> ModelCosts {
    let mut costs = HashMap::new();

    if let Some(model_costs_table) = &config_root.model_costs {
        for (model_name, cost) in model_costs_table {
            costs.insert(model_name.clone(), *cost);
        }
    }

    for entry in routing.values() {
        if let Some(override_cost) = entry.cost_per_1m_input_tokens {
            costs.insert(entry.primary().model.clone(), override_cost);
        }
    }
    ModelCosts::from_costs(costs)
}

// ── Config Format Detection & Loading ──

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ConfigFormat {
    Toml,
    Yaml,
}

#[cfg(test)]
pub(crate) fn detect_format(path: &str) -> ConfigFormat {
    match Path::new(path).extension().and_then(|s| s.to_str()) {
        Some("yaml" | "yml") => ConfigFormat::Yaml,
        _ => ConfigFormat::Toml,
    }
}

/// Load weighted regex patterns from an external pattern file.
///
/// The file format is one entry per line:
/// ```text
/// <weight (u8)> | <regex>
/// ```
/// Lines starting with `#` and blank lines are ignored. Both `weight` and
/// `regex` are required on every data line.
///
/// # Security
/// Path traversal is guarded: `path` is resolved relative to `base_dir` and
/// the resolved path must remain inside `base_dir`. Attempts to escape via
/// `../` are rejected with an error rather than silently reading an arbitrary
/// file.
///
/// Returns `Err` with file:line context on any format, regex, or IO error.
pub(crate) fn load_patterns_from_file(
    path: &str,
    base_dir: &Path,
) -> Result<Vec<PatternEntry>, String> {
    let full_path = base_dir
        .join(path)
        .canonicalize()
        .map_err(|e| format!("invalid pattern path {}: {}", path, e))?;
    let base_dir = base_dir
        .canonicalize()
        .map_err(|e| format!("invalid patterns_dir: {}", e))?;
    if !full_path.starts_with(&base_dir) {
        return Err(format!("pattern file path '{}' escapes patterns_dir", path));
    }
    let content = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("cannot read pattern file {}: {}", full_path.display(), e))?;

    let mut entries = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (weight_str, regex) = trimmed.split_once(" | ").ok_or_else(|| {
            format!(
                "{}:{}: invalid format, expected '<weight> | <regex>'",
                path,
                line_num + 1
            )
        })?;
        let weight = weight_str
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("{}:{}: invalid weight: {}", path, line_num + 1, e))?;
        entries.push(PatternEntry {
            regex: regex.to_string(),
            weight,
        });
    }
    Ok(entries)
}

/// Extract the global classifier pipeline config from an already-parsed
/// [`ConfigRoot`]. Falls back to [`ClassifiersConfig::default`] (enabled,
/// order: regex → fewshot → llm) when the `[classifiers]` section is absent.
pub(crate) fn load_classifiers_config_from_value(root: &ConfigRoot) -> ClassifiersConfig {
    root.classifiers.clone().unwrap_or_else(|| {
        tracing::debug!("No [classifiers] section in config; using defaults");
        ClassifiersConfig::default()
    })
}

/// Load regex classifier config from config.toml.
/// Returns default (enabled) if section is absent.
#[cfg(test)]
pub(crate) fn load_regex_classifier_config(path: &str) -> RegexClassifierConfig {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Cannot read config for regex classifier: {e}");
            return RegexClassifierConfig::default();
        }
    };
    let root: ConfigRoot = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Invalid TOML for regex classifier section: {e}");
            return RegexClassifierConfig::default();
        }
    };
    load_regex_classifier_config_from_value(&root)
}

/// Extract regex classifier configuration from an already-parsed [`ConfigRoot`].
/// Falls back to [`RegexClassifierConfig::default`] (enabled, short_prompt_len
/// = 30) when the `[regex_classifier]` section is absent.
pub(crate) fn load_regex_classifier_config_from_value(root: &ConfigRoot) -> RegexClassifierConfig {
    root.regex_classifier.clone().unwrap_or_else(|| {
        debug!("[regex_classifier] section not found; using defaults (enabled)");
        RegexClassifierConfig::default()
    })
}

/// Load LLM classifier config from config.toml.
/// Returns None if section is absent or enabled = false.
#[cfg(test)]
pub(crate) fn load_llm_classifier_config(path: &str) -> Option<LlmClassifierConfig> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Cannot read config for LLM classifier: {e}");
            return None;
        }
    };
    let root: ConfigRoot = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Invalid TOML for LLM classifier section: {e}");
            return None;
        }
    };
    load_llm_classifier_config_from_value(&root)
}

/// Extract few-shot classifier configuration from an already-parsed [`ConfigRoot`].
/// Returns `None` when the `[fewshot_classifier]` section is absent **or**
/// when `enabled = false`, so callers can skip construction of the classifier
/// with a single `?` or `if let Some`.
pub(crate) fn load_fewshot_config_from_value(root: &ConfigRoot) -> Option<FewShotConfig> {
    let cfg = root.fewshot_classifier.as_ref()?;
    if !cfg.enabled {
        return None;
    }
    Some(cfg.clone())
}

/// Extract LLM classifier configuration from an already-parsed [`ConfigRoot`].
/// Returns `None` when the `[llm_classifier]` section is absent **or** when
/// `enabled = false`, so callers can skip LLM classifier construction with a
/// single `?` or `if let Some`.
pub(crate) fn load_llm_classifier_config_from_value(
    root: &ConfigRoot,
) -> Option<LlmClassifierConfig> {
    let cfg = root.llm_classifier.as_ref()?;
    if !cfg.enabled {
        return None;
    }
    Some(cfg.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn test_categories() -> Vec<CategoryConfig> {
        vec![
            CategoryConfig {
                name: "FILE_READING".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 1,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "SYNTAX_FIX".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 2,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "COMPLEX_REASONING".to_string(),
                description: String::new(),
                threshold: 3,
                priority: 3,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
            CategoryConfig {
                name: "CASUAL".to_string(),
                description: String::new(),
                threshold: 1,
                priority: 4,
                patterns: vec![],
                patterns_file: None,
                dual_threshold: None,
            },
        ]
    }

    #[test]
    fn load_routing_from_file_success() {
        // Create temporary TOML content
        let toml_content = r#"
[routing.SYNTAX_FIX]
model = "test-sf-model"
endpoint = "https://test.endpoint"
provider_type = "openai_compatible"
api_key_env = "TEST_API_KEY"
cost_per_1m_input_tokens = 1.23

[routing.COMPLEX_REASONING]
model = "test-cr-model"
endpoint = "https://test.cr"
provider_type = "openai_compatible"
api_key_env = "TEST_API_KEY_CR"

[routing.DEFAULT]
model = "test-fallback"
endpoint = ""
provider_type = ""
api_key_env = ""
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_routing.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let result = load_routing_from_file(file_path.to_str().unwrap());
        assert!(result.is_ok(), "load_routing_from_file should succeed");
        let routing = result.unwrap();

        assert_eq!(routing.len(), 3);
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().model,
            "test-sf-model"
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().endpoint,
            "https://test.endpoint"
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().provider_type,
            "openai_compatible"
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().api_key_env,
            Some("TEST_API_KEY".to_string())
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().cost_per_1m_input_tokens,
            Some(1.23)
        );

        assert_eq!(
            routing.get("COMPLEX_REASONING").unwrap().primary().model,
            "test-cr-model"
        );
    }

    #[test]
    fn load_routing_from_file_missing() {
        let result = load_routing_from_file("/nonexistent/path/routing.toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot read"));
    }

    #[test]
    fn load_routing_from_file_invalid_toml() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("invalid_routing.toml");
        let mut file = std::fs::File::create(&file_path).expect("create temp file");
        file.write_all(b"not valid toml {{").expect("write");
        drop(file);

        let result = load_routing_from_file(file_path.to_str().unwrap());
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.contains("Invalid TOML"));
        } else {
            panic!("expected error");
        }
    }

    #[test]
    #[serial]
    fn hardcoded_routing_produces_expected_defaults() {
        let cats = test_categories();
        let (routing, fallback) = hardcoded_routing(&cats);

        assert_eq!(routing.len(), cats.len());
        for cat in &cats {
            assert!(
                routing.contains_key(cat.name.as_str()),
                "routing missing key for {}",
                cat.name
            );
            let entry = routing.get(cat.name.as_str()).unwrap();
            assert_eq!(entry.primary().model, DEFAULT_MODEL_LOCAL);
            assert!(entry.primary().endpoint.contains("localhost:11434"));
            assert_eq!(entry.primary().provider_type, "ollama");
            assert_eq!(entry.primary().api_key_env, None);
            assert_eq!(entry.cost_per_1m_input_tokens, None);
        }

        assert_eq!(fallback.primary().model, DEFAULT_MODEL_LOCAL);
        assert!(fallback.primary().endpoint.contains("localhost:11434"));
        assert_eq!(fallback.primary().provider_type, "ollama");
        assert_eq!(fallback.primary().api_key_env, None);
    }

    #[test]
    fn hardcoded_routing_uses_hardcoded_endpoint() {
        let (_, fallback) = hardcoded_routing(&test_categories());
        assert_eq!(
            fallback.primary().endpoint,
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    #[serial]
    fn load_routing_behavior() {
        // 1. When CONFIG_PATH points to a valid file, load_routing returns parsed routing and fallback
        let toml_content = r#"
[routing.SYNTAX_FIX]
model = "file-sf-model"
endpoint = "https://file.endpoint"
provider_type = "openai_compatible"
api_key_env = "FILE_API_KEY"

[routing.DEFAULT]
model = "file-fallback"
endpoint = ""
provider_type = ""
api_key_env = ""
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_valid_config.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        std::env::set_var("CONFIG_PATH", file_path.to_str().unwrap());
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 1);
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().primary().model,
            "file-sf-model"
        );
        // fallback should be the file-defined fallback
        assert_eq!(fallback.primary().model, "file-fallback");

        std::env::remove_var("CONFIG_PATH");

        // 2. When file is missing, fall back to hardcoded defaults (empty categories)
        std::env::set_var("CONFIG_PATH", "/nonexistent/config.toml");
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 0);
        assert_eq!(fallback.primary().model, DEFAULT_MODEL_LOCAL);

        std::env::remove_var("CONFIG_PATH");

        // 3. When file exists but TOML is invalid, fall back to hardcoded defaults (empty categories)
        use std::io::Write;
        let file_path_invalid = temp_dir.join("invalid_config.toml");
        let mut file = std::fs::File::create(&file_path_invalid).expect("create temp file");
        file.write_all(b"not valid toml {{").expect("write");
        drop(file);

        std::env::set_var("CONFIG_PATH", file_path_invalid.to_str().unwrap());
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 0);
        assert_eq!(fallback.primary().model, DEFAULT_MODEL_LOCAL);

        std::env::remove_var("CONFIG_PATH");
    }

    #[test]
    fn build_model_costs_applies_route_overrides() {
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "claude-3.5-sonnet".to_string(),
                    endpoint: "".to_string(),
                    provider_type: "".to_string(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: Some(5.00),
            },
        );
        routing.insert(
            "COMPLEX_REASONING".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "gpt-4o".to_string(),
                    endpoint: "".to_string(),
                    provider_type: "".to_string(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            RouteEntry {
                providers: vec![ProviderEntry {
                    model: "unknown-model".to_string(),
                    endpoint: "".to_string(),
                    provider_type: "".to_string(),
                    api_key_env: None,
                    timeout_ms: None,
                }],
                cost_per_1m_input_tokens: Some(2.50),
            },
        );

        let costs = build_model_costs(&ConfigRoot::default(), &routing);

        // Route overrides only (no hardcoded seeds anymore)
        assert_eq!(costs.get("claude-3.5-sonnet"), Some(5.00));
        // gpt-4o has no route override and no TOML entry → absent
        assert_eq!(costs.get("gpt-4o"), None);
        // gpt-4o-mini not in routing → absent
        assert_eq!(costs.get("gpt-4o-mini"), None);
        // deepseek-chat not in routing → absent
        assert_eq!(costs.get("deepseek-chat"), None);
        // Unknown model with route override
        assert_eq!(costs.get("unknown-model"), Some(2.50));
    }

    #[test]
    fn load_regex_classifier_config_default_enabled() {
        // Section absent → default enabled
        let toml_content = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_regex_default.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let cfg = load_regex_classifier_config(file_path.to_str().unwrap());
        assert!(cfg.enabled);
    }

    #[test]
    fn load_regex_classifier_config_explicitly_disabled() {
        let toml_content = r#"
[regex_classifier]
enabled = false

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_regex_disabled.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let cfg = load_regex_classifier_config(file_path.to_str().unwrap());
        assert!(!cfg.enabled);
    }

    #[test]
    fn load_regex_classifier_config_missing_file_returns_default() {
        let cfg = load_regex_classifier_config("/nonexistent/config.toml");
        assert!(cfg.enabled);
    }

    #[test]
    fn load_llm_classifier_config_valid() {
        let toml_content = r#"
[llm_classifier]
enabled = true
model = "gpt-4o-mini"
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "MY_API_KEY"
provider_type = "openai_compatible"
timeout_secs = 5

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_llm_config.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let result = load_llm_classifier_config(file_path.to_str().unwrap());
        assert!(result.is_some());
        let cfg = result.unwrap();
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.endpoint, "https://api.openai.com/v1/chat/completions");
        assert_eq!(cfg.api_key_env, "MY_API_KEY");
        assert_eq!(cfg.provider_type, "openai_compatible");
        assert_eq!(cfg.timeout_secs, 5);
    }

    #[test]
    fn load_llm_classifier_config_missing() {
        let toml_content = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_llm_missing.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let result = load_llm_classifier_config(file_path.to_str().unwrap());
        assert!(result.is_none());
    }

    #[test]
    fn load_llm_classifier_config_disabled() {
        let toml_content = r#"
[llm_classifier]
enabled = false
model = "gpt-4o-mini"

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_llm_disabled.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let result = load_llm_classifier_config(file_path.to_str().unwrap());
        assert!(result.is_none());
    }

    #[test]
    fn load_llm_classifier_config_defaults() {
        let toml_content = r#"
[llm_classifier]
enabled = true

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_llm_defaults.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        let result = load_llm_classifier_config(file_path.to_str().unwrap());
        assert!(result.is_some());
        let cfg = result.unwrap();
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.provider_type, "openai_compatible");
        assert_eq!(cfg.timeout_secs, 3);
    }

    #[test]
    fn load_classifiers_config_defaults_when_missing() {
        let toml_content = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: ConfigRoot = toml::from_str(toml_content).expect("valid TOML");
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(
            cfg.order,
            vec![
                "regex".to_string(),
                "fewshot".to_string(),
                "llm".to_string()
            ]
        );
    }

    #[test]
    fn load_classifiers_config_explicit_values() {
        let toml_content = r#"
[classifiers]
enabled = false
order = ["llm", "regex"]

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: ConfigRoot = toml::from_str(toml_content).expect("valid TOML");
        let cfg = load_classifiers_config_from_value(&root);
        assert!(!cfg.enabled);
        assert_eq!(cfg.order, vec!["llm".to_string(), "regex".to_string()]);
    }

    #[test]
    fn load_classifiers_config_custom_order_replaces_default() {
        let toml_content = r#"
[classifiers]
enabled = true
order = ["llm"]

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: ConfigRoot = toml::from_str(toml_content).expect("valid TOML");
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["llm".to_string()]);
    }

    #[test]
    fn load_classifiers_config_empty_root_returns_defaults() {
        let root = ConfigRoot::default();
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(
            cfg.order,
            vec![
                "regex".to_string(),
                "fewshot".to_string(),
                "llm".to_string()
            ]
        );
    }

    #[test]
    fn load_classifiers_config_non_table_root_returns_defaults() {
        // ConfigRoot::default() has no classifiers section → returns defaults
        let root = ConfigRoot::default();
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(
            cfg.order,
            vec![
                "regex".to_string(),
                "fewshot".to_string(),
                "llm".to_string()
            ]
        );
    }

    #[test]
    #[serial]
    fn parse_env_int_returns_default_when_unset() {
        std::env::remove_var("TEST_PARSE_INT");
        assert_eq!(parse_env_int("TEST_PARSE_INT", 42, None, None), 42);
    }

    #[test]
    #[serial]
    fn parse_env_int_uses_env_when_valid() {
        std::env::set_var("TEST_PARSE_INT", "100");
        let res = parse_env_int("TEST_PARSE_INT", 42, None, None);
        assert_eq!(res, 100);
        std::env::remove_var("TEST_PARSE_INT");
    }

    #[test]
    #[serial]
    fn parse_env_int_fallback_on_invalid() {
        std::env::set_var("TEST_PARSE_INT", "abc");
        let res = parse_env_int("TEST_PARSE_INT", 42, None, None);
        assert_eq!(res, 42);
        std::env::remove_var("TEST_PARSE_INT");
    }

    #[test]
    #[serial]
    fn parse_env_int_clamps_below_min() {
        std::env::set_var("TEST_PARSE_INT", "5");
        let res = parse_env_int("TEST_PARSE_INT", 42, Some(10), None);
        assert_eq!(res, 42);
        std::env::remove_var("TEST_PARSE_INT");
    }

    #[test]
    #[serial]
    fn parse_env_int_clamps_above_max() {
        std::env::set_var("TEST_PARSE_INT", "200");
        let res = parse_env_int("TEST_PARSE_INT", 42, None, Some(100));
        assert_eq!(res, 42);
        std::env::remove_var("TEST_PARSE_INT");
    }

    #[test]
    fn load_categories_table_format() {
        let toml_content = r#"
[categories.FILE_READING]
description = "Reading files"
threshold = 3
priority = 1

[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml_content).expect("valid TOML");
        let cats = load_categories_from_value(&root).expect("load should succeed");
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].name, "FILE_READING");
        assert_eq!(cats[1].name, "CASUAL");
    }

    #[test]
    fn detect_format_yaml_extensions() {
        assert_eq!(detect_format("config.yaml"), ConfigFormat::Yaml);
        assert_eq!(detect_format("config.yml"), ConfigFormat::Yaml);
        assert_eq!(detect_format("config.toml"), ConfigFormat::Toml);
        assert_eq!(detect_format("config.conf"), ConfigFormat::Toml);
        assert_eq!(detect_format("config"), ConfigFormat::Toml);
    }

    #[test]
    fn load_patterns_from_file_basic() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_basic.patterns");
        std::fs::write(&pattern_file, "3 | hello\n2 | world\n").unwrap();

        let patterns = load_patterns_from_file("test_basic.patterns", &tmp_dir).unwrap();
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].regex, "hello");
        assert_eq!(patterns[0].weight, 3);
        assert_eq!(patterns[1].regex, "world");
        assert_eq!(patterns[1].weight, 2);
    }

    #[test]
    fn load_patterns_from_file_with_comments() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_comments.patterns");
        std::fs::write(
            &pattern_file,
            "# This is a comment\n3 | hello\n\n# Another comment\n2 | world\n",
        )
        .unwrap();

        let patterns = load_patterns_from_file("test_comments.patterns", &tmp_dir).unwrap();
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].regex, "hello");
        assert_eq!(patterns[1].regex, "world");
    }

    #[test]
    fn load_patterns_from_file_invalid_weight() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_invalid_weight.patterns");
        std::fs::write(&pattern_file, "not_a_number | hello\n").unwrap();

        let result = load_patterns_from_file("test_invalid_weight.patterns", &tmp_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid weight"));
    }

    #[test]
    fn load_patterns_from_file_invalid_format() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_invalid_format.patterns");
        std::fs::write(&pattern_file, "no delimiter here\n").unwrap();

        let result = load_patterns_from_file("test_invalid_format.patterns", &tmp_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid format"));
    }

    #[test]
    fn load_patterns_from_file_missing_file() {
        let tmp_dir = std::env::temp_dir();
        let result = load_patterns_from_file("nonexistent.patterns", &tmp_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid pattern path"),
            "expected 'invalid pattern path', got: {}",
            err
        );
    }

    #[test]
    fn load_patterns_from_file_leading_trailing_spaces() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_spaces.patterns");
        std::fs::write(&pattern_file, "  3  |  hello world  \n").unwrap();

        let patterns = load_patterns_from_file("test_spaces.patterns", &tmp_dir).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].weight, 3);
        // regex is not trimmed after delimiter; leading spaces preserved
        assert_eq!(patterns[0].regex, " hello world");
    }

    #[test]
    fn load_patterns_from_file_compiles_regex() {
        let tmp_dir = std::env::temp_dir();
        let pattern_file = tmp_dir.join("test_compile.patterns");
        std::fs::write(&pattern_file, "3 | (?i)\\b(?:read|show)\\s+file\\b\n").unwrap();

        let patterns = load_patterns_from_file("test_compile.patterns", &tmp_dir).unwrap();
        assert_eq!(patterns.len(), 1);
        // Verify the regex compiles
        let re = regex::Regex::new(&patterns[0].regex);
        assert!(re.is_ok(), "regex from pattern file should compile");
    }

    #[test]
    fn cache_config_defaults() {
        let toml = r#"
[cache]
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        let cfg = load_cache_config_from_value(&root).expect("cache should be Some with defaults");
        assert_eq!(cfg.ttl_secs, 300);
        assert_eq!(cfg.max_entries, 1000);
    }

    #[test]
    fn cache_config_custom_values() {
        let toml = r#"
[cache]
ttl_secs = 120
max_entries = 500
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        let cfg = load_cache_config_from_value(&root).expect("cache should load custom values");
        assert_eq!(cfg.ttl_secs, 120);
        assert_eq!(cfg.max_entries, 500);
    }

    #[test]
    fn cache_config_disabled_when_absent() {
        let toml = r#"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        assert!(load_cache_config_from_value(&root).is_none());
    }

    #[test]
    fn cache_config_disabled_when_max_entries_zero() {
        let toml = r#"
[cache]
max_entries = 0
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
"#;
        let root: ConfigRoot = toml::from_str(toml).expect("valid TOML");
        assert!(load_cache_config_from_value(&root).is_none());
    }
}
