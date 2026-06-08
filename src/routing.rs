use std::collections::HashMap;

// ── Shared Types ──

#[derive(Clone, Debug)]
pub struct RouteEntry {
    pub model: String,
    pub endpoint: String,
    pub cost_per_1m_input_tokens: Option<f64>,
    pub provider_type: String,
    pub api_key_env: Option<String>,
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
pub const DEFAULT_MODEL_READING: &str = "meta/llama-3.1-70b-instruct";
