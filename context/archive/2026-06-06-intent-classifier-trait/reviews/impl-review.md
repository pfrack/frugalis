<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Intent Classifier Trait

- **Plan**: context/changes/intent-classifier-trait/plan.md
- **Scope**: Full plan (Phases 1-4)
- **Date**: 2026-06-07
- **Verdict**: APPROVED (post-triage)
- **Findings**: 0 critical 2 warnings 2 observations

## Verdicts

| Dimension | Verdict |
|-----------|---------|
| Plan Adherence | WARNING ⚠️ |
| Scope Discipline | WARNING ⚠️ |
| Safety & Quality | WARNING ⚠️ |
| Architecture | PASS ✅ |
| Pattern Consistency | WARNING ⚠️ |
| Success Criteria | PASS ✅ |

## Findings

### F1 — Extra `get_routing()` method on IntentClassify trait

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/intent_classificator.rs:80-84
- **Detail**: Plan (Phase 1, Contract) specified: "The trait block exports one method signature. No associated types, no async, no default methods." The implementation adds `get_routing()` with a default `None` return — a second method with a default body. This changes the trait contract, and the main() initialization (src/main.rs:84-88) relies on it to merge routing from backends (iterating via `backends()` + `get_routing()`).
- **Fix A ⭐ Recommended**: Document `get_routing()` as a planned addendum in the plan's "Key Discoveries" and update the Phase 1 contract to reflect it. The trait remains minimal — only the classify() method is mandatory.
  - Strength: Matches actual code; source of truth is repaired for future reviews.
  - Tradeoff: Plan slightly diverges from original spec (acceptable evolution).
  - Confidence: HIGH — trivial documentation-only fix.
  - Blind spot: None significant.
- **Fix B**: Remove `get_routing()` from trait; directly clone `regex_classifier.routing` in main() (as the plan originally described).
  - Strength: Restores strict one-method trait contract.
  - Tradeoff: Requires updating main() and tests to not use the trait method; `get_routing()` on ClassifierChain would also need separate handling.
  - Confidence: MEDIUM — doable but requires more edits than A.
  - Blind spot: Future backends that don't hold a routing table would need a different mechanism.
- **Decision**: FIXED via Fix A (plan addendum)

### F2 — Extra `backends()` getter on ClassifierChain

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Scope Discipline
- **Location**: src/intent_classificator.rs:120-123
- **Detail**: Plan did not specify any getters on ClassifierChain — only `new()` and the trait impl. The `backends()` getter was added and is used in main() (line 85) to iterate backends for routing merge.
- **Fix**: Accept as-is — trivial accessor. No behavioral concern.
  - Strength: Low cost; getter is idiomatic Rust for testing and inspection.
  - Tradeoff: None.
  - Confidence: HIGH — purely additive, no risk.
  - Blind spot: None.
- **Decision**: ACCEPTED

### F3 — Duplicate dead test code inside `classify()` method body

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/intent_classificator.rs:589-716
- **Detail**: Test code (StubClassifier, impl IntentClassify, and five #[test] functions) is embedded inside the `RegexClassifier::classify()` method body (between negative-suppression and classification-threshold logic). These #[test] attributes are silently ignored on inner functions — they become unreachable dead code. The same test logic correctly exists in `#[cfg(test)] mod tests` (lines 960-1086) and runs under `cargo test`. The dead code emits: "struct StubClassifier is never constructed" warning. Artifact from incomplete refactoring during trait extraction.
- **Fix**: Delete lines 589-716 (the dead test block) and the orphan braces at lines 753-755 that close method bodies after the embedded block. Run `cargo fmt` afterward.
  - Strength: Removes dead code, eliminates compiler warning, follows-through on the refactoring.
  - Tradeoff: Requires careful edit to ensure no logic loss. Production logic is intact but verify via `cargo test` after.
  - Confidence: HIGH — the real tests are identical and already passing in `mod tests`.
  - Blind spot: None — identical tests at lines 960-1086 cover the same ground.
- **Decision**: FIXED (dead test block deleted, auth_headers tests moved to mod tests, cargo fmt)

### F4 — Prior review markers not carried forward

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Pattern Consistency
- **Location**: src/main.rs (completion_handler, classify_and_log)
- **Detail**: Per context/foundation/lessons.md rule "Re-run review after a follow-up change touches the same handler", the F1-F4 review markers from the prior `reqwest-upstream-routing` review should be embedded at key guard points in `completion_handler` and `classify_and_log`. No such markers exist. The fixes themselves are preserved (verified: content-type validation, UTF-8 check, API key graceful degradation, endpoint-empty guard, bounded error body), but the markers that would allow automated regression detection in future changes are absent.
- **Fix**: Add `// F1` through `// F5` comments at the guard points in `completion_handler` and `classify_and_log`:
    - F1: Content-Type validation (main.rs:188, 240)
    - F2: Body UTF-8 validation (main.rs:247)
    - F3: API key graceful degradation (main.rs:314-339)
    - F4: Endpoint-empty → 502 guard (main.rs:343)
    - F5: Bounded upstream error body (main.rs:400, 496)
  - Strength: Allows machine-verifiable regression detection per the established lesson.
  - Tradeoff: Minor source churn; 5 comment lines.
  - Confidence: HIGH — mechanical, no logic change.
  - Blind spot: None significant.
- **Decision**: FIXED (removed opaque F-markers, recorded rule in lessons.md instead)

### F5 — Module naming inconsistency

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classificator.rs (file and module name)
- **Detail**: File/module named `intent_classificator` (with 'c') while all public types use 's': `RegexClassifier`, `ClassifierChain`, `ClassificationResult`, `ClassificationTier`. The backward-compat alias `IntentClassifier` also uses 's'.
- **Fix**: Rename `src/intent_classificator.rs` → `src/intent_classifier.rs`, update `mod intent_classificator` to `mod intent_classifier` in main.rs, update all `use` paths. Standalone refactoring; no behavioral impact.
  - Strength: Eliminates persistent naming mismatch visible across the codebase.
  - Tradeoff: Mechanical rename touches many files.
  - Confidence: HIGH.
  - Blind spot: None.
- **Decision**: FIXED (file renamed to intent_classifier.rs, module alias updated)

### F6 — Indentation inconsistency from dead code artifact

- **Severity**: 🔍 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/intent_classificator.rs:753-755
- **Detail**: After removing F3's dead code block, orphan braces at multiple indentation levels remain (4, 8, 12 spaces). `cargo fmt` will resolve automatically.
- **Fix**: Run `cargo fmt` after deleting the dead test block (F3).
- **Decision**: FIXED (resolved by cargo fmt after F3 fix)
