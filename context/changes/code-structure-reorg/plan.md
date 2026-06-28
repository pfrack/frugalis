# Code Structure Reorganization Implementation Plan

## Overview

Reorganize the flat `src/` directory (20,691 lines across 13 files, with a monolithic 8,460-line `main.rs`) into domain-grouped subdirectories. Pure structural refactoring — no behavior changes, no new features. Each phase produces a compiling codebase with all tests passing.

## Current State Analysis

All source files live flat in `src/`. The largest file (`main.rs`) contains handler definitions, streaming logic, utilities, AppState, router assembly, and ~4,900 lines of tests. The module system is shallow — every module is declared directly in `main.rs`.

### Key Discoveries:

- `src/main.rs:87-109` — AppState is a pure data struct with zero `impl` blocks; includes `response_cache: Option<Arc<cache::ResponseCache>>` field
- `src/main.rs:3610-3659` — `build_app()` is the single coupling point between tests and handlers
- Tests call handlers indirectly through Router via `.oneshot()` — handler extraction is transparent to tests
- `src/translate/mod.rs` — 3-line dead code, never declared as `mod translate` in the crate
- `src/cache.rs` (165 lines) — standalone leaf module using `moka`, consumed by `main.rs` and `dashboard.rs`; exports `ResponseCache`, `CachedEntry`, `CacheStats`
- Tight coupling triangle: `routing` ↔ `intent_classifier` ↔ `config` — these share types bidirectionally

## Desired End State

```
src/
├── main.rs              (~250 lines: mod declarations, main(), CLI, shutdown_signal)
├── app.rs               (~80 lines: AppState struct + build_app)
├── proxy/
│   ├── mod.rs           (re-exports, RequestMetrics)
│   ├── handlers.rs      (completion_handler, messages_handler, count_tokens, models, classify, feedback, health)
│   ├── streaming.rs     (handle_streaming_response, handle_anthropic_streaming, keepalive, SSE error handling)
│   ├── upstream.rs      (build_upstream_request, handle_buffered_response, translate helpers)
│   └── util.rs          (collect_forward_headers, sanitize_for_nim, is_retryable_error, try_optimize, json_response helpers, UsageBreakdown, logging helpers)
├── classification/
│   ├── mod.rs           (re-exports)
│   ├── chain.rs         (ClassifierChain, IntentClassify trait)
│   ├── regex.rs         (RegexClassifier)
│   ├── llm.rs           (LLMClassifier, build_llm_classifier_prompt, auth_headers_for)
│   ├── fewshot.rs       (FewShotClassifier)
│   └── types.rs         (ClassificationResult, ClassificationTier, CategoryConfig, NegativePatternConfig, FewShotExample, PatternMeta)
├── protocol/
│   ├── mod.rs           (re-exports)
│   ├── request.rs       (translate_request, anthropic_to_openai_request, anthropic_to_openai_request_with_cache_signal)
│   ├── response.rs      (translate_response, translate_error, openai_to_anthropic_response, openai_to_anthropic_error)
│   └── stream.rs        (StreamTranslateState, AnthropicStreamState, parse_sse_events, translate_stream_event, openai_to_anthropic_stream_event)
├── config/
│   ├── mod.rs           (re-exports, ConfigRoot)
│   ├── loader.rs        (all load_* functions, merge_configs, run_validation)
│   ├── routing.rs       (RouteEntry, ProviderEntry, ModelCosts, routing_from_value, hardcoded_routing, build_model_costs, DEFAULT_MODEL, DEFAULT_MODEL_COMPLEX, DEFAULT_MODEL_LOCAL)
│   └── types.rs         (ServerConfig, HttpConfig, DatabaseConfig, AuthProviderConfig, DashboardConfig, CorsConfig, PersistenceSettings, FewShotConfig, LlmClassifierConfig, CacheConfig, ClassifiersConfig, RegexClassifierConfig)
├── persistence/
│   ├── mod.rs           (re-exports, PersistenceConfig, log_inference, enqueue helpers)
│   ├── backend.rs       (PersistenceBackend trait, DbBackend enum, CostProvider trait)
│   ├── sqlite.rs        (SqliteBackend)
│   ├── postgres.rs      (PostgresBackend)
│   ├── memory.rs        (MemoryBackend)
│   └── types.rs         (InferenceRecord, InferenceLog, LatencySummaryRow, LatencySummary, SavingsEstimate, QueryError, extract_* helpers, prompt_chars_to_cost)
├── dashboard.rs         (unchanged — 355 lines, already clean)
├── auth.rs              (unchanged — 283 lines, leaf module)
├── cache.rs             (unchanged — 165 lines, leaf module)
├── telemetry.rs         (unchanged — 192 lines, leaf module)
├── quickstart.rs        (unchanged — 474 lines, leaf module)
└── test_util.rs         (unchanged — 12 lines, #[cfg(test)])
```

Verification: `cargo build && cargo build --features otel && cargo test && cargo clippy`

## What We're NOT Doing

- **No behavior changes** — identical binary output, identical API surface
- **No AppState decomposition** — keeping monolithic struct, just moving it
- **No `lib.rs`** — remains a binary crate
- **No dependency changes** — Cargo.toml unchanged
- **No new public API** — everything stays `pub(crate)` or less
- **No test rewriting** — tests move as-is with `use super::*` or updated paths

## Implementation Approach

Move code bottom-up by coupling risk. Start with modules that have zero intra-crate dependencies (leaf modules), then move the coupled cluster together, then finally extract from main.rs. Each phase updates all `use crate::` paths to use the new exposed submodule paths (per Q4 decision).

## Critical Implementation Details

**Import path strategy**: The user chose exposed submodule paths. After each module extraction, ALL consumers must update their imports. For example, `use crate::persistence::InferenceRecord` becomes `use crate::persistence::types::InferenceRecord`. This means each phase requires a grep-and-replace pass across the entire `src/` tree. Do not leave old re-exports — the submodule path IS the canonical path.

**Test compilation**: Tests in main.rs use `use super::*` which pulls in all `mod` declarations from main.rs scope. As handlers move out, tests that reference handlers by name will break — but since tests use Router + `.oneshot()` (never calling handlers directly), only `build_app()` needs to remain visible. Test helper functions that build AppState will need updated field paths.

---

## Phase 1: Dead Code Cleanup + Leaf Module Extraction (protocol, persistence)

### Overview

Delete dead code, then extract the two largest zero/minimal-coupling modules into directory structures. These are the safest moves.

### Changes Required:

#### 1. Delete dead code

**File**: `src/translate/mod.rs` (DELETE entire file + directory)

**Intent**: Remove the unused `translate/` directory. It's never declared as `mod translate` anywhere — pure dead code.

#### 2. Extract protocol_translation → protocol/

**Files**: Delete `src/protocol_translation.rs`, create `src/protocol/mod.rs`, `src/protocol/request.rs`, `src/protocol/response.rs`, `src/protocol/stream.rs`

**Intent**: Split the 3,165-line standalone module into logical subfiles by responsibility (request translation, response translation, stream translation). This module has zero intra-crate dependencies so the split is purely internal.

**Contract**:
- `src/protocol/mod.rs` — declares `pub(crate) mod request; pub(crate) mod response; pub(crate) mod stream;`
- `src/protocol/request.rs` — all request translation functions (`translate_request`, `anthropic_to_openai_request`, `anthropic_to_openai_request_with_cache_signal`)
- `src/protocol/response.rs` — all response translation functions (`translate_response`, `translate_error`, `openai_to_anthropic_response`, `openai_to_anthropic_error`)
- `src/protocol/stream.rs` — `StreamTranslateState`, `AnthropicStreamState`, `parse_sse_events`, `translate_stream_event`, `openai_to_anthropic_stream_event`
- `src/main.rs` — change `mod protocol_translation;` to `mod protocol;`, update all `protocol_translation::X` references to `protocol::request::X`, `protocol::response::X`, or `protocol::stream::X`
- Inline `#[cfg(test)] mod tests` within each subfile for the tests that exercised those functions

#### 3. Extract persistence → persistence/

**Files**: Delete `src/persistence.rs`, create `src/persistence/mod.rs`, `src/persistence/backend.rs`, `src/persistence/sqlite.rs`, `src/persistence/postgres.rs`, `src/persistence/memory.rs`, `src/persistence/types.rs`

**Intent**: Split the 2,727-line module into one file per backend + shared types/trait. Only external dependency is `crate::config::DatabaseConfig`.

**Contract**:
- `src/persistence/mod.rs` — declares submodules, contains `PersistenceConfig` struct and `log_inference` + enqueue helper functions
- `src/persistence/backend.rs` — `PersistenceBackend` trait, `DbBackend` enum, `CostProvider` trait
- `src/persistence/types.rs` — `InferenceRecord`, `InferenceLog`, `LatencySummaryRow`, `LatencySummary`, `SavingsEstimate`, `QueryError`, `extract_last_user_message`, `extract_last_user_message_anthropic`, `extract_snippet`, `prompt_chars_to_cost`
- `src/persistence/sqlite.rs` — `SqliteBackend` impl
- `src/persistence/postgres.rs` — `PostgresBackend` impl  
- `src/persistence/memory.rs` — `MemoryBackend` impl
- Update all consumers: `main.rs`, `dashboard.rs` — change `persistence::X` to `persistence::types::X`, `persistence::backend::DbBackend`, etc.
- The import `use crate::config::DatabaseConfig` in persistence stays as-is (config hasn't moved yet)
- Inline tests stay with their respective subfiles

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds with no warnings
- `cargo build --features otel` succeeds
- `cargo test` — all tests pass
- `cargo clippy` — no new warnings
- `src/translate/` directory no longer exists

#### Manual Verification:

- Confirm `src/protocol/` has 4 files (mod.rs, request.rs, response.rs, stream.rs)
- Confirm `src/persistence/` has 6 files (mod.rs, backend.rs, types.rs, sqlite.rs, postgres.rs, memory.rs)

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Config + Classification Cluster Extraction

### Overview

Extract the tight coupling triangle together. `config.rs` absorbs `routing.rs` types. `intent_classifier.rs` + `fewshot_classifier.rs` merge into `classification/`. These modules share types bidirectionally, so they must move in the same phase to avoid intermediate breakage.

### Changes Required:

#### 1. Extract config + routing → config/

**Files**: Delete `src/config.rs` and `src/routing.rs`, create `src/config/mod.rs`, `src/config/loader.rs`, `src/config/routing.rs`, `src/config/types.rs`

**Intent**: Absorb `routing.rs` (159 lines of pure config types) into the config directory. Split config's 2,476 lines by responsibility.

**Contract**:
- `src/config/mod.rs` — declares submodules, contains `ConfigRoot`
- `src/config/types.rs` — all struct definitions: `ServerConfig`, `HttpConfig`, `DatabaseConfig`, `AuthProviderConfig`, `DashboardConfig`, `CorsConfig`, `PersistenceSettings`, `FewShotConfig`, `LlmClassifierConfig`, `CacheConfig`, `ClassifiersConfig`, `RegexClassifierConfig`
- `src/config/loader.rs` — all `load_*` functions, `merge_configs`, `run_validation`
- `src/config/routing.rs` — `RouteEntry`, `ProviderEntry`, `ModelCosts` (from old `routing.rs`), `routing_from_value`, `hardcoded_routing`, `build_model_costs`, `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX`, `DEFAULT_MODEL_LOCAL`
- Update `main.rs`: `mod config;` stays, remove `mod routing;`. Update all `config::X` paths to submodule paths. Update all `routing::X` to `config::routing::X`
- Update `persistence/backend.rs`: `CostProvider` trait references `ModelCosts` — update import path to `crate::config::routing::ModelCosts`
- Inline tests move with their code

#### 2. Extract intent_classifier + fewshot_classifier → classification/

**Files**: Delete `src/intent_classifier.rs` and `src/fewshot_classifier.rs`, create `src/classification/mod.rs`, `src/classification/chain.rs`, `src/classification/regex.rs`, `src/classification/llm.rs`, `src/classification/fewshot.rs`, `src/classification/types.rs`

**Intent**: Merge the two classifier modules into one domain directory. The re-exports that `intent_classifier` had from `routing` now point to `config::routing::`.

**Contract**:
- `src/classification/mod.rs` — declares submodules
- `src/classification/types.rs` — `ClassificationResult`, `ClassificationTier`, `CategoryConfig`, `NegativePatternConfig`, `FewShotExample`, `PatternMeta`
- `src/classification/chain.rs` — `IntentClassify` trait, `ClassifierChain`
- `src/classification/regex.rs` — `RegexClassifier`
- `src/classification/llm.rs` — `LLMClassifier`, `build_llm_classifier_prompt`, `auth_headers_for`
- `src/classification/fewshot.rs` — `FewShotClassifier`
- `src/main.rs` — remove `mod intent_classifier;` and `mod fewshot_classifier;`, add `mod classification;`. Update `use intent_classifier::IntentClassify` → `use classification::chain::IntentClassify`. Update all `intent_classifier::X` and `fewshot_classifier::X` references to new submodule paths
- `src/config/loader.rs` — update `use crate::intent_classifier::{CategoryConfig, NegativePatternConfig}` → `use crate::classification::types::{CategoryConfig, NegativePatternConfig}`
- Inline tests (including `test_util::CountingClassifier`) move into respective subfiles

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test` — all tests pass
- `cargo clippy` — no new warnings
- `src/routing.rs`, `src/intent_classifier.rs`, `src/fewshot_classifier.rs` no longer exist

#### Manual Verification:

- Confirm `src/config/` has 4 files
- Confirm `src/classification/` has 6 files
- Spot-check that no `use crate::routing::` or `use crate::intent_classifier::` imports remain anywhere

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Proxy Extraction from main.rs

### Overview

Extract ~2,170 lines of handler and proxy logic from main.rs into `src/proxy/`. This is the largest single extraction. After this phase, main.rs shrinks from ~8,460 lines to ~5,500 lines (mostly tests + bootstrap).

### Changes Required:

#### 1. Create proxy/ directory structure

**Files**: Create `src/proxy/mod.rs`, `src/proxy/handlers.rs`, `src/proxy/streaming.rs`, `src/proxy/upstream.rs`, `src/proxy/util.rs`

**Intent**: Group all proxy/gateway functionality — handlers, streaming, upstream request building, and utilities — into one domain directory.

**Contract**:
- `src/proxy/mod.rs` — declares submodules + contains `RequestMetrics` struct and its `impl` behind `#[cfg(feature = "otel")]`
- `src/proxy/handlers.rs` — `health`, `completion_handler`, `messages_handler`, `count_tokens_handler`, `models_handler`, `classify_handler`, `feedback_handler`, `FeedbackRequest`, `default_satisfaction`
- `src/proxy/streaming.rs` — `handle_streaming_response`, `handle_anthropic_streaming_response`, `handle_streaming_error`, `handle_anthropic_streaming_error`, `handle_streaming_error_with_transform`, `handle_translating_anthropic_stream`
- `src/proxy/upstream.rs` — `build_upstream_request`, `handle_buffered_response`, `translate_anthropic_buffered_response`, `translate_openai_buffered_to_anthropic`, `is_retryable_error`
- `src/proxy/util.rs` — `collect_forward_headers`, `sanitize_for_nim`, `try_optimize_request`, `json_response`, `upstream_error_json`, `anthropic_error_json`, `classification_only_json`, `session_id_from_forward`, `UsageBreakdown`, `extract_anthropic_usage`, `extract_openai_usage`, `parse_usage_from_body`, `log_classification`, `log_classification_with_usage`, `enqueue_inference_record`, `classify_and_log`
- All extracted functions receive `AppState` via `State<Arc<AppState>>` — they need `use crate::app::AppState`
- `src/main.rs` — add `mod proxy;`, remove the extracted function bodies, update `build_app()` handler references to `proxy::handlers::completion_handler` etc.

#### 2. Update build_app to reference proxy handlers

**File**: `src/main.rs` (or wherever `build_app` currently lives — it stays in main.rs for now, moves in Phase 4)

**Intent**: Wire the router to the new handler locations.

**Contract**: All `.route()` calls in `build_app` change from bare function names to `proxy::handlers::X`. For example: `.route("/chat/completions", post(proxy::handlers::completion_handler))`.

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test` — all tests pass (tests still live in main.rs, still compile via `build_app`)
- `cargo clippy` — no new warnings
- `main.rs` no longer contains any handler function definitions (only `main()`, `run_init()`, `shutdown_signal()`, `build_app()`, AppState, tests)

#### Manual Verification:

- Confirm `src/proxy/` has 5 files
- `wc -l src/main.rs` is ~5,500 lines or less (down from 8,460)
- Spot-check that `cargo run -- --help` still works

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 4: AppState + Router Extraction + Test Reorganization

### Overview

Move AppState and `build_app()` to `app.rs`. Move tests from main.rs into the modules they test. This is the final phase — after it, main.rs is ~250 lines.

### Changes Required:

#### 1. Create app.rs

**File**: Create `src/app.rs`

**Intent**: Give AppState and the router assembly function a proper home outside main.rs.

**Contract**:
- `src/app.rs` — contains `pub(crate) struct AppState { ... }` and `pub(crate) fn build_app(auth_config: Arc<auth::AuthConfig>, app_state: Arc<AppState>) -> Router`
- `src/main.rs` — add `mod app;`, remove AppState definition and `build_app()`, add `use app::AppState;` where needed
- `src/proxy/handlers.rs` — update `use crate::AppState` to `use crate::app::AppState`
- `src/dashboard.rs` — update `use crate::AppState` to `use crate::app::AppState`

#### 2. Move tests to their domain modules

**File**: `src/main.rs` `mod tests` and `mod slow_tests` blocks → distributed into `src/proxy/handlers.rs`, `src/proxy/streaming.rs`, `src/classification/mod.rs`, etc.

**Intent**: Co-locate tests with the code they exercise. Each module gets its own `#[cfg(test)] mod tests` block.

**Contract**:
- Proxy handler tests → `src/proxy/handlers.rs` `#[cfg(test)] mod tests`
- Streaming tests → `src/proxy/streaming.rs` `#[cfg(test)] mod tests`
- Protocol translation tests (if any remain in main) → `src/protocol/` subfiles
- Dashboard tests → `src/dashboard.rs` `#[cfg(test)] mod tests`
- Slow tests → `src/proxy/streaming.rs` `#[cfg(test)] mod slow_tests` (they test keepalive/streaming behavior)
- Test helper functions (`test_categories`, `make_test_app_state`, `test_app`, `test_app_with_*`) → `src/app.rs` `#[cfg(test)] mod test_helpers` (shared across test modules via `pub(crate)`)
- `src/test_util.rs` — stays as-is (used by persistence tests via `crate::test_util::EnvGuard`)

#### 3. Clean up main.rs

**File**: `src/main.rs`

**Intent**: main.rs becomes a thin entry point: module declarations, `main()` function (CLI + bootstrap), `run_init()`, `shutdown_signal()`, and the `INIT_TEMPLATE` const.

**Contract**: Final main.rs contains only:
- `mod` declarations for all top-level modules
- `const INIT_TEMPLATE: &str = include_str!("../init_template.toml");`
- `fn run_init(path: Option<String>)` 
- `async fn main()` (CLI parsing, config loading, tracing init, classifier/persistence construction, AppState assembly, server start)
- `async fn shutdown_signal()`

### Success Criteria:

#### Automated Verification:

- `cargo build` succeeds
- `cargo build --features otel` succeeds
- `cargo test` — all tests pass (same count as before)
- `cargo clippy` — no new warnings
- `wc -l src/main.rs` is ≤ 300 lines

#### Manual Verification:

- `cargo run -- --help` prints help and exits
- `cargo run -- --validate` with a valid config.toml validates successfully
- Confirm the test count is unchanged: `cargo test 2>&1 | grep 'test result'` shows same number of tests as before Phase 1

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Testing Strategy

### Unit Tests:

- All existing ~105 tests move with their code — no test rewriting
- Tests use `use super::*` pattern within each module's `#[cfg(test)]` block
- Shared test helpers (AppState builders) exposed via `pub(crate)` in `app.rs`

### Integration Tests:

- `test_app_with_http_client` and `build_app_with_persistence` helpers still produce a full Router
- No new integration test infrastructure needed — existing tests cover the full stack

### Manual Testing Steps:

1. `cargo run -- --help` works
2. `cargo run -- --init /tmp/test.toml` writes template
3. Start server with valid config, hit `/health` endpoint
4. Compare test count before and after: should be identical

## Performance Considerations

None — this is a pure compile-time refactoring. Zero runtime behavior changes. Binary output should be identical modulo debug symbol paths.

## Migration Notes

No data migration. No config changes. No API changes. Downstream consumers (Dockerfile, CI, deploy scripts) are unaffected since `cargo build` and the binary name are unchanged.

## References

- Related research: `context/changes/code-structure-reorg/research.md`
- Coupling analysis: research doc "Module Coupling Matrix" section
- Lesson (dead code): `context/foundation/lessons.md` — "Delete dead code rather than suppressing warnings"
- Lesson (DB queries): `context/foundation/lessons.md` — "Favor dynamic WHERE clause building" (relevant to persistence split)

## Progress

> Convention: `- [ ]` pending, `- [x]` done. Append ` — <commit sha>` when a step lands. Do not rename step titles. See `references/progress-format.md`.

### Phase 1: Dead Code Cleanup + Leaf Module Extraction

#### Automated

- [x] 1.1 `cargo build` succeeds with no warnings — 239d48d
- [x] 1.2 `cargo build --features otel` succeeds — 239d48d
- [x] 1.3 `cargo test` — all tests pass — 239d48d
- [x] 1.4 `cargo clippy` — no new warnings — 239d48d
- [x] 1.5 `src/translate/` directory no longer exists — 239d48d

#### Manual

- [x] 1.6 Confirm `src/protocol/` has 4 files — 239d48d
- [x] 1.7 Confirm `src/persistence/` has 6 files — 239d48d

### Phase 2: Config + Classification Cluster Extraction

#### Automated

- [x] 2.1 `cargo build` succeeds — e2416f9
- [x] 2.2 `cargo build --features otel` succeeds — e2416f9
- [x] 2.3 `cargo test` — all tests pass — e2416f9
- [x] 2.4 `cargo clippy` — no new warnings — e2416f9
- [x] 2.5 `src/routing.rs`, `src/intent_classifier.rs`, `src/fewshot_classifier.rs` no longer exist — e2416f9

#### Manual

- [x] 2.6 Confirm `src/config/` has 4 files — e2416f9
- [x] 2.7 Confirm `src/classification/` has 6 files — e2416f9
- [x] 2.8 No `use crate::routing::` or `use crate::intent_classifier::` imports remain — e2416f9

### Phase 3: Proxy Extraction from main.rs

#### Automated

- [x] 3.1 `cargo build` succeeds — 3d1e6a3
- [x] 3.2 `cargo build --features otel` succeeds — 3d1e6a3
- [x] 3.3 `cargo test` — all tests pass — 3d1e6a3
- [x] 3.4 `cargo clippy` — no new warnings — 3d1e6a3
- [x] 3.5 main.rs no longer contains handler function definitions — 3d1e6a3

#### Manual

- [x] 3.6 Confirm `src/proxy/` has 5 files — 3d1e6a3
- [x] 3.7 `wc -l src/main.rs` ≤ 5,500 lines — 3d1e6a3
- [x] 3.8 `cargo run -- --help` still works — 3d1e6a3

### Phase 4: AppState + Router Extraction + Test Reorganization

#### Automated

- [x] 4.1 `cargo build` succeeds — 0ef70aa
- [x] 4.2 `cargo build --features otel` succeeds — 0ef70aa
- [x] 4.3 `cargo test` — all tests pass — 0ef70aa
- [x] 4.4 `cargo clippy` — no new warnings — 0ef70aa
- [x] 4.5 `wc -l src/main.rs` ≤ 300 lines — 0ef70aa

#### Manual

- [x] 4.6 `cargo run -- --help` prints help — 0ef70aa
- [x] 4.7 `cargo run -- --validate` validates config — 0ef70aa
- [x] 4.8 Test count unchanged from before Phase 1 — 0ef70aa
