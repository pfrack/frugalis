use serde::Deserialize;
use std::collections::HashMap;

// ── Shared Types ──

/// A single upstream provider within a routing category.
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderEntry {
    pub model: String,
    pub endpoint: String,
    pub provider_type: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub timeout_ms: Option<u64>,
}

/// A routing category with an ordered list of providers (primary first).
/// Uses custom deserialization via `RouteEntryRaw` to support both legacy
/// flat configs (`model = ..., endpoint = ...`) and new array-of-providers
/// format (`providers = [{...}, ...]`).
#[derive(Clone, Debug, Deserialize)]
#[serde(from = "RouteEntryRaw")]
pub struct RouteEntry {
    pub providers: Vec<ProviderEntry>,
    pub cost_per_1m_input_tokens: Option<f64>,
}

impl RouteEntry {
    /// Returns the first (primary) provider. Panics if `providers` is empty
    /// — the deserializer guarantees at least one element.
    pub fn primary(&self) -> &ProviderEntry {
        &self.providers[0]
    }
}

/// Raw deserialization helper for backward-compatible config parsing.
/// Accepts both legacy flat fields and the new `providers` array.
#[derive(Clone, Debug, Deserialize)]
struct RouteEntryRaw {
    model: Option<String>,
    endpoint: Option<String>,
    provider_type: Option<String>,
    api_key_env: Option<String>,
    #[serde(default)]
    cost_per_1m_input_tokens: Option<f64>,
    providers: Option<Vec<ProviderEntry>>,
}

impl From<RouteEntryRaw> for RouteEntry {
    fn from(raw: RouteEntryRaw) -> Self {
        if let Some(providers) = raw.providers {
            return RouteEntry {
                providers,
                cost_per_1m_input_tokens: raw.cost_per_1m_input_tokens,
            };
        }
        RouteEntry {
            providers: vec![ProviderEntry {
                model: raw.model.unwrap_or_default(),
                endpoint: raw.endpoint.unwrap_or_default(),
                provider_type: raw.provider_type.unwrap_or_default(),
                api_key_env: raw.api_key_env,
                timeout_ms: None,
            }],
            cost_per_1m_input_tokens: raw.cost_per_1m_input_tokens,
        }
    }
}

/// Maps model names to their cost per 1M input tokens.
/// Defaults are hardcoded; routing.toml entries can override.
#[derive(Clone, Debug)]
pub struct ModelCosts {
    costs: HashMap<String, f64>,
}

impl crate::persistence::CostProvider for ModelCosts {
    fn get_cost(&self, model: &str) -> Option<f64> {
        self.get(model)
    }
}

impl ModelCosts {
    /// Look up a model's cost per 1M input tokens.
    pub fn get(&self, model: &str) -> Option<f64> {
        self.costs.get(model).copied()
    }

    /// An empty cost table — all model lookups return None.
    #[cfg(test)]
    pub fn empty() -> Self {
        ModelCosts {
            costs: HashMap::new(),
        }
    }

    pub(crate) fn from_costs(costs: HashMap<String, f64>) -> Self {
        ModelCosts { costs }
    }
}

// ── Default Model Constants ──

pub const DEFAULT_MODEL: &str = "meta/llama-3.1-8b-instruct";
pub const DEFAULT_MODEL_COMPLEX: &str = "meta/llama-3.3-70b-instruct";
/// Default model for the embedded fallback (`hardcoded_routing`). Targets
/// Ollama's canonical model name so a fresh install with no `CONFIG_PATH`
/// works locally without requiring any API key.
pub const DEFAULT_MODEL_LOCAL: &str = "llama3.1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_providers_array_config() {
        let toml_str = r#"
        providers = [
            { model = "claude-sonnet-4", endpoint = "https://api.anthropic.com/v1/messages", provider_type = "anthropic", api_key_env = "ANTHROPIC_API_KEY" },
            { model = "gpt-4o", endpoint = "https://api.openai.com/v1/chat/completions", provider_type = "openai_compatible", api_key_env = "OPENAI_API_KEY", timeout_ms = 5000 },
        ]"#;
        let entry: RouteEntry = toml::from_str(toml_str).expect("providers array should parse");
        assert_eq!(entry.providers.len(), 2);
        assert_eq!(entry.providers[0].model, "claude-sonnet-4");
        assert_eq!(entry.providers[0].provider_type, "anthropic");
        assert_eq!(entry.providers[1].model, "gpt-4o");
        assert_eq!(entry.providers[1].provider_type, "openai_compatible");
        assert_eq!(entry.providers[1].timeout_ms, Some(5000));
        assert!(entry.cost_per_1m_input_tokens.is_none());
    }

    #[test]
    fn parse_legacy_flat_config() {
        let toml_str = r#"
        model = "gpt-4o-mini"
        endpoint = "https://api.openai.com/v1/chat/completions"
        provider_type = "openai_compatible"
        api_key_env = "OPENAI_API_KEY"
        cost_per_1m_input_tokens = 0.15
        "#;
        let entry: RouteEntry = toml::from_str(toml_str).expect("legacy flat config should parse");
        assert_eq!(entry.providers.len(), 1);
        assert_eq!(entry.providers[0].model, "gpt-4o-mini");
        assert_eq!(entry.providers[0].endpoint, "https://api.openai.com/v1/chat/completions");
        assert_eq!(entry.providers[0].provider_type, "openai_compatible");
        assert_eq!(entry.providers[0].api_key_env, Some("OPENAI_API_KEY".to_string()));
        assert!(entry.providers[0].timeout_ms.is_none());
        assert_eq!(entry.cost_per_1m_input_tokens, Some(0.15));
    }
}
