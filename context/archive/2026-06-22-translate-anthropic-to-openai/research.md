# Slice A: Anthropic Messages → OpenAI Chat Completions

> Use case: A new `POST /v1/messages` endpoint receives Anthropic-format traffic
> (e.g. from Claude Code) and routes it to an OpenAI-compatible upstream
> (NVIDIA NIM, OpenRouter, Groq, Cerebras, Ollama).

## Scope

- Request: transform Anthropic Messages body → OpenAI Chat Completions body
- Response (non-streaming): transform OpenAI response → Anthropic response
- Response (streaming): transform OpenAI SSE chunks → Anthropic SSE events
- Headers: set `Authorization: Bearer` instead of `x-api-key`

---

## 1. Request Translation (Anthropic → OpenAI)

### 1.1 Top-Level Fields

| Anthropic Field | OpenAI Field | Notes |
|---|---|---|
| `model` | `model` | Overridden by routing |
| `max_tokens` | `max_tokens` | Copy; omit if ≤ 0 |
| `temperature` | `temperature` | Copy if present |
| `top_p` | `top_p` | Copy if present |
| `top_k` | — | Drop |
| `stop_sequences` | `stop` | Array; single-element → string |
| `stream` | `stream` | Copy |
| — | `stream_options.include_usage` | Set `true` when streaming |
| `system` | first `role: "system"` message | See §1.2 |
| `messages` | `messages` | See §1.2 |
| `tools` | `tools` | See §1.3 |
| `tool_choice` | `tool_choice` | See §1.4 |
| `thinking` | — | See §1.5 |
| `metadata` | — | Drop |

### 1.2 Messages Conversion

#### System prompt → system message

```json
// Anthropic (string)
{"system": "You are helpful.", "messages": [...]}
// Anthropic (block array)
{"system": [{"type": "text", "text": "Part 1"}, {"type": "text", "text": "Part 2"}]}

// OpenAI: prepend to messages
[{"role": "system", "content": "Part 1\n\nPart 2"}, ...]
```

Join block texts with `"\n\n"`.

#### User messages

| Anthropic | OpenAI |
|---|---|
| `{"role": "user", "content": "text"}` | `{"role": "user", "content": "text"}` |
| `{"role": "user", "content": [{"type": "text", "text": "X"}]}` | `{"role": "user", "content": "X"}` (join if multiple) |
| `{"role": "user", "content": [{"type": "image", "source": {"type": "base64", "media_type": "M", "data": "D"}}]}` | `{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:M;base64,D"}}]}` |
| `{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "X", "content": "R"}]}` | `{"role": "tool", "tool_call_id": "X", "content": "R"}` |

Multiple `tool_result` blocks in one user message → each becomes a separate `role: "tool"` message.

#### Assistant messages

| Anthropic Block | OpenAI |
|---|---|
| `[{"type": "text", "text": "X"}]` | `{"role": "assistant", "content": "X"}` |
| `[{"type": "tool_use", "id": "I", "name": "N", "input": {O}}]` | `{"role": "assistant", "content": "", "tool_calls": [{"id": "I", "type": "function", "function": {"name": "N", "arguments": "{...}"}}]}` |
| `[{"type": "thinking", "thinking": "T"}, {"type": "text", "text": "X"}]` | `{"role": "assistant", "content": "X", "reasoning_content": "T"}` |
| Mixed text + tool_use | text → `content`; tool_use → `tool_calls` |

Key: `input` (object) → `arguments` (JSON string via `serde_json::to_string`).

**`redacted_thinking` blocks**: always drop.

**Post-pass reasoning fix** (for DeepSeek/Kimi): if ANY message has `reasoning_content`, ALL assistant messages with `tool_calls` but no reasoning need `reasoning_content: " "` (space).

### 1.3 Tool Definitions

```json
// Anthropic
{"name": "X", "description": "D", "input_schema": {schema}}

// OpenAI
{"type": "function", "function": {"name": "X", "description": "D", "parameters": {schema}}}
```

### 1.4 Tool Choice

| Anthropic | OpenAI |
|---|---|
| `{"type": "auto"}` / `"auto"` | `"auto"` |
| `{"type": "any"}` / `"any"` | `"required"` |
| `{"type": "none"}` / `"none"` | `"none"` |
| `{"type": "tool", "name": "X"}` | `{"type": "function", "function": {"name": "X"}}` |

### 1.5 Thinking/Reasoning

No standard OpenAI request field. Options per provider:
- Providers with native reasoning (DeepSeek, Kimi via OpenAI-compat): leave it; model decides
- Generic providers: drop `thinking` param entirely
- Think-tag providers: no request change needed

---

## 2. Non-Streaming Response (OpenAI → Anthropic)

### 2.1 Top-Level Mapping

```json
// OpenAI response
{
  "id": "chatcmpl-abc",
  "object": "chat.completion",
  "model": "gpt-4o",
  "choices": [{"index": 0, "message": {...}, "finish_reason": "stop"}],
  "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
}

// Anthropic response
{
  "id": "msg_abc",
  "type": "message",
  "role": "assistant",
  "model": "gpt-4o",
  "content": [...],
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "usage": {"input_tokens": 100, "output_tokens": 50}
}
```

### 2.2 Message → Content Blocks

| OpenAI Field | Anthropic Content Block |
|---|---|
| `message.content` (string) | `[{"type": "text", "text": "..."}]` |
| `message.reasoning_content` | `[{"type": "thinking", "thinking": "..."}]` (prepend) |
| `message.tool_calls[i]` | `[{"type": "tool_use", "id": "...", "name": "...", "input": {...}}]` |

Order: thinking → text → tool_use.

Tool calls: `arguments` (string) → `input` (parsed object).

### 2.3 Finish Reason → Stop Reason

| OpenAI `finish_reason` | Anthropic `stop_reason` |
|---|---|
| `"stop"` | `"end_turn"` |
| `"length"` | `"max_tokens"` |
| `"tool_calls"` | `"tool_use"` |
| `"function_call"` | `"tool_use"` |
| `"content_filter"` | `"end_turn"` |

### 2.4 Usage

| OpenAI | Anthropic |
|---|---|
| `prompt_tokens` | `input_tokens` |
| `completion_tokens` | `output_tokens` |
| `total_tokens` | — (omit) |

---

## 3. Streaming (OpenAI SSE → Anthropic SSE)

This requires a **stateful emitter** tracking which content block is open.

### 3.1 State Machine

Variables:
- `block_index: usize` — current content block index
- `open_block: Option<"text"|"thinking"|"tool">` — what's open now
- `message_started: bool`
- `tool_state: HashMap<usize, {id, name}>` — tool call metadata per index

### 3.2 Event Mapping

| OpenAI Chunk | Emitter Action |
|---|---|
| First chunk (has `delta.role`) | Emit `event: message_start` |
| `delta.reasoning_content` (first) | Close any open block → emit `content_block_start` (thinking) → emit `content_block_delta` (thinking_delta) |
| `delta.reasoning_content` (more) | Emit `content_block_delta` (thinking_delta) |
| `delta.content` (first) | Close any open block → emit `content_block_start` (text) → emit `content_block_delta` (text_delta) |
| `delta.content` (more) | Emit `content_block_delta` (text_delta) |
| `delta.tool_calls[i]` (new index) | Close any open block → emit `content_block_start` (tool_use, with id+name) → emit `content_block_delta` (input_json_delta) |
| `delta.tool_calls[i]` (existing) | Emit `content_block_delta` (input_json_delta) |
| `finish_reason` present | Close any open block → emit `message_delta` (stop_reason + usage) |
| `data: [DONE]` | Emit `message_stop` |

"Close any open block" = emit `content_block_stop` with current index, increment `block_index`.

### 3.3 Anthropic SSE Event Payloads

**message_start:**
```
event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"...","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}
```

**content_block_start (text):**
```
event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
```

**content_block_start (thinking):**
```
event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
```

**content_block_start (tool_use):**
```
event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}
```

**content_block_delta (text):**
```
event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
```

**content_block_delta (thinking):**
```
event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"..."}}
```

**content_block_delta (tool input):**
```
event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}
```

**content_block_stop:**
```
event: content_block_stop
data: {"type":"content_block_stop","index":0}
```

**message_delta:**
```
event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":42}}
```

**message_stop:**
```
event: message_stop
data: {"type":"message_stop"}
```

---

## 4. Edge Cases

1. **`stream_options.include_usage = true`** — needed to get usage in last OpenAI chunk; some providers don't support it (emit 0 tokens).
2. **Block transitions** — must close previous block before opening new one.
3. **Empty tool arguments** — if `delta.tool_calls[i].function.arguments` is empty string, still emit as `input_json_delta`.
4. **Usage deferred** — freedius defers `finish_reason` emission until usage arrives (or `[DONE]` flushes).
5. **NIM sanitization** — strip unknown fields before sending to NVIDIA.
6. **No thinking support** — most OpenAI providers won't return `reasoning_content`; only emit thinking blocks if the field is actually present.
