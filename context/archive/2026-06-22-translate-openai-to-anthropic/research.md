# Slice B: OpenAI Chat Completions → Anthropic Messages

> Use case: Cerebrum's existing `POST /v1/chat/completions` endpoint receives OpenAI-format
> traffic and routes it to an Anthropic-compatible upstream (Claude API, DeepSeek /anthropic,
> Kimi, Z.ai, Fireworks).

## Scope

- Request: transform OpenAI Chat body → Anthropic Messages body
- Response (non-streaming): transform Anthropic response → OpenAI response
- Response (streaming): transform Anthropic SSE → OpenAI SSE chunks
- Headers: set `x-api-key` + `anthropic-version` instead of `Authorization: Bearer`

---

## 1. Request Translation (OpenAI → Anthropic)

### 1.1 Top-Level Fields

| OpenAI Chat Field | Anthropic Field | Notes |
|---|---|---|
| `model` | `model` | Overridden by routing |
| `messages` | `system` + `messages` | System extracted; see §1.2 |
| `max_tokens` | `max_tokens` | **Required** in Anthropic; default 4096 if absent |
| `temperature` | `temperature` | Copy if present |
| `top_p` | `top_p` | Copy if present |
| `stop` | `stop_sequences` | string → `["str"]`; array → copy |
| `stream` | `stream` | Copy |
| `tools` | `tools` | See §1.3 |
| `tool_choice` | `tool_choice` | See §1.4 |
| `n` | — | Drop |
| `frequency_penalty` | — | Drop |
| `presence_penalty` | — | Drop |
| `logprobs` | — | Drop |
| `logit_bias` | — | Drop |
| `seed` | — | Drop |
| `response_format` | — | Drop |
| `stream_options` | — | Drop (Anthropic always returns usage) |

### 1.2 Messages Conversion

#### System messages → `system` field

Extract all `role: "system"` messages, join content with `"\n\n"`, set as top-level `system` string. Remove from messages array.

```json
// OpenAI input
{"messages": [
  {"role": "system", "content": "You are helpful."},
  {"role": "user", "content": "Hi"}
]}

// Anthropic output
{
  "system": "You are helpful.",
  "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}]
}
```

#### User messages

| OpenAI | Anthropic |
|---|---|
| `{"role": "user", "content": "text"}` | `{"role": "user", "content": [{"type": "text", "text": "text"}]}` |
| `{"role": "user", "content": [{"type": "text", "text": "..."}]}` | `{"role": "user", "content": [{"type": "text", "text": "..."}]}` |
| `{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,DATA"}}]}` | `{"role": "user", "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "DATA"}}]}` |

#### Assistant messages (text only)

```json
// OpenAI
{"role": "assistant", "content": "Hello"}

// Anthropic
{"role": "assistant", "content": [{"type": "text", "text": "Hello"}]}
```

#### Assistant messages with tool_calls

```json
// OpenAI
{"role": "assistant", "content": null, "tool_calls": [
  {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/src\"}"}}
]}

// Anthropic
{"role": "assistant", "content": [
  {"type": "tool_use", "id": "call_1", "name": "read_file", "input": {"path": "/src"}}
]}
```

Key: `arguments` (JSON string) → `input` (parsed object via `serde_json::from_str`).

#### Assistant messages with reasoning_content

```json
// OpenAI
{"role": "assistant", "content": "Answer", "reasoning_content": "Thinking..."}

// Anthropic
{"role": "assistant", "content": [
  {"type": "thinking", "thinking": "Thinking..."},
  {"type": "text", "text": "Answer"}
]}
```

#### Tool result messages

```json
// OpenAI
{"role": "tool", "tool_call_id": "call_1", "content": "result text"}

// Anthropic (merged into user message)
{"role": "user", "content": [
  {"type": "tool_result", "tool_use_id": "call_1", "content": "result text"}
]}
```

**Critical**: Anthropic requires strict user/assistant alternation. Consecutive `role: "tool"` messages must be merged into ONE `role: "user"` message with multiple `tool_result` blocks.

### 1.3 Tool Definitions

```json
// OpenAI
{"type": "function", "function": {"name": "X", "description": "D", "parameters": {schema}}}

// Anthropic
{"name": "X", "description": "D", "input_schema": {schema}}
```

### 1.4 Tool Choice

| OpenAI | Anthropic |
|---|---|
| `"auto"` | `{"type": "auto"}` |
| `"none"` | omit |
| `"required"` | `{"type": "any"}` |
| `{"type": "function", "function": {"name": "X"}}` | `{"type": "tool", "name": "X"}` |

### 1.5 Headers for Anthropic Upstream

```
x-api-key: <value from api_key_env>
anthropic-version: 2023-06-01
content-type: application/json
```

Do NOT send `Authorization: Bearer ...`.

---

## 2. Non-Streaming Response (Anthropic → OpenAI)

### 2.1 Top-Level Mapping

```json
// Anthropic response
{
  "id": "msg_abc",
  "type": "message",
  "role": "assistant",
  "model": "claude-sonnet-4-20250514",
  "content": [...],
  "stop_reason": "end_turn",
  "usage": {"input_tokens": 100, "output_tokens": 50}
}

// OpenAI response
{
  "id": "chatcmpl-abc",
  "object": "chat.completion",
  "model": "claude-sonnet-4-20250514",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "...", ...},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
}
```

### 2.2 Content Blocks → Message Fields

| Anthropic Block | OpenAI Field |
|---|---|
| `{"type": "text", "text": "X"}` | Concatenate into `message.content` |
| `{"type": "thinking", "thinking": "X"}` | `message.reasoning_content` (join if multiple) |
| `{"type": "redacted_thinking"}` | Omit |
| `{"type": "tool_use", "id", "name", "input"}` | `message.tool_calls[]` entry |

Tool use → tool_calls:
```json
// input (object) → arguments (JSON string)
{"id": "toolu_1", "type": "function", "function": {"name": "fn", "arguments": "{...}"}}
```

### 2.3 Stop Reason → Finish Reason

| Anthropic `stop_reason` | OpenAI `finish_reason` |
|---|---|
| `"end_turn"` | `"stop"` |
| `"max_tokens"` | `"length"` |
| `"tool_use"` | `"tool_calls"` |
| `"stop_sequence"` | `"stop"` |

### 2.4 Usage

| Anthropic | OpenAI |
|---|---|
| `input_tokens` | `prompt_tokens` |
| `output_tokens` | `completion_tokens` |
| — | `total_tokens` = input + output |

---

## 3. Streaming Response (Anthropic SSE → OpenAI SSE)

### 3.1 Event Mapping

| Anthropic Event | OpenAI Chunk Emitted |
|---|---|
| `message_start` | `{"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}` |
| `content_block_start` type=tool_use | `{"choices":[{"delta":{"tool_calls":[{"index":N,"id":"...","type":"function","function":{"name":"...","arguments":""}}]}}]}` |
| `content_block_delta` type=text_delta | `{"choices":[{"delta":{"content":"..."}}]}` |
| `content_block_delta` type=thinking_delta | `{"choices":[{"delta":{"reasoning_content":"..."}}]}` |
| `content_block_delta` type=input_json_delta | `{"choices":[{"delta":{"tool_calls":[{"index":N,"function":{"arguments":"..."}}]}}]}` |
| `content_block_stop` | — (no emission needed) |
| `message_delta` with stop_reason | `{"choices":[{"delta":{},"finish_reason":"stop"}]}` |
| `message_delta` with usage | `{"choices":[],"usage":{"prompt_tokens":N,"completion_tokens":N,"total_tokens":N}}` |
| `message_stop` | `data: [DONE]` |

### 3.2 State Tracking

- `chunk_id`: constant string per stream (from `message_start.message.id`)
- `model`: from `message_start.message.model`
- `tool_index`: counter for tool_calls array indices (increments per new tool_use block)

### 3.3 Chunk Envelope

Every emitted chunk:
```json
{"id": "<chunk_id>", "object": "chat.completion.chunk", "model": "<model>", "choices": [...]}
```

---

## 4. Edge Cases

1. **`max_tokens` is required** — Anthropic rejects requests without it. Default to 4096.
2. **Message alternation** — merge consecutive tool messages into single user message.
3. **Empty content** — OpenAI allows `"content": null` with tool_calls; Anthropic needs content array.
4. **`arguments` parsing** — must parse JSON string to object for `input`; if malformed, pass as `{"raw": "..."}`.
5. **anthropic-version header** — always `2023-06-01`.
6. **Usage always returned** — Anthropic streams usage in `message_delta`; no `stream_options` needed.
