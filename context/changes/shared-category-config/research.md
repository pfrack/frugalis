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
