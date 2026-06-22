# Config UX Improvement — Plan Brief

> Full plan: `context/changes/config-ux/plan.md`
> Research: `context/changes/config-ux/research.md`

## What & Why

Cerebrum's config system is architecturally sound but hostile to new users — zero CLI discoverability, a routing merge that silently breaks when users specify partial overlays, broken examples, and no startup guidance. This plan adds `--help`, `--init`, `--quickstart`, fixes the merge semantics for routing, and updates all examples to work correctly.

## Starting Point

Manual arg parsing recognizes only `--validate`. Config overlay via `CONFIG_PATH` env var — undiscoverable. `merge_configs()` does full-replacement for routing (specify one route → lose four). All 4 routing examples use an obsolete flat format that silently produces no overrides. No startup message hints at configuration.

## Desired End State

A new user can run `cerebrum --help` to discover configuration, `cerebrum --init` to get a starter template, or `cerebrum --quickstart` to interactively generate a working config. Partial routing overlays work intuitively (per-key merge). Routing examples are valid CONFIG_PATH overlays. The server logs a helpful hint when running on defaults.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|----------|--------|-------------------|--------|
| Routing merge semantics | Per-key additive (overlay entries override matching keys, rest inherit) | Eliminates the #1 UX footgun; matches user expectations from server/http merge behavior | Research |
| Include `--quickstart` wizard | Yes | Covers zero-knowledge onboarding for users unfamiliar with TOML | Plan |
| Wizard provider support | All 4 known types + Custom | Covers all provider_type variants without dead-ends | Plan |
| `--init` output behavior | Optional path arg, default stdout | Composable (pipes/redirects) by default with convenient shortcut | Plan |
| `--init` template format | TOML with placeholders | Matches embedded config.toml; provider-neutral | Research |
| Startup messaging | Single-line `info!` log | Non-intrusive in server logs; grep-friendly | Plan |
| CLI framework | None (stay with manual parsing) | 4 flags don't justify a new dependency | Research |

## Scope

**In scope:**
- `--help` flag with usage text
- `--init [PATH]` with embedded template (TOML, placeholder values)
- `--quickstart` interactive wizard (OpenRouter, Anthropic, NVIDIA NIM, Ollama, Custom)
- Per-key additive merge for routing in `merge_configs()`
- Rewrite all 4 routing examples to `[routing.CATEGORY]` format
- Startup info log when no CONFIG_PATH set
- Route summary log on startup

**Out of scope:**
- CLI framework (clap/structopt)
- Changing merge semantics for categories/negative_patterns
- YAML output from init/quickstart
- Provider-specific `--init` templates
- Config validation beyond existing `--validate`

## Architecture / Approach

All changes touch the startup path only — zero impact on request handling. The arg parser expands to dispatch on mode (Help/Init/Quickstart/Validate/Run). Early-exit modes (help, init, quickstart) run before tracing init and print directly to stdout/stderr. The merge fix is a 5-line change in `merge_configs()`. Routing examples are straightforward TOML rewrites.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|-------|-----------------|----------|
| 1. CLI Foundation | `--help` flag, arg parsing restructure | Low — mechanical refactor |
| 2. `--init` Command | Starter template generation | Low — template design quality |
| 3. `--quickstart` Wizard | Interactive provider setup | Medium — stdin UX in server binary |
| 4. Per-Key Routing Merge | Additive routing overlay | Medium — subtle breaking change for full-replacement reliers |
| 5. Examples + Messaging | Fixed examples, startup hints | Low — copy-edit level work |

**Prerequisites:** None — all changes are self-contained within the cerebrum repo.
**Estimated effort:** ~2-3 sessions across 5 phases (each phase is independently shippable).

## Open Risks & Assumptions

- Per-key merge is a behavioral change — any existing user relying on "routing overlay wipes all defaults" will get different behavior (considered unlikely based on the research finding that this behavior is a footgun, not a feature)
- The `--quickstart` wizard uses synchronous stdin reads in a `#[tokio::main]` binary — this works fine since it runs before the async runtime is needed, but the pattern is unusual

## Success Criteria (Summary)

- A new user can go from zero to working config in under 2 minutes using `--init` or `--quickstart`
- A partial routing overlay (1-2 routes) doesn't break the other routes
- `cerebrum --help` provides all information needed to configure the system
