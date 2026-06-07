---
date: 2026-06-07T14:20:26+02:00
researcher: pfrack
git_commit: 792b2618299caf26c5adabf28293e2eb1bafc836
branch: provider-url-derivation
repository: cerebrum
topic: "Shared Category Configuration (S-07b) — extract CategoryConfig consumed by both RegexClassifier and LLMClassifier"
tags: [research, category-config, shared-category, intent-classify, regex-classifier, llm-classifier, s-07b, roadmap]
status: complete
last_updated: 2026-06-07
last_updated_by: pfrack
last_updated_note: "Added critical migration note for NEGATIVE_META references to CAT_* constants"
last_updated_note: "Added classification of generic vs. classifier-specific settings, and per-backend enable/disable flags"
last_updated_note: "Edge case validation: identified 42+ raw string locations, 7 external files with hardcoded category values, 4 silent-failure scenarios, and 2 cross-category coupling risks"
---

# Research: Shared Category Configuration (S-07b)

**Date**: 2026-06-07T14:20:26+02:00
**Researcher**: pfrack
**Git Commit**: 792b2618299caf26c5adabf28293e2eb1bafc836
**Branch**: provider-url-derivation
**Repository**: cerebrum

## Research Question

How are the four intent categories (`FILE_READING`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`) currently defined and used across the codebase, and what would a shared `CategoryConfig` look like that both `RegexClassifier` and future `LLMClassifier` (S-09) can consume?

## Summary

The four intent categories are **hardcoded in at least six separate locations** within `src/intent_classifier.rs`, all tightly coupled inside the `RegexClassifier` implementation. There is no shared source of truth for which categories exist, their descriptions, or their properties. The `LLMClassifier` (S-09) needs category descriptions for its prompt template — without a shared config, the two classifiers would have independent copies of the category list, creating drift risk. A `CategoryConfig` struct with a static `CATEGORIES` slice solves this: ~80-100 line refactor in `src/intent_classifier.rs`, no behavioral change, no trait changes.

## Detailed Findings

### 1. Where Categories Are Currently Defined

All category knowledge lives in `src/intent_classifier.rs` and is scattered across:

**A. Category name constants** (`src/intent_classifier.rs:168-172`):
```rust
const CAT_FILE_READING: &str = "FILE_READING";
const CAT_COMPLEX_REASONING: &str = "COMPLEX_REASONING";
const CAT_SYNTAX_FIX: &str = "SYNTAX_FIX";
const CAT_CASUAL: &str = "CASUAL";
const CAT_NEG: &str = "NEG";
```

**B. Pattern count constants** (`src/intent_classifier.rs:176-180`):
```rust
const FR_COUNT: usize = 12;
const CR_COUNT: usize = 16;
const SF_COUNT: usize = 11;
const CA_COUNT: usize = 5;
const NEG_COUNT: usize = 4;
```

**C. Weight arrays** (`src/intent_classifier.rs:184-187`) — one per positive category, parallel-indexed with the corresponding pattern array:
```rust
const FR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];       // 12 entries
const CR_WEIGHTS: &[u8] = &[3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1]; // 16 entries
const SF_WEIGHTS: &[u8] = &[3, 3, 3, 2, 2, 2, 2, 2, 1, 1, 1];              // 11 entries
const CA_WEIGHTS: &[u8] = &[3, 2, 1, 1, 1];                                // 5 entries
```

**D. Threshold constants** (`src/intent_classifier.rs:191-196`):
```rust
const FR_THRESHOLD: u32 = 3;
const SF_THRESHOLD_HIGH: u32 = 4;
const SF_THRESHOLD_LOW: u32 = 3;
const CR_THRESHOLD: u32 = 3;
const CA_THRESHOLD: u32 = 1;
const SHORT_PROMPT_LEN: usize = 30;
```

**E. Regex pattern arrays** (`src/intent_classifier.rs:205-267`) — five arrays (`FILE_READING`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`, `NEGATIVE`), each implicitly tied to a category name. The pattern arrays and their semantic themes:

| Category | Lines | Count | Semantic Themes |
|---|---|---|---|
| `FILE_READING` | 205–218 | 12 | read/show/display/view/open files; navigate/look at code; find/search/grep/locate; inspect/examine; file extensions, line numbers |
| `COMPLEX_REASONING` | 220–237 | 16 | architecture/design; refactor/restructure; multi-step/concurrent/distributed; scalability/optimization; deep analysis/trade-offs; performance issues; security audits; dependency/coupling |
| `SYNTAX_FIX` | 239–251 | 11 | fix/correct/repair bugs/errors; compilation/syntax errors; type errors/linter warnings; stack traces/panics; missing imports/semicolons; "doesn't work"/"is broken" |
| `CASUAL` | 253–259 | 5 | greetings; thanks; simple definitions; short how-to questions; acknowledgments |
| `NEGATIVE` | 261–266 | 4 | Suppression patterns (penalize false matches) |

**F. Negative suppression metadata** (`src/intent_classifier.rs:270-287`) — four `NegativeMeta` entries referencing specific categories by their constants:
```
NEGATIVE_META[0]: suppressed = CAT_COMPLEX_REASONING, penalty = 2
NEGATIVE_META[1]: suppressed = CAT_COMPLEX_REASONING, penalty = 2
NEGATIVE_META[2]: suppressed = CAT_SYNTAX_FIX,         penalty = 2
NEGATIVE_META[3]: suppressed = CAT_FILE_READING,       penalty = 2
```

**G. `build_all_patterns()`** (`src/intent_classifier.rs:385-430`) — assembles the flat pattern + metadata vectors by iterating each array in hardcoded order (FR → CR → SF → CA → NEG). The iteration order is **critical** because it determines index ranges used by `classify()` to distinguish positive from negative patterns.

**H. Hardcoded routing** (`src/intent_classifier.rs:297-351`) — four separate `routing.insert()` calls mapping category constants to `RouteEntry`:
| Category | Lines | Default Model |
|---|---|---|
| `CAT_COMPLEX_REASONING` | 303–312 | `DEFAULT_MODEL_COMPLEX` (`meta/llama-3.3-70b-instruct`) |
| `CAT_FILE_READING` | 313–322 | `DEFAULT_MODEL_READING` (`meta/llama-3.1-70b-instruct`) |
| `CAT_SYNTAX_FIX` | 323–332 | `DEFAULT_MODEL` (`meta/llama-3.1-8b-instruct`) |
| `CAT_CASUAL` | 333–342 | `DEFAULT_MODEL` (`meta/llama-3.1-8b-instruct`) |

**I. `classify()` scoring and routing** (`src/intent_classifier.rs:585-644`) — hardcoded priority order in the final routing decision (lines 634-643):
```rust
if fr { route_match(CAT_FILE_READING); }    // priority 1
if sf { route_match(CAT_SYNTAX_FIX); }      // priority 2
if cr { route_match(CAT_COMPLEX_REASONING); } // priority 3
route_match(CAT_CASUAL);                     // priority 4 (catch-all)
```
Also hardcoded: the "2+ thresholds met → CASUAL" ambiguity rule (line 630-631), and the short-prompt shortcut (line 613-615).

### 2. Category References Outside intent_classifier.rs

**`src/main.rs`** — all 17 category string references are in **test code only** (within `#[cfg(test)]`). Production code never references category strings directly — it gets them from `ClassificationResult.category` via the classifier chain. Lines: 744, 754, 800, 834, 861, 871, 918, 1302, 1346, 1356, 1393, 1403, 1576, 2107. Most use `"SYNTAX_FIX"` and `"CASUAL"` as test routing table keys.

**`src/dashboard.rs`** — **zero** category string references. Categories are treated as opaque `String` values in `filter_category: Option<String>` (lines 116, 199, 211, 221, 239, 249).

**`src/persistence.rs`** — **zero** category string literals. Category is `Option<String>` in `InferenceRecord` (line 83), used in SQL `WHERE category = $1` (line 134-138) and `GROUP BY category` (line 217).

**`routing_examples/*.toml`** — all four example files use the same category keys as TOML section names (`FILE_READING`, `COMPLEX_REASONING`, `SYNTAX_FIX`, `CASUAL`, `FALLBACK`). The loader at `src/intent_classifier.rs:474` uppercases table keys, matching the `CAT_*` constant values.

### 3. Existing Category Descriptions (Source Material for LLM Prompts)

**Primary source** — NLI hypothesis templates from `context/archive/2026-06-07-proxy-intent-routing/research.md:85-91`:
```
COMPLEX_REASONING: "This prompt requires complex reasoning or multi-step problem solving."
FILE_READING: "This prompt is about reading or viewing the contents of a file."
SYNTAX_FIX: "This prompt is about fixing a bug, error, or compilation issue."
CASUAL: "This prompt is a simple question or casual conversation."
```

**Secondary source** — the regex patterns themselves encode semantic themes (see Section 1E above) that can inform more precise descriptions.

**No existing code carries category descriptions** — `PatternMeta` (line 149) has only `category` + `weight`, `RouteEntry` (line 11) has no description field, and no `.rs` or `.toml` file contains intent descriptions.

### 4. Proposed Shared CategoryConfig

```rust
/// Single source of truth for all intent categories.
/// Consumed by RegexClassifier (patterns, weights, thresholds, routing)
/// and LLMClassifier (prompt template generation from descriptions).
#[derive(Clone)]
pub struct CategoryConfig {
    pub name: &'static str,             // "FILE_READING", etc.
    pub description: &'static str,      // Human-readable for LLM prompts
    pub regex_threshold: Option<u32>,   // Regex scoring threshold; None = regex can't classify
    pub priority: u8,                   // Tie-breaking order (lower = higher priority)
}

pub const CATEGORIES: &[CategoryConfig] = &[
    CategoryConfig {
        name: "FILE_READING",
        description: "Reading, viewing, inspecting, searching, or navigating files or code",
        regex_threshold: Some(3),   // FR_THRESHOLD
        priority: 1,                // Highest — checked first in classify()
    },
    CategoryConfig {
        name: "SYNTAX_FIX",
        description: "Fixing bugs, errors, typos, compilation issues, or broken code",
        regex_threshold: Some(4),   // SF_THRESHOLD_HIGH (with fallback to Some(3) for SF_THRESHOLD_LOW)
        priority: 2,
    },
    CategoryConfig {
        name: "COMPLEX_REASONING",
        description: "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization",
        regex_threshold: Some(3),   // CR_THRESHOLD
        priority: 3,
    },
    CategoryConfig {
        name: "CASUAL",
        description: "Simple questions, greetings, general conversation, or short prompts",
        regex_threshold: Some(1),   // CA_THRESHOLD
        priority: 4,               // Lowest — catch-all, checked last
    },
];
```

**Design notes:**
- `regex_threshold: Option<u32>` — `Some(n)` for categories the regex classifier can detect; `None` for future categories that only the LLM classifier handles (LLM-only categories would have no regex patterns).
- `priority: u8` — replaces the hardcoded `if fr { } if sf { } if cr { }` chain in `classify()`. When multiple thresholds are met, the lowest-priority (highest-priority-number) wins or falls through to CASUAL.
- `description` — sourced from NLI hypothesis templates, with keywords from regex patterns added for precision.
- `CAT_NEG` is **not** a real category — it's an internal regex mechanism. It stays internal to `RegexClassifier`.
- `CATEGORIES` is a static slice — no heap allocation, no Arc, zero-cost at runtime.

### 5. What Changes in RegexClassifier

| Location | Current | After |
|---|---|---|
| Category name constants (lines 168-172) | Private `const CAT_*: &str` | Removed; use `config.name` from `CategoryConfig` |
| Pattern count constants (lines 176-180) | `FR_COUNT`, `CR_COUNT`, etc. | Derived from pattern arrays or stored in `RegexClassifier` fields |
| Weight arrays (lines 184-187) | 4 standalone `&[u8]` arrays | Remain as-is; they're regex-internal data keyed by pattern index, not category |
| Threshold constants (lines 191-196) | `FR_THRESHOLD`, `CR_THRESHOLD`, etc. | Replaced by `config.regex_threshold` |
| `build_all_patterns()` (lines 385-430) | Hardcoded per-category iteration | Iterates `CATEGORIES`, building patterns for entries with `regex_threshold.is_some()` |
| Negative index range (lines 534-535) | Computed from `FR_COUNT + ...` constants | Computed from sum of pattern counts for regex-enabled categories |
| `hardcoded_routing()` (lines 297-351) | 4 separate `routing.insert()` calls | Loop over `CATEGORIES` |
| `classify()` priority chain (lines 634-643) | Hardcoded `if fr { } if sf { } if cr { }` | Sort by `config.priority`, check each |
| Short-prompt shortcut (lines 613-615) | `if sanitized.len() < SHORT_PROMPT_LEN && all_zero` | Unchanged (SHORT_PROMPT_LEN is not category-specific) |
| Ambiguity rule (lines 630-631) | `if met >= 2 { route_fallback(CAT_CASUAL) }` | Unchanged (uses count of thresholds met, not category names) |
| `ClassificationResult::fallback()` (line 517) | Uses `CAT_CASUAL` constant | Uses first category with `priority == max_priority` (CASUAL) |
| `RegexClassifier` struct (lines 99-107) | No category config field | `categories: &'static [CategoryConfig]` (or slice reference) |
| `RegexClassifier::from_env()` (lines 531-558) | Reads env vars, builds internally | Receives or references `CATEGORIES` |

### 6. What Changes in LLMClassifier (S-09, Future)

The `LLMClassifier` constructor receives `&[CategoryConfig]` and builds its prompt template from it:

```
System: You are an intent classifier. Classify user prompts into one of:
- FILE_READING: Reading, viewing, inspecting, searching, or navigating files or code
- COMPLEX_REASONING: Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization
- SYNTAX_FIX: Fixing bugs, errors, typos, compilation issues, or broken code
- CASUAL: Simple questions, greetings, general conversation, or short prompts

Respond with only the category name, nothing else.

Examples:
User: "read the file src/main.rs" → FILE_READING
User: "fix this compilation error" → SYNTAX_FIX
User: "architect a distributed rate limiter" → COMPLEX_REASONING
User: "hello" → CASUAL

User: {prompt}
```

Generated by iterating `CATEGORIES` — no hardcoded category names in the LLM classifier.

**Key benefit:** Adding a new intent category requires: (1) add one `CategoryConfig` entry to `CATEGORIES`, (2) add optional regex patterns, (3) add routing.toml entry, (4) update prompt examples. Both classifiers pick it up from the same source.

### 7. What Does NOT Change

- **`IntentClassify` trait** (`src/intent_classifier.rs:78-87`) — unchanged. `classify()` signature stays the same.
- **`ClassificationResult`** — unchanged. Category is still a `String`.
- **`ClassifierChain`** — unchanged. Fallback logic is category-agnostic.
- **`AppState`** (`src/main.rs:28-37`) — unchanged. Routing merge is category-agnostic.
- **`completion_handler`** / `classify_handler` — unchanged. Trait dispatch is transparent.
- **`src/dashboard.rs`** — unchanged. Categories are opaque strings.
- **`src/persistence.rs`** — unchanged. `category: Option<String>`.
- **`routing.toml`** files — unchanged. Table keys already match category names.
- **`Cargo.toml`** — unchanged. No new dependencies.
- **Test code** in `src/main.rs` — 17 occurrences of raw strings like `"SYNTAX_FIX"`, `"CASUAL"`. Could update to reference `CATEGORIES` but not required for correctness; the strings match `CategoryConfig.name` values.

### 8. SF_THRESHOLD Dual-Threshold Edge Case

The `SYNTAX_FIX` category has a dual-threshold in `classify()` (lines 619-621):
```rust
let sf = scores.get(CAT_SYNTAX_FIX) >= SF_THRESHOLD_HIGH    // >= 4
    || (scores.get(CAT_SYNTAX_FIX) >= SF_THRESHOLD_LOW       // >= 3
        && scores.get(CAT_FILE_READING) == 0);               // AND no FR matches
```

This is the **only** category with context-dependent threshold logic. The simpler `CategoryConfig` proposed above stores a single `regex_threshold`. Options for handling SF's dual threshold:

1. **Store a single threshold in CategoryConfig and handle the dual logic in classify()** — the `CategoryConfig` holds `regex_threshold: Some(3)` (the low threshold), and the `FR == 0` cross-check remains in `classify()` as special-case logic. This keeps the config clean.
2. **Store both thresholds** — adds `regex_threshold_high: Option<u32>` to `CategoryConfig`. Unnecessary complexity for one special case.
3. **Move the cross-check into a separate "scoring rules" concept** — overengineered for this scale.

**Recommendation: Option 1.** Store a single threshold. SYNTAX_FIX gets `regex_threshold: Some(3)`. The `sf >= 4 || (sf >= 3 && fr == 0)` logic stays in `classify()` as implementation detail. The `CategoryConfig` doesn't need to encode interaction rules between categories.

### 9. Negative Suppression Handling

The `NEGATIVE_META` array (lines 270-287) references categories by their `CAT_*` constants. After the refactor, it would reference `CategoryConfig` entries or category name strings. The simplest approach: convert `suppressed: &'static str` references to use the same string values that `CategoryConfig.name` holds — they already match.

### 10. PATTERN_COUNT Computation

Currently the negative index range is computed at construction time (lines 534-535):
```rust
let negative_start = FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT; // = 44
let negative_idx = negative_start..(negative_start + NEG_COUNT);  // = 44..48
```

After the refactor, this becomes:
```rust
let positive_count: usize = categories.iter()
    .filter(|c| c.regex_threshold.is_some())
    .map(|c| c.pattern_count)  // or compute from pattern arrays
    .sum();
let negative_idx = positive_count..(positive_count + NEG_COUNT);
```

The pattern count per category can be derived from the pattern array lengths (which remain as static arrays) or stored as an additional field on a regex-specific config struct.

## Architecture Insights

1. **The category set is currently implicit.** There is no `Vec<Category>` or enum anywhere. The set of four categories is defined by the union of constants, pattern arrays, weight arrays, and routing entries — all inside `RegexClassifier`. An `LLMClassifier` has no way to discover what categories exist without hardcoding its own copy.

2. **Categories form a natural sealed set for MVP.** Four categories, unlikely to change frequently. A static slice is the right data structure — no dynamic registration, no runtime overhead.

3. **The trait boundary is clean.** `CategoryConfig` is a construction-time concern passed to each backend, not embedded in the `IntentClassify` trait. This matches the archived S-07 plan's design principle: "config is bundled at construction time" (plan.md line 41).

4. **The refactor is mechanical, not architectural.** Every change is a substitution: `CAT_FILE_READING` → `categories[FILE_READING_IDX].name`, `FR_THRESHOLD` → `categories[...].regex_threshold.unwrap()`, etc. No logic changes, no test expectations change.

5. **The description field solves S-09's prompt template problem.** Without this, `LLMClassifier` would need to hardcode descriptions (duplicating knowledge) or load them from a separate config (inconsistent with regex classifier). With `CategoryConfig`, the prompt template is generated from the same source that defines the regex thresholds.

6. **SHORT_PROMPT_LEN and SF dual-threshold remain as regex-specific logic.** Not everything belongs in `CategoryConfig` — prompt length shortcuts and cross-category threshold interactions are classifier implementation details.

## Code References

- `src/intent_classifier.rs:168-172` — category name constants (to be removed)
- `src/intent_classifier.rs:176-180` — pattern count constants (to be derived)
- `src/intent_classifier.rs:184-187` — weight arrays (regex-internal, stay)
- `src/intent_classifier.rs:191-196` — threshold constants (move to config)
- `src/intent_classifier.rs:205-267` — pattern arrays with semantic themes
- `src/intent_classifier.rs:270-287` — negative suppression metadata
- `src/intent_classifier.rs:297-351` — `hardcoded_routing()` (loop over config)
- `src/intent_classifier.rs:385-430` — `build_all_patterns()` (iterate config)
- `src/intent_classifier.rs:534-535` — negative index range (config-derived)
- `src/intent_classifier.rs:585-644` — `classify()` threshold/priority logic
- `src/intent_classifier.rs:646-667` — `route_match()` and `route_fallback()`
- `src/intent_classifier.rs:99-107` — `RegexClassifier` struct (add `categories` field)
- `src/intent_classifier.rs:149-152` — `PatternMeta` struct (unchanged)
- `context/archive/2026-06-07-proxy-intent-routing/research.md:85-91` — NLI hypothesis templates (category descriptions)
- `context/archive/2026-06-06-intent-classifier-trait/plan.md:41` — "config is bundled at construction time" principle
- `context/changes/llm-classifier/research.md` — S-09 research (motivation for this change)
- `src/main.rs:744,754,800,834,861,871,918,1302,1346,1356,1393,1403,1576,2107` — test category references

## Related Research

- `context/changes/llm-classifier/research.md` — S-09 research; this change is a derived prerequisite
- `context/archive/2026-06-06-intent-classifier-trait/plan.md` — S-07 plan establishing "config at construction time"
- `context/archive/2026-06-07-proxy-intent-routing/research.md` — Original classification research with NLI templates

## Open Questions

1. **Should `SF_THRESHOLD_HIGH` be stored in CategoryConfig or remain as regex-internal logic?** The dual-threshold is unique to SYNTAX_FIX. Recommendation: store a single threshold, keep the dual logic in `classify()`.

2. **Should pattern arrays be grouped under CategoryConfig?** The pattern arrays are regex-internal data. They can remain as standalone static arrays, with CategoryConfig referencing them indirectly (e.g., by category name lookup in `build_all_patterns()`). No need to bundle them into the config struct.

3. **Should `CATEGORIES` be a `static` or `const`?** `static` allows taking references (`&'static [CategoryConfig]`). `const` would require `&'static` references within each entry anyway. `static CATEGORIES: &[CategoryConfig] = &[...]` is the standard Rust pattern.

4. **Scope: separate change or folded into S-09?** This is small enough (~80-100 lines) to be either. The roadmap treats it as a separate slice (S-07b) because it has distinct verification criteria (all tests pass, no behavioral change) and unblocks S-09 planning.

## Mitigation Recommendations (from Edge Case Validation)

These items should be tracked in the implementation plan for S-07b:

1. **Use `HashMap<&str, u32>` for scores, not index-based arrays.** The SF/FR cross-category check (`scores.get("FILE_READING")`) depends on name-based lookups. Index-based lookups would break silently on category reordering in `CATEGORIES`.

2. **Add `tracing::warn!` in `route_match()` for non-CASUAL HashMap misses.** Currently a classifier-category/routing-key mismatch fails silently. A warning log would catch this at runtime.

3. **Derive fallback category from `CATEGORIES` by priority.** Replace both `ClassificationResult::fallback()` and the short-prompt shortcut's `CAT_CASUAL` with `CATEGORIES.iter().max_by_key(|c| c.priority).unwrap().name` — self-correcting, no hardcoded fallback category.

4. **Document that category names are a PUBLIC API contract.** The OpenAPI spec, HTTP header values, TOML config section names, and dashboard text all depend on stable category name strings. Add a comment on `CategoryConfig` and `CATEGORIES` warning that renaming is a breaking change.

5. **Do NOT add non-[A-Z_] characters to category names.** The `key.to_uppercase()` normalization in `config.rs:109` relies on this. A category named `"file-reading"` would normalize to `"FILE-READING"` which would never match a `CategoryConfig.name` value of `"file-reading"`.

6. **Update all 7 external files as part of the S-07b implementation** if any category name changes. Even if names don't change now, add comments in each file pointing to `CATEGORIES` as the source of truth.

7. **Tests: export `CATEGORIES` publicly (or `pub(crate)`) so tests can use `CATEGORIES[0].name` instead of raw `"FILE_READING"`.** This gives compile-time protection against renames. Use `#[cfg(test)]` re-exports if the slice shouldn't be part of the public API.

## Critical Migration: NEGATIVE_META References CAT_* Constants

`src/intent_classifier.rs:270-287` defines `NEGATIVE_META` as:

```rust
const NEGATIVE_META: &[NegativeMeta] = &[
    NegativeMeta { suppressed: CAT_COMPLEX_REASONING, penalty: 2 },  // line 272
    NegativeMeta { suppressed: CAT_COMPLEX_REASONING, penalty: 2 },  // line 276
    NegativeMeta { suppressed: CAT_SYNTAX_FIX,         penalty: 2 },  // line 280
    NegativeMeta { suppressed: CAT_FILE_READING,       penalty: 2 },  // line 284
];
```

These references use the `CAT_*` constants that are removed by this slice. After removing the constants, these must be updated to string literals matching `CategoryConfig.name` values:

```rust
const NEGATIVE_META: &[NegativeMeta] = &[
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "COMPLEX_REASONING", penalty: 2 },
    NegativeMeta { suppressed: "SYNTAX_FIX",         penalty: 2 },
    NegativeMeta { suppressed: "FILE_READING",       penalty: 2 },
];
```

**Verification:** After refactoring, run `cargo test` — all existing tests must pass. The `NEGATIVE_META` suppression behavior (reducing scores for `read the architecture document` to avoid COMPLEX_REASONING false positives) must be unchanged.

**Why this matters:** If `NEGATIVE_META` is not updated, the code will fail to compile with "cannot find value `CAT_COMPLEX_REASONING`" errors. This is a compile-time blocker, not a runtime regression — easy to catch but must be explicitly tracked.

## Follow-up Research: Edge Case Validation (2026-06-07)

The original research identified the primary migration surface (~6 definition points, ~17 test locations). A deeper audit against the live code (commit `a4c22bd`, main branch) found **significant gaps** — 42+ raw string occurrences across 8 files, 7 external consumers of category values, and 4 silent-failure scenarios.

### Finding A: Raw String Literal Count is ~42, Not ~17

The original research counted ~17 category string references in `src/main.rs` only. The actual count across the entire project is **~42**:

| File | Count | Type |
|------|-------|------|
| `src/intent_classifier.rs` (tests) | ~16 | Assertions, routing keys, stub classifiers |
| `src/main.rs` (tests) | ~11 | Routing keys, assertions, header values |
| `src/config.rs` (production + tests) | ~15 | `hardcoded_routing()` inserts, test assertions |
| `routing_examples/*.toml` (4 files) | 20 | Section names (`[FILE_READING]`, etc.) |
| `openapi/completions.yaml` | 4 | `enum` constraint values |
| `manual-test/run.sh` | ~2 | Header values, comments |
| `templates/dashboard/inferences.html` | 1 | Placeholder text |

After the refactor, the **production code** strings (config.rs `hardcoded_routing()` + intent_classifier.rs constants) will reference `CategoryConfig.name`. But **every test assertion and every external file** remains a string literal point that must match `CategoryConfig.name`. A future rename of any category would require touching ~35+ locations.

### Finding B: 7 External Files Hardcode Category Names — Missed Entirely

The original research only mentioned `routing_examples/*.toml`. The full set:

1. **`routing_examples/routing-unreachable.toml:3,9,15,21,27`** — section names `[FILE_READING]`, `[COMPLEX_REASONING]`, `[SYNTAX_FIX]`, `[CASUAL]`, `[FALLBACK]`
2. **`routing_examples/routing-manual-tests.toml:5,11,17,23,29`** — same
3. **`routing_examples/routing-openrouter.toml:4,10,16,22,28`** — same
4. **`routing_examples/routing-nvidia-nim.toml:5,10,15,20,25`** — same
5. **`openapi/completions.yaml:44,111`** — `enum: [COMPLEX_REASONING, FILE_READING, SYNTAX_FIX, CASUAL]` — OpenAPI schema constraint
6. **`manual-test/run.sh:179`** — passes `COMPLEX_REASONING` as `x-cerebrum-category` header
7. **`templates/dashboard/inferences.html:19`** — `placeholder="e.g. COMPLEX_REASONING"`

**Impact**: Category names are a **public API contract**, not just an internal detail. The OpenAPI spec, HTTP headers, TOML configs, and UI text all depend on these exact strings. Any rename is a **breaking change** to the API surface, the config format, and the dashboard UX.

### Finding C: `key.to_uppercase()` in Config Loader Creates Silent Mismatch Risk

`src/config.rs:109` normalizes TOML section keys with `key.to_uppercase()`. This means:
- TOML `[FILE_READING]` → stored as `"FILE_READING"` ✓
- TOML `[file_reading]` → stored as `"FILE_READING"` ✓ (same result after uppercasing)
- **But**: If a `CategoryConfig.name` were changed to contain non-alpha characters (e.g., `"file-reading"` or `"FILE_READING_V2"`), `key.to_uppercase()` would NOT match the `ClassificationResult.category` produced by `route_match()`. The routing lookup would silently fall back — **no error, no log, just cascaded fallback**.

This is not a risk for the current four categories (all uppercase alpha-only), but any future category name that deviates from `[A-Z_]+` would break silently.

### Finding D: `ClassificationResult::fallback()` Hardcodes `CAT_CASUAL` in Two Separate Places

The original research noted `ClassificationResult::fallback()` (line 9 of the migration table) but missed that `CAT_CASUAL` appears as the fallback destination in **two independent code paths**:

1. **`ClassificationResult::fallback()`** (`intent_classifier.rs:322-323`) — constructs `ClassificationResult { category: CAT_CASUAL.to_string(), ... }` when no classifier matches
2. **`classify()` short-prompt shortcut** (`intent_classifier.rs:398-401`) — returns `self.route_fallback(CAT_CASUAL)` for `<30 char prompts with zero regex matches`

Both independently hardcode `CAT_CASUAL`. If CASUAL were renamed to something with a different priority (e.g., a new catch-all category), both locations must change together. The `CATEGORIES` slice approach (iterate sorted by priority, use lowest-priority entry as fallback destination) would make this self-correcting.

### Finding E: SF Dual-Threshold Has Undocumented Cross-Category Coupling

Beyond the single/dual-threshold design question (Section 8), there's a **deeper coupling risk**: the SF low-threshold check at `intent_classifier.rs:405-407` uses `CAT_FILE_READING` **by name**:

```rust
*scores.get(CAT_FILE_READING).unwrap_or(&0) == 0
```

If the `CategoryConfig` refactor replaces `CAT_FILE_READING` with a string lookup by name, this line breaks cleanly (compile error if the constant is removed). But if the CategoryConfig approach uses **index-based** category lookups (e.g., `categories[0].name` instead of name-based `scores.get("FILE_READING")`), this coupling becomes fragile — changing category order would point to the wrong category's score.

**Recommendation**: Keep `scores` as a `HashMap<&str, u32>` keyed by category name string, not by index. This makes the SF/FR coupling resilient to ordering changes in `CATEGORIES`.

### Finding F: Routing Key Mismatch = Silent Fallback (Not Compile Error)

Every location where `ClassificationResult.category` is used to look up the routing table (`route_match()`, `completion_handler` header bypass) does:

```rust
self.routing.get(category)  // HashMap lookup by String
```

If the classifier outputs a category name that doesn't exist in the routing table (e.g., typo in `CategoryConfig.name`, or mismatch with TOML section keys), the HashMap returns `None` and the request silently falls back to the `fallback_entry`. There is **no log, no error, no telemetry** for "classifier produced unknown category" — the request just routes to the fallback model.

**Mitigation**: Add an `debug_assert!` or `tracing::warn!` in `route_match()` when the HashMap lookup misses for a non-CASUAL category.

### Finding G: Test Code Uses Raw Strings, Not CAT_* Constants — No Compile-Time Protection

Every test assertion (`assert_eq!(result.category, "FILE_READING")`) and every test routing insert (`routing.insert("SYNTAX_FIX".to_string(), entry)`) uses string literals, not `CAT_SYNTAX_FIX` etc. This means:

- Renaming a category constant silently breaks tests (assertion failures), but does **not** produce a compile error.
- The tests and production code share no import of category names — they're independently maintained string literals.
- The `ClassifyBuilder` test helper constructs `RegexClassifier` with `routing: HashMap::new()`, using `"FILE_READING"` etc. as raw keys. If the classifier constructor changes signature in the refactor, `ClassifyBuilder` tests break at compile time (good). But if only the string values change, they fail at runtime (worse).

### Edge Case Severity Summary

| # | Risk | Severity | Mechanism |
|---|------|----------|-----------|
| 1 | 42+ string literal occurrences split across 8 files — any rename touches ~35 locations | **HIGH** | Manual audit burden; no single `use` import |
| 2 | `key.to_uppercase()` vs `CategoryConfig.name` — silent mismatch if names contain non-[A-Z_] chars | **HIGH** | HashMap miss → fallback routing; no log |
| 3 | `build_all_patterns` iteration order determines `negative_idx` range — reordering breaks negative suppression | **HIGH** | Wrong patterns treated as negative |
| 4 | 7 external files (TOML, YAML, shell, HTML) hardcode category values — not covered in migration table | **HIGH** | Breaking API/UX/config changes if names change |
| 5 | `route_match()` HashMap miss = silent fallback; no telemetry for unknown category | **MEDIUM** | Undetected routing errors |
| 6 | SF dual-threshold name-couples `CAT_FILE_READING` — index-based lookup would break on reorder | **MEDIUM** | Cross-category score dependency |
| 7 | Two independent fallback-to-CASUAL code paths — both must agree on fallback category | **MEDIUM** | Divergent fallback behavior |
| 8 | Test assertions use raw strings, not constants — no compile error on rename | **MEDIUM** | Silent test breakage |
| 9 | DB `category` column is free-text — no validation, historical data skew on rename | **LOW** | Permanent data inconsistency |
| 10 | Dashboard placeholder `"e.g. COMPLEX_REASONING"` in HTML template | **LOW** | Cosmetic |

## Follow-up Research: Constructor, Merge, and Fallback Details (2026-06-07)

Deeper investigation of the classifier construction pipeline, routing map merge, fallback entry duplication, and streaming path — areas the original research left as open questions.

### Finding H: `from_env()`/`from_values()` Hardcode `FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT`

Both constructor methods at `src/intent_classifier.rs:337-367` compute `negative_start` from the four pattern count constants:

```rust
// from_env() line 340
let negative_start = FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT;
// from_values() line 357 — identical
let negative_start = FR_COUNT + CR_COUNT + SF_COUNT + CA_COUNT;
```

After removing `FR_COUNT` etc., this must become:

```rust
let positive_count: usize = CATEGORIES.iter()
    .filter(|c| c.regex_threshold.is_some())  // only regex-enabled categories
    .map(|c| /* pattern array length per category */)
    .sum();
let negative_start = positive_count;
```

But `CATEGORIES` doesn't carry pattern counts. The pattern arrays (`FILE_READING`, `COMPLEX_REASONING`, etc.) remain as standalone `&[&str]` static arrays. Two approaches:
1. Add `pattern_count: usize` to `CategoryConfig` (couples category config to regex internals)
2. Keep a separate `const REGEX_CATEGORY_PATTERN_COUNTS: &[usize]` parallel array — fragile
3. Compute negative_start in `build_all_patterns()` and return it as part of the result tuple — cleanest

**Recommendation**: `build_all_patterns()` returns `(Vec<&'static str>, Vec<PatternMeta>, Range<usize>)` — it knows when it starts appending negative patterns, so it can return the correct range directly. No need for `negative_start` in the constructor.

### Finding I: `test_classifier()` — Single Test Helper, Not a Builder Pattern

There is no `ClassifyBuilder` pattern in the codebase. The only test helper that constructs a `RegexClassifier` directly is `fn test_classifier()` at `src/intent_classifier.rs:460-510`.

It constructs a routing HashMap with **4 hardcoded string keys**: `"FILE_READING"`, `"COMPLEX_REASONING"`, `"SYNTAX_FIX"`, `"CASUAL"` (lines 463, 473, 483, 493), plus a fallback entry (line 502-508). These are raw string literals — they compile regardless of whether `CAT_FILE_READING` exists.

After CategoryConfig refactor: the `test_classifier()` helper should either:
- Iterate `CATEGORIES` to populate the routing map dynamically
- Or keep raw strings but add a test assertion that the map has exactly 4 keys matching `CATEGORIES`

### Finding J: `make_test_app_state()` Merge Path Is Unchanged by CategoryConfig

`src/main.rs:695-722` wraps a `RegexClassifier` in `ClassifierChain`, then merges routing via `backend.get_routing()` which returns `Some(&self.routing)` (`src/intent_classifier.rs:53-55`). The routing map was injected at construction time — what changes is **who constructs it**, not how it's extracted.

The merge at `src/main.rs:91-96` (production) is identical:
```rust
for backend in classifier.backends().iter() {
    if let Some(r) = backend.get_routing() {
        merged_routing.extend(r.clone());
    }
}
```

**No merge logic changes needed.** The routing map content is determined by who calls `RegexClassifier::from_env()` — currently `config::load_routing()` + `config::hardcoded_routing()`, after refactor same callers but with CategoryConfig-driven key generation.

### Finding K: Fallback Entry Constructed at 12 Locations — Not a CategoryConfig Concern

A fallback `RouteEntry` struct is constructed in **12 separate locations** across 3 files:

| File | Lines | Context |
|------|-------|---------|
| `config.rs:59-65` | `hardcoded_routing()` | Production: nvidia_nim fallback |
| `config.rs:135-141` | `load_routing()` | Production: when TOML `[FALLBACK]` absent |
| `intent_classifier.rs:502-508` | `test_classifier()` | Test helper |
| `main.rs:773-779` | `test_app_with_classifier()` | Test |
| `main.rs:890-896` | `test_app_with_enriched_classifier()` | Test |
| `main.rs:1375-1381` | `test_app_with_http_client()` | Test |
| `main.rs:1422-1428` | `test_app_with_dead_endpoint()` | Test |
| `main.rs:2126-2132` | `test_streaming_keepalive_injected()` | Test |

There are two separate "fallback" concepts:
1. **`RouteEntry` fallback** — the model/endpoint used when a category has no routing entry (used by `route_match()` as `unwrap_or` and `route_fallback()`)
2. **`ClassificationResult::fallback()`** — static constructor for when no classifier is configured at all (uses `CAT_CASUAL` hardcoded)

**The fallback `RouteEntry` is NOT a CategoryConfig concern.** It belongs to the routing layer. CategoryConfig should NOT carry fallback model/endpoint — those come from the router, not the category definitions.

**But `ClassificationResult::fallback()` hardcoding `CAT_CASUAL` IS a CategoryConfig concern** — it should derive the fallback category from `CATEGORIES` by lowest priority (see Finding G #3).

### Finding L: Streaming Path Uses Identical Routing — No Separate Concern

The SSE streaming response path in `src/main.rs:436-531` goes through the **exact same** `completion_handler` as non-streaming requests. Classification and routing happen at lines 275-323 (streaming vs non-streaming routing is identical), and the stream check happens later at line 394. The upstream URL and model are already determined by the time the streaming begins.

**No streaming-specific category edge cases exist.** The `x-cerebrum-category` header bypass works the same for streaming and non-streaming.

### Finding M: `hardcoded_routing()` in config.rs Creates 4 RouteEntry Per Category

`src/config.rs:13-67` is the production fallback when no `routing.toml` exists. It constructs a `HashMap<String, RouteEntry>` with 4 hardcoded entries:

| Line | Category Key | Model Env Var |
|------|-------------|---------------|
| 18-27 | `"COMPLEX_REASONING"` | `DEFAULT_MODEL_COMPLEX` |
| 29-37 | `"FILE_READING"` | `DEFAULT_MODEL_READING` |
| 39-47 | `"SYNTAX_FIX"` | `DEFAULT_MODEL` |
| 49-57 | `"CASUAL"` | `DEFAULT_MODEL` |
| 59-65 | `(fallback)` | `DEFAULT_MODEL` |

After CategoryConfig refactor, this function should iterate `CATEGORIES` and map model env vars via a lookup table `CategoryConfig.name → env_var_name`. The simplest approach: add an optional `model_env_var: Option<&'static str>` field to `CategoryConfig`.

### Finding N: `build_all_patterns()` Iteration Order Is the Critical Migration Point

The current `build_all_patterns()` at `src/intent_classifier.rs:269-314` iterates categories in hardcoded order: FR → CR → SF → CA → NEG. After CategoryConfig refactor, this becomes a loop over `CATEGORIES` entries with `regex_threshold.is_some()`, followed by NEGATIVE appended last.

The iteration order of `CATEGORIES` determines the index positions assigned to each category's patterns. The `classify()` function at line 376 uses `scores: HashMap<&str, u32>` keyed by category name (`meta.category` at line 378), so **pattern index order doesn't affect scores** — the HashMap aggregates by name. But `negative_idx` at line 378 (`i < self.negative_idx.start`) relies on NEGATIVE patterns being the LAST entries in the flat arrays. As long as NEGATIVE is always appended last, this is order-independent of the positive categories.

**Conclusion**: `CATEGORIES` iteration order for `build_all_patterns()` is **flexible** — any order works for scoring, provided NEGATIVE patterns come last. The `classify()` priority chain (lines 420-428) handles tie-breaking independently.

### Constructor Change Summary

| Constructor | File:Line | Current Signature | After Refactor |
|-------------|-----------|-------------------|----------------|
| `from_env()` | intent_classifier.rs:337 | `(routing, fallback_entry, short_prompt_len)` | Same signature; internally reads `CATEGORIES` for thresholds + pattern building |
| `from_values()` | intent_classifier.rs:354 | `(routing, fallback_entry, short_prompt_len)` | Same signature; same internal change |
| `test_classifier()` | intent_classifier.rs:460 | Builds routing + fallback inline | Builds routing from `CATEGORIES` names; same constructor call |
| `make_test_app_state()` | main.rs:695 | Takes `RegexClassifier` directly | Unchanged; wraps whatever classifier it receives |
| All test app builders | main.rs:744+ | Construct `HashMap<String, RouteEntry>` inline | String keys in test routing maps match `CATEGORIES[].name` |
