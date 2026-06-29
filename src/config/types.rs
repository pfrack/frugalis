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

/// In-memory cache configuration loaded from the `[cache]` section.
///
/// The cache is disabled entirely when the section is absent **or** when
/// `max_entries` is set to `0`. This allows operators to turn off caching
/// at runtime without removing the section.
#[derive(Clone, Debug, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_cache_ttl_secs")]
    pub ttl_secs: u64,
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: u64,
}

/// Dashboard UI configuration loaded from the `[dashboard]` section.
///
/// Controls the default query window, selectable range bounds, and page-size
/// limits exposed by every dashboard page. Templates read these values via
/// `NavContext` so they do not need their own query-string parsing.
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

/// Cross-Origin Resource Sharing configuration loaded from the `[cors]` section.
///
/// An empty `allowed_origins` list disables CORS headers entirely. Each entry
/// is matched against the `Origin` request header as a literal string.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CorsConfig {
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

/// Top-level server configuration loaded from the `[server]` section.
///
/// Controls the TCP port the Axum listener binds to and the tracing subscriber
/// format. `log_level` accepts the standard `tracing` filter strings
/// (`trace`, `debug`, `info`, `warn`, `error`). `log_format` must be one of
/// `compact`, `full`, `json`, or `pretty`.
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

/// HTTP layer configuration loaded from the `[http]` section.
///
/// Governs both the Axum body-size limits for incoming requests and the reqwest
/// client used to proxy requests upstream. `streaming_channel_capacity` sets
/// the bounded mpsc channel depth for SSE streaming responses.
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

/// SQLite connection pool configuration loaded from the `[database]` section.
///
/// Controls pool size, acquisition timeout, idle-connection lifetime, and the
/// exponential back-off parameters used on initial pool creation. Increase
/// `log_concurrency_limit` to allow more parallel async log writes under high
/// inference throughput.
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

/// Persistence backend configuration loaded from the `[persistence]` section.
///
/// `backend` selects the storage driver: `"memory"` stores inference logs
/// in a `DashMap` (lost on restart, useful for testing) and `"sqlite"` persists
/// them to the file at `sqlite_path`.
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

/// Upstream authentication provider loaded from an `[[auth_provider]]` array entry.
///
/// When the proxy forwards a request, it may need to inject provider-specific
/// credentials that are separate from the client's own bearer token. Each
/// entry describes how to derive and inject a single auth header. The
/// `value_template` may reference environment variables.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthProviderConfig {
    #[serde(rename = "type")]
    pub type_: String,
    pub header: Option<String>,
    pub value_template: Option<String>,
}

/// Global classifier pipeline configuration loaded from the `[classifiers]` section.
///
/// `enabled` is the master switch: setting it to `false` skips all
/// classification and routes every request through the default entry. `order`
/// defines the evaluation sequence; the first classifier that returns a
/// non-`unknown` label wins, and later classifiers are not called.
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

/// Regex classifier configuration loaded from the `[regex_classifier]` section.
///
/// The regex classifier scores prompts against weighted pattern lists. Very
/// short prompts (under `short_prompt_len` characters) are skipped because
/// regex patterns are unreliable on minimal input.
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

/// LLM classifier configuration loaded from the `[llm_classifier]` section.
///
/// The LLM classifier makes a secondary inference call to a smaller model to
/// determine the intent category when the regex and few-shot classifiers
/// return `unknown`. `timeout_secs` caps the classification call independently
/// of the proxy's own `client_timeout_secs`.
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

/// Few-shot (online Naive-Bayes) classifier configuration loaded from the
/// `[fewshot_classifier]` section.
///
/// The classifier operates in two modes:
/// - **Cold-start**: fewer than `cold_start_feedback_count` labelled examples
///   have been seen; uses the stricter `cold_start_threshold` to avoid
///   premature over-fitting.
/// - **Warm**: uses `confidence_threshold` once enough examples are available.
///
/// `feature_dimensions` controls the hashing trick space for term features;
/// larger values reduce collision probability at the cost of memory.
/// `retraining_threshold` sets how many new examples must accumulate before
/// the model weights are recomputed.
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

// ── Classification config types ──

fn default_weight() -> u8 {
    1
}

/// A single regex pattern entry with its weight for intent classification.
#[derive(Clone, Debug, Deserialize)]
pub struct PatternEntry {
    pub regex: String,
    #[serde(default = "default_weight")]
    pub weight: u8,
}

fn default_alt_score() -> u32 {
    1
}

/// Dual-threshold configuration for a category.
#[derive(Clone, Debug, Deserialize)]
pub struct DualThreshold {
    #[serde(default = "default_alt_score")]
    pub alt_score: u32,
    pub suppress_if_present: String,
}

fn default_penalty() -> u8 {
    2
}

/// A negative suppression pattern configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct NegativePatternConfig {
    pub regex: String,
    pub suppressed: String,
    #[serde(default = "default_penalty")]
    pub penalty: u8,
}

fn default_threshold() -> u32 {
    1
}
fn default_priority() -> u8 {
    99
}

/// Single source of truth for intent category definitions.
/// Consumed by RegexClassifier (patterns, thresholds, routing) and
/// LLMClassifier (prompt template descriptions).
///
/// External files hardcoding category name strings:
/// - routing_examples/routing-*.toml (4 files) — section names
/// - openapi/completions.yaml — enum constraint values (line 44, 111)
/// - manual-test/run.sh — x-frugalis-category header (line 179)
/// - templates/dashboard/inferences.html — placeholder text (line 19)
///
/// Category names are a PUBLIC API contract. Renaming any value here
/// is a breaking change requiring updates to all listed consumers.
/// Names must stay [A-Z_]+ for compatibility with key.to_uppercase()
/// normalization in the routing config loader.
#[derive(Clone, Debug, Deserialize)]
pub struct CategoryConfig {
    #[serde(default)]
    pub name: String,
    pub description: String,
    #[serde(default = "default_threshold")]
    pub threshold: u32,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub patterns: Vec<PatternEntry>,
    #[serde(default)]
    pub patterns_file: Option<String>,
    pub dual_threshold: Option<DualThreshold>,
}
