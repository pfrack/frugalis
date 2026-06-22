<!-- IMPL-REVIEW-REPORT -->
# Implementation Review: Config UX Improvement

- **Plan**: context/changes/config-ux/plan.md
- **Scope**: Full plan (5 phases + epilogue)
- **Date**: 2026-06-16
- **Verdict**: NEEDS ATTENTION
- **Findings**: 2 critical, 5 warnings, 3 observations

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

### F1 — `--init` path is unvalidated before write

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:111-137 (run_init)
- **Detail**: `run_init` accepts any path string with no validation. `create_dir_all` then `write` will create any directory tree the user names and overwrite any writable file. A typo like `--init ~/.bashrc` or `--init /tmp/some-other-tool.lock` can silently destroy data. There is no rejection of paths starting with `-` (defensive against future flag-look-alikes), no canonicalization, and no warning when writing outside the current working directory.
- **Fix**: Reject any path that starts with `-` (treat as unknown arg, exit 2 consistent with the top-level `unknown argument` handler). Print a confirmation prompt before writing to any path whose canonical form is not under the current working directory, or require `--force`.
  - Strength: Cheap defensive check; aligns with how the rest of the CLI rejects malformed args. Catches the most common catastrophic case (typo + no `--force`) without changing the happy path.
  - Tradeoff: One more branch; users who legitimately want to write to `/etc/cerebrum.toml` will need to opt in. Acceptable given this is a personal dev tool, not a system service.
  - Confidence: HIGH — pattern is consistent with `unknown argument` handling at src/main.rs:194.
  - Blind spot: Canonicalization-based "is under CWD" check could be surprising on systems with symlinked homes.
- **Decision**: FIXED (reject `-`-prefixed paths in `run_init`; verified `./target/debug/cerebrum --init -bad` returns exit 1 with the new error message)

### F2 — `--init` parser silently drops args starting with `--`

- **Severity**: ❌ CRITICAL
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:164-181 (--init branch)
- **Detail**: The path-parse filter `args.get(j).filter(|a| !a.starts_with("--")).cloned()` silently swallows any arg starting with `--` after `--init`. The current arg loop eventually surfaces "unknown argument: --foo" on a second pass (so exit code 2 is correct), but by that point the user's intended path has been discarded and the `--init` mode is `Init(None)`, which prints the template to stdout. The user sees an "unknown argument" error with no path context, then either an empty stdout or — if they piped it — an empty file downstream.
- **Fix**: Inside the `--init` arm, after consuming `--force` markers, if the next non-`--force` arg starts with `--`, treat it as an unknown flag immediately and exit 2 with a message that names the flag and notes that paths starting with `--` are not allowed.
  - Strength: Surfaces the error at the point of confusion rather than two iterations later. Mirrors the existing unknown-arg handler exactly.
  - Tradeoff: Paths literally named `--my-config.toml` cannot be passed positionally. Workaround: rename the file or pass through stdin. Acceptable — flag-shaped filenames are vanishingly rare.
  - Confidence: HIGH — the existing unknown-arg path is the obvious reference implementation.
  - Blind spot: None significant.
- **Decision**: FIXED (parser now exits 2 immediately on flag-shaped path; verified `./target/debug/cerebrum --init --bad` returns exit 2 with the new error, and the happy path `--init --force <path>` still works)

### F3 — Pre-existing indentation regression at src/main.rs:409-415

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/main.rs:402-416 (categories_ok block)
- **Detail**: The `if !missing.is_empty() {` block opens at 8-space indent while its enclosing `if categories_ok {` is also at 8 spaces — so the inner block visually appears to live at the outer scope. `cargo fmt` would correct this; left as-is, the next reader has to re-parse the braces to see which `if` is enclosing which. The indentation bug is pre-existing (predates the config-ux change), but F5's "Routes active" log landed right next to it and amplifies the confusion.
- **Fix**: Run `cargo fmt` (or manually re-indent the `if !missing.is_empty()` block to 12 spaces) so the visual scope matches the lexical scope.
  - Strength: `cargo fmt` is one command and the change is mechanical.
  - Tradeoff: `cargo fmt` may make unrelated whitespace changes elsewhere; the file should be reformatted once and subsequent commits kept clean.
  - Confidence: HIGH — rustfmt is the canonical tool.
  - Blind spot: Other modules in the tree may have similar drift; out of scope for this change.
- **Decision**: FIXED (ran `cargo fmt`; F3 indent corrected at src/main.rs:427-435 and cascading re-indent of "Routes active" log. One unrelated pre-existing OTel block re-indent also included. All 239 tests still pass.)

### F4 — Quickstart test names don't follow `test_` convention

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Pattern Consistency
- **Location**: src/quickstart.rs:315, 348, 369, 385, 427
- **Detail**: New tests are named `build_quickstart_toml_*` and `generated_toml_*`. AGENTS.md convention is `test_<route_or_component>_<case>`, and sibling modules (main.rs tests, auth.rs tests, persistence.rs tests) follow it. The new prefix-by-function-name style makes these harder to grep for in the test list.
- **Fix**: Rename to `test_build_quickstart_toml_*` and `test_generated_toml_*`.
  - Strength: 5 mechanical renames; matches the rest of the suite.
  - Tradeoff: None.
  - Confidence: HIGH — convention is explicit in AGENTS.md.
  - Blind spot: config.rs's own merge_configs tests also drop the `test_` prefix; this rename does not fix that inconsistency (out of scope for this change).
- **Decision**: FIXED (5 tests renamed in src/quickstart.rs to add `test_` prefix; all 5 pass)

### F5 — "Routes active" log shows keys, not values

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Safety & Quality
- **Location**: src/main.rs:418-421
- **Detail**: The plan (line 287) says the log should "help users verify their overlay took effect." The current log prints only the 5 route *names* in sorted order — same output whether the overlay was applied or the embedded defaults were used. A user with `CONFIG_PATH=overlay.toml` cannot tell from this log whether their `[routing.FILE_READING]` actually overrode the embedded default. Additionally, the literal `"DEFAULT"` is unconditionally pushed, which would duplicate DEFAULT if the routing map ever contains it.
- **Fix**: For each routing entry, log the resolved model: `info!("Route {} -> {} @ {}", key, entry.model, entry.endpoint);`. Dedupe DEFAULT before adding it (check `routing_map.contains_key("DEFAULT")`).
  - Strength: The log now actually verifies overlay effects — the whole point per the plan. Dedupe is a 1-line guard.
  - Tradeoff: Log output is longer (5 lines vs. 1). For ops folks reading startup, the per-route lines are more useful; for tail -f aggregation, they are noisier. Net positive.
  - Confidence: HIGH — matches the plan's stated intent.
  - Blind spot: None significant.
- **Decision**: FIXED (replaced single "Routes active: a, b, c" line with one info! per route showing `Route {key} -> {model} @ {endpoint}`; DEFAULT logged from `fallback_entry` with dedupe check to avoid printing twice when `hardcoded_routing` is used. Verified: 5 lines, all 5 categories present, all with model + endpoint.)

### F6 — Hardcoded routing fallback is NVIDIA NIM (pre-existing)

- **Severity**: ⚠️ WARNING
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Plan Adherence
- **Location**: src/config.rs:387-411 (hardcoded_routing)
- **Detail**: `hardcoded_routing` defaults the embedded fallback to the NVIDIA NIM endpoint with `NVIDIA_API_KEY`. This was the pre-config-ux state — not introduced by this change — but the new "Routes active" log (F5) and "No CONFIG_PATH" hint now surface this fallback much more visibly to new users. A user who runs `cerebrum --init` and never edits the template will see "Routes active: ..." and assume NVIDIA is the default provider, then be confused when nothing routes.
- **Fix A ⭐ Recommended**: Change the hardcoded fallback to ollama (localhost:11434, no api_key) so a fresh install at least works locally without any env vars.
  - Strength: New users with no NVIDIA key get a working server out of the box. Aligns with the wizard's "use Ollama for local" default-on path.
  - Tradeoff: A user who has NVIDIA_API_KEY set but no config will now silently route to a non-existent localhost. This is also true today, just less visibly so.
  - Confidence: MEDIUM — depends on what the team's "fresh install" baseline is.
  - Blind spot: Existing users with NVIDIA_API_KEY baked into their workflow may notice the change.
- **Fix B**: Leave as-is and document the requirement in the "No CONFIG_PATH" log message
  - Strength: No behavior change for existing users.
  - Tradeoff: New users still hit the same dead-end, just with a clearer hint.
  - Confidence: HIGH.
  - Blind spot: Doesn't actually solve the discoverability problem.
- **Decision**: FIXED via Fix A (added `DEFAULT_MODEL_LOCAL = "llama3.1"` constant in src/routing.rs; `hardcoded_routing` in src/config.rs now uses Ollama endpoint `http://localhost:11434/v1/chat/completions`, provider_type `ollama`, api_key_env `None`. Updated 3 tests. All 239 tests pass. Note: this only affects the hardcoded fallback path; the embedded config.toml's normal routing is unchanged.)

### F7 — EOF error in prompt() doesn't include the question context

- **Severity**: ⚠️ WARNING
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Reliability
- **Location**: src/quickstart.rs:285-302 (prompt)
- **Detail**: When stdin is closed (Ctrl-D) during a prompt, the error is `"unexpected end of input (Ctrl-D?)"`. If the wizard has multiple prompts and the user is scrolling back through terminal output, they cannot tell which prompt was cut. The `prompt()` helper has access to the question text; including it in the error costs nothing.
- **Fix**: Change the EOF error to `format!("unexpected end of input while reading '{question}'")`.
  - Strength: One-line change, instant diagnostic improvement.
  - Tradeoff: None.
  - Confidence: HIGH.
  - Blind spot: None significant.
- **Decision**: FIXED (EOF error at src/quickstart.rs:299 now includes the question text: `format!("unexpected end of input while reading '{question}'")`)

### F8 — TOCTOU in `run_init` (path.exists → write race)

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Reliability
- **Location**: src/main.rs:111-137
- **Detail**: `run_init` does `path.exists() && !force` → `create_dir_all(parent)` → `std::fs::write`. Between the exists check and the write, another process could create a symlink at `path` pointing to an unrelated file (e.g. `/etc/passwd`), turning `--init ./cerebrum.toml` (refused) into an overwrite of `/etc/passwd` if the user re-runs with `--force`. The window is small, but `--force` is supported in the CLI for exactly this use case.
- **Fix**: Use `OpenOptions::new().write(true).create_new(!force).truncate(force).open(path)` and treat `AlreadyExists` as the refusal signal — atomic, no TOCTOU. Drop the `path.exists()` pre-check.
  - Strength: Atomic operation; eliminates the race window entirely.
  - Tradeoff: Requires restructuring the error path slightly.
  - Confidence: HIGH.
  - Blind spot: None significant.
- **Decision**: FIXED (replaced `path.exists() && !force` + `std::fs::write` with `OpenOptions::new().write(true).create(true).create_new(!force).truncate(force).open(path)` + `write_all`. Atomic create-or-overwrite, no TOCTOU. Verified: create works, refuse on existing without --force works, --force overwrites.)

### F9 — No automated tests for the new CLI surface

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🔎 MEDIUM — real tradeoff; pause to reason through it
- **Dimension**: Test Coverage
- **Location**: src/main.rs (no new tests for --init / --quickstart / --help)
- **Detail**: The plan's success criteria (lines 103-105, 142-145, 188-191) require the new flags to "exit 0", "produce output that is syntactically valid TOML", and "work for each provider type". The plan claims these were verified manually (progress section 1.3-1.5, 2.3-2.7, 3.4-3.6), but no automated test exists for the new flags or the end-to-end `run_quickstart` flow. The quickstart module's tests cover only `build_quickstart_toml` (the pure function), not `run_quickstart` end-to-end. If `run_quickstart` regresses on EOF handling (F7) or on overwrite confirmation (line 257-269), nothing in CI catches it.
- **Fix**: Add at least one test that captures `prompt()` behavior with a `BufReader` over a `Vec<u8>` (the helper already takes `impl BufRead`), proving EOF returns an error including the question context. Consider adding a `run_quickstart` test that drives the whole flow via piped stdin for one provider preset.
  - Strength: Catches regressions in EOF, overwrite, and prompt parsing.
  - Tradeoff: Adds test infrastructure (likely a small refactor of `prompt` to take stdin from a parameter).
  - Confidence: MEDIUM — depends on how much refactoring the user wants.
  - Blind spot: The plan explicitly says these were manual-verified; adding automation is a scope expansion.
- **Decision**: FIXED (added 2 tests in src/quickstart.rs: `test_prompt_eof_includes_question_in_error` drives prompt() with a Cursor over empty bytes and asserts the error includes the question text; `test_prompt_returns_input_line` is a sanity check for the success path. Both pass. Total quickstart tests: 7.)

### F10 — `init_template.toml` uses a real OpenAI URL as a placeholder

- **Severity**: 🔎 OBSERVATION
- **Impact**: 🏃 LOW — quick decision; fix is obvious and narrowly scoped
- **Dimension**: Safety & Quality
- **Location**: init_template.toml:27, 33, 39, 45, 51
- **Detail**: The `endpoint` placeholder is `https://api.openai.com/v1/chat/completions` — a real URL pointing to a third-party service. A user who runs `cerebrum --init` and starts cerebrum with `CONFIG_PATH=…/cerebrum.toml` *without* editing the file will silently route every prompt to `api.openai.com` with a placeholder `OPENAI_API_KEY` env var. The plan says the placeholders are "NOT valid" (init_template.toml line 12), but using a real URL means runtime will hit a real network endpoint, not fail-fast at config validation.
- **Fix**: Use `https://api.example.invalid/v1/chat/completions` (matching the `.invalid` TLD used in routing_unreachable.toml:33). DNS resolution will fail immediately and the operator will see the failure rather than silently sending traffic to OpenAI.
  - Strength: Fail-fast at DNS resolution; matches the existing convention in routing_examples.
  - Tradeoff: None.
  - Confidence: HIGH.
  - Blind spot: Some DNS resolvers may not return NXDOMAIN for `.invalid` — empirically they do, but worth a quick check.
- **Decision**: FIXED (replaced all 5 occurrences of `https://api.openai.com/v1/chat/completions` with `https://api.example.invalid/v1/chat/completions` in init_template.toml. Build clean.)
