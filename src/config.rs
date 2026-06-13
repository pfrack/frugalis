use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use serde::Deserialize;

use crate::intent_classifier::{
    CategoryConfig, NegativePatternConfig,
};
use crate::routing::*;

#[cfg(test)]
pub(crate) const CONFIG_DEFAULT: &str = "config.toml";
#[cfg(test)]
pub(crate) const ROUTING_CONFIG_LEGACY: &str = "routing.toml";

// ── Serde default-value helpers ──

fn default_port() -> u16 { 10000 }
fn default_log_level() -> String { "info".to_string() }
fn default_log_format() -> String { "compact".to_string() }
fn default_max_body_bytes() -> usize { 10_485_760 }
fn default_keepalive_interval() -> u64 { 15 }
fn default_client_timeout() -> u64 { 120 }
fn default_client_connect_timeout() -> u64 { 30 }
fn default_streaming_chan_cap() -> usize { 32 }
fn default_connection_retries() -> u32 { 3 }
fn default_retry_base_ms() -> u64 { 1000 }
fn default_max_connections() -> u32 { 10 }
fn default_acquire_timeout() -> u64 { 30 }
fn default_idle_timeout() -> u64 { 1800 }
fn default_log_concurrency() -> u32 { 100 }
fn default_backend() -> String { "memory".to_string() }
fn default_db_path() -> String { "./cerebrum.db".to_string() }
fn default_dashboard_hours() -> u32 { 24 }
fn default_hours_min() -> u32 { 1 }
fn default_hours_max() -> u32 { 720 }
fn default_page_limit() -> u32 { 20 }
fn default_page_limit_max() -> u32 { 100 }
fn default_recent_count() -> u32 { 5 }
fn default_short_prompt_len() -> usize { 30 }
fn default_timeout_secs() -> u64 { 3 }
fn default_classifier_order() -> Vec<String> { vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()] }
fn default_llm_model() -> String { "gpt-4o-mini".to_string() }
fn default_llm_api_key_env() -> String { "OPENAI_API_KEY".to_string() }
fn default_provider_type() -> String { "openai_compatible".to_string() }
fn default_enabled_true() -> bool { true }
fn default_confidence_threshold() -> f64 { 0.4 }
fn default_cold_start_threshold() -> f64 { 0.6 }
fn default_cold_start_feedback_count() -> usize { 5 }
fn default_feature_dimensions() -> usize { 1000 }
fn default_retraining_threshold() -> usize { 5 }
fn default_fewshot_data_path() -> String { "data/fewshot_training.yaml".to_string() }
fn default_max_vocabulary_warn() -> usize { 5000 }
fn default_max_training_examples() -> usize { 10000 }

/// Load dashboard configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_dashboard_config_from_value(root: &ConfigRoot) -> DashboardConfig {
    root.dashboard.clone().unwrap_or_else(|| {
        debug!("[dashboard] section not found; using defaults");
        DashboardConfig::default()
    })
}

/// Load server configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_server_config_from_value(root: &ConfigRoot) -> ServerConfig {
    root.server.clone().unwrap_or_else(|| {
        debug!("[server] section not found; using defaults");
        ServerConfig::default()
    })
}

/// Load HTTP configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_http_config_from_value(root: &ConfigRoot) -> HttpConfig {
    root.http.clone().unwrap_or_else(|| {
        debug!("[http] section not found; using defaults");
        HttpConfig::default()
    })
}

/// Load database configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_database_config_from_value(root: &ConfigRoot) -> DatabaseConfig {
    root.database.clone().unwrap_or_else(|| {
        debug!("[database] section not found; using defaults");
        DatabaseConfig::default()
    })
}

/// Load auth providers from a parsed ConfigRoot.
/// Returns empty vec if section is absent.
pub(crate) fn load_auth_providers_from_value(root: &ConfigRoot) -> Vec<AuthProviderConfig> {
    root.auth_providers.clone().unwrap_or_else(|| {
        debug!("[auth_provider] section not found; no auth providers configured");
        vec![]
    })
}

/// Dashboard configuration for page defaults.
#[derive(Clone, Debug, Deserialize)]
pub struct DashboardConfig {
    #[serde(default = "default_dashboard_hours")]
    pub default_hours: u32,
    #[serde(default = "default_hours_min")]
    pub hours_min: u32,
    #[serde(default = "default_hours_max")]
    pub hours_max: u32,
    #[serde(default = "default_page_limit")]
    pub page_limit: u32,
    #[serde(default = "default_page_limit_max")]
    pub page_limit_max: u32,
    #[serde(default = "default_recent_count")]
    pub recent_count: u32,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            default_hours: 24,
            hours_min: 1,
            hours_max: 720,
            page_limit: 20,
            page_limit_max: 100,
            recent_count: 5,
        }
    }
}

/// CORS configuration loaded from [cors] section.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CorsConfig {
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

/// Load CORS configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_cors_config_from_value(root: &ConfigRoot) -> CorsConfig {
    root.cors.clone().unwrap_or_else(|| {
        debug!("[cors] section not found; using defaults (empty allowed_origins)");
        CorsConfig::default()
    })
}

/// Server configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 10000,
            log_level: "info".to_string(),
            log_format: "compact".to_string(),
        }
    }
}

/// HTTP configuration for client limits and timeouts.
#[derive(Clone, Debug, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_max_body_bytes")]
    pub max_upstream_body_bytes: usize,
    #[serde(default = "default_keepalive_interval")]
    pub keepalive_interval_secs: u64,
    #[serde(default = "default_max_body_bytes")]
    pub request_body_limit_bytes: usize,
    #[serde(default = "default_client_timeout")]
    pub client_timeout_secs: u64,
    #[serde(default = "default_client_connect_timeout")]
    pub client_connect_timeout_secs: u64,
    #[serde(default = "default_streaming_chan_cap")]
    pub streaming_channel_capacity: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            max_upstream_body_bytes: 10_485_760,
            keepalive_interval_secs: 15,
            request_body_limit_bytes: 10_485_760,
            client_timeout_secs: 120,
            client_connect_timeout_secs: 30,
            streaming_channel_capacity: 32,
        }
    }
}

/// Database configuration for pool and retry settings.
#[derive(Clone, Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_connection_retries")]
    pub connection_retries: u32,
    #[serde(default = "default_retry_base_ms")]
    pub retry_base_ms: u64,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_acquire_timeout")]
    pub acquire_timeout_secs: u64,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_log_concurrency")]
    pub log_concurrency_limit: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            connection_retries: 3,
            retry_base_ms: 1000,
            max_connections: 10,
            acquire_timeout_secs: 30,
            idle_timeout_secs: 1800,
            log_concurrency_limit: 100,
        }
    }
}

/// Persistence backend configuration loaded from [persistence] section.
#[derive(Clone, Debug, Deserialize)]
pub struct PersistenceSettings {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_db_path")]
    pub sqlite_path: String,
}

impl Default for PersistenceSettings {
    fn default() -> Self {
        Self {
            backend: "memory".to_string(),
            sqlite_path: "./cerebrum.db".to_string(),
        }
    }
}

/// Load persistence configuration from a parsed ConfigRoot.
/// Returns defaults if section is absent.
pub(crate) fn load_persistence_config_from_value(root: &ConfigRoot) -> PersistenceSettings {
    root.persistence.clone().unwrap_or_else(|| {
        debug!("[persistence] section not found; using defaults (memory backend)");
        PersistenceSettings::default()
    })
}

/// Authentication provider configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthProviderConfig {
    #[serde(rename = "type")]
    pub type_: String,
    pub header: Option<String>,
    pub value_template: Option<String>,
}



/// Parse an integer environment variable with optional min/max validation.
/// Returns `default` if the variable is unset, empty, invalid, or out of range.
/// Logs a warning on invalid or out-of-range values.
#[cfg(test)]
pub(crate) fn parse_env_int(
    var: &str,
    default: i32,
    min: Option<i32>,
    max: Option<i32>,
) -> i32 {
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
            warn!("Invalid integer value for {}: '{:?}'; using default {}", var, val_str, default);
            return default;
        }
    };
    if let Some(min) = min {
        if val < min {
            warn!("{} value {} below minimum {}; using default {}", var, val, min, default);
            return default;
        }
    }
    if let Some(max) = max {
        if val > max {
            warn!("{} value {} above maximum {}; using default {}", var, val, max, default);
            return default;
        }
    }
    val
}

pub(crate) fn hardcoded_routing(
    categories: &[CategoryConfig],
) -> (HashMap<String, RouteEntry>, RouteEntry) {
    let endpoint = "https://integrate.api.nvidia.com/v1/chat/completions";
    let mut routing = HashMap::new();

    for cat in categories {
        routing.insert(
            cat.name.clone(),
            RouteEntry {
                model: DEFAULT_MODEL.to_string(),
                endpoint: endpoint.to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "nvidia_nim".to_string(),
                api_key_env: Some("NVIDIA_API_KEY".to_string()),
            },
        );
    }

    let fallback = RouteEntry {
        model: DEFAULT_MODEL.to_string(),
        endpoint: endpoint.to_string(),
        cost_per_1m_input_tokens: None,
        provider_type: "nvidia_nim".to_string(),
        api_key_env: Some("NVIDIA_API_KEY".to_string()),
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
                model,
                endpoint,
                cost_per_1m_input_tokens,
                provider_type,
                api_key_env,
            },
        );
    }
    Ok(routing)
}

#[cfg(test)]
pub(crate) fn load_routing() -> (HashMap<String, RouteEntry>, RouteEntry) {
    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| CONFIG_DEFAULT.to_string());

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
        model: DEFAULT_MODEL.to_string(),
        endpoint: String::new(),
        cost_per_1m_input_tokens: None,
        provider_type: String::new(),
        api_key_env: None,
    });
    (routing, fallback_entry)
}

/// Build routing map and fallback entry from a parsed ConfigRoot.
/// Reads from the `[routing]` section. Returns (routing map, fallback entry).
pub(crate) fn routing_from_value(
    config_root: &ConfigRoot,
) -> Result<(HashMap<String, RouteEntry>, RouteEntry), String> {
    let routing_table = match &config_root.routing {
        Some(t) => t,
        None => {
            debug!("[routing] section not found; no routing entries configured");
            let fallback = RouteEntry {
                model: DEFAULT_MODEL.to_string(),
                endpoint: String::new(),
                cost_per_1m_input_tokens: None,
                provider_type: String::new(),
                api_key_env: None,
            };
            return Ok((HashMap::new(), fallback));
        }
    };

    let default_model = routing_table
        .get("DEFAULT")
        .map(|e| e.model.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();

    let mut routing = HashMap::new();
    for (key, entry) in routing_table {
        let model = if !entry.model.is_empty() {
            entry.model.clone()
        } else {
            warn!(category = %key, "routing section missing 'model' for category; using DEFAULT model");
            default_model.clone()
        };
        let endpoint = entry.endpoint.clone();
        let cost_per_1m_input_tokens = entry.cost_per_1m_input_tokens;
        let provider_type = if !entry.provider_type.is_empty() {
            entry.provider_type.clone()
        } else {
            warn!(category = %key, "routing section missing 'provider_type' for category; defaulting to empty");
            String::new()
        };
        let api_key_env = if entry.api_key_env.is_some() {
            entry.api_key_env.clone()
        } else {
            warn!(category = %key, "routing section missing 'api_key_env' for category; no API key will be resolved");
            None
        };
        routing.insert(
            key.to_uppercase(),
            RouteEntry {
                model,
                endpoint,
                cost_per_1m_input_tokens,
                provider_type,
                api_key_env,
            },
        );
    }
    let fallback = routing.remove("DEFAULT").unwrap_or_else(|| RouteEntry {
        model: default_model,
        endpoint: String::new(),
        cost_per_1m_input_tokens: None,
        provider_type: String::new(),
        api_key_env: None,
    });
    Ok((routing, fallback))
}

/// Load categories from a parsed ConfigRoot.
/// Reads categories from the `[categories]` table where each key is a category name.
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

/// Load negative suppression patterns from a parsed ConfigRoot.
/// Returns empty vec if section is absent.
pub(crate) fn load_negative_patterns_from_value(root: &ConfigRoot) -> Vec<NegativePatternConfig> {
    root.negative_patterns.clone().unwrap_or_else(|| {
        debug!("[negative_patterns] section not found; no negative patterns configured");
        vec![]
    })
}

pub(crate) fn build_model_costs(config_root: &ConfigRoot, routing: &HashMap<String, RouteEntry>) -> ModelCosts {
    let mut costs = HashMap::new();

    if let Some(model_costs_table) = &config_root.model_costs {
        for (model_name, cost) in model_costs_table {
            costs.insert(model_name.clone(), *cost);
        }
    }

    for entry in routing.values() {
        if let Some(override_cost) = entry.cost_per_1m_input_tokens {
            costs.insert(entry.model.clone(), override_cost);
        }
    }
    ModelCosts::from_costs(costs)
}

// ── Config Format Detection & Loading ──

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ConfigFormat {
    Toml,
    Yaml,
}

fn detect_format(path: &str) -> ConfigFormat {
    match Path::new(path).extension().and_then(|s| s.to_str()) {
        Some("yaml" | "yml") => ConfigFormat::Yaml,
        _ => ConfigFormat::Toml,
    }
}

/// Load a config file (TOML or YAML) and deserialize into ConfigRoot.
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

/// Load patterns from an external pattern file.
/// Lines starting with `#` are comments; empty lines are skipped.
/// Each non-comment line must match: `<weight> | <regex>`
pub(crate) fn load_patterns_from_file(
    path: &str,
    base_dir: &Path,
) -> Result<Vec<crate::intent_classifier::PatternEntry>, String> {
    let full_path = base_dir.join(path).canonicalize()
        .map_err(|e| format!("invalid pattern path {}: {}", path, e))?;
    let base_dir = base_dir.canonicalize()
        .map_err(|e| format!("invalid patterns_dir: {}", e))?;
    if !full_path.starts_with(&base_dir) {
        return Err(format!("pattern file path '{}' escapes patterns_dir", path));
    }
    let content =
        std::fs::read_to_string(&full_path).map_err(|e| format!("cannot read pattern file {}: {}", full_path.display(), e))?;

    let mut entries = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (weight_str, regex) = trimmed
            .split_once(" | ")
            .ok_or_else(|| format!("{}:{}: invalid format, expected '<weight> | <regex>'", path, line_num + 1))?;
        let weight = weight_str
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("{}:{}: invalid weight: {}", path, line_num + 1, e))?;
        entries.push(crate::intent_classifier::PatternEntry {
            regex: regex.to_string(),
            weight,
        });
    }
    Ok(entries)
}

/// Validate config schema and compile all regex patterns.
/// Returns Ok(()) if everything is valid, or Err with a list of error messages.
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
            let default_content = include_str!("../config.toml");
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
            _ => errors.push(format!("server.log_level: unknown level '{}'", server.log_level)),
        }
        match server.log_format.as_str() {
            "compact" | "full" | "json" | "pretty" => {}
            _ => errors.push(format!("server.log_format: unknown format '{}'", server.log_format)),
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
                    errors.push(format!("routing.{}: references unknown category '{}'", route_key, route_key));
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
        errors.push(format!("patterns_dir '{}': exists but is not a directory", patterns_dir.display()));
    }

    // ── Pattern file resolution & regex validation ──
    if let Some(ref cats) = config_root.categories {
        for (name, cat) in cats {
            // Resolve patterns from external file or inline
            let patterns: Vec<crate::intent_classifier::PatternEntry> =
                if let Some(ref pf) = cat.patterns_file {
                    match load_patterns_from_file(pf, &patterns_dir) {
                        Ok(entries) => {
                            // Validate each compiled regex with file:line context
                            let mut has_error = false;
                            for (idx, entry) in entries.iter().enumerate() {
                                if let Err(e) = regex::Regex::new(&entry.regex) {
                                    errors.push(format!("{}:{}: pattern {}: {}", pf, idx + 1, entry.regex, e));
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
                    errors.push(format!("categories.{}.patterns[{}]: {}: {}", name, idx, entry.regex, e));
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

/// Merge overlay ConfigRoot into base, respecting override-key semantics.
/// Override keys (classifiers, regex_classifier, llm_classifier, categories,
/// auth_provider, model_costs, routing, negative_patterns) are completely replaced.
/// Non-override struct fields are merged field-by-field (overlay values win).
/// Non-override scalars (baseline_model, classify_db_log) are replaced by overlay.
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
    if let Some(v) = overlay.routing {
        base.routing = Some(v);
    }
    if let Some(v) = overlay.patterns_dir {
        base.patterns_dir = Some(v);
    }
    if let Some(v) = overlay.negative_patterns {
        base.negative_patterns = Some(v);
    }
}

/// Top-level configuration root, mirroring all sections in config.toml/config.yaml.
/// Every field is `Option` so missing sections deserialize as `None`.
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
}

/// Configuration for global classifier settings.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ClassifiersConfig {
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default = "default_classifier_order")]
    pub order: Vec<String>,
}

impl Default for ClassifiersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            order: vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()],
        }
    }
}

/// Load classifiers config from a parsed ConfigRoot.
/// Returns default if section is absent.
pub(crate) fn load_classifiers_config_from_value(root: &ConfigRoot) -> ClassifiersConfig {
    root.classifiers.clone().unwrap_or_else(|| {
        tracing::debug!("No [classifiers] section in config; using defaults");
        ClassifiersConfig::default()
    })
}

/// Configuration for the regex classifier backend.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RegexClassifierConfig {
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default = "default_short_prompt_len")]
    pub short_prompt_len: usize,
}

impl Default for RegexClassifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            short_prompt_len: 30,
        }
    }
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

pub(crate) fn load_regex_classifier_config_from_value(root: &ConfigRoot) -> RegexClassifierConfig {
    root.regex_classifier.clone().unwrap_or_else(|| {
        debug!("[regex_classifier] section not found; using defaults (enabled)");
        RegexClassifierConfig::default()
    })
}

/// Configuration for the LLM classifier backend.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LlmClassifierConfig {
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default = "default_llm_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_provider_type")]
    pub provider_type: String,
    #[serde(default)]
    pub prompt_template_path: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
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

/// Configuration for the few-shot classifier backend.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct FewShotConfig {
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    #[serde(default = "default_cold_start_threshold")]
    pub cold_start_threshold: f64,
    #[serde(default = "default_cold_start_feedback_count")]
    pub cold_start_feedback_count: usize,
    #[serde(default = "default_feature_dimensions")]
    pub feature_dimensions: usize,
    #[serde(default = "default_retraining_threshold")]
    pub retraining_threshold: usize,
    #[serde(default = "default_fewshot_data_path")]
    pub data_path: String,
    #[serde(default = "default_max_vocabulary_warn")]
    pub max_vocabulary_warn: usize,
    #[serde(default = "default_max_training_examples")]
    pub max_training_examples: usize,
}

/// Load few-shot classifier config from a parsed ConfigRoot.
/// Returns None if section is absent or enabled = false.
pub(crate) fn load_fewshot_config_from_value(root: &ConfigRoot) -> Option<FewShotConfig> {
    let cfg = root.fewshot_classifier.as_ref()?;
    if !cfg.enabled {
        return None;
    }
    Some(cfg.clone())
}

/// Load LLM classifier config from a parsed ConfigRoot.
/// Returns None if section is absent or enabled = false.
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
    use crate::routing::RouteEntry;
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
        assert_eq!(routing.get("SYNTAX_FIX").unwrap().model, "test-sf-model");
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().endpoint,
            "https://test.endpoint"
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().provider_type,
            "openai_compatible"
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().api_key_env,
            Some("TEST_API_KEY".to_string())
        );
        assert_eq!(
            routing.get("SYNTAX_FIX").unwrap().cost_per_1m_input_tokens,
            Some(1.23)
        );

        assert_eq!(
            routing.get("COMPLEX_REASONING").unwrap().model,
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
            assert_eq!(entry.model, DEFAULT_MODEL);
            assert!(entry.endpoint.contains("integrate.api.nvidia.com"));
            assert_eq!(entry.provider_type, "nvidia_nim");
            assert_eq!(entry.api_key_env, Some("NVIDIA_API_KEY".to_string()));
            assert_eq!(entry.cost_per_1m_input_tokens, None);
        }

        assert_eq!(fallback.model, DEFAULT_MODEL);
        assert!(fallback.endpoint.contains("integrate.api.nvidia.com"));
        assert_eq!(fallback.provider_type, "nvidia_nim");
        assert_eq!(fallback.api_key_env, Some("NVIDIA_API_KEY".to_string()));
    }

    #[test]
    fn hardcoded_routing_uses_hardcoded_endpoint() {
        let (_, fallback) = hardcoded_routing(&test_categories());
        assert_eq!(
            fallback.endpoint,
            "https://integrate.api.nvidia.com/v1/chat/completions"
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
        assert_eq!(routing.get("SYNTAX_FIX").unwrap().model, "file-sf-model");
        // fallback should be the file-defined fallback
        assert_eq!(fallback.model, "file-fallback");

        std::env::remove_var("CONFIG_PATH");

        // 2. When file is missing, fall back to hardcoded defaults (empty categories)
        std::env::set_var("CONFIG_PATH", "/nonexistent/config.toml");
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 0);
        assert_eq!(fallback.model, DEFAULT_MODEL);

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
        assert_eq!(fallback.model, DEFAULT_MODEL);

        std::env::remove_var("CONFIG_PATH");
    }

    #[test]
    fn build_model_costs_applies_route_overrides() {
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            RouteEntry {
                model: "claude-3.5-sonnet".to_string(),
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: Some(5.00),
                provider_type: "".to_string(),
                api_key_env: None,
            },
        );
        routing.insert(
            "COMPLEX_REASONING".to_string(),
            RouteEntry {
                model: "gpt-4o".to_string(),
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: None,
                provider_type: "".to_string(),
                api_key_env: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            RouteEntry {
                model: "unknown-model".to_string(),
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: Some(2.50),
                provider_type: "".to_string(),
                api_key_env: None,
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
        assert_eq!(cfg.order, vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()]);
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
        assert_eq!(cfg.order, vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()]);
    }

    #[test]
    fn load_classifiers_config_non_table_root_returns_defaults() {
        // ConfigRoot::default() has no classifiers section → returns defaults
        let root = ConfigRoot::default();
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["regex".to_string(), "fewshot".to_string(), "llm".to_string()]);
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
            toml_root.categories.as_ref().and_then(|c| c.get("CASUAL")).map(|c| c.threshold),
            yaml_root.categories.as_ref().and_then(|c| c.get("CASUAL")).map(|c| c.threshold)
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
        let providers = root.auth_providers.expect("auth_providers should be present");
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
        assert!(result.is_ok(), "TOML load should succeed: {:?}", result.err());
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
        assert!(result.is_ok(), "YAML load should succeed: {:?}", result.err());
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
    fn detect_format_yaml_extensions() {
        assert_eq!(detect_format("config.yaml"), ConfigFormat::Yaml);
        assert_eq!(detect_format("config.yml"), ConfigFormat::Yaml);
        assert_eq!(detect_format("config.toml"), ConfigFormat::Toml);
        assert_eq!(detect_format("config.conf"), ConfigFormat::Toml);
        assert_eq!(detect_format("config"), ConfigFormat::Toml);
    }

    #[test]
    fn merge_configs_overrides_categories() {
        let mut base: ConfigRoot = toml::from_str(r#"
[categories.CASUAL]
description = "Original"
threshold = 1
priority = 4
"#).expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(r#"
[categories.FILE_READING]
description = "Override"
threshold = 3
priority = 1
"#).expect("valid TOML");

        merge_configs(&mut base, overlay);
        // Categories is an override key → complete replacement
        let cats = base.categories.unwrap();
        assert!(!cats.contains_key("CASUAL"));
        assert_eq!(cats.get("FILE_READING").unwrap().description, "Override");
    }

    #[test]
    fn merge_configs_shallow_merge_server() {
        let mut base: ConfigRoot = toml::from_str(r#"
[server]
port = 10000
log_level = "info"
log_format = "compact"
"#).expect("valid TOML");

        let overlay: ConfigRoot = toml::from_str(r#"
[server]
port = 20000
"#).expect("valid TOML");

        merge_configs(&mut base, overlay);
        let server = base.server.unwrap();
        // port overridden, log_level and log_format preserved
        assert_eq!(server.port, 20000);
        assert_eq!(server.log_level, "info");
        assert_eq!(server.log_format, "compact");
    }

    // ── Phase 3: External Pattern File Tests ──

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
        assert!(err.contains("invalid pattern path"), "expected 'invalid pattern path', got: {}", err);
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
        std::fs::write(
            &pattern_file,
            "3 | (?i)\\b(?:read|show)\\s+file\\b\n",
        )
        .unwrap();

        let patterns = load_patterns_from_file("test_compile.patterns", &tmp_dir).unwrap();
        assert_eq!(patterns.len(), 1);
        // Verify the regex compiles
        let re = regex::Regex::new(&patterns[0].regex);
        assert!(re.is_ok(), "regex from pattern file should compile");
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
        assert_eq!(root.patterns_dir.as_ref().map(|p: &PathBuf| p.to_str()), Some(Some("./custom_patterns")));
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

    // ── Phase 4: Validation Tests ──

    #[test]
    fn validate_success_on_embedded_config() {
        // Validates the embedded config.toml (should always be valid)
        let result = run_validation(None);
        assert!(result.is_ok(), "embedded config should be valid: {:?}", result.err());
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
        assert!(all.contains("patterns[0]"), "should include pattern index: {all}");
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
        assert!(all.contains("log_level"), "should report log_level error: {all}");
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
        assert!(all.contains("categories"), "should report missing categories: {all}");
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
        assert!(all.contains("threshold"), "should report threshold error: {all}");
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
        assert!(errors.len() >= 2, "should collect multiple errors, got {}: {:?}", errors.len(), errors);
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
        assert!(all.contains("nonexistent.patterns") || all.contains("cannot read"), "should report missing pattern file: {all}");
    }
}
