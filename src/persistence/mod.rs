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

/// Shared persistence configuration injected into the app router.
/// Wraps an `Arc<DbBackend>` and a semaphore for bounding concurrent logging tasks.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub backend: Arc<DbBackend>,
    /// Bounds the number of concurrent background logging tasks to prevent
    /// unbounded memory growth under high throughput.
    pub task_semaphore: Arc<Semaphore>,
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
                tracing::error!("semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = backend.insert_inference(&record).await {
            tracing::error!("final insert failure request_id={request_id} class={class}");
        }
    })
}
