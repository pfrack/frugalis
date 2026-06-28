use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::Row;
use sqlx::SqlitePool;
use tracing::error;

use super::backend::{percentile_99, retry_once, PersistenceBackend};
use super::types::{
    prompt_chars_to_cost, CostProvider, InferenceLog, InferenceRecord, LatencySummary,
    LatencySummaryRow, QueryError, SavingsEstimate,
};

/// File-backed (or in-memory) SQLite persistence backend via `sqlx::SqlitePool`.
///
/// The schema is created idempotently on construction via
/// `CREATE TABLE IF NOT EXISTS`. Missing columns from later migrations are
/// detected with `PRAGMA table_info` and added via `ALTER TABLE … ADD COLUMN`
/// so existing databases are upgraded automatically without a separate
/// migration tool.
///
/// **p99 latency** is computed in Rust via [`percentile_99`] because SQLite
/// does not have a native `PERCENTILE_CONT` function.
pub struct SqliteBackend {
    pub pool: SqlitePool,
}

impl SqliteBackend {
    /// Create a `SqliteBackend` from a file path.
    ///
    /// The string `":memory:"` is treated specially: instead of a plain
    /// in-memory database (which would be invisible to other connections),
    /// it maps to a **shared-cache** URI
    /// (`sqlite:file:frugalis?mode=memory&cache=shared`) so that multiple pool
    /// connections within the same process share the same data — important for
    /// tests that open the backend and then query it through a separate pool.
    pub async fn from_path(path: &str) -> Result<Self, String> {
        let uri = if path == ":memory:" {
            "sqlite:file:frugalis?mode=memory&cache=shared".to_string()
        } else {
            format!("sqlite:{path}?mode=rwc")
        };
        Self::from_uri(&uri).await
    }

    /// Create a `SqliteBackend` from an arbitrary SQLite URI.
    ///
    /// Opens the pool (max 1 connection, min 1 to keep the file alive), then
    /// calls `init_schema()` to create the table and run column migrations.
    /// Returns `Err` if the pool cannot be opened or any DDL statement fails.
    pub async fn from_uri(uri: &str) -> Result<Self, String> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .connect(uri)
            .await
            .map_err(|e| format!("failed to create SQLite pool: {e}"))?;
        let backend = Self { pool };
        backend.init_schema().await?;
        Ok(backend)
    }

    async fn init_schema(&self) -> Result<(), String> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS inferences (\
             request_id TEXT PRIMARY KEY, \
             status TEXT NOT NULL, \
             category TEXT, \
             upstream_model TEXT, \
             duration_ms INTEGER, \
             prompt_snippet TEXT NOT NULL, \
             prompt_char_count INTEGER, \
             created_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| format!("failed to initialize SQLite schema: {e}"))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_inferences_created_at \
             ON inferences(created_at)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| format!("failed to create SQLite index: {e}"))?;

        // Migration: add provider_attempts and final_provider columns if missing.
        // Uses PRAGMA table_info to check column existence instead of matching
        // error strings, which are not part of SQLite's stable API contract.
        {
            let cols: Vec<String> = sqlx::query("PRAGMA table_info(inferences)")
                .fetch_all(&self.pool)
                .await
                .map(|rows| rows.iter().map(|r| r.get::<String, _>("name")).collect())
                .unwrap_or_default();
            for (col, typ) in [
                ("provider_attempts", "SMALLINT DEFAULT 1"),
                ("final_provider", "TEXT"),
                // Migration 005: token usage + Claude Code attribution. All
                // nullable so existing rows stay valid. Mirrors the Postgres
                // migration 005_add_token_and_attribution_columns.sql.
                ("input_tokens", "INTEGER"),
                ("output_tokens", "INTEGER"),
                ("cache_read_tokens", "INTEGER"),
                ("cache_creation_tokens", "INTEGER"),
                ("client_session_id", "TEXT"),
            ] {
                if cols.iter().any(|c| c == col) {
                    continue;
                }
                let sql = format!("ALTER TABLE inferences ADD COLUMN {} {}", col, typ);
                sqlx::query(&sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| format!("failed to add column {}: {}", col, e))?;
            }
        }

        Ok(())
    }
}

async fn insert_once_sqlite(
    pool: &SqlitePool,
    record: &InferenceRecord,
) -> Result<(), sqlx::Error> {
    // Note: `created_at` is omitted and SQLite will use its default CURRENT_TIMESTAMP.
    sqlx::query(
        "INSERT INTO inferences \
         (request_id, status, category, upstream_model, duration_ms, prompt_snippet, prompt_char_count, provider_attempts, final_provider, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, client_session_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )
    .bind(record.request_id.to_string())
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

// NOTE: The dynamic WHERE clause building is intentionally duplicated between SqliteBackend
// (uses `?` positional placeholders) and PostgresBackend (uses `$N` numbered placeholders).
// The plan explicitly chose no ORM and no sqlx::Any, so dialect-specific bind syntax requires
// separate implementations.
#[async_trait]
impl PersistenceBackend for SqliteBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        retry_once(|| insert_once_sqlite(&self.pool, record))
            .await
            .map_err(|e| {
                error!("SQLite insert failed: {e}");
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
        let mut where_clause = String::new();
        let mut has_where = false;

        if filter_category.is_some() {
            where_clause.push_str("category = ? ");
            has_where = true;
        }
        if filter_model.is_some() {
            if has_where {
                where_clause.push_str("AND ");
            }
            where_clause.push_str("upstream_model = ? ");
            has_where = true;
        }
        let where_clause = if has_where {
            format!(" WHERE {}", where_clause.trim_end())
        } else {
            String::new()
        };

        let count_sql = format!("SELECT COUNT(*) FROM inferences{}", where_clause);
        let data_sql = format!(
            "SELECT created_at, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider \
             FROM inferences{} ORDER BY created_at DESC LIMIT ? OFFSET ?",
            where_clause,
        );

        // Execute count query.
        let mut count_query = sqlx::query(&count_sql);
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
            .fetch_all(&self.pool)
            .await
            .map_err(|e| QueryError(e.to_string()))?;

        let records: Vec<InferenceLog> = rows
            .iter()
            .map(|row| {
                let created_at: String = row.try_get("created_at")?;
                let timestamp = created_at;
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
        // Fetch grouped aggregation for non-null categories.
        let rows = sqlx::query(
            "SELECT category, \
             COUNT(*) AS count, \
             ROUND(AVG(duration_ms)) AS avg_duration_ms \
             FROM inferences \
             WHERE created_at >= datetime('now', '-' || CAST(? AS TEXT) || ' hours') \
             AND category IS NOT NULL \
             GROUP BY category \
             ORDER BY count DESC",
        )
        .bind(hours as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| QueryError(e.to_string()))?;

        let mut summary_rows = Vec::new();

        for row in &rows {
            let category: Option<String> = row.try_get("category").unwrap_or(None);
            let request_count: i64 = row.try_get("count").unwrap_or(0);
            let avg_duration_ms: Option<i32> = row.try_get("avg_duration_ms").unwrap_or(None);

            if let Some(cat) = category {
                summary_rows.push(LatencySummaryRow {
                    category: cat,
                    request_count,
                    avg_duration_ms,
                    p99_duration_ms: None,
                });
            }
        }

        // Compute p99 per category by fetching sorted durations in Rust.
        let duration_rows = sqlx::query(
            "SELECT category, duration_ms FROM inferences \
             WHERE created_at >= datetime('now', '-' || CAST(? AS TEXT) || ' hours') \
             AND category IS NOT NULL AND duration_ms IS NOT NULL \
             ORDER BY category, duration_ms ASC",
        )
        .bind(hours as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| QueryError(e.to_string()))?;

        let mut p99_groups: std::collections::HashMap<String, Vec<i32>> =
            std::collections::HashMap::new();
        for row in &duration_rows {
            let cat: String = row.try_get("category").unwrap_or_default();
            let dur: i32 = row.try_get("duration_ms").unwrap_or(0);
            p99_groups.entry(cat).or_default().push(dur);
        }

        for row in &mut summary_rows {
            if let Some(durations) = p99_groups.remove(&row.category) {
                row.p99_duration_ms = percentile_99(&durations);
            }
        }

        // Count unclassified (NULL category) records.
        let unclassified: i64 = sqlx::query(
            "SELECT COUNT(*) FROM inferences \
             WHERE created_at >= datetime('now', '-' || CAST(? AS TEXT) || ' hours') \
             AND category IS NULL",
        )
        .bind(hours as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| QueryError(e.to_string()))?
        .try_get(0)
        .map_err(|e| QueryError(e.to_string()))?;

        let total_classified_count: i64 = summary_rows.iter().map(|r| r.request_count).sum();

        Ok(LatencySummary {
            rows: summary_rows,
            unclassified_count: unclassified,
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
             COUNT(*) AS count, \
             COALESCE(SUM(prompt_char_count), 0) AS total_chars, \
             COALESCE(SUM(LENGTH(prompt_snippet)), 0) AS total_fallback_chars, \
             COALESCE(SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END), 0) \
             AS fallback_count \
             FROM inferences \
             WHERE created_at >= datetime('now', '-' || CAST(? AS TEXT) || ' hours') \
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
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    use crate::persistence::{DbBackend, InferenceRecord, PersistenceConfig};

    /// Create a SQLite in-memory backend for SQLite-specific tests.
    /// Each invocation uses a unique shared-cache URI for isolation.
    async fn test_sqlite_backend() -> PersistenceConfig {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let uri = format!(
            "sqlite:file:test_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        let backend = SqliteBackend::from_uri(&uri)
            .await
            .expect("test SQLite backend setup failed");
        PersistenceConfig {
            backend: Arc::new(DbBackend::Sqlite(backend)),
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

    #[tokio::test]
    async fn test_sqlite_schema_init() {
        let pc = test_sqlite_backend().await;
        // Verify the table exists by running a query.
        let row: Result<sqlx::sqlite::SqliteRow, sqlx::Error> =
            sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name='inferences'")
                .fetch_one(match &*pc.backend {
                    DbBackend::Sqlite(b) => &b.pool,
                    _ => panic!("expected SQLite backend"),
                })
                .await;
        assert!(row.is_ok(), "inferences table should exist");

        let idx: Result<sqlx::sqlite::SqliteRow, sqlx::Error> = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_inferences_created_at'"
        )
        .fetch_one(match &*pc.backend {
            DbBackend::Sqlite(b) => &b.pool,
            _ => panic!("expected SQLite backend"),
        })
        .await;
        assert!(idx.is_ok(), "idx_inferences_created_at index should exist");
    }

    #[tokio::test]
    async fn test_sqlite_insert_and_fetch() {
        let pc = test_sqlite_backend().await;
        let request_id = uuid::Uuid::new_v4();
        let record = InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("SQLITE_TEST".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(42),
            prompt_snippet: "sqlite test snippet".to_string(),
            prompt_char_count: Some(100),
            created_at: chrono::Utc::now(),
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
        };
        pc.backend
            .insert_inference(&record)
            .await
            .expect("insert should succeed");

        let (records, count) = pc
            .backend
            .fetch_inferences(0, 10, Some("SQLITE_TEST"), None)
            .await
            .expect("fetch should succeed");

        assert_eq!(count, 1, "expected 1 record");
        assert_eq!(records[0].prompt_snippet, "sqlite test snippet");
        assert_eq!(records[0].duration_ms, Some(42));
    }

    #[tokio::test]
    async fn test_sqlite_isolation() {
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let uri1 = format!("sqlite:file:test_{id1}?mode=memory&cache=shared");
        let uri2 = format!("sqlite:file:test_{id2}?mode=memory&cache=shared");
        let backend1 = SqliteBackend::from_uri(&uri1)
            .await
            .expect("test backend1 setup failed");
        let backend2 = SqliteBackend::from_uri(&uri2)
            .await
            .expect("test backend2 setup failed");
        let pc1 = PersistenceConfig {
            backend: Arc::new(DbBackend::Sqlite(backend1)),
            task_semaphore: Arc::new(Semaphore::new(100)),
        };
        let pc2 = PersistenceConfig {
            backend: Arc::new(DbBackend::Sqlite(backend2)),
            task_semaphore: Arc::new(Semaphore::new(100)),
        };

        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("ISO_TEST".to_string()),
            upstream_model: None,
            duration_ms: Some(10),
            prompt_snippet: "isolated".to_string(),
            prompt_char_count: None,
            created_at: chrono::Utc::now(),
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
        };
        pc1.backend.insert_inference(&record).await.expect("insert");

        // pc2 should have no records.
        let (records, count) = pc2
            .backend
            .fetch_inferences(0, 10, Some("ISO_TEST"), None)
            .await
            .expect("fetch should succeed");
        assert_eq!(count, 0, "pc2 should be empty");
        assert!(records.is_empty(), "pc2 should have no records");
    }

    #[tokio::test]
    async fn test_log_inference_integration() {
        use crate::persistence::log_inference;
        let pc = test_sqlite_backend().await;
        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("LOG_TEST".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(50),
            prompt_snippet: "log inference test".to_string(),
            prompt_char_count: Some(25),
            created_at: chrono::Utc::now(),
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
        };

        let handle = log_inference(pc.backend.clone(), pc.task_semaphore.clone(), record);
        handle.await.expect("logging task should complete");

        // Read back.
        let (records, _) = pc
            .backend
            .fetch_inferences(0, 10, Some("LOG_TEST"), None)
            .await
            .expect("fetch should succeed");
        assert_eq!(records.len(), 1, "should have logged 1 record");
        assert_eq!(records[0].prompt_snippet, "log inference test");
    }
}
