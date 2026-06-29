use std::sync::Arc;

use tokio::sync::Semaphore;

pub(crate) mod backend;
pub(crate) mod memory;
pub(crate) mod postgres;
pub(crate) mod sqlite;
pub(crate) mod types;

pub(crate) use backend::{DbBackend, PersistenceBackend};
pub(crate) use types::{
    extract_last_user_message, extract_last_user_message_anthropic, InferenceLog,
    InferenceRecord, LatencySummary, SavingsEstimate,
};

/// Shared persistence handle injected into the Axum router state.
///
/// Cheaply cloned per-request (both fields are `Arc`). The `task_semaphore`
/// bounds the number of in-flight background log tasks so a slow database
/// cannot cause unbounded memory growth under burst traffic.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub backend: Arc<DbBackend>,
    /// Semaphore whose capacity equals `[database].log_concurrency_limit`.
    /// Tasks block on `acquire()` rather than spawning unboundedly when the
    /// database cannot keep up with insert throughput.
    pub task_semaphore: Arc<Semaphore>,
}

/// Enqueue one [`InferenceRecord`] for asynchronous background persistence.
///
/// Returns immediately — the caller is never blocked by database latency.
/// Internally, a detached `tokio::spawn` task:
/// 1. Acquires one permit from `semaphore` (blocks if the pool is exhausted).
/// 2. Calls `backend.insert_inference(&record)`.
/// 3. On failure, logs `tracing::error!` with the `request_id` for observability.
///
/// The returned `JoinHandle` can be awaited in tests; production code discards it.
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
                tracing::error!("semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = backend.insert_inference(&record).await {
            tracing::error!("final insert failure request_id={request_id} class={class}");
        }
    })
}
