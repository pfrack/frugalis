use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use uuid::Uuid;
use tokio::sync::Semaphore;

/// Custom error type for inference query failures.
#[derive(Debug, Clone)]
pub enum QueryError {
    Database(String),      // Connection, query, or pool error
    InvalidFilter(String), // Invalid filter value
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Database(msg) => write!(f, "Database error: {}", msg),
            Self::InvalidFilter(msg) => write!(f, "Invalid filter: {}", msg),
        }
    }
}

/// One row from the `inferences` table, pre-formatted for dashboard display.
#[derive(Debug, Clone)]
pub struct InferenceLog {
    pub id: String,
    pub timestamp: String,
    pub prompt_snippet: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
}

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

use sqlx::postgres::{PgConnectOptions};
use std::str::FromStr;

impl PersistenceConfig {
    pub async fn from_env() -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;

        let options = PgConnectOptions::from_str(&url)
            .map_err(|e| format!("DB connection string parse error: {e}"))?;

        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .idle_timeout(std::time::Duration::from_secs(1800))
            .connect_lazy_with(options);

        Ok(Self {
            pool: Arc::new(pool),
            task_semaphore: Arc::new(Semaphore::new(100)),
        })
    }

    /// Fetch recent inference records with optional pagination and filtering.
    ///
    /// Returns both the matching records (formatted for display) and the total
    /// count of matching rows (for pagination metadata), avoiding N+1 queries
    /// by running a separate COUNT query with the same filters.
    pub async fn fetch_inferences(
        &self,
        offset: u32,
        limit: u32,
        filter_category: Option<&str>,
        filter_model: Option<&str>,
    ) -> Result<(Vec<InferenceLog>, i64), QueryError> {
        // Build the data query and count query with proper parameter indices.
        let (data_sql, count_sql) = match (filter_category, filter_model) {
            (Some(_), Some(_)) => (
                "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
                 FROM inferences WHERE category = $1 AND upstream_model = $2 \
                 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
                "SELECT COUNT(*) FROM inferences WHERE category = $1 AND upstream_model = $2",
            ),
            (Some(_), None) => (
                "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
                 FROM inferences WHERE category = $1 \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
                "SELECT COUNT(*) FROM inferences WHERE category = $1",
            ),
            (None, Some(_)) => (
                "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
                 FROM inferences WHERE upstream_model = $1 \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
                "SELECT COUNT(*) FROM inferences WHERE upstream_model = $1",
            ),
            (None, None) => (
                "SELECT id, created_at, prompt_snippet, category, upstream_model, duration_ms \
                 FROM inferences ORDER BY created_at DESC LIMIT $1 OFFSET $2",
                "SELECT COUNT(*) FROM inferences",
            ),
        };

        // Execute the count query.
        let total_count: i64 = match (filter_category, filter_model) {
            (Some(cat), Some(model)) => sqlx::query(count_sql)
                .bind(cat)
                .bind(model)
                .fetch_one(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?
                .try_get(0)
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (Some(cat), None) => sqlx::query(count_sql)
                .bind(cat)
                .fetch_one(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?
                .try_get(0)
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (None, Some(model)) => sqlx::query(count_sql)
                .bind(model)
                .fetch_one(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?
                .try_get(0)
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (None, None) => sqlx::query(count_sql)
                .fetch_one(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?
                .try_get(0)
                .map_err(|e| QueryError::Database(e.to_string()))?,
        };

        // Execute the data query.
        let rows = match (filter_category, filter_model) {
            (Some(cat), Some(model)) => sqlx::query(data_sql)
                .bind(cat)
                .bind(model)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (Some(cat), None) => sqlx::query(data_sql)
                .bind(cat)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (None, Some(model)) => sqlx::query(data_sql)
                .bind(model)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?,
            (None, None) => sqlx::query(data_sql)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| QueryError::Database(e.to_string()))?,
        };

        // Map rows to InferenceLog, formatting timestamps and durations.
        let records: Vec<InferenceLog> = rows
            .iter()
            .map(|row| {
                let id: Uuid = row.try_get("id").unwrap_or_default();
                let created_at: chrono::DateTime<chrono::Utc> =
                    row.try_get("created_at").unwrap_or_default();
                let timestamp = created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                let prompt_snippet: String =
                    row.try_get("prompt_snippet").unwrap_or_default();
                let category: Option<String> = row.try_get("category").unwrap_or(None);
                let upstream_model: Option<String> =
                    row.try_get("upstream_model").unwrap_or(None);
                let duration_ms: Option<i32> = row.try_get("duration_ms").unwrap_or(None);

                InferenceLog {
                    id: id.to_string(),
                    timestamp,
                    prompt_snippet,
                    category,
                    upstream_model,
                    duration_ms,
                }
            })
            .collect();

        Ok((records, total_count))
    }
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
        Some(content.chars().take(10_000).collect())
    })();

    match result {
        Some(s) => s,
        None => {
            eprintln!(
                "WARN persistence: could not extract user message from request body; \
                 storing empty prompt"
            );
            String::new()
        }
    }
}

/// Extract a 200-char privacy-safe snippet from an OpenAI-compatible request body.
///
/// Delegates to [`extract_last_user_message`] for JSON parsing and last-user-message
/// logic, then truncates to 200 characters. On any parse failure returns `""`.
/// Never panics, never blocks the response path.
pub fn extract_snippet(body: &str) -> String {
    let full = extract_last_user_message(body);
    full.chars().take(200).collect()
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
    // Intentional fire-and-forget: JoinHandle is dropped — tokio's default
    // panic handler prints any panic in the spawned task to stderr.
    tokio::spawn(async move {
        let request_id = record.request_id;
        let _permit = match semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => {
                eprintln!(
                    "ERROR persistence: semaphore closed for request_id={request_id}"
                );
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
    retry_once(|| insert_once(pool, record))
        .await
        .map_err(|e| {
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
            eprintln!(
                "WARN persistence: first insert attempt failed ({first}); retrying once"
            );
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

    // ── fetch_inferences ─────────────────────────────────────────────────────

    async fn test_pool() -> Option<Arc<PgPool>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        Some(Arc::new(pool))
    }

    fn make_persistence(pool: Arc<PgPool>) -> PersistenceConfig {
        PersistenceConfig {
            pool,
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

    #[tokio::test]
    async fn test_fetch_inferences_empty_list() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_empty_list: DATABASE_URL not set");
                return;
            }
        };
        // Clean slate for this test using a unique category prefix unlikely to collide.
        let pc = make_persistence(pool);
        // Fetch with a filter that will match nothing.
        let result = pc
            .fetch_inferences(0, 20, Some("NONEXISTENT_CATEGORY_XYZ"), None)
            .await;
        let (records, count) = result.expect("fetch should succeed");
        assert!(records.is_empty(), "expected no records");
        assert_eq!(count, 0, "expected count=0");
    }

    #[tokio::test]
    async fn test_fetch_inferences_with_records() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_with_records: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        // Insert a test record.
        let request_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, upstream_model, duration_ms, prompt_snippet) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(request_id)
        .bind("ok")
        .bind("TEST_CAT_FETCH")
        .bind("test-model")
        .bind(42i32)
        .bind("test snippet")
        .execute(pool.as_ref())
        .await
        .expect("insert should succeed");

        let (records, count) = pc
            .fetch_inferences(0, 20, Some("TEST_CAT_FETCH"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = $1")
            .bind(request_id)
            .execute(pool.as_ref())
            .await
            .ok();

        assert!(count >= 1, "expected at least one record");
        let found = records.iter().any(|r| r.prompt_snippet == "test snippet");
        assert!(found, "inserted record should appear in results");
    }

    #[tokio::test]
    async fn test_fetch_inferences_filter_by_category() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_filter_by_category: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let id_a = uuid::Uuid::new_v4();
        let id_b = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
        )
        .bind(id_a)
        .bind("ok")
        .bind("CAT_ALPHA")
        .bind("alpha snippet")
        .execute(pool.as_ref())
        .await
        .ok();
        sqlx::query(
            "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
        )
        .bind(id_b)
        .bind("ok")
        .bind("CAT_BETA")
        .bind("beta snippet")
        .execute(pool.as_ref())
        .await
        .ok();

        let (records, _) = pc
            .fetch_inferences(0, 100, Some("CAT_ALPHA"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(vec![id_a, id_b])
            .execute(pool.as_ref())
            .await
            .ok();

        let has_alpha = records.iter().any(|r| r.prompt_snippet == "alpha snippet");
        let has_beta = records.iter().any(|r| r.prompt_snippet == "beta snippet");
        assert!(has_alpha, "CAT_ALPHA record should appear");
        assert!(!has_beta, "CAT_BETA record should not appear when filtering by CAT_ALPHA");
    }

    #[tokio::test]
    async fn test_fetch_inferences_returns_total_count() {
        let pool = match test_pool().await {
            Some(p) => p,
            None => {
                eprintln!("SKIP test_fetch_inferences_returns_total_count: DATABASE_URL not set");
                return;
            }
        };
        let pc = make_persistence(pool.clone());
        let ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();
        for id in &ids {
            sqlx::query(
                "INSERT INTO inferences (request_id, status, category, prompt_snippet) VALUES ($1,$2,$3,$4)",
            )
            .bind(*id)
            .bind("ok")
            .bind("TOTAL_COUNT_TEST")
            .bind("snippet")
            .execute(pool.as_ref())
            .await
            .ok();
        }

        // Fetch only 1 row but total_count should reflect all matching rows.
        let (records, total_count) = pc
            .fetch_inferences(0, 1, Some("TOTAL_COUNT_TEST"), None)
            .await
            .expect("fetch should succeed");

        // Cleanup.
        sqlx::query("DELETE FROM inferences WHERE request_id = ANY($1)")
            .bind(ids)
            .execute(pool.as_ref())
            .await
            .ok();

        assert_eq!(records.len(), 1, "should return only 1 record (limit=1)");
        assert!(total_count >= 3, "total_count should be at least 3");
    }
}
