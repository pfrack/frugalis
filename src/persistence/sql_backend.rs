use async_trait::async_trait;
use sea_query::{Expr, ExprTrait, Iden, Query, SqliteQueryBuilder, PostgresQueryBuilder};
use sqlx::Row;
use tracing::error;

use super::backend::{percentile_99, retry_once, PersistenceBackend};
use super::types::{
    prompt_chars_to_cost, CostProvider, InferenceLog, InferenceRecord, LatencySummary,
    LatencySummaryRow, QueryError, SavingsEstimate,
};

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("./migrations");
}

/// Column identifiers for the `inferences` table.
#[derive(Iden)]
enum Inferences {
    Table,
    RequestId,
    Status,
    Category,
    UpstreamModel,
    DurationMs,
    PromptSnippet,
    PromptCharCount,
    CreatedAt,
    ProviderAttempts,
    FinalProvider,
    InputTokens,
    OutputTokens,
    CacheReadTokens,
    CacheCreationTokens,
    ClientSessionId,
}

/// Database dialect selector.
#[derive(Debug, Clone, Copy)]
pub enum Dialect {
    Postgres,
    Sqlite,
}

/// Pool wrapper that holds either a Postgres or SQLite pool.
pub enum Pool {
    Postgres(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

/// Unified SQL backend that uses sea-query for query building.
/// Supports both Postgres and SQLite via the `dialect` field.
pub struct SqlBackend {
    pub pool: Pool,
    pub dialect: Dialect,
}

impl SqlBackend {
    /// Create a SQLite in-memory backend for testing.
    pub async fn new_sqlite_in_memory() -> Result<Self, String> {
        let uri = format!("sqlite:file:sql_backend_test_{}?mode=memory&cache=shared", uuid::Uuid::new_v4());
        Self::connect(&uri).await
    }

    /// Connect to a database and create a SqlBackend.
    /// Determines dialect from the URL scheme and runs migrations via refinery.
    pub async fn connect(url: &str) -> Result<Self, String> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            // Run refinery migrations via tokio-postgres before creating the sqlx pool.
            let (mut client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
                .await
                .map_err(|e| format!("failed to connect to Postgres for migrations: {e}"))?;
            tokio::spawn(async move {
                if let Err(e) = connection.await {
                    tracing::error!("Postgres connection error: {e}");
                }
            });
            embedded::migrations::runner()
                .run_async(&mut client)
                .await
                .map_err(|e| format!("Postgres migration failed: {e}"))?;
            drop(client);

            let pool = sqlx::PgPool::connect(url)
                .await
                .map_err(|e| format!("failed to create Postgres pool: {e}"))?;
            Ok(Self {
                pool: Pool::Postgres(pool),
                dialect: Dialect::Postgres,
            })
        } else if url.starts_with("sqlite:") {
            // Run refinery migrations for file-based SQLite databases.
            if let Some(path) = Self::sqlite_path_from_url(url) {
                let mut conn = rusqlite::Connection::open(&path)
                    .map_err(|e| format!("failed to open SQLite DB: {e}"))?;
                embedded::migrations::runner()
                    .run(&mut conn)
                    .map_err(|e| format!("SQLite migration failed: {e}"))?;
            }

            let pool = sqlx::SqlitePool::connect(url)
                .await
                .map_err(|e| format!("failed to create SQLite pool: {e}"))?;
            let backend = Self {
                pool: Pool::Sqlite(pool),
                dialect: Dialect::Sqlite,
            };

            // For in-memory databases (cannot use rusqlite migrations), init schema directly.
            if !Self::is_file_based_sqlite(url) {
                backend.init_sqlite_schema().await?;
            }

            Ok(backend)
        } else {
            Err(format!("unsupported database URL scheme: {}", url))
        }
    }

    /// Extract the file path from a sqlite URL, or None for in-memory.
    fn sqlite_path_from_url(url: &str) -> Option<String> {
        let stripped = url.strip_prefix("sqlite:")?;
        let (before_query, query) = stripped.split_once('?').unwrap_or((stripped, ""));
        let path = before_query.strip_prefix("file:").unwrap_or(before_query);
        if path.is_empty() || path.contains("memory") || query.contains("mode=memory") {
            None
        } else {
            Some(path.to_string())
        }
    }

    /// Returns true if the SQLite URL refers to a file-backed database.
    fn is_file_based_sqlite(url: &str) -> bool {
        Self::sqlite_path_from_url(url).is_some()
    }

    /// Initialize SQLite schema (CREATE TABLE IF NOT EXISTS + migrations).
    async fn init_sqlite_schema(&self) -> Result<(), String> {
        let pool = match &self.pool {
            Pool::Sqlite(p) => p,
            _ => return Err("init_sqlite_schema called on non-SQLite backend".to_string()),
        };

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
        .execute(pool)
        .await
        .map_err(|e| format!("failed to initialize SQLite schema: {e}"))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_inferences_created_at \
             ON inferences(created_at)",
        )
        .execute(pool)
        .await
        .map_err(|e| format!("failed to create SQLite index: {e}"))?;

        // Migration: add provider_attempts and final_provider columns if missing.
        {
            let cols: Vec<String> = sqlx::query("PRAGMA table_info(inferences)")
                .fetch_all(pool)
                .await
                .map(|rows| rows.iter().map(|r| r.get::<String, _>("name")).collect())
                .unwrap_or_default();
            for (col, typ) in [
                ("provider_attempts", "SMALLINT DEFAULT 1"),
                ("final_provider", "TEXT"),
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
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .execute(pool)
                    .await
                    .map_err(|e| format!("failed to add column {}: {}", col, e))?;
            }
        }

        Ok(())
    }

    /// Execute a query that returns no rows (INSERT, UPDATE, DELETE).
    async fn execute_query(&self, sql: String) -> Result<(), sqlx::Error> {
        match &self.pool {
            Pool::Postgres(pool) => {
                sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
            }
            Pool::Sqlite(pool) => {
                sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
            }
        }
        Ok(())
    }

}

async fn insert_once_sql_backend(backend: &SqlBackend, record: &InferenceRecord) -> Result<(), sqlx::Error> {
    let sql = {
        let mut q = Query::insert();
        q.into_table(Inferences::Table)
            .columns([
                Inferences::RequestId,
                Inferences::Status,
                Inferences::Category,
                Inferences::UpstreamModel,
                Inferences::DurationMs,
                Inferences::PromptSnippet,
                Inferences::PromptCharCount,
                Inferences::ProviderAttempts,
                Inferences::FinalProvider,
                Inferences::InputTokens,
                Inferences::OutputTokens,
                Inferences::CacheReadTokens,
                Inferences::CacheCreationTokens,
                Inferences::ClientSessionId,
            ])
            .values_panic([
                record.request_id.to_string().into(),
                record.status.clone().into(),
                record.category.clone().into(),
                record.upstream_model.clone().into(),
                record.duration_ms.into(),
                record.prompt_snippet.clone().into(),
                record.prompt_char_count.into(),
                (record.provider_attempts as i16).into(),
                record.final_provider.clone().into(),
                record.input_tokens.into(),
                record.output_tokens.into(),
                record.cache_read_tokens.into(),
                record.cache_creation_tokens.into(),
                record.client_session_id.clone().into(),
            ]);
        match backend.dialect {
            Dialect::Postgres => q.to_string(PostgresQueryBuilder),
            Dialect::Sqlite => q.to_string(SqliteQueryBuilder),
        }
    };
    backend.execute_query(sql).await
}

#[async_trait]
impl PersistenceBackend for SqlBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        retry_once(|| insert_once_sql_backend(self, record))
            .await
            .map_err(|e| {
                error!("SQL insert failed: {e}");
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
        let count_sql = {
            let mut q = Query::select();
            q.expr(Expr::cust("COUNT(*)")).from(Inferences::Table);
            if let Some(cat) = filter_category {
                q.and_where(Expr::col(Inferences::Category).equals(cat.to_string()));
            }
            if let Some(model) = filter_model {
                q.and_where(Expr::col(Inferences::UpstreamModel).equals(model.to_string()));
            }
            match self.dialect {
                Dialect::Postgres => q.to_string(PostgresQueryBuilder),
                Dialect::Sqlite => q.to_string(SqliteQueryBuilder),
            }
        };
        let data_sql = {
            let mut q = Query::select();
            q.columns([
                Inferences::CreatedAt,
                Inferences::PromptSnippet,
                Inferences::Category,
                Inferences::UpstreamModel,
                Inferences::DurationMs,
                Inferences::ProviderAttempts,
                Inferences::FinalProvider,
            ])
            .from(Inferences::Table)
            .order_by(Inferences::CreatedAt, sea_query::Order::Desc)
            .limit(limit as u64)
            .offset(offset as u64);
            if let Some(cat) = filter_category {
                q.and_where(Expr::col(Inferences::Category).equals(cat.to_string()));
            }
            if let Some(model) = filter_model {
                q.and_where(Expr::col(Inferences::UpstreamModel).equals(model.to_string()));
            }
            match self.dialect {
                Dialect::Postgres => q.to_string(PostgresQueryBuilder),
                Dialect::Sqlite => q.to_string(SqliteQueryBuilder),
            }
        };

        match &self.pool {
            Pool::Sqlite(pool) => {
                let count_row = sqlx::query(sqlx::AssertSqlSafe(count_sql))
                    .fetch_one(pool).await.map_err(|e| QueryError(e.to_string()))?;
                let total_count: i64 = count_row.try_get(0).map_err(|e| QueryError(e.to_string()))?;

                let rows = sqlx::query(sqlx::AssertSqlSafe(data_sql))
                    .fetch_all(pool).await.map_err(|e| QueryError(e.to_string()))?;

                let records: Vec<InferenceLog> = rows.iter().map(|row| {
                    let timestamp: String = row.try_get("created_at")?;
                    let prompt_snippet: String = row.try_get("prompt_snippet")?;
                    let category: Option<String> = row.try_get("category")?;
                    let upstream_model: Option<String> = row.try_get("upstream_model")?;
                    let duration_ms: Option<i32> = row.try_get("duration_ms")?;
                    let provider_attempts: Option<i16> = row.try_get("provider_attempts")?;
                    let final_provider: Option<String> = row.try_get("final_provider")?;
                    Ok(InferenceLog { timestamp, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider })
                }).collect::<Result<Vec<_>, sqlx::Error>>().map_err(|e| QueryError(e.to_string()))?;

                Ok((records, total_count))
            }
            Pool::Postgres(pool) => {
                let count_row = sqlx::query(sqlx::AssertSqlSafe(count_sql))
                    .fetch_one(pool).await.map_err(|e| QueryError(e.to_string()))?;
                let total_count: i64 = count_row.try_get(0).map_err(|e| QueryError(e.to_string()))?;

                let rows = sqlx::query(sqlx::AssertSqlSafe(data_sql))
                    .fetch_all(pool).await.map_err(|e| QueryError(e.to_string()))?;

                let records: Vec<InferenceLog> = rows.iter().map(|row| {
                    let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
                    let timestamp = created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                    let prompt_snippet: String = row.try_get("prompt_snippet")?;
                    let category: Option<String> = row.try_get("category")?;
                    let upstream_model: Option<String> = row.try_get("upstream_model")?;
                    let duration_ms: Option<i32> = row.try_get("duration_ms")?;
                    let provider_attempts: Option<i16> = row.try_get("provider_attempts")?;
                    let final_provider: Option<String> = row.try_get("final_provider")?;
                    Ok(InferenceLog { timestamp, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider })
                }).collect::<Result<Vec<_>, sqlx::Error>>().map_err(|e| QueryError(e.to_string()))?;

                Ok((records, total_count))
            }
        }
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        match self.dialect {
            Dialect::Postgres => {
                let sql = {
                    let mut q = Query::select();
                    q.column(Inferences::Category)
                        .expr(Expr::cust("COUNT(*)::BIGINT AS count"))
                        .expr(Expr::cust("ROUND(AVG(duration_ms))::INTEGER AS avg_duration_ms"))
                        .expr(Expr::cust("ROUND(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms))::INTEGER AS p99_duration_ms"))
                        .from(Inferences::Table)
                        .and_where(Expr::cust(format!("created_at >= '{}'", cutoff_str)))
                        .add_group_by([Expr::col(Inferences::Category)])
                        .order_by_expr(Expr::cust("count"), sea_query::Order::Desc);
                    q.to_string(PostgresQueryBuilder)
                };

                let pool = match &self.pool {
                    Pool::Postgres(p) => p,
                    _ => return Err(QueryError("pool type mismatch for Postgres dialect".to_string())),
                };

                let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
                    .fetch_all(pool).await?;

                let mut summary_rows = Vec::new();
                let mut unclassified_count: i64 = 0;

                for row in &rows {
                    let category: Option<String> = row.try_get("category").unwrap_or(None);
                    let request_count: i64 = row.try_get("count").unwrap_or(0);

                    match category {
                        Some(cat) => {
                            let avg_duration_ms: Option<i32> = row.try_get("avg_duration_ms").unwrap_or(None);
                            let p99_duration_ms: Option<i32> = row.try_get("p99_duration_ms").unwrap_or(None);
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
            Dialect::Sqlite => {
                let pool = match &self.pool {
                    Pool::Sqlite(p) => p,
                    _ => return Err(QueryError("pool type mismatch for SQLite dialect".to_string())),
                };

                // Grouped aggregation query for non-null categories.
                let agg_sql = {
                    let mut q = Query::select();
                    q.column(Inferences::Category)
                        .expr(Expr::cust("COUNT(*) AS count"))
                        .expr(Expr::cust("CAST(ROUND(AVG(duration_ms)) AS INTEGER) AS avg_duration_ms"))
                        .from(Inferences::Table)
                        .and_where(Expr::cust(format!("created_at >= '{}'", cutoff_str)))
                        .and_where(Expr::col(Inferences::Category).is_not_null())
                        .add_group_by([Expr::col(Inferences::Category)])
                        .order_by_expr(Expr::cust("count"), sea_query::Order::Desc);
                    q.to_string(SqliteQueryBuilder)
                };

                let mut summary_rows: Vec<LatencySummaryRow> = Vec::new();

                let agg_rows = sqlx::query(sqlx::AssertSqlSafe(agg_sql))
                    .fetch_all(pool).await?;

                for row in &agg_rows {
                    let category: String = row.try_get("category").unwrap_or_default();
                    let request_count: i64 = row.try_get("count").unwrap_or(0);
                    let avg_duration_ms: Option<i32> = row.try_get("avg_duration_ms").unwrap_or(None);
                    summary_rows.push(LatencySummaryRow {
                        category,
                        request_count,
                        avg_duration_ms,
                        p99_duration_ms: None,
                    });
                }

                // Fetch raw durations per category for Rust-side p99.
                let dur_sql = {
                    let mut q = Query::select();
                    q.column(Inferences::Category)
                        .column(Inferences::DurationMs)
                        .from(Inferences::Table)
                        .and_where(Expr::cust(format!("created_at >= '{}'", cutoff_str)))
                        .and_where(Expr::col(Inferences::Category).is_not_null())
                        .and_where(Expr::col(Inferences::DurationMs).is_not_null())
                        .order_by(Inferences::Category, sea_query::Order::Asc)
                        .order_by(Inferences::DurationMs, sea_query::Order::Asc);
                    q.to_string(SqliteQueryBuilder)
                };

                let dur_rows = sqlx::query(sqlx::AssertSqlSafe(dur_sql))
                    .fetch_all(pool).await?;

                let mut p99_groups: std::collections::HashMap<String, Vec<i32>> = std::collections::HashMap::new();
                for row in &dur_rows {
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
                let unclassified_sql = {
                    let mut q = Query::select();
                    q.expr(Expr::cust("COUNT(*)"))
                        .from(Inferences::Table)
                        .and_where(Expr::cust(format!("created_at >= '{}'", cutoff_str)))
                        .and_where(Expr::col(Inferences::Category).is_null());
                    q.to_string(SqliteQueryBuilder)
                };

                let unclassified: i64 = sqlx::query(sqlx::AssertSqlSafe(unclassified_sql))
                    .fetch_one(pool).await?
                    .try_get(0)
                    .map_err(|e| QueryError(e.to_string()))?;

                let total_classified_count: i64 = summary_rows.iter().map(|r| r.request_count).sum();

                Ok(LatencySummary {
                    rows: summary_rows,
                    unclassified_count: unclassified,
                    total_classified_count,
                })
            }
        }
    }

    async fn fetch_savings_estimate(
        &self,
        hours: u32,
        model_costs: &dyn CostProvider,
        baseline_model: &str,
    ) -> Result<SavingsEstimate, QueryError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        fn build_sql(dialect: Dialect, cutoff_str: &str) -> String {
            let mut q = Query::select();
            q.column(Inferences::UpstreamModel)
                .expr(Expr::cust("COUNT(*) AS count"))
                .expr(Expr::cust("COALESCE(SUM(prompt_char_count), 0) AS total_chars"))
                .expr(Expr::cust("COALESCE(SUM(LENGTH(prompt_snippet)), 0) AS total_fallback_chars"))
                .expr(Expr::cust("COALESCE(SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END), 0) AS fallback_count"))
                .from(Inferences::Table)
                .and_where(Expr::cust(format!("created_at >= '{}'", cutoff_str)))
                .and_where(Expr::col(Inferences::Category).is_not_null())
                .and_where(Expr::col(Inferences::UpstreamModel).is_not_null())
                .add_group_by([Expr::col(Inferences::UpstreamModel)]);
            match dialect {
                Dialect::Postgres => q.to_string(PostgresQueryBuilder),
                Dialect::Sqlite => q.to_string(SqliteQueryBuilder),
            }
        }

        let rows = match &self.pool {
            Pool::Postgres(pool) => {
                let sql = build_sql(Dialect::Postgres, &cutoff_str);
                let r = sqlx::query(sqlx::AssertSqlSafe(sql))
                    .fetch_all(pool).await?;
                r.iter().map(|row| {
                    let model: String = row.try_get("upstream_model").unwrap_or_default();
                    let count: i64 = row.try_get("count").unwrap_or(0);
                    let total_chars: i64 = row.try_get("total_chars").unwrap_or(0);
                    let total_fallback_chars: i64 = row.try_get("total_fallback_chars").unwrap_or(0);
                    let fallback_count: i64 = row.try_get("fallback_count").unwrap_or(0);
                    (model, count, total_chars, total_fallback_chars, fallback_count)
                }).collect::<Vec<_>>()
            }
            Pool::Sqlite(pool) => {
                let sql = build_sql(Dialect::Sqlite, &cutoff_str);
                let r = sqlx::query(sqlx::AssertSqlSafe(sql))
                    .fetch_all(pool).await?;
                r.iter().map(|row| {
                    let model: String = row.try_get("upstream_model").unwrap_or_default();
                    let count: i64 = row.try_get("count").unwrap_or(0);
                    let total_chars: i64 = row.try_get("total_chars").unwrap_or(0);
                    let total_fallback_chars: i64 = row.try_get("total_fallback_chars").unwrap_or(0);
                    let fallback_count: i64 = row.try_get("fallback_count").unwrap_or(0);
                    (model, count, total_chars, total_fallback_chars, fallback_count)
                }).collect::<Vec<_>>()
            }
        };

        let mut total_actual_cost: f64 = 0.0;
        let mut total_chars_all: i64 = 0;
        let mut classified_count: i64 = 0;
        let mut unknown_cost_count: i64 = 0;
        let mut has_historical_fallback = false;

        for (model, count, total_chars, total_fallback_chars, fallback_count) in &rows {
            if *fallback_count > 0 {
                has_historical_fallback = true;
            }

            classified_count += count;

            let effective_chars = if *total_chars > 0 {
                *total_chars
            } else {
                *total_fallback_chars
            };
            total_chars_all += effective_chars;

            if let Some(cost) = model_costs.get_cost(model) {
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
    use crate::persistence::{DbBackend, InferenceRecord, PersistenceConfig};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    async fn test_sql_backend_config() -> PersistenceConfig {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let backend = SqlBackend::new_sqlite_in_memory()
            .await
            .expect("test SQLite backend setup failed");
        PersistenceConfig {
            backend: Arc::new(DbBackend::Sql(backend)),
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

    #[tokio::test]
    async fn test_sql_backend_insert_and_fetch() {
        let pc = test_sql_backend_config().await;
        let request_id = uuid::Uuid::new_v4();
        let record = InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("SQL_TEST".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(42),
            prompt_snippet: "sql backend test snippet".to_string(),
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
            .fetch_inferences(0, 10, Some("SQL_TEST"), None)
            .await
            .expect("fetch should succeed");

        assert_eq!(count, 1, "expected 1 record");
        assert_eq!(records[0].prompt_snippet, "sql backend test snippet");
        assert_eq!(records[0].duration_ms, Some(42));
    }

    #[tokio::test]
    async fn test_sql_backend_insert_and_fetch_with_model_filter() {
        let pc = test_sql_backend_config().await;
        let request_id = uuid::Uuid::new_v4();
        let record = InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("FILTER_TEST".to_string()),
            upstream_model: Some("gpt-4".to_string()),
            duration_ms: Some(50),
            prompt_snippet: "filter test snippet".to_string(),
            prompt_char_count: Some(80),
            created_at: chrono::Utc::now(),
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
        };
        pc.backend
            .insert_inference(&record)
            .await
            .expect("insert should succeed");

        // Filter by model
        let (records, count) = pc
            .backend
            .fetch_inferences(0, 10, None, Some("gpt-4"))
            .await
            .expect("fetch should succeed");

        assert_eq!(count, 1, "expected 1 record");
        assert_eq!(records[0].prompt_snippet, "filter test snippet");

        // Filter by wrong model - should return 0
        let (records, count) = pc
            .backend
            .fetch_inferences(0, 10, None, Some("gpt-3.5"))
            .await
            .expect("fetch should succeed");

        assert_eq!(count, 0, "expected 0 records");
        assert!(records.is_empty(), "expected no records");
    }

    #[tokio::test]
    async fn test_sql_backend_no_filters() {
        let pc = test_sql_backend_config().await;
        
        // Insert 3 records
        for i in 0..3 {
            let record = InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some(format!("CAT_{}", i)),
                upstream_model: Some("model".to_string()),
                duration_ms: Some(10 * i as i32),
                prompt_snippet: format!("snippet {}", i),
                prompt_char_count: Some(50),
                created_at: chrono::Utc::now(),
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
            };
            pc.backend.insert_inference(&record).await.expect("insert");
        }

        // Fetch all
        let (records, count) = pc
            .backend
            .fetch_inferences(0, 10, None, None)
            .await
            .expect("fetch should succeed");

        assert_eq!(count, 3, "expected 3 records");
        assert_eq!(records.len(), 3, "expected 3 records in vec");
    }

    #[tokio::test]
    async fn test_sql_backend_latency_summary() {
        let pc = test_sql_backend_config().await;
        let cat = format!("Z_LAT_SQL_{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now();

        for dur in [100, 200, 300] {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: Some(cat.clone()),
                    upstream_model: None,
                    duration_ms: Some(dur),
                    prompt_snippet: "latency test".to_string(),
                    prompt_char_count: None,
                    created_at: now,
                    final_provider: String::new(),
                    provider_attempts: 1,
                    ..Default::default()
                })
                .await
                .expect("insert");
        }

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch_latency_summary should succeed");

        let row = result
            .rows
            .iter()
            .find(|r| r.category == cat)
            .expect("test category should appear");

        assert_eq!(row.request_count, 3);
        assert_eq!(row.avg_duration_ms, Some(200));
        assert_eq!(row.p99_duration_ms, Some(300));
    }

    #[tokio::test]
    async fn test_sql_backend_latency_summary_unclassified() {
        let pc = test_sql_backend_config().await;
        let now = chrono::Utc::now();

        for snippet in ["uncls 1", "uncls 2"] {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: None,
                    upstream_model: None,
                    duration_ms: Some(100),
                    prompt_snippet: snippet.to_string(),
                    prompt_char_count: None,
                    created_at: now,
                    final_provider: String::new(),
                    provider_attempts: 1,
                    ..Default::default()
                })
                .await
                .expect("insert");
        }

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch_latency_summary should succeed");

        assert!(
            result.unclassified_count >= 2,
            "expected at least 2 unclassified, got {}",
            result.unclassified_count
        );
    }

    #[tokio::test]
    async fn test_sql_backend_latency_summary_time_filter() {
        let pc = test_sql_backend_config().await;
        let cat = format!("Z_LAT_TIME_{}", uuid::Uuid::new_v4());
        let two_hours_ago = chrono::Utc::now() - chrono::Duration::hours(2);

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some(cat.clone()),
                upstream_model: None,
                duration_ms: Some(100),
                prompt_snippet: "old record".to_string(),
                prompt_char_count: None,
                created_at: two_hours_ago,
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
            })
            .await
            .expect("insert");

        let result = pc
            .backend
            .fetch_latency_summary(1)
            .await
            .expect("fetch_latency_summary should succeed");

        let found = result.rows.iter().any(|r| r.category == cat);
        assert!(
            !found,
            "old record should be excluded from 1-hour window"
        );
    }

    #[tokio::test]
    async fn test_sql_backend_savings_estimate() {
        let pc = test_sql_backend_config().await;
        let model = format!("Z_SAV_SQL_{}", uuid::Uuid::new_v4());
        let mut costs = std::collections::HashMap::new();
        costs.insert(model.clone(), 0.15);
        let mc = crate::config::routing::ModelCosts::from_costs(costs);

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("SAV_CAT".to_string()),
                upstream_model: Some(model.clone()),
                duration_ms: None,
                prompt_snippet: "cheap prompt".to_string(),
                prompt_char_count: Some(1000),
                created_at: chrono::Utc::now(),
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
            })
            .await
            .expect("insert");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, &model)
            .await
            .expect("fetch_savings_estimate should succeed");

        assert!(
            result.classified_count >= 1,
            "classified_count should be >= 1, got {}",
            result.classified_count
        );
    }

    #[tokio::test]
    async fn test_sql_backend_savings_estimate_unknown_cost() {
        let pc = test_sql_backend_config().await;
        let mc = crate::config::routing::ModelCosts::from_costs(std::collections::HashMap::new());

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("UNK_CAT".to_string()),
                upstream_model: Some("unknown-model".to_string()),
                duration_ms: None,
                prompt_snippet: "no cost info".to_string(),
                prompt_char_count: Some(500),
                created_at: chrono::Utc::now(),
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
            })
            .await
            .expect("insert");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, "gpt-4")
            .await
            .expect("fetch_savings_estimate should succeed");

        assert!(
            result.unknown_cost_count >= 1,
            "expected at least 1 unknown cost model, got {}",
            result.unknown_cost_count
        );
    }
}
