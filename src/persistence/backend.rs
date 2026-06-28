use async_trait::async_trait;
use tracing::warn;

use super::memory::MemoryBackend;
use super::sql_backend::SqlBackend;
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

/// Dispatch enum wrapping the two backend variants.
pub enum DbBackend {
    Memory(MemoryBackend),
    Sql(SqlBackend),
}

#[async_trait]
impl PersistenceBackend for DbBackend {
    async fn insert_inference(&self, record: &InferenceRecord) -> Result<(), String> {
        match self {
            DbBackend::Memory(b) => b.insert_inference(record).await,
            DbBackend::Sql(b) => b.insert_inference(record).await,
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
            DbBackend::Sql(b) => {
                b.fetch_inferences(offset, limit, filter_category, filter_model)
                    .await
            }
        }
    }

    async fn fetch_latency_summary(&self, hours: u32) -> Result<LatencySummary, QueryError> {
        match self {
            DbBackend::Memory(b) => b.fetch_latency_summary(hours).await,
            DbBackend::Sql(b) => b.fetch_latency_summary(hours).await,
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
            DbBackend::Sql(b) => {
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

#[cfg(test)]
mod tests {
    use super::*;

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
