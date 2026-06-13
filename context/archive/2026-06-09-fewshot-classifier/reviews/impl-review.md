<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Few-Shot Intent Classifier

- **Plan**: context/changes/fewshot-classifier/plan.md
- **Scope**: All Phases (1–5)
- **Date**: 2026-06-13
- **Verdict**: APPROVED
- **Findings**: 0 critical · 3 warnings · 4 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | PASS |
| Architecture | PASS |
| Pattern Consistency | PASS |
| Success Criteria | PASS |

## Findings

### F1 — Regex compiled on every preprocess() call

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality (Performance)
- **Location**: src/fewshot_classifier.rs:67
- **Detail**: `preprocess()` calls `Regex::new(r"(?s)```[^`]*```")` on every invocation — once per classify call and once per training example during retrain. The existing `intent_classifier.rs` already provides a lazily-compiled singleton via `code_block_re()` using `OnceLock` (lines 433-435). Regex compilation is expensive relative to the match itself.
- **Fix**: Replace with a `OnceLock`-based static or reuse `code_block_re()` from `intent_classifier.rs`.
- **Decision**: FIXED (reused existing `code_block_re()` after making it `pub(crate)`)

### F2 — Synchronous file I/O in async context

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality (Performance/Reliability)
- **Location**: src/fewshot_classifier.rs:216–228
- **Detail**: `save_training_data()` uses `std::fs::write` synchronously. It's called from `add_feedback()` which holds a `tokio::sync::RwLock` read guard on the training data. This blocks the async runtime thread during disk I/O. With the current low feedback frequency this is unlikely to cause observable issues, but violates the async contract.
- **Fix A ⭐ Recommended**: Use `tokio::task::spawn_blocking` for the write
  - Strength: Non-blocking, preserves async runtime health, minimal code change (wrap the fs::write in spawn_blocking).
  - Tradeoff: Adds a task spawn per save. Negligible cost at feedback frequency.
  - Confidence: HIGH — standard tokio pattern for file I/O.
  - Blind spot: None significant.
- **Fix B**: Accept and document as intentional
  - Strength: Zero code change. Retraining is rare (every N feedback items) so the blocking time budget is tiny in practice.
  - Tradeoff: Leaves a correctness landmine if feedback volume grows.
  - Confidence: MEDIUM — safe today, risky at scale.
  - Blind spot: Unknown future feedback frequency.
- **Decision**: FIXED (converted `save_training_data` to async and used `tokio::fs::write`)

### F3 — Missing log before fallback in load_training_data

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency (Lessons Compliance)
- **Location**: src/fewshot_classifier.rs:242
- **Detail**: When `std::fs::read_to_string(path)` fails, the outer `Err(_)` arm returns `vec![]` with no log. Project lesson "Log operational failures before falling back" requires at minimum a `debug!` for internal defaults. The `data_path` is user-configurable, so a `debug!` is appropriate here since file-not-found is expected on first run — but permission errors or path typos would be silently swallowed.
- **Fix**: Add `tracing::debug!("No persisted training data at {}: {}", path, e)` in the `Err(e)` arm (change `Err(_)` to `Err(e)`).
- **Decision**: FIXED (added debug logging for file read errors)

### F4 — Retrain race condition under concurrent feedback

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality (Reliability)
- **Location**: src/fewshot_classifier.rs:190–211
- **Detail**: `add_feedback` drops the write guard after pushing, then re-acquires a read guard to check threshold and retrain. Concurrent feedback calls could trigger multiple simultaneous retrains. `retrain_internal` calls `DashMap::clear()` without coordination. In practice, with current usage (low feedback volume, single user), the window is negligible, and the plan explicitly notes "Concurrent classify calls during retraining may see an empty vocabulary and return Fallback, which is fine."
- **Fix**: Hold write guard through push + threshold check, or add an `AtomicBool` retrain-in-progress guard.
- **Decision**: FIXED (added `retraining_in_progress: AtomicBool` check-and-set logic)

### F5 — Unbounded training data growth

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality (Data Safety)
- **Location**: src/fewshot_classifier.rs:196–210
- **Detail**: No upper bound on training data size. Each feedback call appends without limit. The vocabulary warn at `max_vocabulary_warn` is advisory only. With hundreds of thousands of examples, retrain cost grows linearly and the persisted YAML balloons.
- **Fix**: Consider a `max_training_examples` config; evict oldest non-bootstrap entries when exceeded. Future optimization.
- **Decision**: FIXED (added `max_training_examples` config, eviction logic, and updated config.toml)

### F6 — No satisfaction bounds validation

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality (Security)
- **Location**: src/main.rs:928–932
- **Detail**: OpenAPI spec documents `satisfaction` as 0.0–1.0, but the handler accepts arbitrary f64 values. Values >0.99 would be treated as bootstrap-quality (skipped in cold-start count), potentially defeating cold-start threshold protection.
- **Fix**: Clamp `satisfaction` to [0.0, 1.0] in `feedback_handler` before forwarding.
- **Decision**: FIXED (added `let satisfaction = body.satisfaction.max(0.0).min(1.0);`)

### F7 — ClassifiersConfig::default() order inconsistency

- **Severity**: 💡 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency (Plan Drift)
- **Location**: src/config.rs:913
- **Detail**: Plan specified updating `ClassifiersConfig::default()` to include "fewshot" in the order. The `default_classifier_order()` serde default (line 43) correctly returns `["regex", "fewshot", "llm"]`, but `Default for ClassifiersConfig` (line 913) still returns `["regex", "llm"]`. In practice this path is only hit when the entire `[classifiers]` section is absent AND the embedded config isn't loaded — very unlikely given the `include_str!` config.toml.
- **Fix**: Update line 913 to `vec!["regex".into(), "fewshot".into(), "llm".into()]` for consistency.
- **Decision**: FIXED (updated `ClassifiersConfig::default()` order)
