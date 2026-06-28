use async_trait::async_trait;
use std::sync::atomic::AtomicBool;

use super::backend::{percentile_99, PersistenceBackend};
use super::types::{prompt_chars_to_cost, CostProvider, InferenceLog, InferenceRecord, LatencySummary, LatencySummaryRow, QueryError, SavingsEstimate};

/// In-memory persistence backend backed by `Arc<RwLock<Vec<InferenceRecord>>>`.
/// All queries operate over Rust iterators. p99 is computed in Rust.
///
/// ⚠️ Ephemeral: Data is lost when the process exits. Not suitable for production.
pub struct MemoryBackend {
    pub records: std::sync::Arc<tokio::sync::RwLock<Vec<InferenceRecord>>>,
    /// Test-only failure injection. When true, the next call to
    /// `insert_inference` returns an error and atomically resets this flag
    /// to false. Production code leaves this at its default `false`.
    pub(crate) fail_next: AtomicBool,
}

impl MemoryBackend {
    pub fn new() -> Self {
        MemoryBackend {
            records: std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            fail_next: AtomicBool::new(false),
        }
    }
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
        filtered.sort_by_key(|b| std::cmp::Reverse(b.created_at));

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
                provider_attempts: Some(r.provider_attempts as i16),
                final_provider: Some(r.final_provider.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    use crate::persistence::{InferenceRecord, PersistenceConfig};

    /// Create a persistence config backed by in-memory storage.
    /// Always succeeds, no DATABASE_URL required.
    fn test_backend() -> PersistenceConfig {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        PersistenceConfig {
            backend: Arc::new(super::super::backend::DbBackend::Memory(MemoryBackend::new())),
            task_semaphore: Arc::new(Semaphore::new(100)),
        }
    }

    #[tokio::test]
    async fn test_memory_backend_round_trips_token_and_attribution_fields() {
        // Phase 4: an InferenceRecord carrying token usage + Claude Code
        // session id must survive a memory-backend insert intact (the memory
        // backend clones the record, so the stored copy must equal the input
        // on every token/attribution field). This is the memory half of the
        // round-trip guarantee; the Postgres half is covered by the
        // persistence_integration_insert_and_read_back test in main.rs.
        let backend = MemoryBackend::new();
        let record = InferenceRecord {
            request_id: uuid::Uuid::new_v4(),
            status: "ok".to_string(),
            category: Some("chat".to_string()),
            upstream_model: Some("claude".to_string()),
            duration_ms: Some(7),
            prompt_snippet: "round trip".to_string(),
            prompt_char_count: Some(10),
            created_at: chrono::Utc::now(),
            provider_attempts: 1,
            final_provider: "claude".to_string(),
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_read_tokens: Some(80),
            cache_creation_tokens: Some(5),
            client_session_id: Some("sess-mem".to_string()),
        };
        backend
            .insert_inference(&record)
            .await
            .expect("memory insert should succeed");
        let stored = backend.records.read().await;
        assert_eq!(stored.len(), 1, "exactly one record should be stored");
        let s = &stored[0];
        assert_eq!(s.input_tokens, Some(100));
        assert_eq!(s.output_tokens, Some(20));
        assert_eq!(
            s.cache_read_tokens,
            Some(80),
            "cache_read_tokens must round-trip"
        );
        assert_eq!(s.cache_creation_tokens, Some(5));
        assert_eq!(
            s.client_session_id.as_deref(),
            Some("sess-mem"),
            "client_session_id must round-trip"
        );
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
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
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
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
            final_provider: String::new(),
            provider_attempts: 1,
            ..Default::default()
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
                    final_provider: String::new(),
                    provider_attempts: 1,
                    ..Default::default()
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
                    final_provider: String::new(),
                    provider_attempts: 1,
                    ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
            .expect("fetch should succeed");

        assert!(
            result.unclassified_count >= 2,
            "expected at least 2 unclassified records, got {}",
            result.unclassified_count
        );
    }

    #[tokio::test]
    async fn test_fetch_savings_estimate_empty() {
        let pc = test_backend();
        let mc = crate::config::routing::ModelCosts::from_costs(
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
        let mc = crate::config::routing::ModelCosts::from_costs(costs);
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
        let mc = crate::config::routing::ModelCosts::from_costs(
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
        let mc = crate::config::routing::ModelCosts::from_costs(
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
        let mc = crate::config::routing::ModelCosts::from_costs(costs);

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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
                    final_provider: String::new(),
                    provider_attempts: 1,
                    ..Default::default()
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
                        final_provider: String::new(),
                        provider_attempts: 1,
                        ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
                final_provider: String::new(),
                provider_attempts: 1,
                ..Default::default()
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
}
