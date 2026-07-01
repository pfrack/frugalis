use async_trait::async_trait;
use sea_query::{Expr, ExprTrait, Iden, Query, SqliteQueryBuilder, PostgresQueryBuilder};
use sqlx::Row;
use tracing::error;

use super::backend::{retry_once, PersistenceBackend};
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
    PreviousResponseId,
    CodexInstallationId,
    CodexTurnState,
    CodexWindowId,
    CodexTurnMetadata,
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
    #[cfg(test)]
    pub async fn new_sqlite_in_memory() -> Result<Self, String> {
        let uri = format!("sqlite:file:sql_backend_test_{}?mode=memory&cache=shared", uuid::Uuid::new_v4());
        Self::connect(&uri, &crate::config::types::DatabaseConfig::default()).await
    }

    /// Connect to a database and create a SqlBackend.
    /// Determines dialect from the URL scheme and runs migrations via refinery.
    /// Pool tuning (max connections, timeouts) comes from `db_config`.
    pub async fn connect(url: &str, db_config: &crate::config::types::DatabaseConfig) -> Result<Self, String> {
        use std::time::Duration;
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            // rustls 0.23 requires a CryptoProvider to be installed before any
            // TLS operation (including sqlx's tls-rustls pool setup). Idempotent:
            // returns Err if already installed, which we safely ignore.
            let _ = rustls::crypto::ring::default_provider().install_default();

            let tls_mode = url.contains("sslmode=require")
                || url.contains("sslmode=verify-ca")
                || url.contains("sslmode=verify-full");

            if tls_mode {
                let roots = rustls::RootCertStore::from_iter(
                    webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                );
                let tls = tokio_postgres_rustls::MakeRustlsConnect::new(
                    rustls::ClientConfig::builder()
                        .with_root_certificates(roots)
                        .with_no_client_auth(),
                );
                let (mut client, connection) = tokio_postgres::connect(url, tls)
                    .await
                    .map_err(|e| format!("failed to connect to Postgres for migrations: {e}"))?;
                let conn_handle = tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        tracing::error!("Postgres connection error: {e}");
                    }
                });
                let migration_result = embedded::migrations::runner()
                    .run_async(&mut client)
                    .await;
                drop(client);
                conn_handle.abort();
                migration_result.map_err(|e| format!("Postgres migration failed: {e}"))?;
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(db_config.max_connections)
                    .acquire_timeout(Duration::from_secs(db_config.acquire_timeout_secs))
                    .idle_timeout(Some(Duration::from_secs(db_config.idle_timeout_secs)))
                    .connect(url)
                    .await
                    .map_err(|e| format!("failed to create Postgres pool: {e}"))?;
                Ok(Self { pool: Pool::Postgres(pool), dialect: Dialect::Postgres })
            } else {
                let (mut client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
                    .await
                    .map_err(|e| format!("failed to connect to Postgres for migrations: {e}"))?;
                let conn_handle = tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        tracing::error!("Postgres connection error: {e}");
                    }
                });
                let migration_result = embedded::migrations::runner()
                    .run_async(&mut client)
                    .await;
                drop(client);
                conn_handle.abort();
                migration_result.map_err(|e| format!("Postgres migration failed: {e}"))?;
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(db_config.max_connections)
                    .acquire_timeout(Duration::from_secs(db_config.acquire_timeout_secs))
                    .idle_timeout(Some(Duration::from_secs(db_config.idle_timeout_secs)))
                    .connect(url)
                    .await
                    .map_err(|e| format!("failed to create Postgres pool: {e}"))?;
                Ok(Self { pool: Pool::Postgres(pool), dialect: Dialect::Postgres })
            }
        } else if url.starts_with("sqlite:") {
            // Check if this is an in-memory database (can't persist migrations across connections).
            let is_in_memory = url.contains("mode=memory");

            if !is_in_memory {
                // File-based SQLite: run refinery migrations via rusqlite before creating the pool.
                let path_part = url.strip_prefix("sqlite:").unwrap_or(url);
                let db_path = path_part.split('?').next().unwrap_or(path_part).to_string();
                let runner = embedded::migrations::runner();
                tokio::task::spawn_blocking(move || {
                    let mut conn = rusqlite::Connection::open(&db_path)
                        .map_err(|e| format!("failed to open SQLite for migrations: {e}"))?;
                    runner.run(&mut conn)
                        .map_err(|e| format!("SQLite migration failed: {e}"))?;
                    Ok::<(), String>(())
                })
                .await
                .map_err(|e| format!("spawn_blocking failed: {e}"))??;
            }

            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(db_config.max_connections)
                .acquire_timeout(Duration::from_secs(db_config.acquire_timeout_secs))
                .idle_timeout(Some(Duration::from_secs(db_config.idle_timeout_secs)))
                .connect(url)
                .await
                .map_err(|e| format!("failed to create SQLite pool: {e}"))?;
            let backend = Self {
                pool: Pool::Sqlite(pool),
                dialect: Dialect::Sqlite,
            };

            // For in-memory databases, run schema init directly (refinery can't persist across connections).
            if is_in_memory {
                backend.init_sqlite_schema().await?;
            }

            Ok(backend)
        } else {
            Err(format!("unsupported database URL scheme: {}", url))
        }
    }

    /// Initialize SQLite schema for in-memory databases.
    ///
    /// File-based SQLite runs refinery via rusqlite in `connect()`. In-memory
    /// databases can't persist migration state across connections, so this
    /// method applies the same schema V1 defines, directly via the sqlx pool.
    /// Keep this DDL in sync with `migrations/V1__create_inferences.sql`.
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
              created_at TEXT NOT NULL, \
              prompt_snippet TEXT, \
              prompt_char_count INTEGER, \
              provider_attempts SMALLINT DEFAULT 1, \
              final_provider TEXT, \
              input_tokens INTEGER, \
              output_tokens INTEGER, \
              cache_read_tokens INTEGER, \
              cache_creation_tokens INTEGER, \
              client_session_id TEXT, \
              previous_response_id TEXT, \
              codex_installation_id TEXT, \
              codex_turn_state TEXT, \
              codex_window_id TEXT, \
              codex_turn_metadata TEXT)",
        )
        .execute(pool)
        .await
        .map_err(|e| format!("failed to initialize SQLite schema: {e}"))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS inferences_created_at_idx \
             ON inferences(created_at DESC)",
        )
        .execute(pool)
        .await
        .map_err(|e| format!("failed to create SQLite index: {e}"))?;

        Ok(())
    }

    /// Execute a query that returns no rows (INSERT, UPDATE, DELETE).
    ///
    /// # Safety basis
    /// Queries are built with sea-query's `to_string()` builder, which escapes
    /// all `Value` parameters inline. This is the recognized safe pattern when
    /// `sea-query-sqlx` (parameterized `build_sqlx` + `query_with`) is not
    /// available — sea-query-sqlx 0.9.1 has a non-exhaustive-match bug with
    /// sea-query 1.0's `with-chrono` feature. The `AssertSqlSafe` wrapper
    /// acknowledges that the SQL string is not compile-time checked but is
    /// trusted because sea-query generated it. All user-supplied values flow
    /// through sea-query's `Value` type, which handles escaping; no raw
    /// `format!` interpolation is used in sea-query-built queries.
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
                Inferences::PreviousResponseId,
                Inferences::CodexInstallationId,
                Inferences::CodexTurnState,
                Inferences::CodexWindowId,
                Inferences::CodexTurnMetadata,
                Inferences::CreatedAt,
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
                record.previous_response_id.clone().into(),
                record.codex_installation_id.clone().into(),
                record.codex_turn_state.clone().into(),
                record.codex_window_id.clone().into(),
                record.codex_turn_metadata.clone().into(),
                record.created_at.format("%Y-%m-%d %H:%M:%S").to_string().into(),
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
                q.and_where(Expr::col(Inferences::Category).eq(Expr::val(cat)));
            }
            if let Some(model) = filter_model {
                q.and_where(Expr::col(Inferences::UpstreamModel).eq(Expr::val(model)));
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
                Inferences::PreviousResponseId,
            ])
            .from(Inferences::Table)
            .order_by(Inferences::CreatedAt, sea_query::Order::Desc)
            .limit(limit as u64)
            .offset(offset as u64);
            if let Some(cat) = filter_category {
                q.and_where(Expr::col(Inferences::Category).eq(Expr::val(cat)));
            }
            if let Some(model) = filter_model {
                q.and_where(Expr::col(Inferences::UpstreamModel).eq(Expr::val(model)));
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
                    let previous_response_id: Option<String> = row.try_get("previous_response_id")?;
                    Ok(InferenceLog { timestamp, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider, previous_response_id })
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
                    let previous_response_id: Option<String> = row.try_get("previous_response_id")?;
                    Ok(InferenceLog { timestamp, prompt_snippet, category, upstream_model, duration_ms, provider_attempts, final_provider, previous_response_id })
                }).collect::<Result<Vec<_>, sqlx::Error>>().map_err(|e| QueryError(e.to_string()))?;

                Ok((records, total_count))
            }
        }
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

        match self.dialect {
            Dialect::Postgres => {
                let sql = {
                    let mut q = Query::select();
                    q.column(Inferences::Category)
                        .expr(Expr::cust("COUNT(*)::BIGINT AS count"))
                        .expr(Expr::cust("ROUND(AVG(duration_ms))::INTEGER AS avg_duration_ms"))
                        .expr(Expr::cust("ROUND(PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms))::INTEGER AS p99_duration_ms"))
                        .from(Inferences::Table)
                        .and_where(Expr::col(Inferences::CreatedAt).gte(Expr::val(cutoff_str.clone())))
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

                // Single query using window functions to compute COUNT, AVG, and p99 per category.
                // p99 is the value at the 99th percentile position within each category,
                // selected via ROW_NUMBER / COUNT window functions — no data leaves the database.
                let summary_sql =
                    "SELECT category, count, avg_duration_ms, p99_duration_ms \
                     FROM ( \
                         SELECT category, \
                             COUNT(*) OVER (PARTITION BY category) AS count, \
                             CAST(ROUND(AVG(duration_ms) OVER (PARTITION BY category)) AS INTEGER) AS avg_duration_ms, \
                             duration_ms AS p99_duration_ms, \
                             ROW_NUMBER() OVER (PARTITION BY category ORDER BY duration_ms ASC) AS rn, \
                             (COUNT(*) OVER (PARTITION BY category) * 99 + 99) / 100 AS p99_pos \
                         FROM inferences \
                         WHERE created_at >= ? AND category IS NOT NULL AND duration_ms IS NOT NULL \
                     ) \
                     WHERE rn = p99_pos \
                     GROUP BY category \
                     ORDER BY count DESC";

                let summary_rows_result: Vec<LatencySummaryRow> = sqlx::query(sqlx::AssertSqlSafe(summary_sql))
                    .bind(&cutoff_str)
                    .fetch_all(pool).await?
                    .iter()
                    .map(|row| {
                        let category: String = row.try_get("category").unwrap_or_default();
                        let request_count: i64 = row.try_get("count").unwrap_or(0);
                        let avg_duration_ms: Option<i32> = row.try_get("avg_duration_ms").unwrap_or(None);
                        let p99_duration_ms: Option<i32> = row.try_get("p99_duration_ms").unwrap_or(None);
                        LatencySummaryRow { category, request_count, avg_duration_ms, p99_duration_ms }
                    })
                    .collect();

                // Count unclassified (NULL category) records.
                let unclassified_sql = {
                    let mut q = Query::select();
                    q.expr(Expr::cust("COUNT(*)"))
                        .from(Inferences::Table)
                        .and_where(Expr::col(Inferences::CreatedAt).gte(Expr::val(cutoff_str.clone())))
                        .and_where(Expr::col(Inferences::Category).is_null());
                    q.to_string(SqliteQueryBuilder)
                };

                let unclassified: i64 = sqlx::query(sqlx::AssertSqlSafe(unclassified_sql))
                    .fetch_one(pool).await?
                    .try_get(0)
                    .map_err(|e| QueryError(e.to_string()))?;

                let total_classified_count: i64 = summary_rows_result.iter().map(|r| r.request_count).sum();

                Ok(LatencySummary {
                    rows: summary_rows_result,
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
        let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

        fn build_sql(dialect: Dialect, cutoff_str: &str) -> String {
            let mut q = Query::select();
            q.column(Inferences::UpstreamModel)
                .expr(Expr::cust("COUNT(*) AS count"))
                .expr(Expr::cust("COALESCE(SUM(prompt_char_count), 0) AS total_chars"))
                .expr(Expr::cust("COALESCE(SUM(LENGTH(prompt_snippet)), 0) AS total_fallback_chars"))
                .expr(Expr::cust("COALESCE(SUM(CASE WHEN prompt_char_count IS NULL THEN 1 ELSE 0 END), 0) AS fallback_count"))
                .from(Inferences::Table)
                .and_where(Expr::col(Inferences::CreatedAt).gte(Expr::val(cutoff_str.to_string())))
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
                duration_ms: Some(10 * i),
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
        let mc = crate::routing::ModelCosts::from_costs(costs);

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
        let mc = crate::routing::ModelCosts::from_costs(std::collections::HashMap::new());

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

    /// Exercises the refinery migration path (V1–V5) on a fresh file-based
    /// SQLite database. Unlike the in-memory tests (which bypass refinery via
    /// `init_sqlite_schema`), this test catches schema-completeness regressions
    /// in the migration files.
    #[tokio::test]
    async fn test_sql_backend_connect_file_sqlite_refinery() {
        let db_path = std::env::temp_dir().join(format!(
            "frugalis_refinery_test_{}.db",
            uuid::Uuid::new_v4()
        ));
        let url = format!("sqlite:{}", db_path.display());

        let backend = SqlBackend::connect(&url, &crate::config::types::DatabaseConfig::default())
            .await
            .expect("connect should succeed on fresh file-based SQLite (refinery V1)");

        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("SQLITE_REFINERY".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(42),
            prompt_snippet: "sqlite refinery test".to_string(),
            prompt_char_count: Some(100),
            created_at: chrono::Utc::now(),
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
        };
        backend
            .insert_inference(&record)
            .await
            .expect("insert should succeed on fresh file-based SQLite schema");

        let (records, count) = backend
            .fetch_inferences(0, 10, Some("SQLITE_REFINERY"), None)
            .await
            .expect("fetch should succeed");
        assert_eq!(count, 1, "expected 1 record after refinery insert");
        assert_eq!(records[0].prompt_snippet, "sqlite refinery test");

        let _ = std::fs::remove_file(&db_path);
    }

    mod slow_tests {
        use super::*;
        use crate::persistence::{reference_inference_record, InferenceRecord};

        /// Start a fresh Postgres testcontainer and return its URL + container
        /// handle. The container stays alive while the caller holds the handle.
        /// Returns `None` if Docker is unavailable — the test skips.
        async fn fresh_postgres() -> Option<(
            String,
            testcontainers::ContainerAsync<testcontainers::GenericImage>,
        )> {
            use testcontainers::{
                core::{IntoContainerPort, WaitFor},
                runners::AsyncRunner,
                GenericImage, ImageExt,
            };
            let container = GenericImage::new("postgres", "16-alpine")
                .with_exposed_port(5432.tcp())
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_USER", "test")
                .with_env_var("POSTGRES_PASSWORD", "test")
                .with_env_var("POSTGRES_DB", "test")
                .with_startup_timeout(std::time::Duration::from_secs(60))
                .start()
                .await
                .ok()?;
            let port = container.get_host_port_ipv4(5432.tcp()).await.ok()?;
            let url = format!("postgres://test:test@127.0.0.1:{port}/test");
            Some((url, container))
        }

        /// Exercises the refinery migration path (V1) on a **fresh**
        /// Postgres database. This is the test that catches cross-dialect
        /// migration bugs invisible to SQLite-only tests — e.g. SQLite-only
        /// `datetime('now')` in a DEFAULT clause (F1) or a `PRIMARY KEY`
        /// column the code never inserts (F2). Skips gracefully when Docker
        /// is unavailable. Never falls back to `DATABASE_URL` — the whole
        /// point is to test against a database that has never seen V1.
        #[tokio::test]
        async fn test_sql_backend_connect_postgres_refinery() {
            let Some((url, _container)) = fresh_postgres().await else {
                eprintln!("SKIP: Docker Postgres container unavailable");
                return;
            };
            let backend = SqlBackend::connect(&url, &crate::config::types::DatabaseConfig::default())
                .await
                .expect("connect should succeed on fresh Postgres (refinery V1)");

            let record = InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("PG_REFINERY".to_string()),
                upstream_model: Some("test-model".to_string()),
                duration_ms: Some(42),
                prompt_snippet: "postgres refinery test".to_string(),
                prompt_char_count: Some(100),
                created_at: chrono::Utc::now(),
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
            };
            backend
                .insert_inference(&record)
                .await
                .expect("insert should succeed on fresh Postgres schema");

            let (records, count) = backend
                .fetch_inferences(0, 10, Some("PG_REFINERY"), None)
                .await
                .expect("fetch should succeed");
            assert_eq!(count, 1, "expected 1 record after refinery insert");
            assert_eq!(records[0].prompt_snippet, "postgres refinery test");
        }

        /// Cross-backend identity test for Postgres: inserts the canonical
        /// reference record and verifies that the fetched `InferenceLog` matches
        /// the expected values from the memory/SQLite identity test.
        ///
        /// Uses the same `reference_inference_record()` that the memory/SQLite
        /// test uses, ensuring all three backends test identical input.
        ///
        /// Skips gracefully when Docker is unavailable.
        #[tokio::test]
        async fn test_cross_backend_identity_postgres() {
            let Some((url, _container)) = fresh_postgres().await else {
                eprintln!("SKIP: Docker Postgres container unavailable");
                return;
            };

            let backend = SqlBackend::connect(&url, &crate::config::types::DatabaseConfig::default())
                .await
                .expect("Postgres backend should connect");

            let record = reference_inference_record();
            let filter_category = record.category.as_deref();

            backend
                .insert_inference(&record)
                .await
                .expect("Postgres insert should succeed");

            let (records, count) = backend
                .fetch_inferences(0, 10, filter_category, None)
                .await
                .expect("Postgres fetch should succeed");

            assert_eq!(count, 1, "expected exactly 1 record");
            let pg_log = &records[0];

            // Assert all fields match the expected values from reference_inference_record()
            assert_eq!(pg_log.prompt_snippet, "reference record for cross-backend identity test");
            assert_eq!(pg_log.category, Some("SYNTAX_FIX".to_string()));
            assert_eq!(pg_log.upstream_model, Some("claude-sonnet-4".to_string()));
            assert_eq!(pg_log.duration_ms, Some(1234));
            assert_eq!(pg_log.provider_attempts, Some(2));
            assert_eq!(pg_log.final_provider, Some("anthropic".to_string()));
            assert_eq!(pg_log.previous_response_id, Some("prev-456".to_string()));

            // Timestamp should be "1970-01-01 00:00:00" (epoch, no UTC suffix for Postgres)
            assert!(
                pg_log.timestamp.starts_with("1970-01-01 00:00:00"),
                "timestamp should be epoch: got {}",
                pg_log.timestamp
            );
        }
    }
}
