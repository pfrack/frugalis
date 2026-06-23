---
id: translate-openai-to-anthropic
status: implemented
created: 2026-06-22
updated: 2026-06-23
user: pfrack
tags: [protocol-translation, anthropic, openai, proxy, streaming]
---
# translate-openai-to-anthropic

## What
Enhance the existing `POST /v1/chat/completions` endpoint to detect when the routed upstream speaks Anthropic protocol, translate the OpenAI Chat Completions request to Anthropic Messages format, forward it, and translate the response (including SSE streaming) back to OpenAI format.

## Why
Enables existing OpenAI-speaking clients to use Anthropic-compatible providers (Claude API, DeepSeek /anthropic, Kimi, Z.ai, Fireworks AI) through cerebrum without changing the client's protocol.

## Related Research
`context/changes/translate-openai-to-anthropic/research.md`
