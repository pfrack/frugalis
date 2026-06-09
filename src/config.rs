use std::collections::HashMap;
use tracing::warn;

#[allow(unused_imports)]
use crate::intent_classifier::{hardcoded_categories, CategoryConfig};
use crate::routing::*;

#[cfg(test)]
pub(crate) const CONFIG_DEFAULT: &str = "config.toml";
#[cfg(test)]
pub(crate) const ROUTING_CONFIG_LEGACY: &str = "routing.toml";
pub(crate) const NVIDIA_ENDPOINT_DEFAULT: &str =
    "https://integrate.api.nvidia.com/v1/chat/completions";

pub(crate) fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// HTTP client configuration with limits and timeouts.
#[derive(Clone, Debug)]
pub struct HttpClientConfig {
    pub max_upstream_body_bytes: i32,
    pub keepalive_interval_secs: i32,
}

impl HttpClientConfig {
    /// Load configuration from environment variables with sensible defaults.
    pub fn from_env() -> Self {
        Self {
            max_upstream_body_bytes: parse_env_int(
                "MAX_UPSTREAM_BODY_BYTES",
                10_485_760,
                Some(1_048_576),
                Some(100_485_760),
            ),
            keepalive_interval_secs: parse_env_int(
                "KEEPALIVE_INTERVAL_SECS",
                15,
                Some(1),
                None,
            ),
        }
    }
}

/// Parse an integer environment variable with optional min/max validation.
/// Returns `default` if the variable is unset, empty, invalid, or out of range.
/// Logs a warning on invalid or out-of-range values.
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

fn hardcoded_model_default(env_var: &str) -> &'static str {
    match env_var {
        "DEFAULT_MODEL" => DEFAULT_MODEL,
        "DEFAULT_MODEL_COMPLEX" => DEFAULT_MODEL_COMPLEX,
        "DEFAULT_MODEL_READING" => DEFAULT_MODEL_READING,
        _ => DEFAULT_MODEL,
    }
}

pub(crate) fn hardcoded_routing(
    categories: &[CategoryConfig],
) -> (HashMap<String, RouteEntry>, RouteEntry) {
    let endpoint = env_or_default("NVIDIA_ENDPOINT", NVIDIA_ENDPOINT_DEFAULT);
    let mut routing = HashMap::new();

    for cat in categories {
        let model = match &cat.model_env_var {
            Some(env_var) => env_or_default(env_var, hardcoded_model_default(env_var)),
            None => DEFAULT_MODEL.to_string(),
        };
        routing.insert(
            cat.name.clone(),
            RouteEntry {
                model,
                endpoint: endpoint.clone(),
                cost_per_1m_input_tokens: None,
                provider_type: "nvidia_nim".to_string(),
                api_key_env: Some("NVIDIA_API_KEY".to_string()),
            },
        );
    }

    let fallback = RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint,
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
    let mut routing = HashMap::new();
    for (key, value) in table {
        if key == "fallback" || key == "categories" {
            continue;
        }
        let model = if let Some(m) = value.get("model").and_then(|v| v.as_str()) {
            m.to_string()
        } else {
            warn!(category = %key, "routing.toml missing 'model' for category; using DEFAULT_MODEL");
            env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)
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
        .or_else(|_| std::env::var("ROUTING_CONFIG_PATH"))
        .unwrap_or_else(|_| CONFIG_DEFAULT.to_string());

    // Try config.toml first, then routing.toml for backward compat
    let path = if std::path::Path::new(&config_path).exists() {
        config_path
    } else if std::path::Path::new(ROUTING_CONFIG_LEGACY).exists() {
        tracing::info!("Using legacy routing.toml; consider renaming to config.toml");
        ROUTING_CONFIG_LEGACY.to_string()
    } else {
        tracing::warn!("No config.toml or routing.toml found; using hardcoded routing defaults");
        return hardcoded_routing(&hardcoded_categories());
    };

    let mut routing = match load_routing_from_file(&path) {
        Ok(r) => {
            tracing::info!("Routing: loaded from {path}");
            r
        }
        Err(e) => {
            tracing::warn!("{e}; using hardcoded routing defaults");
            return hardcoded_routing(&hardcoded_categories());
        }
    };
    let fallback_entry = routing.remove("FALLBACK").unwrap_or_else(|| RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint: String::new(),
        cost_per_1m_input_tokens: None,
        provider_type: String::new(),
        api_key_env: None,
    });
    (routing, fallback_entry)
}

/// Build routing map and fallback entry from a parsed TOML value.
/// Returns (routing map, fallback entry). If the root is not a table, returns error.
pub(crate) fn routing_from_value(
    root: &toml::Value,
) -> Result<(HashMap<String, RouteEntry>, RouteEntry), String> {
    let table = root
        .as_table()
        .ok_or_else(|| "Root must be a table".to_string())?;
    let mut routing = HashMap::new();
    for (key, value) in table {
        if key == "fallback" || key == "categories" {
            continue;
        }
        let model = if let Some(m) = value.get("model").and_then(|v| v.as_str()) {
            m.to_string()
        } else {
            warn!(category = %key, "routing section missing 'model' for category; using DEFAULT_MODEL");
            env_or_default("DEFAULT_MODEL", DEFAULT_MODEL)
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
            warn!(category = %key, "routing section missing 'provider_type' for category; defaulting to empty");
            String::new()
        };
        let api_key_env = if let Some(ake) = value.get("api_key_env").and_then(|v| v.as_str()) {
            Some(ake.to_string())
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
    let fallback = routing.remove("FALLBACK").unwrap_or_else(|| RouteEntry {
        model: env_or_default("DEFAULT_MODEL", DEFAULT_MODEL),
        endpoint: String::new(),
        cost_per_1m_input_tokens: None,
        provider_type: String::new(),
        api_key_env: None,
    });
    Ok((routing, fallback))
}

/// Load categories from a parsed toml::Value.
pub(crate) fn load_categories_from_value(
    root: &toml::Value,
) -> Result<Vec<CategoryConfig>, String> {
    let table = root
        .as_table()
        .ok_or_else(|| "Root must be a table".to_string())?;

    let cats_array = match table.get("categories") {
        Some(toml::Value::Array(arr)) => arr,
        _ => return Err("No [[categories]] section found".to_string()),
    };

    let mut categories = Vec::new();
    for (i, cat) in cats_array.iter().enumerate() {
        let t = cat
            .as_table()
            .ok_or_else(|| format!("categories[{i}] must be a table"))?;
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("categories[{i}]: missing 'name'"))?
            .to_string();
        let description = t
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let threshold = t.get("threshold").and_then(|v| v.as_integer()).unwrap_or(1) as u32;
        let priority = t.get("priority").and_then(|v| v.as_integer()).unwrap_or(99) as u8;
        let model_env_var = t
            .get("model_env_var")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        categories.push(CategoryConfig {
            name,
            description,
            threshold,
            priority,
            model_env_var,
        });
    }

    if categories.is_empty() {
        return Err("[[categories]] is empty".to_string());
    }
    Ok(categories)
}

pub(crate) fn build_model_costs(routing: &HashMap<String, RouteEntry>) -> ModelCosts {
    let mut costs = crate::intent_classifier::hardcoded_model_costs();
    for entry in routing.values() {
        if let Some(override_cost) = entry.cost_per_1m_input_tokens {
            costs.insert(entry.model.clone(), override_cost);
        }
    }
    ModelCosts::from_costs(costs)
}

/// Configuration for global classifier settings.
#[derive(Clone, Debug)]
pub(crate) struct ClassifiersConfig {
    pub enabled: bool,
    pub order: Vec<String>,
}

impl Default for ClassifiersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            order: vec!["regex".to_string(), "llm".to_string()],
        }
    }
}

/// Load classifiers config from a parsed toml::Value.
/// Returns default if section is absent.
pub(crate) fn load_classifiers_config_from_value(root: &toml::Value) -> ClassifiersConfig {
    let table = match root.as_table() {
        Some(t) => t,
        None => {
            tracing::debug!("Config root is not a table for classifiers config; using defaults");
            return ClassifiersConfig::default();
        }
    };
    let classifiers_section = match table.get("classifiers").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => {
            tracing::debug!("No [classifiers] section in config; using defaults");
            return ClassifiersConfig::default();
        }
    };
    let enabled = classifiers_section
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let order = classifiers_section
        .get("order")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec!["regex".to_string(), "llm".to_string()]);

    ClassifiersConfig { enabled, order }
}

/// Configuration for the regex classifier backend.
#[derive(Clone, Debug)]
pub(crate) struct RegexClassifierConfig {
    pub enabled: bool,
}

impl Default for RegexClassifierConfig {
    fn default() -> Self {
        Self { enabled: true }
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
    let root: toml::Value = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Invalid TOML for regex classifier section: {e}");
            return RegexClassifierConfig::default();
        }
    };
    load_regex_classifier_config_from_value(&root)
}

pub(crate) fn load_regex_classifier_config_from_value(root: &toml::Value) -> RegexClassifierConfig {
    let table = match root.as_table() {
        Some(t) => t,
        None => {
            tracing::warn!("Config file root is not a table for regex classifier");
            return RegexClassifierConfig::default();
        }
    };
    let regex_section = match table.get("regex_classifier") {
        Some(toml::Value::Table(t)) => t,
        _ => return RegexClassifierConfig::default(),
    };
    let enabled = regex_section
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    RegexClassifierConfig { enabled }
}

/// Configuration for the LLM classifier backend.
#[derive(Clone, Debug)]
pub(crate) struct LlmClassifierConfig {
    pub model: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub provider_type: String,
    pub prompt_template_path: Option<String>,
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
    let root: toml::Value = match toml::from_str(&content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Invalid TOML for LLM classifier section: {e}");
            return None;
        }
    };
    load_llm_classifier_config_from_value(&root)
}

/// Load LLM classifier config from a parsed toml::Value.
/// Returns None if section is absent or enabled = false.
pub(crate) fn load_llm_classifier_config_from_value(
    root: &toml::Value,
) -> Option<LlmClassifierConfig> {
    let table = root.as_table()?;
    let llm_section = table.get("llm_classifier")?.as_table()?;

    if !llm_section
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }

    let model = llm_section
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-4o-mini")
        .to_string();

    let endpoint = llm_section
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let api_key_env = llm_section
        .get("api_key_env")
        .and_then(|v| v.as_str())
        .unwrap_or("OPENAI_API_KEY")
        .to_string();

    let provider_type = llm_section
        .get("provider_type")
        .and_then(|v| v.as_str())
        .unwrap_or("openai_compatible")
        .to_string();

    let prompt_template_path = llm_section
        .get("prompt_template_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let timeout_secs = (llm_section
        .get("timeout_secs")
        .and_then(|v| v.as_integer())
        .unwrap_or(3) as u64)
        .max(1);

    Some(LlmClassifierConfig {
        model,
        endpoint,
        api_key_env,
        provider_type,
        prompt_template_path,
        timeout_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_classifier::hardcoded_categories;
    use crate::routing::RouteEntry;
    use serial_test::serial;
    use std::collections::HashMap;

    #[test]
    #[serial]
    fn env_or_default_returns_env_var_when_set() {
        std::env::set_var("TEST_CONFIG_VAR", "override");
        assert_eq!(env_or_default("TEST_CONFIG_VAR", "default"), "override");
        std::env::remove_var("TEST_CONFIG_VAR");
    }

    #[test]
    #[serial]
    fn env_or_default_returns_default_when_unset() {
        std::env::remove_var("UNSET_CONFIG_VAR");
        assert_eq!(env_or_default("UNSET_CONFIG_VAR", "default"), "default");
    }

    #[test]
    fn load_routing_from_file_success() {
        // Create temporary TOML content
        let toml_content = r#"
[SYNTAX_FIX]
model = "test-sf-model"
endpoint = "https://test.endpoint"
provider_type = "openai_compatible"
api_key_env = "TEST_API_KEY"
cost_per_1m_input_tokens = 1.23

[COMPLEX_REASONING]
model = "test-cr-model"
endpoint = "https://test.cr"
provider_type = "openai_compatible"
api_key_env = "TEST_API_KEY_CR"

[fallback]
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

        assert_eq!(routing.len(), 2);
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
        let cats = hardcoded_categories();
        let (routing, fallback) = hardcoded_routing(&cats);

        assert_eq!(routing.len(), cats.len());
        for cat in &cats {
            assert!(
                routing.contains_key(cat.name.as_str()),
                "routing missing key for {}",
                cat.name
            );
            let entry = routing.get(cat.name.as_str()).unwrap();
            let expected_model = match cat.model_env_var.as_deref() {
                Some("DEFAULT_MODEL") => DEFAULT_MODEL,
                Some("DEFAULT_MODEL_COMPLEX") => DEFAULT_MODEL_COMPLEX,
                Some("DEFAULT_MODEL_READING") => DEFAULT_MODEL_READING,
                _ => DEFAULT_MODEL,
            };
            assert_eq!(entry.model, expected_model);
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
    #[serial]
    fn hardcoded_routing_respects_nvidia_endpoint_env() {
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var("NVIDIA_ENDPOINT");
            }
        }

        let _guard = EnvGuard;
        std::env::set_var(
            "NVIDIA_ENDPOINT",
            "https://custom.endpoint.example.com/v1/chat/completions",
        );
        let (_, fallback) = hardcoded_routing(&hardcoded_categories());
        assert_eq!(
            fallback.endpoint,
            "https://custom.endpoint.example.com/v1/chat/completions"
        );
    }

    #[test]
    #[serial]
    fn load_routing_behavior() {
        // 1. When ROUTING_CONFIG_PATH points to a valid file, load_routing returns parsed routing and fallback
        let toml_content = r#"
[SYNTAX_FIX]
model = "file-sf-model"
endpoint = "https://file.endpoint"
provider_type = "openai_compatible"
api_key_env = "FILE_API_KEY"

[FALLBACK]
model = "file-fallback"
endpoint = ""
provider_type = ""
api_key_env = ""
"#;
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_valid_routing.toml");
        std::fs::write(&file_path, toml_content).expect("write temp file");

        std::env::set_var("ROUTING_CONFIG_PATH", file_path.to_str().unwrap());
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 1);
        assert_eq!(routing.get("SYNTAX_FIX").unwrap().model, "file-sf-model");
        // fallback should be the file-defined fallback
        assert_eq!(fallback.model, "file-fallback");

        std::env::remove_var("ROUTING_CONFIG_PATH");

        // 2. When file is missing, fall back to hardcoded defaults
        std::env::set_var("ROUTING_CONFIG_PATH", "/nonexistent/routing.toml");
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 4);
        assert!(routing.contains_key("SYNTAX_FIX"));
        assert_eq!(fallback.model, DEFAULT_MODEL);

        std::env::remove_var("ROUTING_CONFIG_PATH");

        // 3. When file exists but TOML is invalid, fall back to hardcoded defaults
        use std::io::Write;
        let file_path_invalid = temp_dir.join("invalid_routing.toml");
        let mut file = std::fs::File::create(&file_path_invalid).expect("create temp file");
        file.write_all(b"not valid toml {{").expect("write");
        drop(file);

        std::env::set_var("ROUTING_CONFIG_PATH", file_path_invalid.to_str().unwrap());
        let (routing, fallback) = load_routing();

        assert_eq!(routing.len(), 4);
        assert!(routing.contains_key("SYNTAX_FIX"));
        assert_eq!(fallback.model, DEFAULT_MODEL);

        std::env::remove_var("ROUTING_CONFIG_PATH");
    }

    #[test]
    fn build_model_costs_seeds_with_hardcoded_and_applies_overrides() {
        let mut routing = HashMap::new();
        routing.insert(
            "SYNTAX_FIX".to_string(),
            RouteEntry {
                model: "claude-3.5-sonnet".to_string(),
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: Some(5.00), // Override
                provider_type: "".to_string(),
                api_key_env: None,
            },
        );
        routing.insert(
            "COMPLEX_REASONING".to_string(),
            RouteEntry {
                model: "gpt-4o".to_string(),
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: None, // Use hardcoded
                provider_type: "".to_string(),
                api_key_env: None,
            },
        );
        routing.insert(
            "CASUAL".to_string(),
            RouteEntry {
                model: "unknown-model".to_string(), // Not in hardcoded, should be absent
                endpoint: "".to_string(),
                cost_per_1m_input_tokens: Some(2.50),
                provider_type: "".to_string(),
                api_key_env: None,
            },
        );

        let costs = build_model_costs(&routing);

        // Hardcoded defaults
        assert_eq!(costs.get("claude-3.5-sonnet"), Some(5.00)); // Overridden
        assert_eq!(costs.get("gpt-4o"), Some(2.50)); // Hardcoded
        assert_eq!(costs.get("gpt-4o-mini"), Some(0.15));
        assert_eq!(costs.get("deepseek-chat"), Some(0.14));
        // Unknown model with override
        assert_eq!(costs.get("unknown-model"), Some(2.50));
    }

    #[test]
    fn load_regex_classifier_config_default_enabled() {
        // Section absent → default enabled
        let toml_content = r#"
[[categories]]
name = "CASUAL"
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

[[categories]]
name = "CASUAL"
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

[[categories]]
name = "CASUAL"
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
[[categories]]
name = "CASUAL"
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

[[categories]]
name = "CASUAL"
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

[[categories]]
name = "CASUAL"
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
        // Section absent → default values
        let toml_content = r#"
[[categories]]
name = "CASUAL"
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: toml::Value = toml::from_str(toml_content).expect("valid TOML");
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["regex".to_string(), "llm".to_string()]);
    }

    #[test]
    fn load_classifiers_config_explicit_values() {
        let toml_content = r#"
[classifiers]
enabled = false
order = ["llm", "regex"]

[[categories]]
name = "CASUAL"
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: toml::Value = toml::from_str(toml_content).expect("valid TOML");
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

[[categories]]
name = "CASUAL"
description = "Simple"
threshold = 1
priority = 1
"#;
        let root: toml::Value = toml::from_str(toml_content).expect("valid TOML");
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["llm".to_string()]);
    }

    #[test]
    fn load_classifiers_config_empty_root_returns_defaults() {
        let root = toml::Value::Table(toml::value::Table::new());
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["regex".to_string(), "llm".to_string()]);
    }

    #[test]
    fn load_classifiers_config_non_table_root_returns_defaults() {
        let root = toml::Value::String("not a table".to_string());
        let cfg = load_classifiers_config_from_value(&root);
        assert!(cfg.enabled);
        assert_eq!(cfg.order, vec!["regex".to_string(), "llm".to_string()]);
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
}
