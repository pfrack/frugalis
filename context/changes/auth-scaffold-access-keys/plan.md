# Auth Scaffold Access Keys Implementation Plan

## Overview

Implement the F-01 security foundation for Cerebrum by adding two auth gates: Bearer token auth for proxy API routes and HTTP Basic auth for dashboard routes. This establishes a fail-closed baseline so later slices can build on secure routing without reworking auth semantics.

## Current State Analysis

The service currently exposes only a public health endpoint and has no auth middleware, no route grouping, and no security-focused tests.

### Key Discoveries:

- Current routing is a single public route in src/main.rs:12-28.
- Dependencies are minimal and include only Axum/Tokio in Cargo.toml:1-8.
- Deploy path already supports env-driven config and secrets through Render + GitHub webhook flow in render.yaml:1-9 and .github/workflows/deploy.yml:1-37.
- Roadmap F-01 explicitly requires proxy key validation and dashboard auth gate in context/foundation/roadmap.md.

## Desired End State

Proxy routes are protected with Authorization: Bearer <token> validation, dashboard route is protected with HTTP Basic auth, and health checks remain public. Service startup fails fast if required auth configuration is missing, and unauthorized requests get explicit 401 responses with predictable contracts.

Verification for this end state is complete when automated tests validate both auth mechanisms and manual curl/browser checks confirm protected/public route behavior with expected status codes.

## What We're NOT Doing

- Multi-user auth, role matrices, or account lifecycle.
- OAuth/OIDC/session-based login UX.
- API key issuance/rotation API endpoints.
- Database-backed credential storage.
- JWT signing/verification or external identity providers.

## Implementation Approach

Use a small modular split: keep router composition in main and add a dedicated auth module for parsing, validation, and auth guards. This is the minimum structure that prevents main.rs from becoming a bottleneck while keeping phase scope small and aligned with the MVP timeline.

## Critical Implementation Details

Startup must fail fast when required auth env vars are absent to avoid running in a partially secure state. Auth checks must use strict header parsing and constant-time token comparison to reduce accidental bypass and timing-leak risk.

## Phase 1: Auth Contracts and Configuration Guardrails

### Overview

Define auth configuration contracts and reusable validation helpers so route protection can be wired consistently in Phase 2.

### Changes Required:

#### 1. Auth module scaffold

**File**: src/auth.rs

**Intent**: Introduce a focused module containing proxy Bearer auth validation and dashboard Basic auth validation primitives. Keep parsing and comparison logic out of route handlers.

**Contract**: Add reusable functions/guards for:
- Extracting and validating Authorization Bearer token against PROXY_API_BEARER_TOKEN.
- Extracting and validating HTTP Basic credentials against DASHBOARD_BASIC_USER and DASHBOARD_BASIC_PASSWORD.
- Returning route-appropriate unauthorized responses (API JSON 401, dashboard challenge 401).

#### 2. Startup configuration validation

**File**: src/main.rs

**Intent**: Ensure service refuses to boot when required auth secrets are missing to enforce fail-closed configuration semantics.

**Contract**: At startup, validate presence of PROXY_API_BEARER_TOKEN, DASHBOARD_BASIC_USER, and DASHBOARD_BASIC_PASSWORD. Missing values abort startup with clear error logs.

#### 3. Dependency updates

**File**: Cargo.toml

**Intent**: Add only required dependencies to support Basic auth decoding and secure token comparison behavior.

**Contract**: Update dependency list to support Basic auth header decode and constant-time secret comparison while staying compatible with current Axum/Tokio stack.

### Success Criteria:

#### Automated Verification:

- Project builds after auth scaffolding: cargo build --release
- Unit tests for auth parsing/validation pass: cargo test auth
- No formatting regressions: cargo fmt -- --check

#### Manual Verification:

- Service exits with explicit startup error when any required auth env var is missing.
- Service starts successfully when all required auth env vars are present.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 2: Route Protection and Response Semantics

### Overview

Apply auth gates to the correct route groups and enforce explicit unauthorized behavior while keeping health checks public.

### Changes Required:

#### 1. Router segmentation and guard wiring

**File**: src/main.rs

**Intent**: Protect only the intended surfaces: proxy and dashboard. Preserve open health endpoint for Render health checks.

**Contract**: Route contracts after this phase:
- Public: GET /health (no auth)
- Protected proxy path(s): Bearer auth required
- Protected dashboard path(s): Basic auth required

#### 2. Unauthorized response contracts

**File**: src/auth.rs

**Intent**: Standardize client-facing auth failure behavior so integrations and future features rely on stable semantics.

**Contract**:
- Proxy auth failure returns 401 JSON contract (machine-readable error payload).
- Dashboard auth failure returns 401 with WWW-Authenticate Basic challenge.
- Missing and invalid credentials are both fail-closed with no fallback to partial access.

#### 3. Placeholder protected handlers

**File**: src/main.rs

**Intent**: Add minimal protected handlers to validate auth wiring now, before S-01 and S-02 implement full business behavior.

**Contract**: Introduce simple placeholder routes for protected proxy and dashboard surfaces that can be hit during verification and later replaced by real slice handlers.

### Success Criteria:

#### Automated Verification:

- Build passes with route guards in place: cargo build --release
- Route-level integration tests for auth pass: cargo test routes_auth
- Lint checks pass if enabled in environment: cargo clippy --all-targets --all-features -- -D warnings

#### Manual Verification:

- GET /health returns 200 without auth.
- Protected proxy route returns 401 without/invalid Bearer token and success with valid token.
- Protected dashboard route returns Basic challenge on missing/invalid creds and success with valid creds.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

---

## Phase 3: Verification and Deployment Readiness

### Overview

Harden verification coverage and align deployment configuration so auth can be safely exercised in deployed environments.

### Changes Required:

#### 1. Deployment secret contract alignment

**File**: render.yaml

**Intent**: Reflect required auth env keys in deployment config so operators know required runtime inputs.

**Contract**: Document required env var keys for auth in service config without committing real secret values.

#### 2. CI verification enhancement

**File**: .github/workflows/deploy.yml

**Intent**: Prevent deployment of obvious auth regressions by running test gates before webhook trigger.

**Contract**: Add automated test step(s) for auth behavior prior to deploy hook execution.

#### 3. Change-local implementation notes

**File**: context/changes/auth-scaffold-access-keys/change.md

**Intent**: Record final auth contracts and operational assumptions for downstream slices.

**Contract**: Update notes with final header semantics, required env keys, and route protection matrix.

### Success Criteria:

#### Automated Verification:

- Full test run passes: cargo test
- Release build still passes: cargo build --release
- Deploy workflow syntax remains valid and references required checks.

#### Manual Verification:

- Render service has required auth env vars configured before deploy.
- Post-deploy smoke check confirms /health public and protected routes gated as expected.
- Rotating one auth secret via env update + redeploy invalidates previous credential behavior.

**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding to the next phase.

## Testing Strategy

### Unit Tests:

- Bearer parser accepts only strict Authorization: Bearer shape.
- Basic auth parser rejects malformed/non-base64 payloads.
- Constant-time comparison path returns correct match/no-match behavior.
- Missing env values at startup produce deterministic failure.

### Integration Tests:

- Public health endpoint remains unauthenticated.
- Proxy route auth matrix: no header, malformed header, wrong token, correct token.
- Dashboard auth matrix: no auth, wrong user, wrong pass, correct credentials.
- Response contract checks: JSON 401 for API and Basic challenge 401 for dashboard.

### Manual Testing Steps:

1. Start service without one required auth env var and confirm fail-fast startup error.
2. Start service with full auth env set and verify /health returns 200 without credentials.
3. Validate protected proxy endpoint with curl for unauthorized vs authorized Bearer requests.
4. Validate dashboard endpoint with browser/curl for Basic challenge and successful authenticated access.
5. After deploy, rotate one secret and verify old credential is rejected.

## Performance Considerations

Auth checks run on every protected request and must stay O(1) with minimal allocations. Keep header parsing simple, avoid external auth roundtrips, and ensure failed auth short-circuits before expensive handler logic.

## Migration Notes

No data migration is required. This phase introduces runtime config requirements only. Deployment environments must be updated with required auth env keys before enabling protected routes in production.

## References

- Roadmap item: context/foundation/roadmap.md
- Product constraints: context/foundation/prd.md
- Deploy configuration: render.yaml
- CI deploy path: .github/workflows/deploy.yml
- Current app entrypoint: src/main.rs

## Progress

> Convention: - [ ] pending, - [x] done. Append — <commit sha> when a step lands. Do not rename step titles.

### Phase 1: Auth Contracts and Configuration Guardrails

#### Automated

- [ ] 1.1 Project builds after auth scaffolding: cargo build --release
- [ ] 1.2 Unit tests for auth parsing/validation pass: cargo test auth
- [ ] 1.3 No formatting regressions: cargo fmt -- --check

#### Manual

- [ ] 1.4 Service exits with explicit startup error when any required auth env var is missing
- [ ] 1.5 Service starts successfully when all required auth env vars are present

### Phase 2: Route Protection and Response Semantics

#### Automated

- [ ] 2.1 Build passes with route guards in place: cargo build --release
- [ ] 2.2 Route-level integration tests for auth pass: cargo test routes_auth
- [ ] 2.3 Lint checks pass if enabled: cargo clippy --all-targets --all-features -- -D warnings

#### Manual

- [ ] 2.4 GET /health returns 200 without auth
- [ ] 2.5 Protected proxy route enforces Bearer token contract
- [ ] 2.6 Protected dashboard route enforces Basic auth challenge/acceptance

### Phase 3: Verification and Deployment Readiness

#### Automated

- [ ] 3.1 Full test run passes: cargo test
- [ ] 3.2 Release build still passes: cargo build --release
- [ ] 3.3 Deploy workflow remains valid with pre-deploy checks

#### Manual

- [ ] 3.4 Render service has required auth env vars configured before deploy
- [ ] 3.5 Post-deploy smoke check confirms public/protected route behavior
- [ ] 3.6 Secret rotation via env update + redeploy invalidates old credential behavior
