<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: LLM Classifier Backend (S-09)

- **Plan**: context/changes/llm-classifier/plan.md
- **Scope**: Full Plan (4 phases + extension)
- **Date**: 2026-06-08
- **Verdict**: APPROVED
- **Findings**: 0 critical | 3 warnings | 5 observations

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

### F1 — load_regex_classifier_config silent fallback

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/config.rs:222-239
- **Detail**: Function silently falls back to defaults on file read/parse errors without logging, violating lessons.md "Log operational failures before falling back."
- **Fix**: Added `tracing::warn!` before each `return RegexClassifierConfig::default()` in the error arms.
- **Decision**: FIXED

### F2 — Regex failure unnecessarily disables LLM classifier

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:161-163
- **Detail**: When regex_enabled=true but RegexClassifier::from_env fails, code disabled LLM classifier too. LLM could serve as sole backend.
- **Fix**: Removed the early-return guard; regex failure now falls through to LLM-only path with a warning.
- **Decision**: FIXED

### F3 — test.sh code duplication with run.sh

- **Severity**: ⚠️ WARNING
- **Impact**: 🔬 HIGH — architectural stakes; think carefully before deciding
- **Dimension**: Pattern Consistency
- **Location**: manual-test/test.sh vs manual-test/run.sh
- **Detail**: test.sh (1008 lines) duplicates infrastructure from run.sh (start_server, stop_server, cleanup, classify, logging). Minor inconsistencies between the two.
- **Fix A (applied)**: Extracted shared infrastructure into manual-test/lib.sh, sourced by both test.sh and run.sh.
- **Decision**: FIXED

### F4 — Dead _template_path parameter on build_llm_classifier_prompt

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classifier.rs:326
- **Detail**: Parameter is never used. File-reading logic lives in LLMClassifier::new instead.
- **Fix**: Removed the parameter from signature and both call sites.
- **Decision**: FIXED

### F5 — timeout_secs=0 invalid, no clamping

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: src/config.rs:327
- **Detail**: Loading timeout_secs with unwrap_or(3). If user sets 0, Duration::from_secs(0) causes immediate timeout.
- **Fix**: Added `.max(1)` clamp.
- **Decision**: FIXED

### F6 — Config.toml read multiple times during startup

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Architecture
- **Location**: src/main.rs:73,138
- **Detail**: load_regex_classifier_config, load_llm_classifier_config, and load_categories each read+parse config.toml separately.
- **Fix**: Added `_from_value` variants to each loader; main.rs reads+parses config once and passes the value.
- **Decision**: FIXED

### F7 — Env var read on every classification call

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Performance
- **Location**: src/intent_classifier.rs:228
- **Detail**: std::env::var called on each classify_async call instead of resolving once at construction.
- **Fix**: Resolved api_key in LLMClassifier::new and stored as self.api_key field.
- **Decision**: FIXED

### F8 — Active Groq endpoint in committed config.toml

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Security
- **Location**: config.toml:16-22
- **Detail**: Active (non-commented) [llm_classifier] pointing to Groq's production API.
- **Fix**: Commented out the active block; kept both commented examples for OpenAI and Groq.
- **Decision**: FIXED
