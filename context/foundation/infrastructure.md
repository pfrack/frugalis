---
project: cerebrum
researched_at: 2026-05-25
recommended_platform: Render
runner_up: Railway
context_type: mvp
tech_stack:
  language: Rust
  framework: Axum
  runtime: Tokio (async)
---

## Recommendation

**Deploy on Render.**

Render scored 5/5 on the agent-friendly criteria after a corrected research pass that confirmed `llms.txt` / `llms-full.txt` at `render.com/docs/`, markdown versions of every doc page via `.md` URL suffix, a GA MCP server, and a dedicated agent skills system (`render skills install`) that installs `render-deploy`, `render-debug`, and `render-monitor` into Claude Code, Cursor, and Codex in one command. Three hard-filter eliminations (Cloudflare Workers, Vercel, Netlify — all incompatible with a persistent Rust/Tokio binary) left only Render, Railway, and Fly.io as viable; Render's skills depth (deploy + debug + monitor + Jules integration) and fully accessible docs surface tipped the decision.

## Platform Comparison

### Hard-filter eliminations

The following platforms were dropped before scoring due to fundamental Rust/Axum/Tokio incompatibility:

| Platform | Reason dropped |
|---|---|
| Cloudflare Workers | WASM-only JS runtime; Axum/Tokio cannot compile to `wasm32-unknown-unknown`; no persistent process, no Tokio event loop. Cloudflare Containers could run Axum but is a paid-only product with a Worker wrapper layer and materially higher operational complexity. |
| Vercel | Serverless-functions-only platform; Rust support is beta and requires `vercel_runtime` bridge crate; persistent `TcpListener` is impossible. The official "Rust Axum" template is misleading — Axum runs inside a serverless wrapper, not as a real server. |
| Netlify | Rust is not a supported function runtime; no persistent process concept. Hard no. |

### Scoring matrix — viable candidates

| Criterion | Render | Railway | Fly.io |
|---|---|---|---|
| CLI-first | **Pass** — `render deploys create --wait`; `render logs`; `render ssh`; `render skills install` for agent skills | **Pass** — `railway` CLI covers deploy, logs, env vars, domains | **Pass** — `flyctl` covers deploy, rollback, logs, secrets, SSH |
| Managed/Serverless | **Pass** — fully managed; TLS, routing, health checks, autoscaling automatic | **Pass** — fully managed container platform | **Pass** — Firecracker micro-VMs; TLS, routing automatic |
| Agent-readable docs | **Pass** — `llms.txt` + `llms-full.txt` at `render.com/docs/`; every page available as `.md` via URL suffix; docs MCP server (experimental) | **Pass** — `llms.txt` + `llms-full.txt` published; all docs on GitHub as markdown | **Pass** — all docs on GitHub as `.md`/`.markerb`; raw GitHub URLs agent-fetchable |
| Stable deploy API | **Pass** — `render deploys create --wait`; `render-deploy` agent skill deploys via Blueprints or MCP; deploy hooks for CI | **Pass** — `railway up` / `railway redeploy`; REST API available | **Pass** — `fly deploy`; `--yes` flag for CI; `fly deploy --image <ref>` for rollback |
| MCP/Integration | **Pass** — GA MCP server (`render.com/docs/mcp-server`); `render-deploy` + `render-debug` + `render-monitor` skills; Jules by Google Labs integration for auto PR-fix | **Pass** — `@railway/mcp-server` GA; remote hosted at `mcp.railway.com`; `railway mcp install` auto-configures IDEs | **Fail** — no dedicated MCP server; CLI with `--json` flags is the agent interface |

**Score totals:** Render 5 Pass · Railway 5 Pass · Fly.io 4 Pass, 1 Fail

### Shortlisted Platforms

#### 1. Render (Recommended)

Render matched Railway's 5/5 score on the corrected research pass, and tips the decision on two axes: (a) its agent skills system (`render skills install`) goes deeper than MCP alone — `render-debug` diagnoses deployment failures using logs, metrics, and DB queries; `render-monitor` surfaces health and resource usage in real time — giving the agent a structured operational loop, not just a deploy command; (b) its Jules integration auto-detects and fixes failing PR preview builds without human intervention. Native Rust runtime support (no Dockerfile required for simple cases), `$7/month` Starter tier for always-on service, and a fully accessible doc surface (`llms.txt`, `llms-full.txt`, `.md` URL suffix on every page) complete the case.

#### 2. Railway

Railway also scores 5/5 and is marginally cheaper at low traffic (~$2–3/month above the $5 Hobby base when credits offset compute costs). Its 60-second proxy keep-alive timeout is a concrete risk for an AI gateway that may call slow upstream models — SSE keepalive pings are required in the Axum handler. Railpack (the Rust auto-build system) is under 3 months old and has minimal community battle-testing. Strong runner-up; prefer it over Render if cost is the absolute top constraint and the streaming timeout risk has been mitigated.

#### 3. Fly.io

Fly.io has the most battle-tested Rust deployment story in the candidate pool — a dedicated Axum guide, canonical `cargo-chef` Dockerfile pattern, and `fly deploy --image <ref>` for single-command rollback. It loses to both Render and Railway solely on MCP/Integration (no MCP server; CLI-only agent interface). Its pay-per-second autostop can make it the cheapest option at very low traffic, but it requires a credit card with no free tier and has no agent skills or debug tooling.

## Anti-Bias Cross-Check: Render

### Devil's Advocate — Weaknesses

1. **`render-deploy` skill expects a `render.yaml` Blueprint; the repo has none.** The skill deploys "using IaC with Render Blueprints or directly via MCP" — but the scaffolded repo has no `render.yaml`. The first agent-driven deploy attempt will stall or fall back to a guided dashboard flow. Writing a correct Blueprint (port, health check path, `PORT` env var, build command, start command) is not zero-config and is not covered in the quickstart.

2. **Free tier spins down after 15 minutes; Starter is a flat $7/month** — no credit offset like Railway's $5 Hobby plan. For a cost-first priority (Q2 = Minimize cost), Railway is marginally cheaper if the binary is mostly idle.

3. **Docs MCP server (`mcp.inkeep.com/render/mcp`) is explicitly experimental and third-party.** Render's own docs warn it may be discontinued without notice. It is operated by Inkeep, not Render — a third-party outage removes doc-query capability from the agent. The operational MCP server (for deploys and logs) is separate and GA.

4. **Rollback is not a single CLI command.** `render deploys list [SERVICE_ID]` exists, but triggering a rollback programmatically requires the REST API (`POST /v1/deploys/{id}/rollback`). The `render-deploy` skill focuses on new deploys, not rollbacks. Agent-driven rollback needs a shell wrapper or REST call.

5. **`render skills install` requires CLI v2.10+.** If an older `render` CLI is installed (no auto-update mechanism), the `skills` subcommand is absent and fails with an unhelpful error. Always run `render --version` before relying on the skills workflow.

### Pre-Mortem — How This Could Fail

> Development started on Render's free tier. Every session after a break started with a 60-second cold boot — the free tier spins down after 15 minutes of inactivity. Three redeploys per day at 12 minutes each (cold `cargo build --release`) meant 36 minutes of build wait daily. The dev switched to Starter ($7/month) after a week, eliminating spin-down but not fixing build times.
>
> The `render skills install` experience was smooth, but the first agent-driven deploy attempt via the `render-deploy` skill failed because no `render.yaml` was in the repo. After 2 hours writing and debugging the Blueprint config — documenting the `PORT` variable, health check path, and start command — the MCP deploy finally worked. This friction wasn't surfaced in any quickstart.
>
> Month three: the Inkeep docs MCP went offline for two days. The agent started hallucinating Render API syntax. A rollback was needed after a bad deploy; the agent couldn't find a rollback command in the CLI and emitted a REST API call with the wrong endpoint format. A human clicked the dashboard. The "fully agentic ops" promise had a hidden human-required gate that was never surfaced upfront.

### Unknown Unknowns

- **`render.yaml` Blueprint is required for the best agent-driven deploy experience.** Without it, `render-deploy` skill falls back to guided dashboard flow. Write a minimal `render.yaml` before wiring up any agent skill or CI automation.
- **Same PORT hardcoding gotcha as Railway.** The axum starter binds to port 3000; Render injects `PORT` dynamically (default 10000). First deploy health check will fail with "health check failed" rather than "wrong port."
- **Free workspace build minutes.** 500 free minutes/month. A cold Rust release build consumes 10–15 minutes; 2 deploys/day = up to 900 minutes/month — exceeds the free tier. Charged at $5/1000 minutes after that. Budget this before the Starter plan is active.
- **Docs MCP is third-party (Inkeep), not Render-operated.** Don't build agent workflows that depend on it for reliability; use `llms.txt` / the `.md` doc URL suffix as the primary doc-reading path — those are Render-operated and stable.
- **`render skills install` requires CLI ≥ v2.10.** Run `render --version` before assuming skills are available; upgrade with `brew upgrade render` or the install script if needed.

## Operational Story

- **Preview deploys**: Render creates pull-request preview URLs automatically when "Pull Request Previews" is enabled on a service. Preview URLs are public by default; protect with IP allowlists or a custom auth header if needed. Jules integration can automatically push fixes to failing PR previews when enabled via `dashboard.render.com/jules`.
- **Secrets**: Environment variables live in Render's service environment vault, set via `render env set KEY=value` or the dashboard. Group secrets in Render Environment Groups for reuse across services. Rotation: update the value via CLI or dashboard → Render triggers an automatic redeploy. Secret values are masked in logs and not readable after first set via CLI.
- **Rollback**: No single `render rollback` CLI command. Steps: `render deploys list [SERVICE_ID] --output json` to find the prior deploy ID → `POST https://api.render.com/v1/deploys/{deployId}/rollback` via REST API. Or: Dashboard → service → "Deploys" tab → "Rollback" button on the target deploy. Typical time-to-revert: 1–5 minutes (re-runs build pipeline unless image is cached). DB migrations do not roll back automatically.
- **Approval**: Human required for: deleting a service or environment, dropping a database, rotating the Render API key, enabling/disabling autoscaling. An agent may perform unattended: deploy (via CLI or `render-deploy` skill), set environment variables, restart a service, tail logs (via `render-debug` skill or CLI), query metrics (via `render-monitor` skill).
- **Logs**: `render logs [SERVICE_ID]` streams live runtime logs. MCP `render-debug` skill queries logs, metrics, and DB data in structured form for the agent. Build logs visible in dashboard or via REST API (`/v1/deploys/{deployId}/logs`). Syslog streaming to external providers (Datadog, etc.) is available on paid plans.

## Risk Register

| Risk | Source | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| No `render.yaml` causes agent-driven deploy to fall back to dashboard flow | Devil's advocate | High | Medium | Write `render.yaml` Blueprint as first commit before running `render skills install`; see Getting Started step 2 |
| Hardcoded port 3000 fails first deploy health check | Unknown unknowns | High | Medium | Update `src/main.rs` to read `PORT` from env before first `render deploys create`; see Getting Started step 1 |
| Free tier spin-down (15 min idle) creates slow feedback loop in development | Devil's advocate | High | Low | Switch to Starter ($7/month) on first paid deploy; avoid free tier for iterative Rust development |
| Docs MCP (Inkeep) outage disrupts agent doc-query workflows | Devil's advocate | Low | Medium | Use `https://render.com/docs/llms-full.txt` as primary agent doc source; treat docs MCP as a convenience only |
| Rollback requires REST API; no single CLI verb | Devil's advocate | Medium | Medium | Write a `scripts/rollback.sh` wrapper using `render deploys list --output json` + `curl` REST call; document in AGENTS.md |
| Free build minutes exhausted by slow Rust compile | Unknown unknowns | High | Low | Add `cargo-chef` Dockerfile for layer caching (cuts subsequent builds from ~12 min to ~1–2 min); upgrade to Starter plan |
| PORT env var not read by scaffolded `main.rs` | Unknown unknowns | High | Medium | One-line fix; same as Railway; must be applied before any deploy attempt |
| Upstream AI provider slow response causes long-response streaming issues | Research finding | Medium | High | Render does not have Railway's hard 60s proxy timeout, but add SSE keepalive pings in Axum handler as a defensive measure regardless |

## Getting Started

The following steps assume `cargo` and `render` CLI (v2.10+) are installed, and you have a Render account on the Starter plan ($7/month) or free trial.

1. **Fix the PORT binding before first deploy.** In `src/main.rs`, replace the hardcoded bind address:
   ```rust
   let port: u16 = std::env::var("PORT")
       .unwrap_or_else(|_| "10000".to_string())
       .parse()
       .expect("PORT must be a number");
   let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
   axum::serve(listener, app).await.unwrap();
   ```

2. **Add a `render.yaml` Blueprint** (required for agent-driven deploys via `render-deploy` skill):
   ```yaml
   services:
     - type: web
       name: cerebrum
       runtime: rust
       buildCommand: cargo build --release
       startCommand: ./target/release/cerebrum
       healthCheckPath: /health
       envVars:
         - key: RUST_LOG
           value: info
   ```
   Or use Docker with `cargo-chef` for faster incremental builds (recommended for production):
   ```yaml
   services:
     - type: web
       name: cerebrum
       runtime: docker
       healthCheckPath: /health
   ```

3. **Install agent skills** (enables `render-deploy`, `render-debug`, `render-monitor` in Claude Code / Cursor):
   ```bash
   render --version   # must be ≥ 2.10
   render skills install
   # Follow prompts to select Claude Code / Cursor / Codex; restart your IDE after
   ```

4. **Link repo and deploy:**
   ```bash
   render login
   render deploy create [SERVICE_ID] --wait --confirm
   # Or: push to the connected GitHub branch — Render auto-deploys on push
   ```

5. **Verify with agent debug skill.** After first deploy, ask your agent:
   ```
   Debug my Render deployment for cerebrum
   ```
   The `render-debug` skill will check for missing env vars, port binding errors, and resource constraints automatically.

## Out of Scope

The following were not evaluated in this research:
- Docker image configuration (beyond the recommended cargo-chef pattern above)
- CI/CD pipeline setup (GitHub Actions integration with Render deploy hooks)
- Production-scale architecture (multi-region, HA, DR)