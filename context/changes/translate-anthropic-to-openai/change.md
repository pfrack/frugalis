---
id: translate-anthropic-to-openai
status: implementing
created: 2026-06-22
updated: 2026-06-23
user: pfrack
tags: [protocol-translation, anthropic, openai, proxy, streaming]
---
# translate-anthropic-to-openai

## What
Add a `POST /v1/messages` endpoint that accepts Anthropic Messages API format, translates it to OpenAI Chat Completions, forwards to an OpenAI-compatible upstream, and translates the response (including SSE streaming) back to Anthropic format.

## Why
Enables Claude Code (and any Anthropic-speaking client) to use cerebrum as a proxy to OpenAI-compatible providers (NVIDIA NIM, OpenRouter, Groq, Cerebras, Ollama) without changing the client's protocol.

## Related Research
`context/changes/translate-anthropic-to-openai/research.md`
