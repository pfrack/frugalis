---
id: sse-streaming-proxy
status: implemented
created: 2026-06-01
updated: 2026-06-02
user: pfrack
tags: [upstream-routing, sse-streaming, keepalive, axum, change-4-of-4]
---
# sse-streaming-proxy

## What
Add SSE streaming responses to `POST /v1/chat/completions`. Upstream responses are streamed incrementally as SSE events with keepalive pings. Respects the client's `stream` field for backward compatibility. Part 4 of 4 in the upstream proxy routing sequence.

## Why
FR-004 (streaming) is a must-have requirement. Without streaming, long-running completions buffer entirely on the gateway, creating memory pressure and latency. SSE with keepalive prevents Render's 60s proxy timeout from killing active completions.

## Related Research
Master research: `context/changes/upstream-proxy-routing/research.md` (Sections 3-5, 7, 9, 12, 25-29)
