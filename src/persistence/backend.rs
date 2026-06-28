use async_trait::async_trait;
use tracing::warn;

#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use sqlx::PgPool;

use super::memory::MemoryBackend;
use super::postgres::PostgresBackend;
use super::sqlite::SqliteBackend;
use super::types::{
    CostProvider, InferenceLog, InferenceRecord, LatencySummary, QueryError, SavingsEstimate,
};

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

/// Dispatch enum wrapping the three backend variants.
pub enum DbBackend {
    Memory(MemoryBackend),
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
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

/// Compute the 99th percentile from a sorted slice of durations.
/// Returns the value at the 99th percentile index. Returns `None` for empty input.
pub(crate) fn percentile_99(durations: &[i32]) -> Option<i32> {
    if durations.is_empty() {
        return None;
    }
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    let idx = (0.99 * sorted.len() as f64).ceil() as usize - 1;
    Some(sorted[idx])
}

/// Retry an async operation exactly once.
/// Logs a warning on the first failure and returns the second error if both fail.
pub(crate) async fn retry_once<F, Fut, T, E>(f: F) -> Result<T, E>
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

/// Ephemeral PostgreSQL container for integration tests.
/// Spins up via `testcontainers`; falls back to DATABASE_URL when Docker unavailable.
#[cfg(test)]
pub(crate) struct TestDb {
    pub pool: std::sync::Arc<PgPool>,
    pub _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
}

#[cfg(test)]
impl TestDb {
    pub(crate) async fn new() -> Option<Self> {
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
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .ok()?;
        sqlx::migrate!().run(&pool).await.ok()?;
        eprintln!("Test DB: postgres://test:test@127.0.0.1:{port}/test");
        Some(Self {
            pool: std::sync::Arc::new(pool),
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
pub(crate) async fn test_pool() -> Option<std::sync::Arc<PgPool>> {
    // Try disposable PostgreSQL container first (in-memory, Docker-backed)
    if let Some(tdb) = TestDb::new().await {
        // Quick health check — if the container is flaky, skip gracefully
        let ok = tokio::time::timeout(
            Duration::from_secs(3),
            sqlx::query("SELECT 1").execute(tdb.pool.as_ref()),
        )
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
}
