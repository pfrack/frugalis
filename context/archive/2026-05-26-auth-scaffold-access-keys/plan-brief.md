# Auth Scaffold Access Keys — Plan Brief

> Full plan: context/changes/auth-scaffold-access-keys/plan.md

## What & Why

We are implementing the F-01 foundation that secures Cerebrum’s two core surfaces: proxy API routes and operator dashboard routes. The goal is to establish a fail-closed baseline now so all later slices (routing logic and dashboard visibility) inherit stable auth semantics instead of retrofitting security later.

## Starting Point

Current service is a minimal Axum binary with a single public /health endpoint and no auth middleware, no auth dependencies, and no auth test coverage. Deploy already supports env-based secrets through Render and GitHub webhook flow.

## Desired End State

Proxy traffic is gated by OpenAI-style Authorization Bearer token validation, dashboard traffic is gated by HTTP Basic auth, and /health remains public for platform health checks. Startup fails fast when auth secrets are missing, and unauthorized responses follow explicit contracts that clients and future slices can rely on.

## Key Decisions Made

| Decision | Choice | Why (1 sentence) | Source |
|---|---|---|---|
| Proxy auth scheme | Authorization Bearer token | Must match OpenAI-style usage for drop-in client behavior. | Plan (user input) |
| Proxy secret source | Single env var token | Lowest complexity and aligns with current Render secret model. | Plan |
| Dashboard auth | HTTP Basic with env user/pass | Matches single-operator MVP while keeping implementation lightweight. | Plan |
| Unauthorized behavior | Fail-closed explicit 401 contracts | Clear security semantics and predictable integration behavior. | Plan |
| Protected route scope | Protect proxy + dashboard; keep /health public | Meets security requirements without breaking Render health checks. | Plan |
| Missing config behavior | Fail fast at startup | Prevents insecure partial runtime and makes misconfig obvious in deploy logs. | Plan |
| Validation strictness | Strict parsing + constant-time compare | Reduces bypass/timing-risk with manageable implementation cost. | Plan |
| Plan architecture | Small modular split (auth + main) | Keeps scope tight now while avoiding future main.rs sprawl. | Plan |

## Scope

**In scope:**
- Bearer auth guard for proxy route group.
- Basic auth guard for dashboard route group.
- Startup-time env validation for required auth vars.
- Automated and manual verification for protected/public route contracts.
- Deploy/CI alignment for auth-related checks and required env keys.

**Out of scope:**
- OAuth/OIDC/session login flows.
- Multi-user roles/permissions.
- API key management endpoints.
- Database-backed credentials.
- JWT or external IdP integration.

## Architecture / Approach

Use a compact modular design: auth logic lives in a dedicated module (parsing, validation, response contracts), while router composition and app startup stay in main. Route groups apply appropriate guards by surface (proxy vs dashboard), and config is read from environment to stay aligned with Render secret handling.

## Phases at a Glance

| Phase | What it delivers | Key risk |
|---|---|---|
| 1. Auth Contracts and Configuration Guardrails | Reusable auth validation helpers + fail-fast env contract | Mis-specified env contract can block startup unexpectedly |
| 2. Route Protection and Response Semantics | Guarded proxy/dashboard routes with explicit 401 behavior | Incorrect route scoping can accidentally expose or over-block endpoints |
| 3. Verification and Deployment Readiness | Test coverage + CI/deploy alignment for auth | Missing pre-deploy checks could let auth regressions reach production |

**Prerequisites:** Access to set Render environment variables and run local cargo test/build commands.
**Estimated effort:** ~2-3 focused sessions across 3 phases.

## Open Risks & Assumptions

- Assumes a single static proxy bearer token is sufficient for MVP traffic patterns.
- Assumes Basic auth credentials are acceptable UX for operator-only dashboard access.
- Assumes future slices will keep route namespaces clear so auth grouping remains maintainable.

## Success Criteria (Summary)

- Protected routes reject missing/invalid credentials and accept valid credentials with stable response contracts.
- Public health endpoint remains accessible without auth and deployment health checks stay green.
- Auth startup and deployment guardrails prevent insecure runtime caused by missing secrets.
