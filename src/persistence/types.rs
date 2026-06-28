use tracing::warn;
use uuid::Uuid;

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
    #[allow(dead_code)]
    pub provider_attempts: Option<i16>,
    #[allow(dead_code)]
    pub final_provider: Option<String>,
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

/// Finalized inference metadata payload ready for background persistence.
///
/// `Default` is derived so test fixtures and the streaming open-path can omit
/// the optional token/attribution fields via `..Default::default()`; the
/// production builder (`enqueue_inference_record` in main.rs) sets every field
/// explicitly, so deriving Default does not relax production invariants.
#[derive(Clone, Default)]
pub struct InferenceRecord {
    pub request_id: Uuid,
    pub status: String,
    pub category: Option<String>,
    pub upstream_model: Option<String>,
    pub duration_ms: Option<i32>,
    pub prompt_snippet: String,
    pub prompt_char_count: Option<i32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub provider_attempts: u8,
    pub final_provider: String,
    // ── Token usage + Claude Code attribution (all nullable so existing rows
    //    and the memory backend stay valid; populated only when the upstream
    //    reports usage and the client sends attribution headers). ──────────
    /// Non-cached input tokens (Anthropic input_tokens / OpenAI prompt minus
    /// cached). `None` when usage was not captured.
    pub input_tokens: Option<i32>,
    /// Completion/output tokens. `None` when usage was not captured.
    pub output_tokens: Option<i32>,
    /// Cache-read hit tokens (savings). `None` when usage was not captured.
    pub cache_read_tokens: Option<i32>,
    /// Cache-creation tokens (write cost). `None` when usage was not captured
    /// or the upstream has no concept (OpenAI).
    pub cache_creation_tokens: Option<i32>,
    /// Claude Code session id from `x-claude-code-session-id`, for per-session
    /// attribution. `None` when the header was absent.
    pub client_session_id: Option<String>,
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
            warn!(
                "could not extract user message from Anthropic request body; storing empty prompt"
            );
            String::new()
        }
    }
}

/// Convert character count to estimated dollar cost.
///
/// Uses a simple 4-characters-to-1-token heuristic. Rounds to 6 decimal places.
pub fn prompt_chars_to_cost(char_count: i32, cost_per_1m_input_tokens: f64) -> f64 {
    let tokens = char_count as f64 / 4.0;
    let cost = tokens * cost_per_1m_input_tokens / 1_000_000.0;
    (cost * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
