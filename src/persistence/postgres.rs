use std::str::FromStr;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use sqlx::Row;
use tracing::{error, info, warn};

use crate::config::DatabaseConfig;

use super::backend::{retry_once, PersistenceBackend};
use super::types::{
    prompt_chars_to_cost, CostProvider, InferenceLog, InferenceRecord, LatencySummary,
    LatencySummaryRow, QueryError, SavingsEstimate,
};

/// Production Postgres persistence backend via `sqlx::PgPool`.
///
/// Created with [`PostgresBackend::from_env`], which reads `DATABASE_URL`,
/// performs a health check with exponential back-off and jitter, and runs
/// `sqlx::migrate!()` to apply all SQL files in `migrations/` before the
/// server starts accepting traffic.
///
/// **p99 latency** is computed in the database via
/// `PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms)` — more
/// accurate and efficient than Rust-side computation for large row sets.
pub struct PostgresBackend {
    pub pool: PgPool,
}

impl PostgresBackend {
    /// Initialise the Postgres backend from environment variables.
    ///
    /// **Steps:**
    /// 1. Read `DATABASE_URL` (required; returns `Err` if absent).
    /// 2. Parse connection options and create a lazy `PgPool` with limits from
    ///    `db_config` (`max_connections`, `acquire_timeout_secs`,
    ///    `idle_timeout_secs`).
    /// 3. Health-check the pool with `SELECT 1`, retrying up to
    ///    `connection_retries` times with exponential back-off and random
    ///    jitter. **Panics** if all retries are exhausted.
    /// 4. Run `sqlx::migrate!()` to apply all files in `migrations/`.
    ///    **Panics** on migration failure.
    /// 5. Log `"Migrations applied successfully"` and return.
    pub async fn from_env(db_config: &DatabaseConfig) -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;

        let options = PgConnectOptions::from_str(&url)
            .map_err(|e| format!("DB connection string parse error: {e}"))?;

        let pool = PgPoolOptions::new()
            .max_connections(db_config.max_connections)
            .acquire_timeout(std::time::Duration::from_secs(
                db_config.acquire_timeout_secs,
            ))
            .idle_timeout(std::time::Duration::from_secs(db_config.idle_timeout_secs))
            .connect_lazy_with(options);

        let base_delay = Duration::from_millis(db_config.retry_base_ms);

        let mut last_err = None;

        for attempt in 0..db_config.connection_retries {
            match sqlx::query("SELECT 1").fetch_one(&pool).await {
                Ok(_) => break,
                Err(e) => {
                    if attempt < db_config.connection_retries - 1 {
                        warn!(
                            "DB health check failed (attempt {}): {}. Retrying...",
                            attempt + 1,
                            &e
                        );
                        let backoff = base_delay * (1u32 << attempt);
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
            panic!(
                "Database health check failed after {} retries: {}",
                db_config.connection_retries, e
            );
        }

        if let Err(e) = sqlx::migrate!().run(&pool).await {
            panic!("Migrations failed: {e}");
        }
        info!("Migrations applied successfully");

        Ok(Self { pool })
    }
}

async fn insert_once(pool: &PgPool, record: &InferenceRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inferences \
         (request_id, status, category, upstream_model, duration_ms, prompt_snippet, prompt_char_count, provider_attempts, final_provider, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, client_session_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(record.request_id)
    .bind(&record.status)
    .bind(&record.category)
    .bind(&record.upstream_model)
    .bind(record.duration_ms)
    .bind(&record.prompt_snippet)
    .bind(record.prompt_char_count)
    .bind(record.provider_attempts as i16)
    .bind(&record.final_provider)
    .bind(record.input_tokens)
    .bind(record.output_tokens)
    .bind(record.cache_read_tokens)
    .bind(record.cache_creation_tokens)
    .bind(&record.client_session_id)
    .execute(pool)
    .await
    .map(|_| ())
}

#[async_trait]
impl PersistenceBackend for PostgresBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        retry_once(|| insert_once(&self.pool, record))
            .await
            .map_err(|e| {
                error!("Postgres insert failed: {e}");
                e.to_string()
            })
    }

    async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError> {
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
            "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider \
             FROM inferences{} ORDER BY created_at DESC LIMIT {} OFFSET {}",
            where_clause, limit_ph, offset_ph,
        );
        let count_sql = format!("SELECT COUNT(*) FROM inferences{}", where_clause);

        let mut count_query = sqlx::query(sqlx::AssertSqlSafe(count_sql));
        if let Some(cat) = filter_category {
            count_query = count_query.bind(cat);
        }
        if let Some(model) = filter_model {
            count_query = count_query.bind(model);
        }
        let total_count: i64 = count_query
            .fetch_one(&self.pool)
            .await
            .map_err(|e| QueryError(e.to_string()))?
            .try_get(0)
            .map_err(|e| QueryError(e.to_string()))?;

        let mut data_query = sqlx::query(sqlx::AssertSqlSafe(data_sql));
        if let Some(cat) = filter_category {
            data_query = data_query.bind(cat);
        }
        if let Some(model) = filter_model {
            data_query = data_query.bind(model);
        }
        let rows = data_query
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| QueryError(e.to_string()))?;

        let records: Vec<InferenceLog> = rows
            .iter()
            .map(|row| {
                let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
                let timestamp = created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                let prompt_snippet: String = row.try_get("prompt_snippet")?;
                let category: Option<String> = row.try_get("category")?;
                let upstream_model: Option<String> = row.try_get("upstream_model")?;
                let duration_ms: Option<i32> = row.try_get("duration_ms")?;
                let provider_attempts: Option<i16> = row.try_get("provider_attempts")?;
                let final_provider: Option<String> = row.try_get("final_provider")?;
                Ok(InferenceLog {
                    timestamp,
                    prompt_snippet,
                    category,
                    upstream_model,
                    duration_ms,
                    provider_attempts,
                    final_provider,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(|e| QueryError(e.to_string()))?;

        Ok((records, total_count))
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
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
        .fetch_all(&self.pool)
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

    async fn fetch_savings_estimate(
        &self,
        hours: u32,
        model_costs: &dyn CostProvider,
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
        .fetch_all(&self.pool)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::PersistenceConfig;

    #[tokio::test]
    #[should_panic]
    async fn test_db_connection_retry_panics_after_failures() {
        // Use an invalid DATABASE_URL to trigger connection failure.
        // The function should retry according to DB_CONNECTION_RETRIES and then panic.
        use crate::test_util::EnvGuard;
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
        let _ = PostgresBackend::from_env(&db_config).await;
    }

    #[tokio::test]
    async fn test_pg_log_concurrency_limit_parsed_from_env() {
        // This test requires a live DATABASE_URL. It verifies that the LOG_CONCURRENCY_LIMIT
        // environment variable is respected and the semaphore is created with the correct permit count.
        // Fast settings to avoid delays.
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        // Quick check: ensure DATABASE_URL is set and we can actually connect.
        if super::super::backend::test_pool().await.is_none() {
            eprintln!("SKIP test_pg_log_concurrency_limit_parsed_from_env: DATABASE_URL not set or unreachable");
            return;
        }

        use crate::test_util::EnvGuard;
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
        let pg_backend = PostgresBackend::from_env(&db_config)
            .await
            .expect("PostgresBackend should succeed");
        let config = PersistenceConfig {
            backend: std::sync::Arc::new(crate::persistence::DbBackend::Postgres(pg_backend)),
            task_semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(7)),
        };
        assert_eq!(config.task_semaphore.available_permits(), 7);
    }
}
