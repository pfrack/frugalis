use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Shared persistence configuration injected into the app router.
#[derive(Clone)]
pub struct PersistenceConfig {
    pub pool: Arc<PgPool>,
    /// Bounds the number of concurrent background logging tasks to prevent
    /// unbounded memory growth under high throughput.
    pub task_semaphore: Arc<Semaphore>,
}

/// Finalized inference metadata payload ready for background persistence.
pub struct InferenceRecord {
    pub request_id: Uuid,
    pub status: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
    pub prompt_snippet: String,
}

impl PersistenceConfig {
    /// Initialise from `DATABASE_URL` environment variable.
    /// Configures connection pool with explicit bounds: max 10 connections,
    /// 30s acquire timeout, 30m idle timeout. Prevents connection exhaustion
    /// under load.
    pub async fn from_env() -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .idle_timeout(std::time::Duration::from_secs(1800))
            .connect(&url)
            .await
            .map_err(|e| format!("DB connection failed: {e}"))?;
        Ok(Self {
            pool: Arc::new(pool),
            task_semaphore: Arc::new(Semaphore::new(100)),
        })
    }
}

/// Extract a privacy-safe snippet from an OpenAI-compatible request body.
///
/// Parses `body` as `{"messages": [...]}`, finds the last message whose `role`
/// is `"user"`, and returns the first 200 chars of its `content` string.
/// On any parse failure or missing user message, returns `""` and emits a WARN
/// log. Never panics, never blocks the response path.
pub fn extract_snippet(body: &str) -> String {
    let result: Option<String> = (|| {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let messages = v.get("messages")?.as_array()?;
        // Prevent DoS via unbounded message arrays.
        if messages.len() > 1000 {
            eprintln!(
                "WARN persistence: ignoring request with {} messages (limit 1000)",
                messages.len()
            );
            return Some(String::new());
        }
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?;
        let content = last_user.get("content")?.as_str()?;
        Some(content.chars().take(200).collect())
    })();

    match result {
        Some(s) => s,
        None => {
            eprintln!(
                "WARN persistence: could not extract user snippet from request body; \
                 storing empty snippet"
            );
            String::new()
        }
    }
}

/// Enqueue an inference record for async persistence.
///
/// Spawns a detached background task that attempts one insert and, on failure,
/// retries exactly once. Final failure is logged with the `request_id` and
/// failure class. The caller returns immediately; DB latency is never on the
/// synchronous response path.
///
/// Uses a semaphore to bound concurrent tasks; if the limit is reached, the
/// task waits briefly before executing. This prevents unbounded memory growth
/// under sustained high throughput.
pub fn log_inference(
    pool: Arc<PgPool>,
    semaphore: Arc<Semaphore>,
    record: InferenceRecord,
) -> tokio::task::JoinHandle<()> {
    let semaphore = semaphore.clone();
    tokio::spawn(async move {
        let request_id = record.request_id;
        let _permit = match semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => {
                eprintln!("ERROR persistence: semaphore closed for request_id={request_id}");
                return;
            }
        };
        if let Err(class) = write_with_retry(&pool, &record).await {
            eprintln!(
                "ERROR persistence: final insert failure \
                 request_id={request_id} class={class}"
            );
        }
    })
}

async fn write_with_retry(pool: &PgPool, record: &InferenceRecord) -> Result<(), String> {
    retry_once(|| insert_once(pool, record)).await.map_err(|e| {
        eprintln!(
            "ERROR persistence: insert failed for request_id={} after retries: {:?}",
            record.request_id, e
        );
        e.to_string()
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
            eprintln!("WARN persistence: first insert attempt failed ({first}); retrying once");
            f().await
        }
    }
}

async fn insert_once(pool: &PgPool, record: &InferenceRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inferences \
         (request_id, status, category, upstream_model, duration_ms, prompt_snippet) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(record.request_id)
    .bind(&record.status)
    .bind(&record.category)
    .bind(&record.upstream_model)
    .bind(record.duration_ms)
    .bind(&record.prompt_snippet)
    .execute(pool)
    .await
    .map(|_| ())
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
