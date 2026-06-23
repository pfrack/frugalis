use serde_json::json;
use tracing::debug;

/// Tracks state across an Anthropic→OpenAI streaming translation session.
/// Each field is updated as Anthropic SSE events arrive and used to
/// produce well-formed OpenAI SSE chunks.
#[derive(Debug, Default)]
pub struct StreamTranslateState {
    /// Stable chunk ID for all OpenAI chunks in this stream (from message_start).
    pub chunk_id: String,
    /// Model name forwarded into every OpenAI chunk envelope.
    pub model: String,
    /// Maps Anthropic content_block index → OpenAI tool_calls array index.
    /// Multiple tool_use blocks map to incrementing tool_call indices.
    pub tool_index: usize,
    /// Tracks which content block type is currently open so we know
    /// whether to emit finish_reason on message_delta.
    pub has_tool_use: bool,
    /// Set once message_start has been emitted so the role chunk
    /// is only sent once.
    pub started: bool,
}

// ──────────────────────────────────────────────────────────────────────
// §1  Request Translation  (OpenAI → Anthropic)
// ──────────────────────────────────────────────────────────────────────

/// Translate an OpenAI Chat Completions request body into an Anthropic
/// Messages request body.
///
/// Key transformations:
/// - System messages → top-level `system` field
/// - User/assistant/tool messages → Anthropic content-block arrays
/// - Tool definitions: `function.name/description/parameters` → `name/description/input_schema`
/// - Tool choice: `"auto"/"none"/"required"/{"type":"function",…}` → Anthropic equivalents
/// - `max_tokens` defaults to 4096 when absent (Anthropic requires it)
pub fn translate_request(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = body.as_object().ok_or("request body must be a JSON object")?;

    let mut out = serde_json::Map::new();

    // ── model ──────────────────────────────────────────────────────────
    if let Some(model) = obj.get("model") {
        out.insert("model".into(), model.clone());
    }

    // ── max_tokens (required by Anthropic; default 4096) ───────────────
    let max_tokens = obj
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(4096);
    out.insert("max_tokens".into(), json!(max_tokens));

    // ── temperature / top_p (pass through if present) ──────────────────
    if let Some(t) = obj.get("temperature") {
        out.insert("temperature".into(), t.clone());
    }
    if let Some(tp) = obj.get("top_p") {
        out.insert("top_p".into(), tp.clone());
    }

    // ── stop_sequences ─────────────────────────────────────────────────
    if let Some(stop) = obj.get("stop") {
        let seqs = match stop {
            serde_json::Value::String(s) => json!([s]),
            serde_json::Value::Array(_) => stop.clone(),
            _ => json!([]),
        };
        out.insert("stop_sequences".into(), seqs);
    }

    // ── stream ─────────────────────────────────────────────────────────
    if let Some(stream) = obj.get("stream") {
        out.insert("stream".into(), stream.clone());
    }

    // ── messages ───────────────────────────────────────────────────────
    let messages = obj
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or("messages must be an array")?;

    let (system_text, converted_messages) = convert_messages(messages)?;

    if !system_text.is_empty() {
        out.insert("system".into(), json!(system_text));
    }
    out.insert("messages".into(), serde_json::Value::Array(converted_messages));

    // ── tools ──────────────────────────────────────────────────────────
    if let Some(tools) = obj.get("tools") {
        if let Some(arr) = tools.as_array() {
            let anthropic_tools: Vec<serde_json::Value> = arr
                .iter()
                .filter_map(|t| {
                    let func = t.get("function")?;
                    Some(json!({
                        "name": func.get("name")?,
                        "description": func.get("description"),
                        "input_schema": func.get("parameters").cloned().unwrap_or(json!({}))
                    }))
                })
                .collect();
            if !anthropic_tools.is_empty() {
                out.insert("tools".into(), json!(anthropic_tools));
            }
        }
    }

    // ── tool_choice ────────────────────────────────────────────────────
    if let Some(tc) = obj.get("tool_choice") {
        let anthropic_tc = match tc {
            serde_json::Value::String(s) => match s.as_str() {
                "auto" => Some(json!({"type": "auto"})),
                "none" => None, // omit
                "required" => Some(json!({"type": "any"})),
                _ => Some(json!({"type": "auto"})),
            },
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|v| v.as_str()) == Some("function") {
                    let name = map
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    Some(json!({"type": "tool", "name": name}))
                } else {
                    Some(json!({"type": "auto"}))
                }
            }
            _ => Some(json!({"type": "auto"})),
        };
        if let Some(tc_val) = anthropic_tc {
            out.insert("tool_choice".into(), tc_val);
        }
    }

    Ok(serde_json::Value::Object(out))
}

/// Extract system messages and convert remaining messages to Anthropic format.
/// Returns `(system_text, converted_messages)`.
fn convert_messages(
    messages: &[serde_json::Value],
) -> Result<(String, Vec<serde_json::Value>), String> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut converted: Vec<serde_json::Value> = Vec::new();

    // First pass: extract system messages, convert everything else.
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "system" => {
                let text = extract_content_text(msg);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "user" => {
                converted.push(convert_user_message(msg));
            }
            "assistant" => {
                converted.push(convert_assistant_message(msg));
            }
            "tool" => {
                // Collect; will merge consecutive tool messages below.
                converted.push(convert_tool_message(msg));
            }
            _ => {
                // Unknown role — pass through as-is.
                converted.push(msg.clone());
            }
        }
    }

    // Merge consecutive role:"user" messages with only tool_result blocks
    // into a single user message. Anthropic requires strict alternation.
    let merged = merge_consecutive_roles(converted);

    let system_text = system_parts.join("\n\n");
    Ok((system_text, merged))
}

/// Extract text from a message's `content` field (string or array of blocks).
fn extract_content_text(msg: &serde_json::Value) -> String {
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            texts.join("\n\n")
        }
        _ => String::new(),
    }
}

/// Convert an OpenAI user message to Anthropic format.
fn convert_user_message(msg: &serde_json::Value) -> serde_json::Value {
    let content = match msg.get("content") {
        Some(serde_json::Value::String(s)) => {
            json!([{"type": "text", "text": s}])
        }
        Some(serde_json::Value::Array(blocks)) => {
            let converted: Vec<serde_json::Value> = blocks
                .iter()
                .map(|b| {
                    let btype = b.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                    match btype {
                        "text" => {
                            let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            json!({"type": "text", "text": text})
                        }
                        "image_url" => {
                            let url = b
                                .get("image_url")
                                .and_then(|iu| iu.get("url"))
                                .and_then(|u| u.as_str())
                                .unwrap_or("");
                            // Parse data URI: data:<media_type>;base64,<data>
                            if let Some((media_type, data)) = parse_data_uri(url) {
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data
                                    }
                                })
                            } else {
                                // URL-based image — pass as-is (some Anthropic providers support URL)
                                json!({
                                    "type": "image",
                                    "source": {
                                        "type": "url",
                                        "url": url
                                    }
                                })
                            }
                        }
                        _ => b.clone(),
                    }
                })
                .collect();
            json!(converted)
        }
        _ => json!([{"type": "text", "text": ""}]),
    };
    json!({"role": "user", "content": content})
}

/// Parse a `data:<media_type>;base64,<data>` URI into (media_type, data).
fn parse_data_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("data:")?;
    let (media_type, data_part) = rest.split_once(';')?;
    let data = data_part.strip_prefix("base64,")?;
    Some((media_type.to_string(), data.to_string()))
}

/// Convert an OpenAI assistant message to Anthropic format.
fn convert_assistant_message(msg: &serde_json::Value) -> serde_json::Value {
    let mut blocks: Vec<serde_json::Value> = Vec::new();

    // reasoning_content → thinking block (prepend)
    if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            blocks.push(json!({"type": "thinking", "thinking": reasoning}));
        }
    }

    // text content → text block
    if let Some(content) = msg.get("content") {
        match content {
            serde_json::Value::String(s) if !s.is_empty() => {
                blocks.push(json!({"type": "text", "text": s}));
            }
            serde_json::Value::String(_) => {
                // null or empty string — skip
            }
            serde_json::Value::Array(arr) => {
                // OpenAI assistant content is rarely an array, but handle it.
                for b in arr {
                    if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                        let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            serde_json::Value::Null => {}
            _ => {}
        }
    }

    // tool_calls → tool_use blocks
    if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments_str = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let input = serde_json::from_str::<serde_json::Value>(arguments_str)
                .unwrap_or_else(|_| json!({"raw": arguments_str}));
            blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            }));
        }
    }

    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }

    json!({"role": "assistant", "content": blocks})
}

/// Convert an OpenAI tool result message to a user message with tool_result blocks.
fn convert_tool_message(msg: &serde_json::Value) -> serde_json::Value {
    let tool_call_id = msg
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = msg
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": tool_call_id,
            "content": content
        }]
    })
}

/// Merge consecutive same-role messages to satisfy Anthropic's strict
/// user/assistant alternation requirement. Specifically, consecutive
/// `role: "user"` messages (which include converted tool results) are
/// merged by concatenating their content arrays.
fn merge_consecutive_roles(messages: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut merged: Vec<serde_json::Value> = Vec::new();
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(last) = merged.last_mut() {
            let last_role = last.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == last_role && role == "user" {
                // Merge content arrays.
                let last_content = last
                    .get_mut("content")
                    .and_then(|c| c.as_array_mut());
                let new_content = msg.get("content").and_then(|c| c.as_array());
                if let (Some(existing), Some(new_blocks)) = (last_content, new_content) {
                    existing.extend(new_blocks.iter().cloned());
                    continue;
                }
            }
        }
        merged.push(msg);
    }
    merged
}

// ──────────────────────────────────────────────────────────────────────
// §2  Response Translation  (Anthropic → OpenAI)
// ──────────────────────────────────────────────────────────────────────

/// Translate an Anthropic Messages response body into an OpenAI Chat
/// Completions response body.
pub fn translate_response(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = body.as_object().ok_or("response body must be a JSON object")?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("chatcmpl-unknown");
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let content_blocks = obj
        .get("content")
        .and_then(|v| v.as_array())
        .ok_or("content must be an array")?;

    // Build OpenAI message fields from content blocks.
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    for block in content_blocks {
        let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "thinking" => {
                if let Some(thinking) = block.get("thinking").and_then(|v| v.as_str()) {
                    reasoning_parts.push(thinking.to_string());
                }
            }
            "tool_use" => {
                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or(json!({}));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|e| {
                    debug!("tool input serialization failed: {e}");
                    "{}".to_string()
                });
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }));
            }
            "redacted_thinking" => {
                // Omit redacted thinking blocks.
            }
            _ => {}
        }
    }

    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));

    // content: concatenate text parts
    let content_str = text_parts.join("");
    message.insert("content".into(), json!(content_str));

    // reasoning_content (if any)
    if !reasoning_parts.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning_parts.join("")));
    }

    // tool_calls
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), json!(tool_calls));
    }

    // ── finish_reason ──────────────────────────────────────────────────
    let stop_reason = obj
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "stop_sequence" => "stop",
        _ => "stop",
    };

    // ── usage ──────────────────────────────────────────────────────────
    let (prompt_tokens, completion_tokens) = if let Some(usage) = obj.get("usage") {
        let inp = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let out = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        (inp, out)
    } else {
        (0, 0)
    };

    Ok(json!({
        "id": format!("chatcmpl-{}", id),
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": serde_json::Value::Object(message),
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    }))
}

// ──────────────────────────────────────────────────────────────────────
// §3  Error Translation  (Anthropic error → OpenAI error envelope)
// ──────────────────────────────────────────────────────────────────────

/// Translate an Anthropic error body into an OpenAI error envelope.
///
/// Anthropic shape: `{"type":"error","error":{"type":"…","message":"…"}}`
/// OpenAI shape:    `{"error":{"message":"…","type":"…","code":"…"}}`
pub fn translate_error(body: &str, status: u16) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "error": {
                    "message": body,
                    "type": "upstream_error",
                    "code": status
                }
            })
            .to_string();
        }
    };

    let (error_type, message) = if let Some(error_obj) = parsed.get("error") {
        let t = error_obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("upstream_error");
        let m = error_obj
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(body);
        (t, m)
    } else if let (Some(t), Some(m)) = (
        parsed.get("type").and_then(|v| v.as_str()),
        parsed.get("message").and_then(|v| v.as_str()),
    ) {
        (t, m)
    } else {
        ("upstream_error", body)
    };

    json!({
        "error": {
            "message": message,
            "type": error_type,
            "code": status
        }
    })
    .to_string()
}

// ──────────────────────────────────────────────────────────────────────
// §4  Streaming Translation  (Anthropic SSE → OpenAI SSE)
// ──────────────────────────────────────────────────────────────────────

/// Parse raw SSE bytes into `(event_type, data)` pairs.
///
/// SSE format:
/// ```
/// event: <type>\n
/// data: <json>\n
/// \n
/// ```
pub fn parse_sse_events(bytes: &[u8]) -> Vec<(String, String)> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut events = Vec::new();
    let mut current_event = String::new();
    let mut current_data = String::new();

    for line in text.lines() {
        if line.is_empty() {
            // Empty line = end of event.
            if !current_data.is_empty() {
                events.push((
                    if current_event.is_empty() {
                        "message".to_string()
                    } else {
                        current_event.clone()
                    },
                    current_data.clone(),
                ));
            }
            current_event.clear();
            current_data.clear();
        } else if let Some(rest) = line.strip_prefix("event: ") {
            current_event = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("data: ") {
            if current_data.is_empty() {
                current_data = rest.to_string();
            } else {
                current_data.push('\n');
                current_data.push_str(rest);
            }
        }
        // Lines starting with `:` are comments — ignore.
    }

    // Flush trailing event (no trailing blank line).
    if !current_data.is_empty() {
        events.push((
            if current_event.is_empty() {
                "message".to_string()
            } else {
                current_event
            },
            current_data,
        ));
    }

    events
}

/// Translate a single Anthropic SSE event into one or more OpenAI SSE
/// chunks. Returns `None` if the event produces no output (e.g.
/// `content_block_stop`).
pub fn translate_stream_event(
    event_type: &str,
    data: &str,
    state: &mut StreamTranslateState,
) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(data).ok()?;

    match event_type {
        "message_start" => {
            let msg = parsed.get("message")?;
            state.chunk_id = msg
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("chatcmpl-stream")
                .to_string();
            state.model = msg
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            state.started = true;
            state.tool_index = 0;
            state.has_tool_use = false;

            Some(make_openai_chunk(
                &state.chunk_id,
                &state.model,
                json!([{
                    "index": 0,
                    "delta": {"role": "assistant", "content": ""},
                    "finish_reason": null
                }]),
            ))
        }

        "content_block_start" => {
            let _index = parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let block = parsed.get("content_block")?;
            let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match btype {
                "tool_use" => {
                    state.has_tool_use = true;
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let tool_idx = state.tool_index;
                    // tool_index will be incremented when we see the next tool_use.
                    // For now, the index in the chunk matches the tool_index.
                    Some(make_openai_chunk(
                        &state.chunk_id,
                        &state.model,
                        json!([{
                            "index": tool_idx,
                            "delta": {
                                "tool_calls": [{
                                    "index": tool_idx,
                                    "id": id,
                                    "type": "function",
                                    "function": {"name": name, "arguments": ""}
                                }]
                            },
                            "finish_reason": null
                        }]),
                    ))
                }
                "text" | "thinking" => {
                    // No chunk emitted at start — content arrives in deltas.
                    None
                }
                _ => {
                    debug!("unknown content_block_start type: {btype}");
                    None
                }
            }
        }

        "content_block_delta" => {
            let delta = parsed.get("delta")?;
            let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match dtype {
                "text_delta" => {
                    let text = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    if text.is_empty() {
                        return None;
                    }
                    Some(make_openai_chunk(
                        &state.chunk_id,
                        &state.model,
                        json!([{
                            "index": 0,
                            "delta": {"content": text},
                            "finish_reason": null
                        }]),
                    ))
                }
                "thinking_delta" => {
                    let thinking = delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                    if thinking.is_empty() {
                        return None;
                    }
                    Some(make_openai_chunk(
                        &state.chunk_id,
                        &state.model,
                        json!([{
                            "index": 0,
                            "delta": {"reasoning_content": thinking},
                            "finish_reason": null
                        }]),
                    ))
                }
                "input_json_delta" => {
                    let partial = delta
                        .get("partial_json")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Determine the tool_call index from the content_block index
                    // in the parent event. We track via tool_index counter.
                    // The Anthropic event doesn't carry the content_block index
                    // in the delta, so we use the current tool_index.
                    let tool_idx = state.tool_index;
                    Some(make_openai_chunk(
                        &state.chunk_id,
                        &state.model,
                        json!([{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": tool_idx,
                                    "function": {"arguments": partial}
                                }]
                            },
                            "finish_reason": null
                        }]),
                    ))
                }
                _ => {
                    debug!("unknown content_block_delta type: {dtype}");
                    None
                }
            }
        }

        "content_block_stop" => {
            // If we were in a tool_use block, advance tool_index.
            if state.has_tool_use {
                state.tool_index += 1;
                state.has_tool_use = false;
            }
            None
        }

        "message_delta" => {
            let delta = parsed.get("delta")?;
            let usage = parsed.get("usage");

            let mut chunks = Vec::new();

            // stop_reason → finish_reason
            if let Some(stop_reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                let finish_reason = match stop_reason {
                    "end_turn" => "stop",
                    "max_tokens" => "length",
                    "tool_use" => "tool_calls",
                    "stop_sequence" => "stop",
                    _ => "stop",
                };
                chunks.push(make_openai_chunk(
                    &state.chunk_id,
                    &state.model,
                    json!([{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]),
                ));
            }

            // usage → separate usage chunk
            if let Some(usage) = usage {
                let prompt_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let completion_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let usage_chunk = json!({
                    "id": format!("chatcmpl-{}", state.chunk_id),
                    "object": "chat.completion.chunk",
                    "model": state.model,
                    "choices": [],
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": prompt_tokens + completion_tokens
                    }
                });
                chunks.push(format!("data: {}\n\n", usage_chunk));
            }

            if chunks.is_empty() {
                None
            } else {
                Some(chunks.join("\n"))
            }
        }

        "message_stop" => Some("data: [DONE]\n\n".to_string()),

        // Ping and other events — ignore.
        _ => {
            debug!("unhandled SSE event type: {event_type}");
            None
        }
    }
}

/// Build a `data: <json>\n\n` SSE frame from an OpenAI chunk object.
fn make_openai_chunk(
    chunk_id: &str,
    model: &str,
    choices: serde_json::Value,
) -> String {
    let chunk = json!({
        "id": format!("chatcmpl-{}", chunk_id),
        "object": "chat.completion.chunk",
        "model": model,
        "choices": choices
    });
    format!("data: {}\n\n", chunk)
}

// ──────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request translation tests ──────────────────────────────────────

    #[test]
    fn test_basic_text_request() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"}
            ]
        });
        let result = translate_request(&input).unwrap();
        assert_eq!(result.get("system").unwrap().as_str().unwrap(), "You are helpful.");
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "user");
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content[0].get("text").unwrap().as_str().unwrap(), "Hello");
    }

    #[test]
    fn test_multiple_system_messages_joined() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Part 1"},
                {"role": "system", "content": "Part 2"},
                {"role": "user", "content": "Hi"}
            ]
        });
        let result = translate_request(&input).unwrap();
        assert_eq!(
            result.get("system").unwrap().as_str().unwrap(),
            "Part 1\n\nPart 2"
        );
    }

    #[test]
    fn test_user_image_url() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Look:"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc123"}}
                ]}
            ]
        });
        let result = translate_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1].get("type").unwrap().as_str().unwrap(), "image");
        assert_eq!(
            content[1].get("source").unwrap().get("media_type").unwrap().as_str().unwrap(),
            "image/png"
        );
        assert_eq!(
            content[1].get("source").unwrap().get("data").unwrap().as_str().unwrap(),
            "abc123"
        );
    }

    #[test]
    fn test_assistant_with_tool_calls() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/src\"}"}}
                ]}
            ]
        });
        let result = translate_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].get("type").unwrap().as_str().unwrap(), "tool_use");
        assert_eq!(content[0].get("id").unwrap().as_str().unwrap(), "call_1");
        assert_eq!(content[0].get("name").unwrap().as_str().unwrap(), "read_file");
        assert_eq!(content[0].get("input").unwrap().get("path").unwrap().as_str().unwrap(), "/src");
    }

    #[test]
    fn test_assistant_with_reasoning_content() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": "Answer", "reasoning_content": "Thinking..."}
            ]
        });
        let result = translate_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].get("type").unwrap().as_str().unwrap(), "thinking");
        assert_eq!(content[0].get("thinking").unwrap().as_str().unwrap(), "Thinking...");
        assert_eq!(content[1].get("type").unwrap().as_str().unwrap(), "text");
        assert_eq!(content[1].get("text").unwrap().as_str().unwrap(), "Answer");
    }

    #[test]
    fn test_consecutive_tool_messages_merged() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "b", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "result1"},
                {"role": "tool", "tool_call_id": "call_2", "content": "result2"}
            ]
        });
        let result = translate_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        // Should be: user, assistant, user (merged tool results)
        assert_eq!(msgs.len(), 3);
        let last = &msgs[2];
        assert_eq!(last.get("role").unwrap().as_str().unwrap(), "user");
        let content = last.get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].get("type").unwrap().as_str().unwrap(), "tool_result");
        assert_eq!(content[1].get("type").unwrap().as_str().unwrap(), "tool_result");
    }

    #[test]
    fn test_tool_definitions() {
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            }]
        });
        let result = translate_request(&input).unwrap();
        let tools = result.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get("name").unwrap().as_str().unwrap(), "get_weather");
        assert_eq!(tools[0].get("description").unwrap().as_str().unwrap(), "Get weather");
        assert!(tools[0].get("input_schema").is_some());
    }

    #[test]
    fn test_tool_choice_mapping() {
        // auto
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": "auto"});
        let result = translate_request(&input).unwrap();
        assert_eq!(result.get("tool_choice").unwrap().get("type").unwrap().as_str().unwrap(), "auto");

        // required → any
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": "required"});
        let result = translate_request(&input).unwrap();
        assert_eq!(result.get("tool_choice").unwrap().get("type").unwrap().as_str().unwrap(), "any");

        // none → omitted
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": "none"});
        let result = translate_request(&input).unwrap();
        assert!(result.get("tool_choice").is_none());

        // specific function
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": {"type": "function", "function": {"name": "my_fn"}}});
        let result = translate_request(&input).unwrap();
        let tc = result.get("tool_choice").unwrap();
        assert_eq!(tc.get("type").unwrap().as_str().unwrap(), "tool");
        assert_eq!(tc.get("name").unwrap().as_str().unwrap(), "my_fn");
    }

    #[test]
    fn test_max_tokens_default() {
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}]});
        let result = translate_request(&input).unwrap();
        assert_eq!(result.get("max_tokens").unwrap().as_u64().unwrap(), 4096);
    }

    #[test]
    fn test_fields_dropped() {
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role":"user","content":"Hi"}],
            "n": 2,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.5,
            "logprobs": true,
            "logit_bias": {"1": 100},
            "seed": 42,
            "response_format": {"type": "json_object"},
            "stream_options": {"include_usage": true}
        });
        let result = translate_request(&input).unwrap();
        assert!(result.get("n").is_none());
        assert!(result.get("frequency_penalty").is_none());
        assert!(result.get("presence_penalty").is_none());
        assert!(result.get("logprobs").is_none());
        assert!(result.get("logit_bias").is_none());
        assert!(result.get("seed").is_none());
        assert!(result.get("response_format").is_none());
        assert!(result.get("stream_options").is_none());
    }

    // ── Response translation tests ─────────────────────────────────────

    #[test]
    fn test_text_content_blocks() {
        let input = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = translate_response(&input).unwrap();
        let msg = result.get("choices").unwrap()[0].get("message").unwrap();
        assert_eq!(msg.get("content").unwrap().as_str().unwrap(), "Hello world");
    }

    #[test]
    fn test_thinking_content() {
        let input = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "thinking", "thinking": "Let me think..."},
                {"type": "text", "text": "Answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = translate_response(&input).unwrap();
        let msg = result.get("choices").unwrap()[0].get("message").unwrap();
        assert_eq!(msg.get("reasoning_content").unwrap().as_str().unwrap(), "Let me think...");
        assert_eq!(msg.get("content").unwrap().as_str().unwrap(), "Answer");
    }

    #[test]
    fn test_tool_use_blocks() {
        let input = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "/src"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = translate_response(&input).unwrap();
        let msg = result.get("choices").unwrap()[0].get("message").unwrap();
        let tool_calls = msg.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].get("id").unwrap().as_str().unwrap(), "toolu_1");
        assert_eq!(
            tool_calls[0].get("function").unwrap().get("name").unwrap().as_str().unwrap(),
            "read_file"
        );
        let args: serde_json::Value =
            serde_json::from_str(tool_calls[0].get("function").unwrap().get("arguments").unwrap().as_str().unwrap())
                .unwrap();
        assert_eq!(args.get("path").unwrap().as_str().unwrap(), "/src");
    }

    #[test]
    fn test_redacted_thinking_omitted() {
        let input = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "redacted_thinking"},
                {"type": "text", "text": "Done"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let result = translate_response(&input).unwrap();
        let msg = result.get("choices").unwrap()[0].get("message").unwrap();
        assert!(msg.get("reasoning_content").is_none());
        assert_eq!(msg.get("content").unwrap().as_str().unwrap(), "Done");
    }

    #[test]
    fn test_stop_reason_mapping() {
        fn check(anthropic: &str, expected_openai: &str) {
            let input = json!({
                "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
                "content": [{"type": "text", "text": "x"}],
                "stop_reason": anthropic,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            });
            let result = translate_response(&input).unwrap();
            let fr = result.get("choices").unwrap()[0]
                .get("finish_reason")
                .unwrap()
                .as_str()
                .unwrap();
            assert_eq!(fr, expected_openai, "stop_reason {anthropic} → {expected_openai}");
        }
        check("end_turn", "stop");
        check("max_tokens", "length");
        check("tool_use", "tool_calls");
        check("stop_sequence", "stop");
    }

    #[test]
    fn test_usage_mapping() {
        let input = json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
            "content": [{"type": "text", "text": "x"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });
        let result = translate_response(&input).unwrap();
        let usage = result.get("usage").unwrap();
        assert_eq!(usage.get("prompt_tokens").unwrap().as_u64().unwrap(), 100);
        assert_eq!(usage.get("completion_tokens").unwrap().as_u64().unwrap(), 50);
        assert_eq!(usage.get("total_tokens").unwrap().as_u64().unwrap(), 150);
    }

    // ── Error translation tests ────────────────────────────────────────

    #[test]
    fn test_error_translation() {
        let input = r#"{"type":"error","error":{"type":"overloaded_error","message":"Too many requests"}}"#;
        let result = translate_error(input, 529);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed.get("error").unwrap().get("message").unwrap().as_str().unwrap(),
            "Too many requests"
        );
        assert_eq!(
            parsed.get("error").unwrap().get("type").unwrap().as_str().unwrap(),
            "overloaded_error"
        );
        assert_eq!(parsed.get("error").unwrap().get("code").unwrap().as_u64().unwrap(), 529);
    }

    #[test]
    fn test_error_translation_malformed_body() {
        let result = translate_error("not json", 500);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed.get("error").unwrap().get("message").unwrap().as_str().unwrap(),
            "not json"
        );
    }

    // ── Streaming translation tests ────────────────────────────────────

    #[test]
    fn test_stream_message_start() {
        let mut state = StreamTranslateState::default();
        let data = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[],"stop_reason":null,"usage":{"input_tokens":0,"output_tokens":0}}}"#;
        let result = translate_stream_event("message_start", data, &mut state);
        assert!(result.is_some());
        let chunk_str = result.unwrap();
        assert!(chunk_str.contains("chatcmpl-msg_1"));
        assert!(state.started);
        assert_eq!(state.chunk_id, "msg_1");
        assert_eq!(state.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_stream_text_delta() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            ..Default::default()
        };
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let result = translate_stream_event("content_block_delta", data, &mut state);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Hello"));
    }

    #[test]
    fn test_stream_thinking_delta() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            ..Default::default()
        };
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm..."}}"#;
        let result = translate_stream_event("content_block_delta", data, &mut state);
        assert!(result.is_some());
        assert!(result.unwrap().contains("reasoning_content"));
    }

    #[test]
    fn test_stream_tool_use_start() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            ..Default::default()
        };
        let data = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#;
        let result = translate_stream_event("content_block_start", data, &mut state);
        assert!(result.is_some());
        let chunk = result.unwrap();
        assert!(chunk.contains("toolu_1"));
        assert!(chunk.contains("read_file"));
        assert!(state.has_tool_use);
    }

    #[test]
    fn test_stream_input_json_delta() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            tool_index: 0,
            has_tool_use: true,
            ..Default::default()
        };
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#;
        let result = translate_stream_event("content_block_delta", data, &mut state);
        assert!(result.is_some());
        let chunk = result.unwrap();
        // OpenAI format uses function.arguments, not input_json_delta
        assert!(chunk.contains("\"arguments\""));
        assert!(chunk.contains("tool_calls"));
    }

    #[test]
    fn test_stream_content_block_stop_tool() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            tool_index: 0,
            has_tool_use: true,
            ..Default::default()
        };
        let result = translate_stream_event("content_block_stop", r#"{"index":1}"#, &mut state);
        assert!(result.is_none());
        assert_eq!(state.tool_index, 1);
        assert!(!state.has_tool_use);
    }

    #[test]
    fn test_stream_message_delta_stop() {
        let mut state = StreamTranslateState {
            chunk_id: "msg_1".into(),
            model: "m".into(),
            started: true,
            ..Default::default()
        };
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#;
        let result = translate_stream_event("message_delta", data, &mut state);
        assert!(result.is_some());
        let chunks = result.unwrap();
        assert!(chunks.contains("\"finish_reason\":\"stop\""));
        assert!(chunks.contains("\"completion_tokens\":42"));
    }

    #[test]
    fn test_stream_message_stop() {
        let mut state = StreamTranslateState::default();
        let result = translate_stream_event("message_stop", "{}", &mut state);
        assert_eq!(result.unwrap(), "data: [DONE]\n\n");
    }

    // ── SSE parser tests ───────────────────────────────────────────────

    #[test]
    fn test_parse_sse_events() {
        let input = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "message_start");
        assert_eq!(events[1].0, "content_block_delta");
    }

    #[test]
    fn test_parse_sse_events_no_event_type() {
        let input = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "message"); // default
    }

    // ── Malformed arguments test ───────────────────────────────────────

    #[test]
    fn test_malformed_tool_arguments() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "fn", "arguments": "not-json"}}
                ]}
            ]
        });
        let result = translate_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        let input_val = content[0].get("input").unwrap();
        assert_eq!(input_val.get("raw").unwrap().as_str().unwrap(), "not-json");
    }
}
