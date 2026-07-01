# Change: Codex Responses API Shim

- id: codex-responses-api
- status: archived
- created: 2026-06-30
- updated: 2026-07-01
- archived_at: 2026-07-01T11:14:18Z
- commit: 46b6b72 (p1), b3d2534 (p2), 3967009 (p3), f3de924 (p4), 61f1cbf (p5)
- summary: New `POST /v1/responses` endpoint implementing the OpenAI Responses API as a translation layer over the existing `/v1/chat/completions` core, so modern Codex CLI (Responses-API-only) can use Frugalis. Reasoning items ↔ reasoning_content, tool-call items ↔ tool_calls, SSE event translation. Roadmap S-21, Tier-1 competitive gap #5. Prerequisites S-01e and S-15 are done.
