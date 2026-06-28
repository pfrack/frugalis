---
date: 2026-06-28T08:09:13+02:00
researcher: kiro
git_commit: 32f2867bb47ce3fd35ec5f5b788387a08a5b4396
branch: rename
repository: frugalis
topic: "Code structure reorganization - flat src/ to domain directories"
tags: [research, codebase, architecture, refactoring, modules]
status: complete
last_updated: 2026-06-28
last_updated_by: kiro
---

# Research: Code Structure Reorganization

**Date**: 2026-06-28T08:09:13+02:00
**Researcher**: kiro
**Git Commit**: 32f2867bb47ce3fd35ec5f5b788387a08a5b4396
**Branch**: rename
**Repository**: frugalis

## Research Question

The entire Rust source lives flat in `src/`. Should we introduce directories to group by domain?

## Summary

The project has **20,691 lines** across 12 source files, with `main.rs` alone at 8,460 lines (41% of total). The flat structure hides 7 distinct domains mixed together. Analysis reveals a clear reorganization path:

1. **Four leaf modules** (auth, protocol_translation, quickstart, telemetry) can be moved to directories independently — zero coupling risk.
2. **One tight coupling cluster** (routing ↔ intent_classifier ↔ config) should move together into a `classification/` domain.
3. **main.rs must be decomposed** — it contains ~2,170 lines of proxy handler logic, ~540 lines of utilities, and ~4,900 lines of tests that can be extracted.

## Detailed Findings

### Current Source Layout

```
src/
├── main.rs                  8,460 lines  (handlers, AppState, router, tests)
├── protocol_translation.rs  3,165 lines  (OpenAI↔Anthropic translation)
├── persistence.rs           2,727 lines  (DB backends, inference logging)
├── config.rs                2,476 lines  (TOML parsing, validation)
├── intent_classifier.rs     1,838 lines  (regex/LLM classifiers, chain)
├── fewshot_classifier.rs      547 lines  (few-shot classifier)
├── quickstart.rs              474 lines  (interactive setup wizard)
├── dashboard.rs               355 lines  (web UI handlers)
├── auth.rs                    283 lines  (bearer/basic auth middleware)
├── telemetry.rs               192 lines  (OpenTelemetry setup)
├── cache.rs                   165 lines  (response cache, moka-backed)
├── routing.rs                 159 lines  (route table types)
├── test_util.rs                12 lines  (test helpers)
└── translate/mod.rs             3 lines  ★ DEAD CODE — unused module
```

### Domain Map (from main.rs analysis)

| Domain | Lines in main.rs | External modules | Total ~lines |
|--------|-----------------|------------------|-------------|
| Proxy/Gateway (OpenAI) | 1,560 | — | 1,560 |
| Proxy/Gateway (Anthropic) | 609 | — | 609 |
| Protocol Translation | — | protocol_translation.rs | 3,165 |
| Classification | 81 + `classify_and_log` | intent_classifier, fewshot_classifier | 2,466 |
| Persistence/Logging | ~100 | persistence.rs | 2,827 |
| Config | — | config.rs, routing.rs | 2,635 |
| Dashboard | — | dashboard.rs | 355 |
| Auth | — | auth.rs | 283 |
| Telemetry | — | telemetry.rs | 192 |
| Bootstrap/CLI | 707 | quickstart.rs | 1,181 |
| Utilities | 540 | — | 540 |
| Tests | 4,927 | test_util.rs | 4,939 |

### Module Coupling Matrix

**Tight coupling triangle:**
- `config` → imports from `routing` (wildcard) + `intent_classifier` (CategoryConfig)
- `intent_classifier` → imports from `routing` (re-exports types) + `config` (AuthProviderConfig)
- `routing` → implements `persistence::CostProvider`

**Linear chain:**
- `fewshot_classifier` → `intent_classifier` → `routing`

**Leaf modules (zero intra-crate dependencies):**
- `auth.rs` — standalone
- `protocol_translation.rs` — standalone
- `quickstart.rs` — standalone
- `telemetry.rs` — standalone

**Dashboard:**
- Depends on `auth` + `persistence` + `AppState` (from crate root)

### AppState Fields (16 fields, 7 domains)

```rust
pub struct AppState {
    // Persistence
    persistence: Option<PersistenceConfig>,
    // Classification
    classifier: Option<Arc<ClassifierChain>>,
    fewshot_classifier: Option<Arc<FewShotClassifier>>,
    // Routing/Gateway
    routing: Arc<RwLock<HashMap<String, RouteEntry>>>,
    http_client: Option<reqwest::Client>,
    max_upstream_body_bytes: Arc<RwLock<usize>>,
    request_body_limit_bytes: usize,
    auth_providers: Arc<Vec<AuthProviderConfig>>,
    // Streaming
    keepalive_interval_secs: Arc<RwLock<u64>>,
    streaming_channel_capacity: usize,
    // Cost Accounting
    model_costs: Arc<RwLock<ModelCosts>>,
    baseline_model: Arc<RwLock<String>>,
    // Config flags
    classify_db_log: Arc<AtomicBool>,
    // Dashboard
    dashboard_config: DashboardConfig,
    // CORS
    allowed_origins: Arc<RwLock<Vec<String>>>,
    // Observability
    metrics: Option<telemetry::Metrics>,
}
```

## Proposed Directory Structure

```
src/
├── main.rs                  (~200 lines: mod declarations, main(), CLI dispatch)
├── app.rs                   (~100 lines: AppState struct + build_app router)
│
├── proxy/
│   ├── mod.rs               (re-exports)
│   ├── handlers.rs          (completion_handler, messages_handler, count_tokens, models_handler)
│   ├── streaming.rs         (handle_streaming_response, keepalive, SSE utilities)
│   ├── upstream.rs          (build_upstream_request, handle_buffered_response, retry logic)
│   └── util.rs              (collect_forward_headers, sanitize_for_nim, is_retryable_error, try_optimize_request)
│
├── classification/
│   ├── mod.rs               (re-exports, classify_handler, feedback_handler, classify_and_log)
│   ├── chain.rs             (ClassifierChain, IntentClassify trait)
│   ├── regex.rs             (RegexClassifier)
│   ├── llm.rs               (LLMClassifier, build_llm_classifier_prompt)
│   ├── fewshot.rs           (FewShotClassifier)
│   └── types.rs             (ClassificationResult, ClassificationTier, CategoryConfig, FewShotExample)
│
├── protocol/
│   ├── mod.rs               (re-exports)
│   ├── translate_request.rs (anthropic_to_openai_request, translate_request)
│   ├── translate_response.rs(translate_response, openai_to_anthropic_response)
│   └── translate_stream.rs  (StreamTranslateState, translate_stream_event, parse_sse_events)
│
├── config/
│   ├── mod.rs               (re-exports, ConfigRoot)
│   ├── loader.rs            (load_*, merge_configs, run_validation)
│   ├── routing.rs           (RouteEntry, ProviderEntry, ModelCosts, routing_from_value)
│   └── types.rs             (ServerConfig, HttpConfig, DatabaseConfig, AuthProviderConfig, etc.)
│
├── persistence/
│   ├── mod.rs               (re-exports, PersistenceConfig, log_inference)
│   ├── backend.rs           (PersistenceBackend trait, DbBackend enum)
│   ├── sqlite.rs            (SqliteBackend)
│   ├── postgres.rs          (PostgresBackend)
│   ├── memory.rs            (MemoryBackend)
│   └── types.rs             (InferenceRecord, InferenceLog, LatencySummaryRow, etc.)
│
├── dashboard/
│   ├── mod.rs               (routes, nav)
│   └── handlers.rs          (index, inferences, latency, savings handlers)
│
├── auth.rs                  (unchanged — already clean leaf module)
├── telemetry.rs             (unchanged — already clean leaf module)
├── quickstart.rs            (unchanged — already clean leaf module)
│
└── tests/                   (integration tests extracted from main.rs)
    ├── mod.rs               (shared test helpers, TestApp builder)
    ├── proxy_tests.rs       (upstream, streaming, buffered tests)
    ├── classification_tests.rs
    ├── protocol_tests.rs    (Anthropic↔OpenAI translation)
    ├── dashboard_tests.rs
    └── slow_tests.rs        (timing-dependent tests)
```

### Migration Priority (by risk & effort)

| Phase | What moves | Risk | Reason |
|-------|-----------|------|--------|
| 1 | Delete `translate/mod.rs` | None | Dead code |
| 2 | Extract `protocol/` from `protocol_translation.rs` | Low | Zero coupling, just split large file |
| 3 | Extract `persistence/` from `persistence.rs` | Low | Only depends on `config::DatabaseConfig` |
| 4 | Extract `config/` + merge `routing.rs` into it | Medium | Part of tight coupling triangle |
| 5 | Extract `classification/` from `intent_classifier.rs` + `fewshot_classifier.rs` | Medium | Coupled to config + routing |
| 6 | Extract `proxy/` from main.rs handlers | Medium-High | Largest change, most handler logic |
| 7 | Extract `app.rs` (AppState + router) from main.rs | Medium | Depends on all domains |
| 8 | Extract `tests/` from inline main.rs tests | Low (after 6-7) | Mechanical once handlers are in modules |

### Key Design Decisions

1. **`config/routing.rs` absorbs current `routing.rs`** — The 159-line `routing.rs` is really config types (RouteEntry, ProviderEntry, ModelCosts). It belongs with config rather than as a standalone module.

2. **`classification/` groups the tight coupling triangle** — Rather than fighting the coupling between `intent_classifier` ↔ `config(categories)` ↔ `routing(types)`, group them in one domain module where the coupling becomes internal cohesion.

3. **`proxy/` is the biggest win** — Extracting ~2,170 lines of handler logic from main.rs makes the file navigable. The streaming/buffered/upstream split maps to distinct responsibilities.

4. **Tests stay near their code initially** — Each `src/proxy/handlers.rs` can have a `#[cfg(test)] mod tests` block. The 4,900-line test extraction from main.rs can happen in phase 8 once the handlers live in their own files.

5. **Leaf modules stay as single files** — `auth.rs`, `telemetry.rs`, `quickstart.rs` are small enough and self-contained enough that wrapping them in directories adds complexity without benefit.

## Architecture Insights

- The current structure emerged from organic growth — the project grew from ~5 files to 12 without reorganizing.
- `main.rs` is doing the work of 5-6 files: CLI bootstrap, handler definitions, response utilities, streaming logic, and test infrastructure.
- The coupling triangle (routing ↔ intent_classifier ↔ config) is architecturally sound — these modules genuinely need each other's types. The fix isn't decoupling; it's acknowledging they're one domain.
- `protocol_translation.rs` at 3,165 lines with zero intra-crate dependencies is the cleanest extraction target.

## Historical Context

- `context/archive/2026-06-22-translate-anthropic-to-openai/` — Protocol translation was added as a standalone module by design
- `context/archive/2026-06-07-shared-category-config/` — Category config was explicitly shared between classification and config modules
- `context/archive/2026-06-09-fewshot-classifier/` — Fewshot was designed to implement the same IntentClassify trait

## Open Questions

1. **Test organization**: Should extracted handler tests use `#[cfg(test)] mod tests` inline (Rust convention) or move to a `tests/` directory inside `src/`? The inline approach is simpler for `cargo test` but the tests add significant line count.

2. **Re-export strategy**: Should `src/lib.rs` exist? Currently this is a binary crate only. Adding `lib.rs` would enable external integration testing but adds complexity.

3. **Feature flag boundaries**: The `otel` feature flag touches main.rs (RequestMetrics) and telemetry.rs. After reorg, should the metrics recording live in `proxy/` (where requests happen) or stay centralized?

4. **`AppState` decomposition**: Should AppState be split into domain-specific sub-states (ProxyState, ClassificationState, etc.) or remain monolithic? Splitting would reduce coupling but requires more boilerplate with Axum's State extractor.
