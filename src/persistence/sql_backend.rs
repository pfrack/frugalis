use async_trait::async_trait;
use sea_query::{Expr, Iden, Query, QueryBuilder, SqliteQueryBuilder, PostgresQueryBuilder};
use sqlx::Row;
use tracing::error;

use super::backend::{percentile_99, retry_once, PersistenceBackend};
use super::types::{
    prompt_chars_to_cost, CostProvider, InferenceLog, InferenceRecord, LatencySummary,
    LatencySummaryRow, QueryError, SavingsEstimate,
};

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
    /// Build SQL string for the current dialect.
    fn build_sql(&self, query: impl QueryBuilder) -> String {
        match self.dialect {
            Dialect::Postgres => query.to_string(PostgresQueryBuilder),
            Dialect::Sqlite => query.to_string(SqliteQueryBuilder),
        }
    }

    /// Create a SQLite in-memory backend for testing.
    pub async fn new_sqlite_in_memory() -> Result<Self, String> {
        let uri = format!("sqlite:file:sql_backend_test_{}?mode=memory&cache=shared", uuid::Uuid::new_v4());
        Self::connect(&uri).await
    }

    /// Connect to a database and create a SqlBackend.
    /// Determines dialect from the URL scheme.
    pub async fn connect(url: &str) -> Result<Self, String> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            let pool = sqlx::PgPool::connect(url)
                .await
                .map_err(|e| format!("failed to connect to Postgres: {e}"))?;
            Ok(Self {
                pool: Pool::Postgres(pool),
                dialect: Dialect::Postgres,
            })
        } else if url.starts_with("sqlite:") {
            let pool = sqlx::SqlitePool::connect(url)
                .await
                .map_err(|e| format!("failed to connect to SQLite: {e}"))?;
            let backend = Self {
                pool: Pool::Sqlite(pool),
                dialect: Dialect::Sqlite,
            };
            backend.init_sqlite_schema().await?;
            Ok(backend)
        } else {
            Err(format!("unsupported database URL scheme: {}", url))
        }
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

    /// Execute a query that returns rows (SELECT).
    async fn fetch_all_query(&self, sql: String) -> Result<Vec<sqlx::any::AnyRow>, sqlx::Error> {
        match &self.pool {
            Pool::Postgres(pool) => {
                let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
                // Convert PgRow to AnyRow
                Ok(rows.into_iter().map(|r| sqlx::any::AnyRow::from(r)).collect())
            }
            Pool::Sqlite(pool) => {
                let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
                // Convert SqliteRow to AnyRow
                Ok(rows.into_iter().map(|r| sqlx::any::AnyRow::from(r)).collect())
            }
        }
    }

    /// Execute a query that returns a single row.
    async fn fetch_one_query(&self, sql: String) -> Result<sqlx::any::AnyRow, sqlx::Error> {
        match &self.pool {
            Pool::Postgres(pool) => {
                let row = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await?;
                Ok(sqlx::any::AnyRow::from(row))
            }
            Pool::Sqlite(pool) => {
                let row = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await?;
                Ok(sqlx::any::AnyRow::from(row))
            }
        }
    }
}

async fn insert_once_sql_backend(backend: &SqlBackend, record: &InferenceRecord) -> Result<(), sqlx::Error> {
    let sql = Query::insert()
        .into_table(Inferences::Table)
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
        ])
        .to_string(match backend.dialect {
            Dialect::Postgres => PostgresQueryBuilder,
            Dialect::Sqlite => SqliteQueryBuilder,
        });

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
        // Build count query
        let mut count_query = Query::select();
        count_query
            .expr(Expr::cust("COUNT(*)"))
            .from(Inferences::Table);

        // Build data query
        let mut data_query = Query::select();
        data_query
            .columns([
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

        // Apply optional filters
        if let Some(cat) = filter_category {
            count_query.and_where(Expr::col(Inferences::Category).eq(cat));
            data_query.and_where(Expr::col(Inferences::Category).eq(cat));
        }
        if let Some(model) = filter_model {
            count_query.and_where(Expr::col(Inferences::UpstreamModel).eq(model));
            data_query.and_where(Expr::col(Inferences::UpstreamModel).eq(model));
        }

        let count_sql = match self.dialect {
            Dialect::Postgres => count_query.to_string(PostgresQueryBuilder),
            Dialect::Sqlite => count_query.to_string(SqliteQueryBuilder),
        };
        let data_sql = match self.dialect {
            Dialect::Postgres => data_query.to_string(PostgresQueryBuilder),
            Dialect::Sqlite => data_query.to_string(SqliteQueryBuilder),
        };

        // Execute count query
        let count_row = self.fetch_one_query(count_sql).await.map_err(|e| QueryError(e.to_string()))?;
        let total_count: i64 = count_row.try_get(0).map_err(|e| QueryError(e.to_string()))?;

        // Execute data query
        let rows = self.fetch_all_query(data_sql).await.map_err(|e| QueryError(e.to_string()))?;

        let records: Vec<InferenceLog> = rows
            .iter()
            .map(|row| {
                let timestamp: String = match self.dialect {
                    Dialect::Postgres => {
                        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
                        created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                    }
                    Dialect::Sqlite => row.try_get("created_at")?,
                };
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

    async fn fetch_latency_summary(&self, _hours: u32) -> Result<LatencySummary, QueryError> {
        // Placeholder - will be implemented in Phase 3
        Err(QueryError("fetch_latency_summary not yet implemented for SqlBackend".to_string()))
    }

    async fn fetch_savings_estimate(
        &self,
        _hours: u32,
        _model_costs: &dyn CostProvider,
        _baseline_model: &str,
    ) -> Result<SavingsEstimate, QueryError> {
        // Placeholder - will be implemented in Phase 3
        Err(QueryError("fetch_savings_estimate not yet implemented for SqlBackend".to_string()))
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
}
