# Anthropic Pass-Through Proxy

> Use case: A new `POST /v1/messages` endpoint receives Anthropic Messages API traffic,
> classifies intent, and forwards verbatim to an Anthropic-compatible upstream.
> No protocol translation — both client and upstream speak Anthropic.

## Scope

- New route: `POST /v1/messages`
- Extract last user message from Anthropic format for classification
- Route via existing classifier (same as `/v1/chat/completions`)
- Auth: `x-api-key` + `anthropic-version` headers for `provider_type: "anthropic"` upstreams
- Streaming: byte-forwarding (same as existing OpenAI pass-through)
- Override `model` field in forwarded request based on classification

---

## 1. Anthropic Messages Request Format

### 1.1 Minimal Request

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "Hello"}
  ]
}
```

### 1.2 Full Request (fields the proxy passes through)

| Field | Type | Notes |
|---|---|---|
| `model` | string | Overridden by routing classification |
| `max_tokens` | integer | Required — pass through |
| `messages` | array | Pass through verbatim |
| `system` | string or array | Pass through verbatim |
| `temperature` | float | Optional — pass through |
| `top_p` | float | Optional — pass through |
| `top_k` | integer | Optional — pass through |
| `stop_sequences` | array | Optional — pass through |
| `stream` | boolean | Determines response mode |
| `tools` | array | Optional — pass through |
| `tool_choice` | object | Optional — pass through |
| `thinking` | object | Optional — pass through |
| `metadata` | object | Optional — pass through |

### 1.3 Content Block Types in Messages

User messages can contain:
- `{"type": "text", "text": "..."}` — plain text
- `{"type": "image", "source": {"type": "base64", ...}}` — images
- `{"type": "tool_result", "tool_use_id": "...", "content": [...]}` — tool results

Assistant messages can contain:
- `{"type": "text", "text": "..."}` — text
- `{"type": "tool_use", "id": "...", "name": "...", "input": {...}}` — tool calls
- `{"type": "thinking", "thinking": "..."}` — reasoning

---

## 2. Extracting User Prompt for Classification

The existing `extract_last_user_message` (src/persistence.rs:1084) parses OpenAI format:
```rust
messages.iter().rev().find(|m| m["role"] == "user")["content"].as_str()
```

For Anthropic format, we need an equivalent that handles:
1. `"content": "string"` — simple text content
2. `"content": [{"type": "text", "text": "..."}]` — array of content blocks
3. Multiple text blocks → join with space
4. Non-text blocks (images, tool_results) → skip

**Implementation**: `extract_last_user_message_anthropic(body: &str) -> String`
- Parse JSON, get `messages` array
- Find last element with `"role": "user"`
- If `content` is a string, return it (truncated to 10,000 chars)
- If `content` is an array, collect all `type: "text"` blocks' `text` fields, join with space
- Cap at 1,000 messages (DoS protection, matching existing behavior)

---

## 3. Auth Headers for Anthropic Upstreams

### 3.1 Current Behavior (src/intent_classifier.rs:427)

`auth_headers_for` looks up `provider_type` in `AuthProviderConfig` list:
- Matching config → use `header` + `value_template` (with `{api_key}` substitution)
- No match → default to `Authorization: Bearer {api_key}`

### 3.2 Required Config for Anthropic

```toml
[[auth_providers]]
type = "anthropic"
header = "x-api-key"
value_template = "{api_key}"
```

Additionally, Anthropic requires `anthropic-version` header. Options:
1. Add as a second header in AuthProviderConfig (requires schema change)
2. Hard-code in the handler when `provider_type == "anthropic"`
3. Add to config with a new `extra_headers` field

**Recommended**: Hard-code `anthropic-version: 2023-06-01` in the handler for anthropic provider_type. It's a protocol constant, not user-configurable.

---

## 4. Model Override

The proxy overrides the client's `model` field with the classifier-selected model. Same pattern as existing `build_upstream_request` (src/main.rs:840-877):

```rust
if let serde_json::Value::Object(map) = &mut req_body {
    map.insert("model".to_string(), serde_json::Value::String(classification.model.clone()));
}
```

Identical for Anthropic format — the `model` field is top-level in both protocols.

---

## 5. Streaming (Byte-Forwarding)

For pass-through, streaming is identical to the existing implementation:
- Check `req_body["stream"] == true`
- If upstream returns 2xx → `handle_streaming_response` (byte-forwarding with keepalive)
- If upstream returns non-2xx → `handle_streaming_error`

Anthropic SSE format is different from OpenAI, but since we're passing through verbatim (client and upstream both speak Anthropic), no parsing is needed.

---

## 6. Response Handling (Non-Streaming)

Pass-through: forward the upstream response body and status code verbatim. Same as existing `handle_buffered_response` with the body-size cap.

---

## 7. Error Responses

The proxy's own errors (auth failure, no upstream, classification failure) should return Anthropic-format errors since the client speaks Anthropic:

```json
{"type": "error", "error": {"type": "invalid_request_error", "message": "..."}}
```

Upstream errors from Anthropic are already in this format — pass through verbatim.

---

## 8. OpenAPI Spec

Add `POST /v1/messages` to `openapi/completions.yaml` per lessons.md ("Use OpenAPI Generator for Endpoints").

---

## 9. Observability

Same OTel metrics as `/v1/chat/completions`:
- `requests_total` with `route: "/v1/messages"`
- `request_duration_seconds`
- `upstream_duration_seconds`
- `classification_total`

Same `log_classification` call for persistence/dashboard visibility.
