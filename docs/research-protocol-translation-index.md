# Protocol Translation Research — Index

Two independent implementation slices for bidirectional Anthropic ↔ OpenAI translation.

## Slice A: Anthropic → OpenAI (`research-slice-a-anthropic-to-openai.md`)

**Client speaks Anthropic** (e.g. Claude Code) → cerebrum translates → **upstream speaks OpenAI** (NVIDIA NIM, OpenRouter, Groq, Cerebras, Ollama).

New endpoint: `POST /v1/messages`

## Slice B: OpenAI → Anthropic (`research-slice-b-openai-to-anthropic.md`)

**Client speaks OpenAI** (existing `/v1/chat/completions` users) → cerebrum translates → **upstream speaks Anthropic** (Claude API, DeepSeek /anthropic, Kimi, Z.ai, Fireworks).

Enhancement to existing endpoint: `POST /v1/chat/completions`

## Implementation Order

Either slice can be built independently. They share:
- The same field mappings (just inverted)
- Same tool/tool_choice conversion logic
- Same stop_reason ↔ finish_reason mapping
- Same usage field renaming

Shared code: a `translate` module with functions callable from both directions.

## Provider Protocol Matrix

| Provider | Native Protocol | Needs Slice A? | Needs Slice B? |
|---|---|---|---|
| NVIDIA NIM | OpenAI | ✅ (for Claude Code clients) | — (passthrough) |
| OpenRouter | OpenAI | ✅ | — |
| Groq | OpenAI | ✅ | — |
| Cerebras | OpenAI | ✅ | — |
| Ollama | OpenAI | ✅ | — |
| **Claude API** | **Anthropic** | — (passthrough) | ✅ (for OpenAI clients) |
| **DeepSeek** | **Anthropic** | — (passthrough) | ✅ |
| **Kimi** | **Anthropic** | — (passthrough) | ✅ |
| **Z.ai** | **Anthropic** | — (passthrough) | ✅ |
| **Fireworks AI** | **Anthropic** | — (passthrough) | ✅ |

"Passthrough" = no body translation needed, just URL rewrite + correct auth headers.
