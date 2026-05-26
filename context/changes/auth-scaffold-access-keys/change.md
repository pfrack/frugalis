---
change_id: auth-scaffold-access-keys
title: Auth Scaffold Access Keys
status: plan_reviewed
created: 2026-05-26
updated: 2026-05-26
archived_at: null
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
