---
change_id: auth-scaffold-access-keys
title: Auth Scaffold Access Keys
status: archived
created: 2026-05-26
updated: 2026-06-01
archived_at: 2026-06-01T20:37:16Z
---

## Notes

Foundation F-01 from roadmap: protect proxy routes with Bearer auth and dashboard route with HTTP Basic auth.

Final auth contracts:
- Proxy auth header: `Authorization: Bearer <token>`.
- Dashboard auth header: HTTP Basic (`Authorization: Basic <base64(user:password)>`).
- Required env vars: `PROXY_API_BEARER_TOKEN`, `DASHBOARD_BASIC_USER`, `DASHBOARD_BASIC_PASSWORD`.

Route protection matrix:
- Public: `GET /health`.
- Protected (Bearer): `POST /v1/chat/completions`.
- Protected (Basic): `GET /dashboard`.

## Deployment Evidence Trail

Date: 2026-05-29

- Render env configured:
	- Service: cerebrum
	- Keys present: PROXY_API_BEARER_TOKEN, DASHBOARD_BASIC_USER, DASHBOARD_BASIC_PASSWORD
	- Evidence: add Render dashboard screenshot or log URL
- Post-deploy smoke check:
	- GET /health -> 200 (public)
	- POST /v1/chat/completions without Bearer -> 401
	- GET /dashboard without Basic -> 401 + WWW-Authenticate Basic
	- Evidence: add command output or log URL
- Secret rotation check:
	- Rotated one auth secret, redeployed, old credential rejected
	- Evidence: add rotation timestamp and log URL
