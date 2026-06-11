---
date: 2026-06-11T00:19:00Z
researcher: Kiro
branch: main
repository: cerebrum
topic: "YAML vs TOML vs hybrid approach for user-facing configuration"
tags: [research, configuration, yaml, toml, user-experience, regex-patterns]
status: complete
last_updated: 2026-06-11
---

# Config Format Research: YAML vs TOML vs Hybrid

## Context

Cerebrum's `config.toml` is ~180 lines containing:
- Simple settings (port, timeouts, log level)
- 40+ regex patterns with weights across 4 categories
- Routing tables (category → model/endpoint/provider)
- Auth provider list
- Negative patterns

The target users are DevOps/platform engineers deploying an AI gateway. They're deeply familiar with YAML from Kubernetes, docker-compose, and GitHub Actions. TOML is less widely known outside the Rust ecosystem.

**Core tension:** User familiarity favors YAML, but the config is ~70% regex patterns — YAML's weakest area.

---

## Format Comparison

### TOML (current)

**Strengths:**
- Literal strings (`'...'`) pass regex through verbatim — zero escaping
- Native Rust ecosystem (`toml = "0.8"` already a dependency)
- Every Rust developer knows it from Cargo.toml
- Type-safe — integers stay integers, no implicit coercion
- Parse errors are loud and line-precise

**Weaknesses:**
- Less familiar to non-Rust DevOps engineers
- Inline tables for patterns get visually dense (120+ char lines)
- No DRY/reuse mechanism (routing entries repeat endpoint/provider/api_key_env)
- `[[array_of_tables]]` syntax non-obvious to newcomers

**Regex example (clean, no escaping):**
```toml
{ regex = '(?i)\b(?:read|show|display)\s+(?:the\s+)?(?:file|contents)\b', weight = 3 }
```

### YAML

**Strengths:**
- Widely known (k8s, docker-compose, GitHub Actions, Ansible)
- Lower onboarding friction for ops engineers
- Natural for nested maps and lists
- Multi-line block scalars (`|`) for readability

**Weaknesses:**
- Regex escaping is a minefield:
  - Double-quoted: `\b` becomes backspace (must write `\\b`) — **silent data corruption**
  - Bare strings: `:`, `#`, `{`, `[`, `*`, `&`, `!` are YAML-special chars common in regex
  - Single-quoted: works but users habitually use double quotes or bare
- Indentation sensitivity (copy-paste between editors breaks structure)
- Implicit type coercion ("Norway problem"): `no` → false, `3.10` → 3.1
- Rust crate ecosystem is fragmented (serde_yaml deprecated 2023, multiple forks)
- Error messages often vague ("mapping values not allowed here")

**Regex example (escaping required in double quotes):**
```yaml
# Silent bug: \b is backspace in double quotes!
- regex: "(?i)\b(?:read|show)\s+file\b"     # WRONG — silent misbehavior
- regex: "(?i)\\b(?:read|show)\\s+file\\b"  # Correct but unreadable
- regex: '(?i)\b(?:read|show)\s+file\b'     # Single-quote works but non-obvious
```

### Alternative Formats Evaluated

| Format | Regex handling | Rust crate | User familiarity | Verdict |
|--------|---------------|------------|------------------|---------|
| JSON5 | Poor (must escape `\`) | serde_json5 | High | Not viable |
| HCL | Good (heredocs) | hcl-rs | Medium (Terraform users) | Overkill |
| KDL v2 | Excellent (`#"..."#` raw strings) | kdl v6.2 | Low | Strong but obscure |
| RON | Excellent (`r#"..."#` raw strings) | ron | Low (Rust gamedev) | Strong but niche |
| CUE | Medium | cue-rs (CGO wrapper) | Low | Not viable (Go dependency) |
| Pkl | Good | rpkl (requires JVM) | Low | Not viable (JVM dependency) |
| Dhall | Good (text literals) | serde_dhall | Very low | Too academic |

---

## Recommended Approach: Multi-Format + Externalized Patterns

### Architecture

```
cerebrum.yaml (or .toml)     # Structural config — user picks format
patterns/                    # Regex patterns — zero-escaping plain text
  file_reading.patterns
  syntax_fix.patterns
  complex_reasoning.patterns
  casual.patterns
  negative.patterns
```

### Structural config in YAML (familiar to target users)

```yaml
server:
  port: 10000
  log_level: info

http:
  client_timeout_secs: 120
  request_body_limit_bytes: 10485760

persistence:
  backend: memory

classifiers:
  enabled: true
  order: [regex, llm]

categories:
  FILE_READING:
    description: "Reading, viewing, inspecting files or code"
    threshold: 3
    priority: 1
    patterns_file: patterns/file_reading.patterns

  SYNTAX_FIX:
    description: "Fixing bugs, errors, typos, compilation issues"
    threshold: 3
    priority: 2
    dual_threshold:
      alt_score: 4
      suppress_if_present: FILE_READING
    patterns_file: patterns/syntax_fix.patterns

routing:
  FILE_READING:
    model: meta/llama-3.1-70b-instruct
    endpoint: https://integrate.api.nvidia.com/v1/chat/completions
    provider_type: nvidia_nim
    api_key_env: NVIDIA_API_KEY
  DEFAULT:
    model: meta/llama-3.1-8b-instruct
    endpoint: https://integrate.api.nvidia.com/v1/chat/completions
    provider_type: nvidia_nim
    api_key_env: NVIDIA_API_KEY

auth_providers:
  - type: openai_compatible
    header: authorization
    value_template: "Bearer {api_key}"
  - type: anthropic
    header: x-api-key
    value_template: "{api_key}"
  - type: ollama
```

### Pattern files (no escaping, copy-paste from regex101)

```
# patterns/file_reading.patterns
# Format: weight | regex (verbatim, rest of line)
#
3 | (?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file)\b
3 | (?i)\b(?:show|display|print|cat)\s+(?:me\s+)?(?:the\s+)?(?:content|output)(?:\s+of)?
3 | (?i)\b(?:[a-zA-Z0-9_\-./\\]+\.(?:rs|py|js|ts|go|java|c|cpp|h|to?ml|ya?ml|json|md|sql|sh|html))
2 | (?i)\b(?:look|go|navigate)\s+(?:at|through|to|into)\s+(?:the\s+)?(?:file|directory|code|source)
2 | (?i)\b(?:list|ls|dir|tree)\s+(?:files|directories|contents|all|the)
1 | (?i)\b(?:see|check|inspect|examine)\s+(?:the\s+)?(?:file|code|content|output|log)
```

### Negative patterns file

```
# patterns/negative.patterns
# Format: penalty | suppressed_category | regex
#
2 | COMPLEX_REASONING | (?i)\b(?:read|show|display|cat|view|open)\s+(?:the|this|my|a)\s+\w*(?:architecture|design|system)
2 | COMPLEX_REASONING | (?i)\b(?:fix|correct|repair)\s+(?:the|this|my)\s+(?:compile|syntax|typo|lint|warning|error)
2 | SYNTAX_FIX | (?i)\b(?:design|architect|refactor)\s+(?:a|the|an)\s+(?:fix|solution|remedy|patch|workaround)
2 | FILE_READING | (?i)\b(?:explain|describe|tell\s+me\s+about)\s+(?:the|this|that)\s+(?:file|code|class|module)
```

---

## Why This Hybrid Wins

| Factor | Pure YAML | Pure TOML | Hybrid (YAML + pattern files) |
|--------|-----------|-----------|-------------------------------|
| User familiarity (structural) | ✅ | ❌ | ✅ |
| Regex authoring safety | ❌ silent bugs | ✅ | ✅ zero escaping |
| Copy-paste from regex101 | ❌ must quote | ✅ literal strings | ✅ verbatim |
| IDE support | ✅ yaml-language-server | ✅ taplo | ✅ YAML linting for structure |
| Adoption friction | Low | Medium | Low |
| Rust implementation | ⚠️ fragmented crates | ✅ native | ✅ serde + trivial parser |

---

## Implementation Plan

### Phase 1: Serde derive refactor (prerequisite)
- Replace 1559 lines of manual `toml::Value` tree-walking with `#[derive(Deserialize)]` structs
- Cuts `config.rs` to ~250 lines
- Enables multi-format support trivially

### Phase 2: Multi-format support
- Add `serde-saphyr` (or `serde_yaml_ng`) dependency
- Detect format by file extension: `.yaml`/`.yml` → YAML, `.toml` → TOML
- Same struct definitions serve both formats via serde
- Keep `config.toml` working forever for backward compat

### Phase 3: External pattern files
- Add `patterns_file` field to `CategoryConfig` (optional, alternative to inline `patterns`)
- Add `patterns_dir` top-level config key (defaults to `./patterns/`)
- Trivial line parser: skip `#` comments, split on first ` | `, parse weight + regex
- Validate all patterns at startup (compile with `regex` crate, report errors with file:line)
- Keep inline patterns working as fallback for single-file deployments

### Phase 4: Migration tooling
- `cerebrum validate` — compiles all patterns, checks config schema
- `cerebrum migrate-config config.toml --output config.yaml --extract-patterns ./patterns/` — one-shot migration

---

## Rust Crate Recommendation for YAML

| Crate | Status (2026) | Notes |
|-------|---------------|-------|
| ~~serde_yaml~~ | Deprecated (2023) | Do not use |
| ~~serde_yml~~ | Deprecated (2026) | Do not use |
| **serde-saphyr** | ✅ Active, recommended | Pure Rust, YAML 1.2, no unsafe, no intermediate Value tree |
| serde_yaml_ng | ✅ Active | Drop-in serde_yaml replacement, uses unsafe-libyaml |

**Recommendation: `serde-saphyr`** — safest, most forward-looking choice.

---

## Decision Matrix

| Approach | Migration cost | User experience | Correctness | Recommended? |
|----------|---------------|-----------------|-------------|--------------|
| Stay TOML-only | 0 | ⚠️ Less familiar | ✅ | Acceptable |
| Switch to YAML-only | 3–5 days + ongoing escaping bugs | ✅ Familiar | ❌ Regex escaping | No |
| Multi-format (TOML + YAML) | 2–3 days (serde refactor) | ✅ User picks | ✅ | Yes |
| **Multi-format + external patterns** | 3–4 days total | ✅✅ Best of both | ✅✅ Zero escaping | **Yes — recommended** |

---

## Conclusion

Don't force a choice between TOML and YAML — support both via serde derive (trivial once refactored). The real UX win is externalizing regex patterns into their own dead-simple format where no config language's escaping rules apply. Default new installations to YAML + pattern files for maximum familiarity; keep TOML working for existing users and Rust developers who prefer it.
