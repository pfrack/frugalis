use serde::Deserialize;
use std::collections::HashMap;

// ── Shared Types ──

/// A single upstream model endpoint within a routing category.
///
/// Each `ProviderEntry` represents one concrete target the proxy can forward
/// requests to. Within a [`RouteEntry`], providers are ordered: index 0 is the
/// primary target and subsequent providers are cascade fallbacks tried in order
/// when the primary is unreachable or returns a retriable error.
///
/// Valid `provider_type` values:
/// - `"openai_compatible"` — OpenAI Chat Completions API
/// - `"anthropic"` — Anthropic Messages API
/// - `"openai_responses"` — OpenAI Responses API (passthrough)
/// - `"nvidia_nim"` — Nvidia NIM (OpenAI-compatible with sanitization)
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderEntry {
    pub model: String,
    pub endpoint: String,
    pub provider_type: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// A routing category mapping an intent label to one or more upstream providers.
///
/// Supports two TOML shapes via [`RouteEntryRaw`] deserialization:
///
/// **Multi-provider (new):**
/// ```toml
/// [routing.COMPLEX]
/// providers = [
///   { model = "claude-sonnet-4", endpoint = "…", provider_type = "anthropic", api_key_env = "ANTHROPIC_API_KEY" },
///   { model = "gpt-4o", endpoint = "…", provider_type = "openai_compatible", api_key_env = "OPENAI_API_KEY" },
/// ]
/// ```
///
/// **Flat (legacy):**
/// ```toml
/// [routing.SYNTAX_FIX]
/// model = "gpt-4o-mini"
/// endpoint = "https://api.openai.com/v1/chat/completions"
/// provider_type = "openai_compatible"
/// api_key_env = "OPENAI_API_KEY"
/// cost_per_1m_input_tokens = 0.15
/// ```
///
/// Both are normalised into `providers: Vec<ProviderEntry>` by
/// `From<RouteEntryRaw>`.
#[derive(Clone, Debug, Deserialize)]
#[serde(from = "RouteEntryRaw")]
pub struct RouteEntry {
    pub providers: Vec<ProviderEntry>,
    pub cost_per_1m_input_tokens: Option<f64>,
}

impl RouteEntry {
    /// Returns the first (primary) provider.
    ///
    /// # Panics
    /// Panics if `providers` is empty. The deserialiser guarantees at least one
    /// entry via [`RouteEntryRaw`], so this should never fire in practice.
    pub fn primary(&self) -> &ProviderEntry {
        &self.providers[0]
    }
}

/// Raw deserialization helper that accepts the union of the legacy flat-field
/// shape and the new `providers` array shape, then normalises them into a
/// [`RouteEntry`] through `From<RouteEntryRaw>`.
///
/// This struct is never exposed outside the module; it exists solely to make
/// the `#[serde(from = "RouteEntryRaw")]` delegation on [`RouteEntry`] work.
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
            if !providers.is_empty() {
                return RouteEntry {
                    providers,
                    cost_per_1m_input_tokens: raw.cost_per_1m_input_tokens,
                };
            }
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

/// Lookup table that maps model names to their cost per 1M input tokens.
///
/// Built by [`loader::build_model_costs`] from two sources (later wins):
/// 1. The top-level `[model_costs]` TOML table.
/// 2. Per-route `cost_per_1m_input_tokens` fields in `[routing.*]` entries.
///
/// Implements [`persistence::types::CostProvider`] so the persistence layer
/// can compute per-request cost estimates without taking a direct dependency
/// on the config module.
#[derive(Clone, Debug)]
pub struct ModelCosts {
    costs: HashMap<String, f64>,
}

impl crate::persistence::types::CostProvider for ModelCosts {
    fn get_cost(&self, model: &str) -> Option<f64> {
        self.get(model)
    }
}

impl ModelCosts {
    /// Look up the cost (USD per 1M input tokens) for `model`.
    /// Returns `None` when no cost has been configured for the model.
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
        assert_eq!(
            entry.providers[0].endpoint,
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(entry.providers[0].provider_type, "openai_compatible");
        assert_eq!(
            entry.providers[0].api_key_env,
            Some("OPENAI_API_KEY".to_string())
        );
        assert!(entry.providers[0].timeout_ms.is_none());
        assert_eq!(entry.cost_per_1m_input_tokens, Some(0.15));
    }

    #[test]
    fn parse_openai_responses_provider_type() {
        let toml_str = r#"
        providers = [
            { model = "gpt-4o", endpoint = "https://api.openai.com/v1/responses", provider_type = "openai_responses", api_key_env = "OPENAI_API_KEY" },
        ]"#;
        let entry: RouteEntry = toml::from_str(toml_str).expect("openai_responses provider should parse");
        assert_eq!(entry.providers[0].provider_type, "openai_responses");
    }
}
