use serde::Deserialize;

// ── Serde default-value helpers ──

fn default_port() -> u16 {
    10000
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_format() -> String {
    "compact".to_string()
}
fn default_max_body_bytes() -> usize {
    10_485_760
}
fn default_keepalive_interval() -> u64 {
    15
}
fn default_client_timeout() -> u64 {
    120
}
fn default_client_connect_timeout() -> u64 {
    30
}
fn default_streaming_chan_cap() -> usize {
    32
}
fn default_connection_retries() -> u32 {
    3
}
fn default_retry_base_ms() -> u64 {
    1000
}
fn default_max_connections() -> u32 {
    10
}
fn default_acquire_timeout() -> u64 {
    30
}
fn default_idle_timeout() -> u64 {
    1800
}
fn default_log_concurrency() -> u32 {
    100
}
fn default_backend() -> String {
    "memory".to_string()
}
fn default_db_path() -> String {
    "./frugalis.db".to_string()
}
fn default_dashboard_hours() -> u32 {
    24
}
fn default_hours_min() -> u32 {
    1
}
fn default_hours_max() -> u32 {
    720
}
fn default_page_limit() -> u32 {
    20
}
fn default_page_limit_max() -> u32 {
    100
}
fn default_recent_count() -> u32 {
    5
}
fn default_short_prompt_len() -> usize {
    30
}
fn default_timeout_secs() -> u64 {
    3
}
fn default_classifier_order() -> Vec<String> {
    vec![
        "regex".to_string(),
        "fewshot".to_string(),
        "llm".to_string(),
    ]
}
fn default_llm_model() -> String {
    "gpt-4o-mini".to_string()
}
fn default_llm_api_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}
fn default_provider_type() -> String {
    "openai_compatible".to_string()
}
fn default_enabled_true() -> bool {
    true
}
fn default_confidence_threshold() -> f64 {
    0.4
}
fn default_cold_start_threshold() -> f64 {
    0.6
}
fn default_cold_start_feedback_count() -> usize {
    5
}
fn default_feature_dimensions() -> usize {
    1000
}
fn default_retraining_threshold() -> usize {
    5
}
fn default_fewshot_data_path() -> String {
    "data/fewshot_training.yaml".to_string()
}
fn default_max_vocabulary_warn() -> usize {
    5000
}
fn default_max_training_examples() -> usize {
    10000
}

fn default_cache_ttl_secs() -> u64 {
    300
}
fn default_cache_max_entries() -> u64 {
    1000
}

/// Cache configuration loaded from [cache] section.
/// When the section is absent or `max_entries == 0`, the cache is disabled.
#[derive(Clone, Debug, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_cache_ttl_secs")]
    pub ttl_secs: u64,
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: u64,
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
            sqlite_path: "./frugalis.db".to_string(),
        }
    }
}

/// Authentication provider configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthProviderConfig {
    #[serde(rename = "type")]
    pub type_: String,
    pub header: Option<String>,
    pub value_template: Option<String>,
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
            order: vec![
                "regex".to_string(),
                "fewshot".to_string(),
                "llm".to_string(),
            ],
        }
    }
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
