<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Move All Config to File

- **Plan**: context/changes/move-all-config-to-file/plan.md
- **Scope**: All 6 phases
- **Date**: 2026-06-11
- **Verdict**: NEEDS ATTENTION
- **Findings**: 0 critical, 5 warnings, 3 observations
- **Triage**: 8 fixed, 0 skipped

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | PASS |
| Scope Discipline | PASS |
| Safety & Quality | WARNING |
| Architecture | PASS |
| Pattern Consistency | WARNING |
| Success Criteria | PASS |

## Findings

### F1 — TOML integer fields silently truncate on overflow

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:705,720,780,795,880
- **Detail**: `priority`, `weight`, and `penalty` are parsed as `i64` via `as_integer()` then cast with `as u8`. Values ≥256 truncate silently (e.g., `weight = 300` becomes `44`). No range validation.
- **Fix**: Use `.try_into().unwrap_or(<default>)` or clamp with `.min(u8::MAX as i64) as u8` on each parsing site.
- **Decision**: FIXED

### F2 — Stale `#[allow(dead_code)]` on `CategoryConfig`

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classifier.rs:51
- **Detail**: `#[allow(dead_code)]` sits on a struct that is actively used across three modules. The annotation is stale — it either suppresses nothing or was left behind during refactoring.
- **Fix**: Remove `#[allow(dead_code)]` from `CategoryConfig`. Run `cargo build` to confirm no dead-code warnings.
- **Decision**: FIXED

### F3 — Stale `#[allow(unused_imports)]` on re-exports

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classifier.rs:10
- **Detail**: `#[allow(unused_imports)]` annotates re-exports of `ModelCosts`, `RouteEntry`, `DEFAULT_MODEL`, `DEFAULT_MODEL_COMPLEX`. All four items are referenced within the module — the suppression is unnecessary.
- **Fix**: Remove `#[allow(unused_imports)]`. If a warning fires for one item, remove only that item from the re-export list.
- **Decision**: FIXED

### F4 — API key env var missing is silently swallowed at startup

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/intent_classifier.rs:195
- **Detail**: `std::env::var(&config.api_key_env).unwrap_or_else(|_| String::new())` silently swallows a missing API key. The empty key is only detected later at classify time (line 254–258), making startup misconfiguration harder to diagnose. Violates the "log operational failures before falling back" lesson.
- **Fix**: Add a `warn!` in the fallback path before defaulting to empty string.
  - Strength: Matches the established lesson, gives operators immediate feedback.
  - Tradeoff: Minor — adds a log line per-loaded provider at startup. False positives if some providers intentionally have no API key.
  - Confidence: HIGH — pattern is well-defined in lessons.md.
  - Blind spot: None significant.
- **Decision**: FIXED

### F5 — `test_hardcoded_categories()` naming suggests dead reference

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/config.rs:1142
- **Detail**: Test helper is named `test_hardcoded_categories()` which reads like it references the removed `hardcoded_categories()`. While functionally correct (it constructs categories inline), the name is misleading and could confuse future readers.
- **Fix**: Rename to `test_categories()` for consistency with the pattern in `intent_classifier.rs`.
- **Decision**: FIXED

### F6 — `allowed_origins` RwLock `.unwrap()` can panic if poisoned

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:852-854
- **Detail**: `try_read().unwrap()` on the `allowed_origins` RwLock. Runs once at startup after initial write, so poisoning is practically impossible, but an `expect` with a descriptive message is cleaner.
- **Fix**: Replace with `.expect("allowed_origins RwLock written at init; poisoning impossible")`.
- **Decision**: FIXED

### F7 — Short-prompt threshold uses byte length, not char count

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/intent_classifier.rs:554
- **Detail**: `sanitized.len()` uses byte length for the `< short_prompt_len` heuristic. For ASCII (predominant in code context) this matches char count, but non-ASCII characters count as ≥2 bytes.
- **Fix**: Use `.chars().count()` if Unicode correctness matters, or document that threshold is measured in bytes.
- **Decision**: FIXED

### F8 — API key cloned per classify call

- **Severity**: 👁️ OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/intent_classifier.rs:252
- **Detail**: `self.api_key.read().await.clone()` clones the API key string on every LLM classify request. Keys are short strings so cost is minimal, but it's in the request hot path.
- **Fix**: Consider `Arc<str>` if profiling shows this as a bottleneck. Acceptable for now.
- **Decision**: FIXED (migrated to `Arc<str>` inner type)
