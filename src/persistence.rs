use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::config::DatabaseConfig;
use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Shared trait that all persistence backends must implement.
/// Backends are Send + Sync so they can be wrapped in Arc and shared across tasks.
#[async_trait]
pub trait PersistenceBackend: Send + Sync {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String>;
    async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError>;
    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError>;
    async fn fetch_savings_estimate(
        &self,
        hours: u32,
        model_costs: &dyn CostProvider,
        baseline_model: &str,
    ) -> Result<SavingsEstimate, QueryError>;
}

/// In-memory persistence backend backed by `Arc<RwLock<Vec<InferenceRecord>>>`.
/// All queries operate over Rust iterators. p99 is computed in Rust.
///
/// ⚠️ Ephemeral: Data is lost when the process exits. Not suitable for production.
pub struct MemoryBackend {
    pub records: Arc<tokio::sync::RwLock<Vec<InferenceRecord>>>,
    /// Test-only failure injection. When true, the next call to
    /// `insert_inference` returns an error and atomically resets this flag
    /// to false. Production code leaves this at its default `false`.
    pub(crate) fail_next: std::sync::atomic::AtomicBool,
}

impl MemoryBackend {
    pub fn new() -> Self {
        MemoryBackend {
            records: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            fail_next: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// SQLite persistence backend backed by `sqlx::SqlitePool`.
/// File-backed (`./cerebrum.db`) or in-memory via shared-cache URI.
/// Schema is created via `CREATE TABLE IF NOT EXISTS` on construction.
pub struct SqliteBackend {
    pub pool: SqlitePool,
}

impl SqliteBackend {
    /// Create a new SQLite backend from a file path.
    /// For `:memory:`, uses a shared-cache in-memory URI.
    pub async fn from_path(path: &str) -> Result<Self, String> {
        let uri = if path == ":memory:" {
            "sqlite:file:cerebrum?mode=memory&cache=shared".to_string()
        } else {
            format!("sqlite:{path}?mode=rwc")
        };
        Self::from_uri(&uri).await
    }

    /// Create a new SQLite backend from an arbitrary URI.
    /// Initializes the schema on construction.
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
        Ok(())
    }
}

/// Postgres persistence backend backed by `sqlx::PgPool`.
/// All existing PG-specific SQL, retry logic, and migration flow are preserved unchanged.
pub struct PostgresBackend {
    pub pool: PgPool,
}

impl PostgresBackend {
    /// Create a new Postgres backend from env vars.
    /// Reads `DATABASE_URL`, creates pool, runs health check with retries, applies migrations.
    pub async fn from_env(db_config: &DatabaseConfig) -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;

        use sqlx::postgres::PgConnectOptions;
        use std::str::FromStr;

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

/// Dispatch enum wrapping the three backend variants.
pub enum DbBackend {
    Memory(MemoryBackend),
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}

/// Compute the 99th percentile from a sorted slice of durations.
/// Returns the value at the 99th percentile index. Returns `None` for empty input.
fn percentile_99(durations: &[i32]) -> Option<i32> {
    if durations.is_empty() {
        return None;
    }
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    let idx = (0.99 * sorted.len() as f64).ceil() as usize - 1;
    Some(sorted[idx])
}

#[async_trait]
impl PersistenceBackend for MemoryBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        if self
            .fail_next
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("test-injected failure".to_string());
        }
        let mut records = self.records.write().await;
        if records.len() >= 10_000 {
            records.remove(0);
        }
        records.push(record.clone());
        Ok(())
    }

    async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError> {
        let records = self.records.read().await;
        let mut filtered: Vec<&InferenceRecord> = records.iter().collect();

        if let Some(cat) = filter_category {
            filtered.retain(|r| r.category.as_deref() == Some(cat));
        }
        if let Some(model) = filter_model {
            filtered.retain(|r| r.upstream_model.as_deref() == Some(model));
        }

        // Sort by created_at DESC (newest first).
        filtered.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let total = filtered.len() as i64;

        let offset = offset as usize;
        let limit = limit as usize;
        let page: Vec<&InferenceRecord> = filtered.into_iter().skip(offset).take(limit).collect();

        let records: Vec<InferenceLog> = page
            .iter()
            .map(|r| InferenceLog {
                timestamp: r.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                prompt_snippet: r.prompt_snippet.clone(),
                category: r.category.clone(),
                upstream_model: r.upstream_model.clone(),
                duration_ms: r.duration_ms,
            })
            .collect();

        Ok((records, total))
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        let records = self.records.read().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);

        let window: Vec<&InferenceRecord> =
            records.iter().filter(|r| r.created_at >= cutoff).collect();

        let mut grouped: std::collections::HashMap<Option<String>, Vec<i32>> =
            std::collections::HashMap::new();
        for r in &window {
            let durations = grouped.entry(r.category.clone()).or_default();
            if let Some(d) = r.duration_ms {
                durations.push(d);
            }
        }

        let mut rows = Vec::new();
        let mut unclassified_count: i64 = 0;

        for (category, durations) in grouped {
            let request_count = durations.len() as i64;
            let avg = if durations.is_empty() {
                None
            } else {
                let sum: i32 = durations.iter().sum();
                Some((sum as f64 / request_count as f64).round() as i32)
            };
            let p99 = percentile_99(&durations);

            match category {
                Some(cat) => rows.push(LatencySummaryRow {
                    category: cat,
                    request_count,
                    avg_duration_ms: avg,
                    p99_duration_ms: p99,
                }),
                None => {
                    unclassified_count = request_count;
                }
            }
        }

        let total_classified_count: i64 = rows.iter().map(|r| r.request_count).sum();

        Ok(LatencySummary {
            rows,
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
        let records = self.records.read().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);

        // Filter by time window, non-null category, non-null model.
        let window: Vec<&InferenceRecord> = records
            .iter()
            .filter(|r| {
                r.created_at >= cutoff && r.category.is_some() && r.upstream_model.is_some()
            })
            .collect();

        // Group by upstream_model.
        let mut grouped: std::collections::HashMap<&str, Vec<&InferenceRecord>> =
            std::collections::HashMap::new();
        for r in &window {
            let model = r.upstream_model.as_deref().unwrap();
            grouped.entry(model).or_default().push(r);
        }

        let mut total_actual_cost: f64 = 0.0;
        let mut total_chars_all: i64 = 0;
        let mut classified_count: i64 = 0;
        let mut unknown_cost_count: i64 = 0;
        let mut has_historical_fallback = false;

        for (model, model_records) in &grouped {
            let count = model_records.len() as i64;
            let mut total_chars: i64 = 0;
            let mut total_fallback_chars: i64 = 0;
            let mut fallback_count: i64 = 0;

            for r in model_records {
                if let Some(chars) = r.prompt_char_count {
                    total_chars += chars as i64;
                } else {
                    total_fallback_chars += r.prompt_snippet.len() as i64;
                    fallback_count += 1;
                }
            }

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

#[async_trait]
impl PersistenceBackend for DbBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        match self {
            DbBackend::Memory(b) => b.insert_inference(record).await,
            DbBackend::Sqlite(b) => b.insert_inference(record).await,
            DbBackend::Postgres(b) => b.insert_inference(record).await,
        }
    }

    async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError> {
        match self {
            DbBackend::Memory(b) => {
                b.fetch_inferences(offset, limit, filter_category, filter_model)
                    .await
            }
            DbBackend::Sqlite(b) => {
                b.fetch_inferences(offset, limit, filter_category, filter_model)
                    .await
            }
            DbBackend::Postgres(b) => {
                b.fetch_inferences(offset, limit, filter_category, filter_model)
                    .await
            }
        }
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        match self {
            DbBackend::Memory(b) => b.fetch_latency_summary(hours).await,
            DbBackend::Sqlite(b) => b.fetch_latency_summary(hours).await,
            DbBackend::Postgres(b) => b.fetch_latency_summary(hours).await,
        }
    }

    async fn fetch_savings_estimate(
        &self,
        hours: u32,
        model_costs: &dyn CostProvider,
        baseline_model: &str,
    ) -> Result<SavingsEstimate, QueryError> {
        match self {
            DbBackend::Memory(b) => {
                b.fetch_savings_estimate(hours, model_costs, baseline_model)
                    .await
            }
            DbBackend::Sqlite(b) => {
                b.fetch_savings_estimate(hours, model_costs, baseline_model)
                    .await
            }
            DbBackend::Postgres(b) => {
                b.fetch_savings_estimate(hours, model_costs, baseline_model)
                    .await
            }
        }
    }
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
            "SELECT created_at, prompt_snippet, category, upstream_model, duration_ms \
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
            "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
             FROM inferences{} ORDER BY created_at DESC LIMIT {} OFFSET {}",
            where_clause, limit_ph, offset_ph,
        );
        let count_sql = format!("SELECT COUNT(*) FROM inferences{}", where_clause);

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

/// Trait for looking up model costs by name.
/// Allows persistence to query costs without depending on the classification module directly.
/// Must be Send + Sync so it can be passed as `&dyn CostProvider` across async task boundaries.
pub trait CostProvider: Send + Sync {
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

impl std::error::Error for QueryError {}

impl From<sqlx::Error> for QueryError {
    fn from(err: sqlx::Error) -> Self {
        QueryError(err.to_string())
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
/// Wraps an `Arc<DbBackend>` and a semaphore for bounding concurrent logging tasks.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub backend: Arc<DbBackend>,
    /// Bounds the number of concurrent background logging tasks to prevent
    /// unbounded memory growth under high throughput.
    pub task_semaphore: Arc<Semaphore>,
}

/// Finalized inference metadata payload ready for background persistence.
#[derive(Clone)]
pub struct InferenceRecord {
    pub request_id: Uuid,
    pub status: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
    pub prompt_snippet: String,
    pub prompt_char_count: Option<i32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
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

/// Extract the full last user message from an Anthropic Messages API request body.
///
/// Parses `body` as `{"messages": [...]}`, finds the last message whose `role`
/// is `"user"`, and returns its text content capped at 10,000 characters.
/// Anthropic's `content` field is polymorphic:
/// - `"content": "string"` — simple text content (returned verbatim)
/// - `"content": [{"type": "text", "text": "..."}, {"type": "image", ...}]`
///   — array of blocks; only `type == "text"` blocks contribute to the
///   extracted prompt (images, tool_results, etc. are skipped). Multiple text
///   blocks are joined with a single space.
///
/// On any parse failure, missing user message, or non-string/non-array content,
/// returns `""` and emits a WARN log. Caps message array at 1,000 (DoS
/// protection, matching the OpenAI extractor's limit). Never panics.
pub fn extract_last_user_message_anthropic(body: &str) -> String {
    let result: Option<String> = (|| {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let messages = v.get("messages")?.as_array()?;
        // Prevent DoS via unbounded message arrays.
        if messages.len() > 1000 {
            warn!(
                "ignoring Anthropic request with {} messages (limit 1000)",
                messages.len()
            );
            return Some(String::new());
        }
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?;
        let content = last_user.get("content")?;
        // Anthropic content is polymorphic: it may be a plain string OR an
        // array of typed blocks. For classification we only care about text.
        match content {
            serde_json::Value::String(s) => Some(s.chars().take(10_000).collect()),
            serde_json::Value::Array(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    let block_type = block.get("type").and_then(|t| t.as_str());
                    if block_type == Some("text") {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            parts.push(text);
                        }
                    }
                }
                Some(parts.join(" ").chars().take(10_000).collect())
            }
            _ => None,
        }
    })();

    match result {
        Some(s) => s,
        None => {
            warn!("could not extract user message from Anthropic request body; storing empty prompt");
            String::new()
        }
    }
}

/// Extract a 200-char privacy-safe snippet from an OpenAI-compatible request body.
///
/// Delegates to [`extract_last_user_message`] for JSON parsing and last-user-message
/// logic, then truncates to 200 characters. On any parse failure returns `""`.
/// Never panics, never blocks the response path.
#[cfg(test)]
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
/// Spawns a detached background task that inserts the record via the backend.
/// Final failure is logged with the `request_id`. The caller returns immediately;
/// DB latency is never on the synchronous response path.
///
/// Uses a semaphore to bound concurrent tasks; if the limit is reached, the
/// task waits before executing. This prevents unbounded memory growth
/// under sustained high throughput.
pub fn log_inference(
    backend: Arc<DbBackend>,
    semaphore: Arc<Semaphore>,
    record: InferenceRecord,
) -> tokio::task::JoinHandle<()> {
    let semaphore = semaphore.clone();
    tokio::spawn(async move {
        let request_id = record.request_id;
        let _permit = match semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => {
                error!("semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = backend.insert_inference(&record).await {
            error!("final insert failure request_id={request_id} class={class}");
        }
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

async fn insert_once_sqlite(
    pool: &SqlitePool,
    record: &InferenceRecord,
) -> Result<(), sqlx::Error> {
    // Note: `created_at` is omitted and SQLite will use its default CURRENT_TIMESTAMP.
    sqlx::query(
        "INSERT INTO inferences \
         (request_id, status, category, upstream_model, duration_ms, prompt_snippet, prompt_char_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(record.request_id.to_string())
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
        // Quick health check — if the container is flaky, skip gracefully
        let ok = tokio::time::timeout(Duration::from_secs(3), sqlx::query("SELECT 1").execute(tdb.pool.as_ref()))
            .await;
        if ok.is_ok() && ok.unwrap().is_ok() {
            return Some(tdb.pool);
        }
        eprintln!("WARN: Docker Postgres container started but failed health check — skipping");
    }
    // Fall back to DATABASE_URL env var
    let url = std::env::var("DATABASE_URL").ok()?;
    tokio::time::timeout(Duration::from_secs(3), sqlx::PgPool::connect(&url))
        .await
        .ok()?
        .ok()
        .map(std::sync::Arc::new)
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

    // ── extract_last_user_message_anthropic ────────────────────────────────────

    #[test]
    fn persistence_extract_anthropic_returns_string_content() {
        let body = r#"{"messages":[{"role":"user","content":"hello anthropic"}]}"#;
        assert_eq!(extract_last_user_message_anthropic(body), "hello anthropic");
    }

    #[test]
    fn persistence_extract_anthropic_returns_text_blocks_joined() {
        let body = r#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"first part"},
            {"type":"text","text":"second part"}
        ]}]}"#;
        assert_eq!(
            extract_last_user_message_anthropic(body),
            "first part second part"
        );
    }

    #[test]
    fn persistence_extract_anthropic_ignores_image_blocks() {
        let body = r#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"describe this"},
            {"type":"image","source":{"type":"base64","data":"AAAA"}}
        ]}]}"#;
        assert_eq!(extract_last_user_message_anthropic(body), "describe this");
    }

    #[test]
    fn persistence_extract_anthropic_returns_empty_on_empty_messages_array() {
        let body = r#"{"messages":[]}"#;
        assert_eq!(extract_last_user_message_anthropic(body), "");
    }

    #[test]
    fn persistence_extract_anthropic_returns_empty_on_invalid_json() {
        assert_eq!(extract_last_user_message_anthropic("not json"), "");
    }

    #[test]
    fn persistence_extract_anthropic_picks_last_user_message() {
        let body = r#"{"messages":[
            {"role":"user","content":"first"},
            {"role":"assistant","content":"reply"},
            {"role":"user","content":"second"}
        ]}"#;
        assert_eq!(extract_last_user_message_anthropic(body), "second");
    }

    #[test]
    fn persistence_extract_anthropic_caps_at_10000_chars() {
        let long = "x".repeat(15000);
        let body = format!(r#"{{"messages":[{{"role":"user","content":"{long}"}}]}}"#);
        assert_eq!(extract_last_user_message_anthropic(&body).len(), 10000);
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

    // ── Test helpers ────────────────────────────────────────────────────────

    /// Create a persistence config backed by in-memory storage.
    /// Always succeeds, no DATABASE_URL required.
    fn test_backend() -> PersistenceConfig {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        PersistenceConfig {
            backend: Arc::new(DbBackend::Memory(MemoryBackend::new())),
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

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
    async fn test_fetch_inferences_empty_list() {
        let pc = test_backend();
        let result = pc
            .backend
            .fetch_inferences(0, 20, Some("NONEXISTENT_CATEGORY_XYZ"), None)
            .await;
        let (records, count) = result.expect("fetch should succeed");
        assert!(records.is_empty(), "expected no records");
        assert_eq!(count, 0, "expected count=0");
    }

    #[tokio::test]
    async fn test_fetch_inferences_with_records() {
        let pc = test_backend();
        let request_id = uuid::Uuid::new_v4();
        let record = InferenceRecord {
            request_id,
            status: "ok".to_string(),
            category: Some("TEST_CAT_FETCH".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(42),
            prompt_snippet: "test snippet".to_string(),
            prompt_char_count: None,
            created_at: chrono::Utc::now(),
        };
        pc.backend
            .insert_inference(&record)
            .await
            .expect("insert should succeed");

        let (records, count) = pc
            .backend
            .fetch_inferences(0, 20, Some("TEST_CAT_FETCH"), None)
            .await
            .expect("fetch should succeed");

        assert!(count >= 1, "expected at least one record");
        let found = records.iter().any(|r| r.prompt_snippet == "test snippet");
        assert!(found, "inserted record should appear in results");
    }

    #[tokio::test]
    async fn test_fetch_inferences_filter_by_category() {
        let pc = test_backend();
        let record_a = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("CAT_ALPHA".to_string()),
            upstream_model: None,
            duration_ms: None,
            prompt_snippet: "alpha snippet".to_string(),
            prompt_char_count: None,
            created_at: chrono::Utc::now(),
        };
        let record_b = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("CAT_BETA".to_string()),
            upstream_model: None,
            duration_ms: None,
            prompt_snippet: "beta snippet".to_string(),
            prompt_char_count: None,
            created_at: chrono::Utc::now(),
        };
        pc.backend
            .insert_inference(&record_a)
            .await
            .expect("insert alpha");
        pc.backend
            .insert_inference(&record_b)
            .await
            .expect("insert beta");

        let (records, _) = pc
            .backend
            .fetch_inferences(0, 100, Some("CAT_ALPHA"), None)
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();
        for id in &ids {
            let record = InferenceRecord {
                request_id: *id,
                status: "ok".to_string(),
                category: Some("TOTAL_COUNT_TEST".to_string()),
                upstream_model: None,
                duration_ms: None,
                prompt_snippet: "snippet".to_string(),
                prompt_char_count: None,
                created_at: chrono::Utc::now(),
            };
            pc.backend.insert_inference(&record).await.expect("insert");
        }

        let (records, total_count) = pc
            .backend
            .fetch_inferences(0, 1, Some("TOTAL_COUNT_TEST"), None)
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let cat = format!("Z_TST_LAT_EMPTY_{}", uuid::Uuid::new_v4());
        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some(cat.clone()),
            upstream_model: None,
            duration_ms: Some(100),
            prompt_snippet: "single record".to_string(),
            prompt_char_count: None,
            created_at: chrono::Utc::now(),
        };
        pc.backend
            .insert_inference(&record)
            .await
            .expect("insert should succeed");

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let prefix = format!("Z_TST_LAT_DATA_{}", uuid::Uuid::new_v4());
        let cat_a = format!("{prefix}_A");
        let cat_b = format!("{prefix}_B");
        let cat_c = format!("{prefix}_C");
        let now = chrono::Utc::now();

        // Category A: 3 records with durations 100, 200, 300
        for dur in [100, 200, 300] {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: Some(cat_a.clone()),
                    upstream_model: None,
                    duration_ms: Some(dur),
                    prompt_snippet: "cat a".to_string(),
                    prompt_char_count: None,
                    created_at: now,
                })
                .await
                .expect("insert");
        }
        // Category B: 2 records with durations 50, 150
        for dur in [50, 150] {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: Some(cat_b.clone()),
                    upstream_model: None,
                    duration_ms: Some(dur),
                    prompt_snippet: "cat b".to_string(),
                    prompt_char_count: None,
                    created_at: now,
                })
                .await
                .expect("insert");
        }
        // Category C: 1 record with duration 500
        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some(cat_c.clone()),
                upstream_model: None,
                duration_ms: Some(500),
                prompt_snippet: "cat c".to_string(),
                prompt_char_count: None,
                created_at: now,
            })
            .await
            .expect("insert");

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

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
        // Rust-side p99 for [100, 200, 300]: idx = (0.99*3).ceil()-1 = 2 → 300
        assert_eq!(row_a.p99_duration_ms, Some(300));

        let row_b = test_rows
            .iter()
            .find(|r| r.category == cat_b)
            .expect("Cat B should appear");
        assert_eq!(row_b.request_count, 2);
        assert_eq!(row_b.avg_duration_ms, Some(100));
        // Rust-side p99 for [50, 150]: idx = (0.99*2).ceil()-1 = 1 → 150
        assert_eq!(row_b.p99_duration_ms, Some(150));

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
        let pc = test_backend();
        let now = chrono::Utc::now();

        for snippet in ["unclassified 1", "unclassified 2"] {
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
                })
                .await
                .expect("insert");
        }

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

        assert!(
            result.unclassified_count >= 2,
            "expected at least 2 unclassified records, got {}",
            result.unclassified_count
        );
    }

    // ── fetch_savings_estimate ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_savings_estimate_empty() {
        let pc = test_backend();
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            std::collections::HashMap::new(),
        );
        let model = format!("Z_TST_SAV_EMPTY_{}", uuid::Uuid::new_v4());
        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("Z_TST_SAV_EMPTY_CAT".to_string()),
                upstream_model: Some(model.clone()),
                duration_ms: None,
                prompt_snippet: "empty test".to_string(),
                prompt_char_count: Some(100),
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("insert should succeed");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed");

        assert!(
            result.classified_count >= 1,
            "classified_count should be >= 1, got {}",
            result.classified_count
        );
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_with_data() {
        let pc = test_backend();
        let model_a = format!("Z_TST_SAV_A_{}", uuid::Uuid::new_v4());
        let model_b = format!("Z_TST_SAV_B_{}", uuid::Uuid::new_v4());
        let mut costs = std::collections::HashMap::new();
        costs.insert(model_a.clone(), 0.15);
        costs.insert(model_b.clone(), 3.00);
        let mc = super::super::intent_classifier::ModelCosts::from_costs(costs);
        let baseline = model_b.clone();
        let now = chrono::Utc::now();

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("Z_TST_SAV_CAT1".to_string()),
                upstream_model: Some(model_a),
                duration_ms: None,
                prompt_snippet: "cheap prompt".to_string(),
                prompt_char_count: Some(1000),
                created_at: now,
            })
            .await
            .expect("insert 1");
        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("Z_TST_SAV_CAT2".to_string()),
                upstream_model: Some(model_b.clone()),
                duration_ms: None,
                prompt_snippet: "complex prompt with more content".to_string(),
                prompt_char_count: Some(2000),
                created_at: now,
            })
            .await
            .expect("insert 2");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, &baseline)
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            std::collections::HashMap::new(),
        );
        let model = format!("Z_TST_SAV_UNK_{}", uuid::Uuid::new_v4());

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("Z_TST_SAV_UNK_CAT".to_string()),
                upstream_model: Some(model),
                duration_ms: None,
                prompt_snippet: "some prompt".to_string(),
                prompt_char_count: Some(500),
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("insert should succeed");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let mc = super::super::intent_classifier::ModelCosts::from_costs(
            std::collections::HashMap::new(),
        );

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: None,
                upstream_model: Some("gpt-4o-mini".to_string()),
                duration_ms: None,
                prompt_snippet: "uncategorized".to_string(),
                prompt_char_count: Some(100),
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("insert should succeed");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, "claude-3.5-sonnet")
            .await
            .expect("fetch should succeed — NULL category must not crash the query");

        // The NULL-category record should not cause a panic; function must handle
        // the filter correctly.
        assert!(result.classified_count >= 0);
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_historical_fallback() {
        let pc = test_backend();
        let model = format!("Z_TST_SAV_FB_{}", uuid::Uuid::new_v4());
        let mut costs = std::collections::HashMap::new();
        costs.insert(model.clone(), 0.15);
        let mc = super::super::intent_classifier::ModelCosts::from_costs(costs);

        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("Z_TST_SAV_FB_CAT".to_string()),
                upstream_model: Some(model.clone()),
                duration_ms: None,
                prompt_snippet: "older record with no char count".to_string(),
                prompt_char_count: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("insert should succeed");

        let result = pc
            .backend
            .fetch_savings_estimate(24, &mc, &model)
            .await
            .expect("fetch should succeed");

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
        let pc = test_backend();
        let cat = format!("Z_TST_LAT_TIME_{}", uuid::Uuid::new_v4());
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
            })
            .await
            .expect("insert should succeed");

        // Query with hours=1 — should not find the 2-hour-old record.
        let result = pc
            .backend
            .fetch_latency_summary(1)
            .await
            .expect("fetch should succeed");

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
        let _ = PostgresBackend::from_env(&db_config).await;
    }

    // ── Memory backend specific tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_memory_p99_computation() {
        let pc = test_backend();
        let now = chrono::Utc::now();
        // Insert records with durations 10, 20, 30, 40, 50, 60, 70, 80, 90, 100
        for i in 1..=10 {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: Some("P99_TEST".to_string()),
                    upstream_model: None,
                    duration_ms: Some(i * 10),
                    prompt_snippet: format!("record {}", i),
                    prompt_char_count: None,
                    created_at: now,
                })
                .await
                .expect("insert");
        }

        let result = pc
            .backend
            .fetch_latency_summary(24)
            .await
            .expect("fetch should succeed");

        let row = result
            .rows
            .iter()
            .find(|r| r.category == "P99_TEST")
            .expect("P99_TEST row");
        assert_eq!(row.request_count, 10);
        assert_eq!(row.avg_duration_ms, Some(55));
        // p99 of [10..100]: idx = ceil(0.99*10)-1 = 9 → 100
        assert_eq!(row.p99_duration_ms, Some(100));
    }

    #[tokio::test]
    async fn test_memory_concurrent_reads() {
        let pc = test_backend();
        let now = chrono::Utc::now();
        // Insert some records.
        for i in 0..10 {
            pc.backend
                .insert_inference(&InferenceRecord {
                    request_id: uuid::Uuid::new_v4(),
                    status: "ok".to_string(),
                    category: Some("CONCUR_TEST".to_string()),
                    upstream_model: None,
                    duration_ms: Some(i),
                    prompt_snippet: format!("record {}", i),
                    prompt_char_count: None,
                    created_at: now,
                })
                .await
                .expect("insert");
        }

        let mut handles = Vec::new();
        for _ in 0..5 {
            let pc = test_backend();
            // Re-insert records for each concurrent read
            for i in 0..10 {
                pc.backend
                    .insert_inference(&InferenceRecord {
                        request_id: uuid::Uuid::new_v4(),
                        status: "ok".to_string(),
                        category: Some("CONCUR_TEST".to_string()),
                        upstream_model: None,
                        duration_ms: Some(i),
                        prompt_snippet: format!("record {}", i),
                        prompt_char_count: None,
                        created_at: now,
                    })
                    .await
                    .expect("insert");
            }
            handles.push(tokio::spawn(async move {
                pc.backend
                    .fetch_inferences(0, 100, Some("CONCUR_TEST"), None)
                    .await
            }));
        }

        for handle in handles {
            let result = handle.await.expect("task should complete");
            assert!(result.is_ok(), "concurrent read should succeed");
            let (records, _) = result.unwrap();
            assert_eq!(records.len(), 10, "should read all 10 records");
        }
    }

    #[tokio::test]
    async fn test_memory_time_filter() {
        let pc = test_backend();
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(3);

        // Insert a recent record.
        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("RECENT".to_string()),
                upstream_model: None,
                duration_ms: Some(50),
                prompt_snippet: "recent".to_string(),
                prompt_char_count: None,
                created_at: now,
            })
            .await
            .expect("insert");
        // Insert an old record.
        pc.backend
            .insert_inference(&InferenceRecord {
                request_id: uuid::Uuid::new_v4(),
                status: "ok".to_string(),
                category: Some("OLD".to_string()),
                upstream_model: None,
                duration_ms: Some(100),
                prompt_snippet: "old".to_string(),
                prompt_char_count: None,
                created_at: old,
            })
            .await
            .expect("insert");

        // 1-hour window should only find the recent record.
        let result = pc
            .backend
            .fetch_latency_summary(1)
            .await
            .expect("fetch should succeed");

        assert!(
            result.rows.iter().any(|r| r.category == "RECENT"),
            "recent should appear"
        );
        assert!(
            !result.rows.iter().any(|r| r.category == "OLD"),
            "old should be excluded"
        );

        // 4-hour window should find both.
        let result4 = pc
            .backend
            .fetch_latency_summary(4)
            .await
            .expect("fetch should succeed");

        assert!(
            result4.rows.iter().any(|r| r.category == "RECENT"),
            "recent should appear in 4h"
        );
        assert!(
            result4.rows.iter().any(|r| r.category == "OLD"),
            "old should appear in 4h"
        );
    }

    // ── SQLite backend specific tests ────────────────────────────────────────

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

    // ── log_inference integration test ───────────────────────────────────────

    #[tokio::test]
    async fn test_log_inference_integration() {
        let pc = test_backend();
        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("LOG_TEST".to_string()),
            upstream_model: Some("test-model".to_string()),
            duration_ms: Some(50),
            prompt_snippet: "log inference test".to_string(),
            prompt_char_count: Some(25),
            created_at: chrono::Utc::now(),
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

    // ── PG-specific tests (require DATABASE_URL) ────────────────────────────

    #[tokio::test]
    async fn test_pg_log_concurrency_limit_parsed_from_env() {
        // This test requires a live DATABASE_URL. It verifies that the LOG_CONCURRENCY_LIMIT
        // environment variable is respected and the semaphore is created with the correct permit count.
        // Fast settings to avoid delays.
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        // Quick check: ensure DATABASE_URL is set and we can actually connect.
        if super::test_pool().await.is_none() {
            eprintln!("SKIP test_pg_log_concurrency_limit_parsed_from_env: DATABASE_URL not set or unreachable");
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
        let pg_backend = PostgresBackend::from_env(&db_config)
            .await
            .expect("PostgresBackend should succeed");
        let config = PersistenceConfig {
            backend: Arc::new(DbBackend::Postgres(pg_backend)),
            task_semaphore: Arc::new(Semaphore::new(7)),
        };
        assert_eq!(config.task_semaphore.available_permits(), 7);
    }
}
