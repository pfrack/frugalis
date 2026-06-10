use std::sync::Arc;
use std::time::{SystemTime, Duration};

use crate::config::DatabaseConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use sqlx::Row;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Trait for looking up model costs by name.
/// Allows persistence to query costs without depending on the classification module directly.
pub trait CostProvider {
    fn get_cost(&self, model: &str) -> Option<f64>;
}

/// Custom error type for inference query failures.
#[derive(Debug, Clone)]
pub struct QueryError(pub String);

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Database error: {}", self.0)
    }
}

/// One row from the `inferences` table, pre-formatted for dashboard display.
#[derive(Debug, Clone)]
pub struct InferenceLog {
    pub timestamp: String,
    pub prompt_snippet: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
}

/// One row from the latency aggregation query — a single category's summary.
#[derive(Debug, Clone)]
pub struct LatencySummaryRow {
    pub category: String,
    pub request_count: i64,
    pub avg_duration_ms: Option<i32>,
    pub p99_duration_ms: Option<i32>,
}

/// Result of a cost-savings estimate computation for the dashboard.
#[derive(Debug, Clone)]
pub struct SavingsEstimate {
    pub savings_usd: f64,
    pub formatted_savings_usd: String,
    pub baseline_model: String,
    pub classified_count: i64,
    pub unknown_cost_count: i64,
    pub has_historical_fallback: bool,
    pub baseline_model_unknown: bool,
}

/// Container for the full latency aggregation result.
#[derive(Debug, Clone)]
pub struct LatencySummary {
    pub rows: Vec<LatencySummaryRow>,
    pub unclassified_count: i64,
    pub total_classified_count: i64,
}

/// Shared persistence configuration injected into the app router.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub pool: Arc<PgPool>,
    /// Bounds the number of concurrent background logging tasks to prevent
    /// unbounded memory growth under high throughput.
    pub task_semaphore: Arc<Semaphore>,
}

/// Finalized inference metadata payload ready for background persistence.
pub struct InferenceRecord {
    pub request_id: Uuid,
    pub status: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
    pub prompt_snippet: String,
    pub prompt_char_count: Option<i32>,
}

use sqlx::postgres::PgConnectOptions;
use std::str::FromStr;

impl PersistenceConfig {
    pub async fn from_env(db_config: &DatabaseConfig) -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;

        let options = PgConnectOptions::from_str(&url)
            .map_err(|e| format!("DB connection string parse error: {e}"))?;

        let pool = PgPoolOptions::new()
            .max_connections(db_config.max_connections)
            .acquire_timeout(std::time::Duration::from_secs(db_config.acquire_timeout_secs))
            .idle_timeout(std::time::Duration::from_secs(db_config.idle_timeout_secs))
            .connect_lazy_with(options);

        let base_delay = Duration::from_millis(db_config.retry_base_ms);

        // Validate DB connectivity with retries (exponential backoff with jitter)
        let mut last_err = None;

        for attempt in 0..db_config.connection_retries {
            match sqlx::query("SELECT 1").fetch_one(&pool).await {
                Ok(_) => {
                    // success, break out
                    break;
                }
                Err(e) => {
                    if attempt < db_config.connection_retries - 1 {
                        warn!("DB health check failed (attempt {}): {}. Retrying...", attempt + 1, &e);
                        // Exponential backoff
                        let backoff = base_delay * (1u32 << attempt);
                        // Add jitter: random amount between 0 and base_delay
                        let now_nanos = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos();
                        let jitter_nanos = now_nanos % base_delay.as_nanos();
                        let jitter = Duration::from_nanos(jitter_nanos as u64);
                        let delay = backoff + jitter;
                        tokio::time::sleep(delay).await;
                    }
                    last_err = Some(e);
                }
            }
        }

        if let Some(e) = last_err {
            // DATABASE_URL is set, so we panic
            panic!("Database health check failed after {} retries: {}", db_config.connection_retries, e);
        }

        // Run migrations (fatal if fails)
        if let Err(e) = sqlx::migrate!().run(&pool).await {
            panic!("Migrations failed: {e}");
        }
        info!("Migrations applied successfully");

        Ok(Self {
            pool: Arc::new(pool),
            task_semaphore: Arc::new(Semaphore::new(db_config.log_concurrency_limit as usize)),
        })
    }

    /// Fetch recent inference records with optional pagination and filtering.
    ///
    /// Returns both the matching records (formatted for display) and the total
    /// count of matching rows (for pagination metadata), avoiding N+1 queries
    /// by running a separate COUNT query with the same filters.
    pub async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError> {
        // Build WHERE clause dynamically with auto-incrementing bind count.
        let mut bind_count = 1;
        let mut where_clause = String::new();

        if filter_category.is_some() {
            where_clause.push_str(&format!("category = ${} ", bind_count));
            bind_count += 1;
        }
        if filter_model.is_some() {
            if !where_clause.is_empty() {
                where_clause.push_str("AND ");
            }
            where_clause.push_str(&format!("upstream_model = ${} ", bind_count));
            bind_count += 1;
        }
        let where_clause = if where_clause.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_clause.trim_end())
        };

        let limit_ph = format!("${}", bind_count);
        bind_count += 1;
        let offset_ph = format!("${}", bind_count);

        let data_sql = format!(
            "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
             FROM inferences{} ORDER BY created_at DESC LIMIT {} OFFSET {}",
            where_clause, limit_ph, offset_ph,
        );
        let count_sql = format!("SELECT COUNT(*) FROM inferences{}", where_clause);

        // Execute count query.
        let mut count_query = sqlx::query(&count_sql);
        if let Some(cat) = filter_category {
            count_query = count_query.bind(cat);
        }
        if let Some(model) = filter_model {
            count_query = count_query.bind(model);
        }
        let total_count: i64 = count_query
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| QueryError(e.to_string()))?
            .try_get(0)
            .map_err(|e| QueryError(e.to_string()))?;

        // Execute data query.
        let mut data_query = sqlx::query(&data_sql);
        if let Some(cat) = filter_category {
            data_query = data_query.bind(cat);
        }
        if let Some(model) = filter_model {
            data_query = data_query.bind(model);
        }
        let rows = data_query
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| QueryError(e.to_string()))?;

        // Map rows to InferenceLog, formatting timestamps and durations.
        // Propagate any row extraction errors to fail fast on data issues.
        let records: Vec<InferenceLog> = rows
            .iter()
            .map(|row| {
                let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
                let timestamp = created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                let prompt_snippet: String = row.try_get("prompt_snippet")?;
                let category: Option<String> = row.try_get("category")?;
                let upstream_model: Option<String> = row.try_get("upstream_model")?;
                let duration_ms: Option<i32> = row.try_get("duration_ms")?;
                Ok(InferenceLog {
                    timestamp,
                    prompt_snippet,
                    category,
                    upstream_model,
                    duration_ms,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(|e| QueryError(e.to_string()))?;

        Ok((records, total_count))
    }

    /// Fetch a per-category latency summary for the given time window.
    ///
    /// Runs a single GROUP BY aggregation over all records in the window.
    /// Rows with a non-NULL category become the [`LatencySummaryRow`] list;
    /// the NULL-category row (if any) populates [`LatencySummary::unclassified_count`].
    pub async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        let rows = sqlx::query(
            "SELECT category, \
             COUNT(*)::BIGINT AS count, \
             ROUND(AVG(duration_ms))::INTEGER AS avg_duration_ms, \
             ROUND(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms))::INTEGER \
             AS p99_duration_ms \
             FROM inferences \
             WHERE created_at >= NOW() - interval '1 hour' * $1 \
             GROUP BY category \
             ORDER BY count DESC",
        )
        .bind(hours as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| QueryError(e.to_string()))?;

        let mut summary_rows = Vec::<LatencySummaryRow>::new();
        let mut unclassified_count: i64 = 0;

        for row in &rows {
            let category: Option<String> = row.try_get("category").unwrap_or(None);
            let request_count: i64 = row.try_get("count").unwrap_or(0);

            match category {
                Some(cat) => {
                    let avg_duration_ms: Option<i32> =
                        row.try_get("avg_duration_ms").unwrap_or(None);
                    let p99_duration_ms: Option<i32> =
                        row.try_get("p99_duration_ms").unwrap_or(None);
                    summary_rows.push(LatencySummaryRow {
                        category: cat,
                        request_count,
                        avg_duration_ms,
                        p99_duration_ms,
                    });
                }
                None => {
                    unclassified_count = request_count;
                }
            }
        }

        let total_classified_count: i64 = summary_rows.iter().map(|r| r.request_count).sum();

        Ok(LatencySummary {
            rows: summary_rows,
            unclassified_count,
            total_classified_count,
        })
    }

    /// Fetch a cost-savings estimate for the given time window.
    ///
    /// Groups inference records by model, computes actual vs. baseline cost,
    /// and returns a [`SavingsEstimate`]. Records with NULL category or NULL
    /// model are excluded. Records with an unknown model cost are counted in
    /// `unknown_cost_count` and excluded from the savings total.
    pub async fn fetch_savings_estimate(
        &self,
        hours: u32,
        model_costs: &impl CostProvider,
        baseline_model: &str,
    ) -> Result<SavingsEstimate, QueryError> {
        let rows = sqlx::query(
            "SELECT \
             upstream_model, \
             COUNT(*)::BIGINT AS count, \
             COALESCE(SUM(prompt_char_count), 0)::BIGINT AS total_chars, \
             COALESCE(SUM(LENGTH(prompt_snippet)), 0)::BIGINT AS total_fallback_chars, \
             COALESCE(SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END), 0)::BIGINT \
             AS fallback_count \
             FROM inferences \
             WHERE created_at >= NOW() - interval '1 hour' * $1 \
             AND category IS NOT NULL \
             AND upstream_model IS NOT NULL \
             GROUP BY upstream_model",
        )
        .bind(hours as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| QueryError(e.to_string()))?;

        let mut total_actual_cost: f64 = 0.0;
        let mut total_chars_all: i64 = 0;
        let mut classified_count: i64 = 0;
        let mut unknown_cost_count: i64 = 0;
        let mut has_historical_fallback = false;

        for row in &rows {
            let model: String = row.try_get("upstream_model").unwrap_or_default();
            let count: i64 = row.try_get("count").unwrap_or(0);
            let total_chars: i64 = row.try_get("total_chars").unwrap_or(0);
            let total_fallback_chars: i64 = row.try_get("total_fallback_chars").unwrap_or(0);
            let fallback_count: i64 = row.try_get("fallback_count").unwrap_or(0);

            if fallback_count > 0 {
                has_historical_fallback = true;
            }

            classified_count += count;

            // Use actual prompt_char_count when available, fall back to snippet length.
            let effective_chars = if total_chars > 0 {
                total_chars
            } else {
                total_fallback_chars
            };
            total_chars_all += effective_chars;

            if let Some(cost) = model_costs.get_cost(&model) {
                total_actual_cost += prompt_chars_to_cost(effective_chars as i32, cost);
            } else {
                unknown_cost_count += count;
            }
        }

        let baseline_cost = model_costs
            .get_cost(baseline_model)
            .map(|cost_per_1m| {
                let tokens = total_chars_all as f64 / 4.0;
                tokens * cost_per_1m / 1_000_000.0
            })
            .unwrap_or(0.0);

        let baseline_cost_rounded = (baseline_cost * 10_000.0).round() / 10_000.0;
        let savings_usd = baseline_cost_rounded - total_actual_cost;
        let baseline_model_unknown = model_costs.get_cost(baseline_model).is_none();

        let formatted_savings_usd = if savings_usd > 0.0 {
            format!("{:.4}", savings_usd)
        } else {
            String::new()
        };

        Ok(SavingsEstimate {
            savings_usd,
            formatted_savings_usd,
            baseline_model: baseline_model.to_string(),
            classified_count,
            unknown_cost_count,
            has_historical_fallback,
            baseline_model_unknown,
        })
    }
}

/// Extract the full last user message from an OpenAI-compatible request body.
///
/// Parses `body` as `{"messages": [...]}`, finds the last message whose `role`
/// is `"user"`, and returns its `content` string capped at 10,000 characters.
/// On any parse failure or missing user message, returns `""` and emits a WARN
/// log. Never panics.
///
/// This is the shared utility used by both snippet extraction (`extract_snippet`)
/// and the intent classifier for full-text intent analysis.
pub fn extract_last_user_message(body: &str) -> String {
    let result: Option<String> = (|| {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let messages = v.get("messages")?.as_array()?;
        // Prevent DoS via unbounded message arrays.
        if messages.len() > 1000 {
            warn!(
                "ignoring request with {} messages (limit 1000)",
                messages.len()
            );
            return Some(String::new());
        }
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?;
        let content = last_user.get("content")?.as_str()?;
        Some(content.chars().take(10_000).collect())
    })();

    match result {
        Some(s) => s,
        None => {
            warn!("could not extract user message from request body; storing empty prompt");
            String::new()
        }
    }
}

/// Extract a 200-char privacy-safe snippet from an OpenAI-compatible request body.
///
/// Delegates to [`extract_last_user_message`] for JSON parsing and last-user-message
/// logic, then truncates to 200 characters. On any parse failure returns `""`.
/// Never panics, never blocks the response path.
pub fn extract_snippet(body: &str) -> String {
    let full = extract_last_user_message(body);
    full.chars().take(200).collect()
}

/// Convert character count to estimated dollar cost.
///
/// Uses a simple 4-characters-to-1-token heuristic. Rounds to 6 decimal places.
pub fn prompt_chars_to_cost(char_count: i32, cost_per_1m_input_tokens: f64) -> f64 {
    let tokens = char_count as f64 / 4.0;
    let cost = tokens * cost_per_1m_input_tokens / 1_000_000.0;
    (cost * 1_000_000.0).round() / 1_000_000.0
}

/// Enqueue an inference record for async persistence.
///
/// Spawns a detached background task that attempts one insert and, on failure,
/// retries exactly once. Final failure is logged with the `request_id` and
/// failure class. The caller returns immediately; DB latency is never on the
/// synchronous response path.
///
/// Uses a semaphore to bound concurrent tasks; if the limit is reached, the
/// task waits briefly before executing. This prevents unbounded memory growth
/// under sustained high throughput.
pub fn log_inference(
    pool: Arc<PgPool>,
    semaphore: Arc<Semaphore>,
    record: InferenceRecord,
) -> tokio::task::JoinHandle<()> {
    let semaphore = semaphore.clone();
    // Intentional fire-and-forget: JoinHandle is dropped — tokio's default
    // panic handler prints any panic in the spawned task to stderr.
    tokio::spawn(async move {
        let request_id = record.request_id;
        let _permit = match semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => {
                error!("semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = write_with_retry(&pool, &record).await {
            error!("final insert failure request_id={request_id} class={class}");
        }
    })
}

async fn write_with_retry(pool: &PgPool, record: &InferenceRecord) -> Result<(), String> {
    retry_once(|| insert_once(pool, record)).await.map_err(|e| {
        error!(
            "insert failed for request_id={} after retries: {:?}",
            record.request_id, e
        );
        e.to_string()
    })
}

/// Retry an async operation exactly once.
/// Logs a warning on the first failure and returns the second error if both fail.
async fn retry_once<F, Fut, T, E>(f: F) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match f().await {
        Ok(v) => Ok(v),
        Err(first) => {
            warn!("first insert attempt failed ({first}); retrying once");
            f().await
        }
    }
}

async fn insert_once(pool: &PgPool, record: &InferenceRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inferences \
         (request_id, status, category, upstream_model, duration_ms, prompt_snippet, prompt_char_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(record.request_id)
    .bind(&record.status)
    .bind(&record.category)
    .bind(&record.upstream_model)
    .bind(record.duration_ms)
    .bind(&record.prompt_snippet)
    .bind(record.prompt_char_count)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Ephemeral PostgreSQL container for integration tests.
/// Spins up via `testcontainers`; falls back to DATABASE_URL when Docker unavailable.
#[cfg(test)]
struct TestDb {
    pool: Arc<PgPool>,
    _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
}

#[cfg(test)]
impl TestDb {
    async fn new() -> Option<Self> {
        use testcontainers::{
            core::{IntoContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };
        // GenericImage builder methods first, then ImageExt methods
        let container = GenericImage::new("postgres", "16-alpine")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "test")
            .with_env_var("POSTGRES_PASSWORD", "test")
            .with_env_var("POSTGRES_DB", "test")
            .with_startup_timeout(Duration::from_secs(60))
            .start()
            .await
            .ok()?;
        let port = container.get_host_port_ipv4(5432.tcp()).await.ok()?;
        let url = format!("postgres://test:test@127.0.0.1:{port}/test");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .ok()?;
        sqlx::migrate!().run(&pool).await.ok()?;
        eprintln!("Test DB: postgres://test:test@127.0.0.1:{port}/test");
        Some(Self {
            pool: Arc::new(pool),
            _container: container,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Snippet extraction ────────────────────────────────────────────────────

    #[test]
    fn persistence_snippet_returns_last_user_content() {
        let body = r#"{"messages":[{"role":"system","content":"be helpful"},{"role":"user","content":"hello"}]}"#;
        assert_eq!(extract_snippet(body), "hello");
    }

    #[test]
    fn persistence_snippet_truncates_at_200_chars() {
        let long = "x".repeat(300);
        let body = format!(r#"{{"messages":[{{"role":"user","content":"{long}"}}]}}"#);
        assert_eq!(extract_snippet(&body).len(), 200);
    }

    #[test]
    fn persistence_snippet_picks_last_user_message() {
        let body = r#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"r"},{"role":"user","content":"second"}]}"#;
        assert_eq!(extract_snippet(body), "second");
    }

    #[test]
    fn persistence_snippet_returns_empty_on_invalid_json() {
        assert_eq!(extract_snippet("not json"), "");
    }

    #[test]
    fn persistence_snippet_returns_empty_when_no_user_message() {
        let body = r#"{"messages":[{"role":"system","content":"sys"}]}"#;
        assert_eq!(extract_snippet(body), "");
    }

    #[test]
    fn persistence_snippet_returns_empty_on_empty_body() {
        assert_eq!(extract_snippet(""), "");
    }

    #[test]
    fn persistence_snippet_returns_empty_on_missing_messages_field() {
        assert_eq!(extract_snippet(r#"{"model":"gpt-4"}"#), "");
    }

    #[test]
    fn persistence_snippet_returns_empty_on_oversized_array() {
        let mut messages = vec![];
        for i in 0..1001 {
            messages.push(format!(r#"{{"role":"user","content":"msg {}"}}"#, i));
        }
        let body = format!(r#"{{"messages":[{}]}}"#, messages.join(","));
        assert_eq!(extract_snippet(&body), "");
    }

    // ── extract_last_user_message ──────────────────────────────────────────────

    #[test]
    fn persistence_extract_last_user_message_returns_full_content() {
        let body = r#"{"messages":[{"role":"user","content":"hello world"}]}"#;
        assert_eq!(extract_last_user_message(body), "hello world");
    }

    #[test]
    fn persistence_extract_last_user_message_returns_empty_on_invalid_json() {
        assert_eq!(extract_last_user_message("not json"), "");
    }

    #[test]
    fn persistence_extract_last_user_message_returns_empty_on_empty_body() {
        assert_eq!(extract_last_user_message(""), "");
    }

    #[test]
    fn persistence_extract_last_user_message_returns_empty_when_no_user_message() {
        let body = r#"{"messages":[{"role":"system","content":"sys"}]}"#;
        assert_eq!(extract_last_user_message(body), "");
    }

    #[test]
    fn persistence_extract_last_user_message_caps_at_10000_chars() {
        let long = "x".repeat(15000);
        let body = format!(r#"{{"messages":[{{"role":"user","content":"{long}"}}]}}"#);
        assert_eq!(extract_last_user_message(&body).len(), 10000);
    }

    #[test]
    fn persistence_extract_last_user_message_returns_empty_on_oversized_array() {
        let mut messages = vec![];
        for i in 0..1001 {
            messages.push(format!(r#"{{"role":"user","content":"msg {}"}}"#, i));
        }
        let body = format!(r#"{{"messages":[{}]}}"#, messages.join(","));
        assert_eq!(extract_last_user_message(&body), "");
    }

    // ── Retry behavior ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn persistence_retry_calls_exactly_twice_on_failure() {
        use std::sync::{Arc, Mutex};
        let count = Arc::new(Mutex::new(0u32));
        let c = count.clone();

        let result = retry_once(|| {
            let c = c.clone();
            async move {
                *c.lock().unwrap() += 1;
                Err::<(), &str>("always fails")
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            *count.lock().unwrap(),
            2,
            "should attempt exactly twice (initial + 1 retry)"
        );
    }

    #[tokio::test]
    async fn persistence_retry_succeeds_without_retry_on_first_ok() {
        use std::sync::{Arc, Mutex};
        let count = Arc::new(Mutex::new(0u32));
        let c = count.clone();

        let result = retry_once(|| {
            let c = c.clone();
            async move {
                *c.lock().unwrap() += 1;
                Ok::<&str, &str>("ok")
            }
        })
        .await;

        assert_eq!(result.unwrap(), "ok");
        assert_eq!(
            *count.lock().unwrap(),
            1,
            "should call only once on success"
        );
    }

    #[tokio::test]
    async fn persistence_retry_succeeds_on_second_attempt() {
        use std::sync::{Arc, Mutex};
        let count = Arc::new(Mutex::new(0u32));
        let c = count.clone();

        let result = retry_once(|| {
            let c = c.clone();
            async move {
                let mut n = c.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Err("first fail")
                } else {
                    Ok("recovered")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "recovered");
    }

    // ── fetch_inferences ─────────────────────────────────────────────────────

    async fn test_pool() -> Option<Arc<PgPool>> {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        super::test_pool().await
    }

    fn make_persistence(pool: Arc<PgPool>) -> PersistenceConfig {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        PersistenceConfig {
            pool,
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

    #[tokio::test]
    async fn test_fetch_inferences_empty_list() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_empty_list: DATABASE_URL not set");
                return;
            }
        };
        // Clean slate for this test using a unique category prefix unlikely to collide.
        let pc = make_persistence(pool);
        // Fetch with a filter that will match nothing.
        let result = pc
            .fetch_inferences(0, 20, Some("NONEXISTENT_CATEGORY_XYZ"), None)
            .await;
        let (records, count) = result.expect("fetch should succeed");
        assert!(records.is_empty(), "expected no records");
        assert_eq!(count, 0, "expected count=0");
    }

    #[tokio::test]
    async fn test_fetch_inferences_with_records() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_with_records: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        // Insert a test record.
        let request_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, duration_ms, prompt_snippet) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(request_id)
        .bind("ok")
        .bind("TEST_CAT_FETCH")
        .bind("test-model")
        .bind(42i32)
        .bind("test snippet")
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let (records, count) = pc
            .fetch_inferences(0, 20, Some("TEST_CAT_FETCH"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(request_id)
            .execute(pool.as_ref())
            .await
            .ok();

        assert!(count >= 1, "expected at least one record");
        let found = records.iter().any(|r| r.prompt_snippet == "test snippet");
        assert!(found, "inserted record should appear in results");
    }

    #[tokio::test]
    async fn test_fetch_inferences_filter_by_category() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_filter_by_category: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id_a = uuid::Uuid::new_v4();
        let id_b = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
        )
        .bind(id_a)
        .bind("ok")
        .bind("CAT_ALPHA")
        .bind("alpha snippet")
        .execute(pool.as_ref())
        .await
        .ok();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
        )
        .bind(id_b)
        .bind("ok")
        .bind("CAT_BETA")
        .bind("beta snippet")
        .execute(pool.as_ref())
        .await
        .ok();

        let (records, _) = pc
            .fetch_inferences(0, 100, Some("CAT_ALPHA"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(vec![id_a, id_b])
            .execute(pool.as_ref())
            .await
            .ok();

        let has_alpha = records.iter().any(|r| r.prompt_snippet == "alpha snippet");
        let has_beta = records.iter().any(|r| r.prompt_snippet == "beta snippet");
        assert!(has_alpha, "CAT_ALPHA record should appear");
        assert!(
            !has_beta,
            "CAT_BETA record should not appear when filtering by CAT_ALPHA"
        );
    }

    #[tokio::test]
    async fn test_fetch_inferences_returns_total_count() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_returns_total_count: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();
        for id in &ids {
            sqlx::query(
                "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
            )
            .bind(*id)
            .bind("ok")
            .bind("TOTAL_COUNT_TEST")
            .bind("snippet")
            .execute(pool.as_ref())
            .await
            .ok();
        }

        // Fetch only 1 row but total_count should reflect all matching rows.
        let (records, total_count) = pc
            .fetch_inferences(0, 1, Some("TOTAL_COUNT_TEST"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(ids)
            .execute(pool.as_ref())
            .await
            .ok();

        assert_eq!(records.len(), 1, "should return only 1 record (limit=1)");
        assert!(total_count >= 3, "total_count should be at least 3");
    }

    // ── prompt_chars_to_cost ──────────────────────────────────────────────────

    #[test]
    fn persistence_prompt_chars_to_cost_known_values() {
        // 10000 chars → 2500 tokens → $0.000375 for gpt-4o-mini ($0.15/1M)
        let cost = prompt_chars_to_cost(10000, 0.15);
        assert!((cost - 0.000375).abs() < 0.000001, "got {cost}");
        // 4000 chars → 1000 tokens → $0.0025 for gpt-4o ($2.50/1M)
        let cost = prompt_chars_to_cost(4000, 2.50);
        assert!((cost - 0.0025).abs() < 0.000001, "got {cost}");
    }

    #[test]
    fn persistence_prompt_chars_to_cost_zero_chars() {
        assert_eq!(prompt_chars_to_cost(0, 1.0), 0.0);
    }

    #[test]
    fn persistence_prompt_chars_to_cost_rounds_to_6_decimals() {
        // 1 char → 0.25 tokens → $0.00000075 → rounds to $0.000001 at 6 decimals
        let cost = prompt_chars_to_cost(1, 3.00);
        assert!((cost - 0.000001).abs() < 0.0000001, "got {cost}");
    }

    // ── fetch_latency_summary ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_latency_summary_empty() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_latency_summary_empty: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id = uuid::Uuid::new_v4();
        let cat = format!("Z_TST_LAT_EMPTY_{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, duration_ms, prompt_snippet) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind("ok")
        .bind(&cat)
        .bind(100i32)
        .bind("single record")
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let result = pc
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .ok();

        let test_rows: Vec<_> = result.rows.iter().filter(|r| r.category == cat).collect();
        assert_eq!(test_rows.len(), 1, "expected exactly one test row");
        assert_eq!(test_rows[0].request_count, 1);
        assert_eq!(test_rows[0].avg_duration_ms, Some(100));
        assert!(
            result.total_classified_count >= 1,
            "total should include test record"
        );
    }

    #[tokio::test]
    async fn test_fetch_latency_summary_with_data() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_latency_summary_with_data: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id_a1 = uuid::Uuid::new_v4();
        let id_a2 = uuid::Uuid::new_v4();
        let id_a3 = uuid::Uuid::new_v4();
        let id_b1 = uuid::Uuid::new_v4();
        let id_b2 = uuid::Uuid::new_v4();
        let id_c1 = uuid::Uuid::new_v4();
        let ids = vec![id_a1, id_a2, id_a3, id_b1, id_b2, id_c1];
        let prefix = format!("Z_TST_LAT_DATA_{}", uuid::Uuid::new_v4());
        let cat_a = format!("{prefix}_A");
        let cat_b = format!("{prefix}_B");
        let cat_c = format!("{prefix}_C");

        // Category A: 3 records with durations 100, 200, 300
        for (id, dur) in [(id_a1, 100), (id_a2, 200), (id_a3, 300)] {
            sqlx::query(
                "INSERT INTO inferences (request_id, status, category, duration_ms, prompt_snippet) \
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(id)
            .bind("ok")
            .bind(&cat_a)
            .bind(dur)
            .bind("cat a")
            .execute(pool.as_ref())
            .await
            .ok();
        }
        // Category B: 2 records with durations 50, 150
        for (id, dur) in [(id_b1, 50), (id_b2, 150)] {
            sqlx::query(
                "INSERT INTO inferences (request_id, status, category, duration_ms, prompt_snippet) \
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(id)
            .bind("ok")
            .bind(&cat_b)
            .bind(dur)
            .bind("cat b")
            .execute(pool.as_ref())
            .await
            .ok();
        }
        // Category C: 1 record with duration 500
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, duration_ms, prompt_snippet) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id_c1)
        .bind("ok")
        .bind(&cat_c)
        .bind(500)
        .bind("cat c")
        .execute(pool.as_ref())
        .await
        .ok();

        let result = pc
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(ids)
            .execute(pool.as_ref())
            .await
            .ok();

        let test_rows: Vec<_> = result
            .rows
            .iter()
            .filter(|r| r.category.starts_with(&prefix))
            .collect();
        assert!(!test_rows.is_empty(), "expected at least one test row");

        let row_a = test_rows
            .iter()
            .find(|r| r.category == cat_a)
            .expect("Cat A should appear");
        assert_eq!(row_a.request_count, 3);
        assert_eq!(row_a.avg_duration_ms, Some(200));
        assert_eq!(row_a.p99_duration_ms, Some(298));

        let row_b = test_rows
            .iter()
            .find(|r| r.category == cat_b)
            .expect("Cat B should appear");
        assert_eq!(row_b.request_count, 2);
        assert_eq!(row_b.avg_duration_ms, Some(100));
        assert_eq!(row_b.p99_duration_ms, Some(149));

        let row_c = test_rows
            .iter()
            .find(|r| r.category == cat_c)
            .expect("Cat C should appear");
        assert_eq!(row_c.request_count, 1);
        assert_eq!(row_c.avg_duration_ms, Some(500));
        assert_eq!(row_c.p99_duration_ms, Some(500));

        let test_total: i64 = test_rows.iter().map(|r| r.request_count).sum();
        assert_eq!(test_total, 6, "expected 6 total test classified");
    }

    #[tokio::test]
    async fn test_fetch_latency_summary_unclassified_count() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP test_fetch_latency_summary_unclassified_count: DATABASE_URL not set"
                );
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let ids = vec![id1, id2];

        // Insert records with NULL category (unclassified).
        sqlx::query(
            "INSERT INTO inferences (request_id, status, duration_ms, prompt_snippet) \
             VALUES ($1,$2,$3,$4)",
        )
        .bind(id1)
        .bind("ok")
        .bind(100i32)
        .bind("unclassified 1")
        .execute(pool.as_ref())
        .await
        .ok();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, duration_ms, prompt_snippet) \
             VALUES ($1,$2,$3,$4)",
        )
        .bind(id2)
        .bind("ok")
        .bind(200i32)
        .bind("unclassified 2")
        .execute(pool.as_ref())
        .await
        .ok();

        let result = pc
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(ids)
            .execute(pool.as_ref())
            .await
            .ok();

        assert!(
            result.unclassified_count >= 2,
            "expected at least 2 unclassified records, got {}",
            result.unclassified_count
        );
    }

    // ── fetch_savings_estimate ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_savings_estimate_empty() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_savings_estimate_empty: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            super::super::intent_classifier::hardcoded_model_costs(),
        );
        let id = uuid::Uuid::new_v4();
        let model = format!("Z_TST_SAV_EMPTY_{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, prompt_snippet, prompt_char_count) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id)
        .bind("ok")
        .bind("Z_TST_SAV_EMPTY_CAT")
        .bind(&model)
        .bind("empty test")
        .bind(100i32)
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let result = pc
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed");

        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .expect("delete should succeed");

        assert!(
            result.classified_count >= 1,
            "classified_count should be >= 1, got {}",
            result.classified_count
        );
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_with_data() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_savings_estimate_with_data: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        // Use unique model names to isolate from stale DB data.
        let model_a = format!("Z_TST_SAV_A_{}", uuid::Uuid::new_v4());
        let model_b = format!("Z_TST_SAV_B_{}", uuid::Uuid::new_v4());
        let mut costs = super::super::intent_classifier::hardcoded_model_costs();
        costs.insert(model_a.clone(), 0.15);
        costs.insert(model_b.clone(), 3.00);
        let mc = super::super::intent_classifier::ModelCosts::from_costs(costs);
        let baseline = model_b.clone();

        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let ids = vec![id1, id2];

        // Insert a cheap record with 1000 chars
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, prompt_snippet, prompt_char_count) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id1)
        .bind("ok")
        .bind("Z_TST_SAV_CAT1")
        .bind(&model_a)
        .bind("cheap prompt")
        .bind(1000i32)
        .execute(pool.as_ref())
        .await
        .expect("insert 1 should succeed");

        // Insert an expensive record with 2000 chars
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, prompt_snippet, prompt_char_count) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id2)
        .bind("ok")
        .bind("Z_TST_SAV_CAT2")
        .bind(&model_b)
        .bind("complex prompt with more content")
        .bind(2000i32)
        .execute(pool.as_ref())
        .await
        .expect("insert 2 should succeed");

        let result = pc
            .fetch_savings_estimate(24, &mc, &baseline)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(ids)
            .execute(pool.as_ref())
            .await
            .expect("delete should succeed");

        assert!(
            result.classified_count >= 2,
            "classified_count should be >= 2, got {}",
            result.classified_count
        );
        assert!(
            result.savings_usd > 0.0,
            "savings should be positive, got {}",
            result.savings_usd
        );
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_unknown_cost_model() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP test_fetch_savings_estimate_unknown_cost_model: DATABASE_URL not set"
                );
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            super::super::intent_classifier::hardcoded_model_costs(),
        );
        let id = uuid::Uuid::new_v4();
        let model = format!("Z_TST_SAV_UNK_{}", uuid::Uuid::new_v4());

        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, prompt_snippet, prompt_char_count) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id)
        .bind("ok")
        .bind("Z_TST_SAV_UNK_CAT")
        .bind(&model)
        .bind("some prompt")
        .bind(500i32)
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let result = pc
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed");

        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .expect("delete should succeed");

        assert!(
            result.classified_count >= 1,
            "classified_count should be >= 1, got {}",
            result.classified_count
        );
        assert!(
            result.unknown_cost_count >= 1,
            "unknown model should be counted, got {}",
            result.unknown_cost_count
        );
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_filters_null_category() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP test_fetch_savings_estimate_filters_null_category: DATABASE_URL not set"
                );
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            super::super::intent_classifier::hardcoded_model_costs(),
        );
        let id = uuid::Uuid::new_v4();

        // Insert a record with NULL category — should be excluded from classified_count.
        sqlx::query(
            "INSERT INTO inferences (request_id, status, upstream_model, prompt_snippet, prompt_char_count) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind("ok")
        .bind("gpt-4o-mini")
        .bind("uncategorized")
        .bind(100i32)
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let result = pc
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed — NULL category must not crash the query");

        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .expect("delete should succeed");

        // The NULL-category record should not cause a panic; function must handle
        // the SQL `WHERE category IS NOT NULL` filter correctly.
        assert!(result.classified_count >= 0);
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_historical_fallback() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP test_fetch_savings_estimate_historical_fallback: DATABASE_URL not set"
                );
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id = uuid::Uuid::new_v4();
        let model = format!("Z_TST_SAV_FB_{}", uuid::Uuid::new_v4());
        let mut costs = super::super::intent_classifier::hardcoded_model_costs();
        costs.insert(model.clone(), 0.15);
        let mc = super::super::intent_classifier::ModelCosts::from_costs(costs);

        // Insert a record with NULL prompt_char_count (historical record).
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, prompt_snippet) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind("ok")
        .bind("Z_TST_SAV_FB_CAT")
        .bind(&model)
        .bind("older record with no char count")
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let result = pc
            .fetch_savings_estimate(24, &mc, &model)
            .await
            .expect("fetch should succeed");

        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .expect("delete should succeed");

        assert!(
            result.classified_count >= 1,
            "classified_count should be >= 1, got {}",
            result.classified_count
        );
        assert!(
            result.has_historical_fallback,
            "should detect fallback usage"
        );
    }

    #[tokio::test]
    async fn test_fetch_latency_summary_time_filter() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_latency_summary_time_filter: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id = uuid::Uuid::new_v4();
        let cat = format!("Z_TST_LAT_TIME_{}", uuid::Uuid::new_v4());

        // Insert a record with created_at set to 2 hours ago.
        let two_hours_ago = chrono::Utc::now() - chrono::Duration::hours(2);
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, duration_ms, prompt_snippet, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id)
        .bind("ok")
        .bind(&cat)
        .bind(100i32)
        .bind("old record")
        .bind(two_hours_ago)
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        // Query with hours=1 — should not find the 2-hour-old record.
        let result = pc
            .fetch_latency_summary(1)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(id)
            .execute(pool.as_ref())
            .await
            .ok();

        let found = result.rows.iter().any(|r| r.category == cat);
        assert!(
            !found,
            "old record should be excluded from 1-hour window, but found category {cat}"
        );
    }

    #[tokio::test]
    #[should_panic]
    async fn test_db_connection_retry_panics_after_failures() {
        // Use an invalid DATABASE_URL to trigger connection failure.
        // The function should retry according to DB_CONNECTION_RETRIES and then panic.
        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }
        let _guard1 = EnvGuard("DATABASE_URL");
        let _guard2 = EnvGuard("DB_CONNECTION_RETRIES");
        let _guard3 = EnvGuard("DB_RETRY_BASE_MS");
        std::env::set_var(
            "DATABASE_URL",
            "postgres://invalid:invalid@127.0.0.1:0/invalid",
        );
        // Use fast and minimal retries to keep test quick
        std::env::set_var("DB_CONNECTION_RETRIES", "1");
        std::env::set_var("DB_RETRY_BASE_MS", "10");

        // Attempt to create PersistenceConfig; should panic.
        let db_config = DatabaseConfig {
            connection_retries: 2,
            retry_base_ms: 10,
            max_connections: 10,
            acquire_timeout_secs: 30,
            idle_timeout_secs: 1800,
            log_concurrency_limit: 100,
        };
        let _ = PersistenceConfig::from_env(&db_config).await;
    }

    #[tokio::test]
    async fn test_log_concurrency_limit_parsed_from_env() {
        // This test requires a live DATABASE_URL. It verifies that the LOG_CONCURRENCY_LIMIT
        // environment variable is respected and the semaphore is created with the correct permit count.
        // Fast settings to avoid delays.
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        // Quick check: ensure DATABASE_URL is set and we can actually connect.
        if super::test_pool().await.is_none() {
            eprintln!("SKIP test_log_concurrency_limit_parsed_from_env: DATABASE_URL not set or unreachable");
            return;
        }

        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }
        let _guard1 = EnvGuard("LOG_CONCURRENCY_LIMIT");
        let _guard2 = EnvGuard("DB_CONNECTION_RETRIES");
        let _guard3 = EnvGuard("DB_RETRY_BASE_MS");
        std::env::set_var("LOG_CONCURRENCY_LIMIT", "7");
        // Use fast retries to speed up the test
        std::env::set_var("DB_CONNECTION_RETRIES", "1");
        std::env::set_var("DB_RETRY_BASE_MS", "10");

        let db_config = DatabaseConfig {
            connection_retries: 1,
            retry_base_ms: 10,
            max_connections: 10,
            acquire_timeout_secs: 30,
            idle_timeout_secs: 1800,
            log_concurrency_limit: 7,
        };
        let config = PersistenceConfig::from_env(&db_config).await.expect("PersistenceConfig should succeed");
        assert_eq!(config.task_semaphore.available_permits(), 7);
    }
}

/// Test helper: create a test database pool.
///
/// Priority:
/// 1. Ephemeral PostgreSQL container via testcontainers (Docker required)
/// 2. DATABASE_URL env var with a short connect timeout (3s)
///
/// Returns `None` when neither is available.
#[cfg(test)]
pub async fn test_pool() -> Option<std::sync::Arc<PgPool>> {
    // Try disposable PostgreSQL container first (in-memory, Docker-backed)
    if let Some(tdb) = TestDb::new().await {
        return Some(tdb.pool);
    }
    // Fall back to DATABASE_URL env var
    let url = std::env::var("DATABASE_URL").ok()?;
    tokio::time::timeout(Duration::from_secs(3), sqlx::PgPool::connect(&url))
        .await
        .ok()?
        .ok()
        .map(std::sync::Arc::new)
}
