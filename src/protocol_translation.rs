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
    // ── Prompt-cache usage accumulation ──────────────────────────────────
    // Anthropic streaming splits usage across events: message_start carries
    // input_tokens + cache_creation_input_tokens, message_delta carries
    // output_tokens + cache_read_input_tokens. We accumulate the message_start
    // half here and combine it with the message_delta half when emitting the
    // terminal OpenAI usage chunk, so the full cache breakdown is reported.
    /// input_tokens captured from message_start.
    pub input_tokens: u64,
    /// cache_creation_input_tokens captured from message_start.
    pub cache_creation_input_tokens: u64,
    /// cache_read_input_tokens captured from message_delta.
    pub cache_read_input_tokens: u64,
    /// output_tokens captured from message_delta. Exposed so callers logging a
    /// stream at close can assemble the full usage breakdown from state alone.
    pub output_tokens: u64,
}

impl StreamTranslateState {
    /// Returns the accumulated usage breakdown as
    /// `(input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)`
    /// for inference-log capture at stream close. Only meaningful when
    /// [`started`] is true (a `message_start` was seen); callers should check
    /// [`started`] first to avoid logging a zero-usage row for streams that
    /// errored before the upstream produced any usage event.
    pub fn collected_usage(&self) -> (u64, u64, u64, u64) {
        (
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
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
    let obj = body
        .as_object()
        .ok_or("request body must be a JSON object")?;

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
    out.insert(
        "messages".into(),
        serde_json::Value::Array(converted_messages),
    );

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

    // ── cache_control (Anthropic automatic prompt caching, GA) ──────────
    // Insert a top-level ephemeral breakpoint when absent so OpenAI clients
    // routed to an Anthropic upstream benefit from automatic prompt caching:
    // Anthropic places the breakpoint on the last cacheable block and moves it
    // forward as the conversation grows, with no per-block surgery. No
    // `anthropic-beta` header is required — prompt caching is GA as of the
    // verified docs (see plan.md references). Respect an explicit
    // cache_control if one is already present rather than overwriting it.
    if !out.contains_key("cache_control") {
        out.insert("cache_control".into(), json!({"type": "ephemeral"}));
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
            let arguments_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
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
    let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
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
                let last_content = last.get_mut("content").and_then(|c| c.as_array_mut());
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
    let obj = body
        .as_object()
        .ok_or("response body must be a JSON object")?;

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
    // Anthropic splits prompt tokens into input_tokens (non-cached),
    // cache_read_input_tokens, and cache_creation_input_tokens. OpenAI's
    // prompt_tokens is the TOTAL prompt (cached + non-cached), with
    // prompt_tokens_details.cached_tokens carrying the cache-read portion. We
    // sum the three Anthropic fields into prompt_tokens so OpenAI clients see
    // an accurate full prompt count, and map cache_read → cached_tokens
    // (cache reads are the OpenAI equivalent of cached input).
    let (input_tokens, output_tokens, cache_read, cache_creation) =
        if let Some(usage) = obj.get("usage") {
            let inp = usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let out = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cr = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cc = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (inp, out, cr, cc)
        } else {
            (0, 0, 0, 0)
        };
    let prompt_tokens = input_tokens + cache_read + cache_creation;

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
            "completion_tokens": output_tokens,
            "total_tokens": prompt_tokens + output_tokens,
            "prompt_tokens_details": {
                "cached_tokens": cache_read
            }
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
            // Anthropic reports input_tokens + cache_creation_input_tokens in
            // message_start (not message_delta); stash them to combine with
            // the message_delta half when the terminal usage chunk is emitted.
            if let Some(usage) = msg.get("usage") {
                state.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                state.cache_creation_input_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
            }

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

            // usage → separate usage chunk. Combine the message_delta half
            // (output_tokens + cache_read_input_tokens) with the message_start
            // half stashed in state (input_tokens + cache_creation) to produce
            // the full OpenAI usage breakdown. prompt_tokens is the total
            // prompt (cached + non-cached); cached_tokens maps from
            // cache_read_input_tokens.
            if let Some(usage) = usage {
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                state.cache_read_input_tokens = cache_read;
                state.output_tokens = output_tokens;
                let prompt_tokens =
                    state.input_tokens + state.cache_creation_input_tokens + cache_read;
                let usage_chunk = json!({
                    "id": format!("chatcmpl-{}", state.chunk_id),
                    "object": "chat.completion.chunk",
                    "model": state.model,
                    "choices": [],
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": output_tokens,
                        "total_tokens": prompt_tokens + output_tokens,
                        "prompt_tokens_details": {
                            "cached_tokens": cache_read
                        }
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
fn make_openai_chunk(chunk_id: &str, model: &str, choices: serde_json::Value) -> String {
    let chunk = json!({
        "id": format!("chatcmpl-{}", chunk_id),
        "object": "chat.completion.chunk",
        "model": model,
        "choices": choices
    });
    format!("data: {}\n\n", chunk)
}

// ──────────────────────────────────────────────────────────────────────
// §5  Request Translation  (Anthropic → OpenAI)
// ──────────────────────────────────────────────────────────────────────

/// Translate an Anthropic Messages request body into an OpenAI Chat
/// Completions request body.
pub fn anthropic_to_openai_request(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = body
        .as_object()
        .ok_or("request body must be a JSON object")?;

    let mut out = serde_json::Map::new();

    // ── model ──
    if let Some(model) = obj.get("model") {
        out.insert("model".into(), model.clone());
    }

    // ── max_tokens ──
    if let Some(mt) = obj.get("max_tokens").and_then(|v| v.as_u64()) {
        if mt > 0 {
            out.insert("max_tokens".into(), json!(mt));
        }
    }

    // ── temperature / top_p (pass through) ──
    if let Some(t) = obj.get("temperature") {
        out.insert("temperature".into(), t.clone());
    }
    if let Some(tp) = obj.get("top_p") {
        out.insert("top_p".into(), tp.clone());
    }
    // top_k: dropped (no OpenAI equivalent)

    // ── stop_sequences → stop ──
    if let Some(seqs) = obj.get("stop_sequences").and_then(|v| v.as_array()) {
        if seqs.len() == 1 {
            out.insert("stop".into(), seqs[0].clone());
        } else if !seqs.is_empty() {
            out.insert("stop".into(), json!(seqs));
        }
    }

    // ── stream + stream_options ──
    let streaming = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    if streaming {
        out.insert("stream".into(), json!(true));
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }

    // ── messages ──
    let messages = obj
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or("messages must be an array")?;

    let mut openai_messages: Vec<serde_json::Value> = Vec::new();

    // system field → prepended system message
    if let Some(system) = obj.get("system") {
        let text = match system {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            openai_messages.push(json!({"role": "system", "content": text}));
        }
    }

    // Track whether any message has reasoning_content for post-pass fix
    let mut has_reasoning = false;

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "user" => {
                convert_anthropic_user_message(msg, &mut openai_messages);
            }
            "assistant" => {
                let converted = convert_anthropic_assistant_message(msg);
                if converted.get("reasoning_content").is_some() {
                    has_reasoning = true;
                }
                openai_messages.push(converted);
            }
            _ => {}
        }
    }

    // Post-pass reasoning fix: if ANY message has reasoning_content,
    // all assistant messages with tool_calls but no reasoning get reasoning_content: " "
    if has_reasoning {
        for msg in openai_messages.iter_mut() {
            if msg.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && msg.get("tool_calls").is_some()
                && msg.get("reasoning_content").is_none()
            {
                msg.as_object_mut()
                    .unwrap()
                    .insert("reasoning_content".into(), json!(" "));
            }
        }
    }

    out.insert("messages".into(), json!(openai_messages));

    // ── tools ──
    if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
        let openai_tools: Vec<serde_json::Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.get("description").cloned().unwrap_or(json!(null)),
                        "parameters": t.get("input_schema").cloned().unwrap_or(json!({}))
                    }
                }))
            })
            .collect();
        if !openai_tools.is_empty() {
            out.insert("tools".into(), json!(openai_tools));
        }
    }

    // ── tool_choice ──
    if let Some(tc) = obj.get("tool_choice") {
        let openai_tc = match tc {
            serde_json::Value::String(s) => match s.as_str() {
                "auto" => Some(json!("auto")),
                "any" => Some(json!("required")),
                "none" => Some(json!("none")),
                _ => Some(json!("auto")),
            },
            serde_json::Value::Object(map) => match map.get("type").and_then(|v| v.as_str()) {
                Some("auto") => Some(json!("auto")),
                Some("any") => Some(json!("required")),
                Some("none") => Some(json!("none")),
                Some("tool") => {
                    let name = map.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    Some(json!({"type": "function", "function": {"name": name}}))
                }
                _ => Some(json!("auto")),
            },
            _ => None,
        };
        if let Some(tc_val) = openai_tc {
            out.insert("tool_choice".into(), tc_val);
        }
    }

    Ok(serde_json::Value::Object(out))
}

/// Translate an Anthropic Messages request body into an OpenAI Chat
/// Completions request body, also reporting whether the source carried any
/// `cache_control` breakpoint (`had_cache_control`). OpenAI Chat Completions
/// has no native cache_control equivalent, so the breakpoint cannot survive
/// translation and is always absent from the returned body; the signal lets
/// callers (logging/metrics in Phase 4) account for the fact that the client
/// requested caching, so a subsequent low/zero `cache_read_input_tokens` is
/// not misread as a translator bug.
pub fn anthropic_to_openai_request_with_cache_signal(
    body: &serde_json::Value,
) -> Result<(serde_json::Value, bool), String> {
    let had_cache_control = anthropic_body_has_cache_control(body);
    let translated = anthropic_to_openai_request(body)?;
    Ok((translated, had_cache_control))
}

/// Returns true if the Anthropic body carries a `cache_control` breakpoint
/// anywhere Anthropic allows one: on a `system` content block, on any message
/// content block, or on a tool definition. A top-level request cache_control
/// is also accepted for completeness. Structural scan (no string matching) so
/// it stays correct if field ordering changes.
fn anthropic_body_has_cache_control(body: &serde_json::Value) -> bool {
    if body.get("cache_control").is_some() {
        return true;
    }
    if let Some(system) = body.get("system").and_then(|v| v.as_array()) {
        for block in system {
            if block.get("cache_control").is_some() {
                return true;
            }
        }
    }
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                for block in blocks {
                    if block.get("cache_control").is_some() {
                        return true;
                    }
                }
            }
        }
    }
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        for tool in tools {
            if tool.get("cache_control").is_some() {
                return true;
            }
        }
    }
    false
}

/// Convert an Anthropic user message into OpenAI message(s).
/// tool_result blocks become separate role:"tool" messages.
fn convert_anthropic_user_message(msg: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => {
            out.push(json!({"role": "user", "content": s}));
        }
        Some(serde_json::Value::Array(blocks)) => {
            let mut text_parts: Vec<String> = Vec::new();
            let mut image_parts: Vec<serde_json::Value> = Vec::new();
            let mut tool_results: Vec<serde_json::Value> = Vec::new();

            for block in blocks {
                let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match btype {
                    "text" => {
                        let t = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        text_parts.push(t.to_string());
                    }
                    "image" => {
                        if let Some(source) = block.get("source") {
                            let media_type = source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png");
                            let data = source.get("data").and_then(|v| v.as_str()).unwrap_or("");
                            let url = format!("data:{media_type};base64,{data}");
                            image_parts
                                .push(json!({"type": "image_url", "image_url": {"url": url}}));
                        }
                    }
                    "tool_result" => {
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let content = match block.get("content") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(serde_json::Value::Array(arr)) => arr
                                .iter()
                                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join(""),
                            _ => String::new(),
                        };
                        tool_results.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}));
                    }
                    _ => {}
                }
            }

            // Emit user message if there's text/image content
            if !text_parts.is_empty() || !image_parts.is_empty() {
                if image_parts.is_empty() {
                    out.push(json!({"role": "user", "content": text_parts.join("\n\n")}));
                } else {
                    let mut content_parts: Vec<serde_json::Value> = Vec::new();
                    if !text_parts.is_empty() {
                        content_parts
                            .push(json!({"type": "text", "text": text_parts.join("\n\n")}));
                    }
                    content_parts.extend(image_parts);
                    out.push(json!({"role": "user", "content": content_parts}));
                }
            }

            // Emit tool result messages
            out.extend(tool_results);
        }
        _ => {
            out.push(json!({"role": "user", "content": ""}));
        }
    }
}

/// Convert an Anthropic assistant message to OpenAI format.
fn convert_anthropic_assistant_message(msg: &serde_json::Value) -> serde_json::Value {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match btype {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                "thinking" => {
                    if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                        reasoning_parts.push(t.to_string());
                    }
                }
                "tool_use" => {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments}
                    }));
                }
                "redacted_thinking" => {} // drop
                _ => {}
            }
        }
    }

    let mut result = serde_json::Map::new();
    result.insert("role".into(), json!("assistant"));
    result.insert("content".into(), json!(text_parts.join("")));

    if !reasoning_parts.is_empty() {
        result.insert("reasoning_content".into(), json!(reasoning_parts.join("")));
    }
    if !tool_calls.is_empty() {
        result.insert("tool_calls".into(), json!(tool_calls));
    }

    serde_json::Value::Object(result)
}

// ──────────────────────────────────────────────────────────────────────
// §6  Response Translation  (OpenAI → Anthropic Messages format)
// ──────────────────────────────────────────────────────────────────────

/// Translate an OpenAI Chat Completions response into an Anthropic Messages response.
pub fn openai_to_anthropic_response(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = body
        .as_object()
        .ok_or("response body must be a JSON object")?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("msg_unknown");
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let choice = obj
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or("choices array missing or empty")?;

    let message = choice.get("message").ok_or("message missing in choice")?;

    // Build content blocks: thinking → text → tool_use
    let mut content: Vec<serde_json::Value> = Vec::new();

    // reasoning_content → thinking block (prepend)
    if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            content.push(json!({"type": "thinking", "thinking": reasoning}));
        }
    }

    // content → text block
    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }

    // tool_calls → tool_use blocks
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input =
                serde_json::from_str::<serde_json::Value>(args_str).unwrap_or_else(|_| json!({}));
            content.push(json!({"type": "tool_use", "id": tc_id, "name": name, "input": input}));
        }
    }

    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    // finish_reason → stop_reason
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");
    let stop_reason = match finish_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "end_turn",
        _ => "end_turn",
    };

    // usage
    // OpenAI reports cache hits as prompt_tokens_details.cached_tokens (a
    // subset of prompt_tokens). Anthropic splits input into
    // cache_read_input_tokens + cache_creation_input_tokens + input_tokens,
    // with the invariant total = cache_read + cache_creation + input_tokens.
    // OpenAI has no cache-creation concept, so cache_creation = 0 and the
    // Anthropic input_tokens is the non-cached portion (prompt_tokens minus
    // cached_tokens). saturating_sub guards against malformed cached > prompt.
    let usage = obj.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached_tokens = usage
        .and_then(|u| u.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let input_tokens = prompt_tokens.saturating_sub(cached_tokens);

    Ok(json!({
        "id": id.strip_prefix("chatcmpl-").unwrap_or(id),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cached_tokens,
            "cache_creation_input_tokens": 0
        }
    }))
}

// ──────────────────────────────────────────────────────────────────────
// §7  Error Translation  (OpenAI error → Anthropic error envelope)
// ──────────────────────────────────────────────────────────────────────

/// Translate an OpenAI error body into an Anthropic error envelope.
///
/// OpenAI shape: `{"error":{"message":"…","type":"…","code":"…"}}`
/// Anthropic shape: `{"type":"error","error":{"type":"…","message":"…"}}`
pub fn openai_to_anthropic_error(body: &str, status: u16) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return json!({
                "type": "error",
                "error": {"type": status_to_anthropic_error_type(status), "message": body}
            })
            .to_string();
        }
    };

    let message = parsed
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or(body);

    json!({
        "type": "error",
        "error": {"type": status_to_anthropic_error_type(status), "message": message}
    })
    .to_string()
}

fn status_to_anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        500..=599 => "api_error",
        _ => "api_error",
    }
}

// ──────────────────────────────────────────────────────────────────────
// §8  Streaming Translation  (OpenAI SSE → Anthropic SSE)
// ──────────────────────────────────────────────────────────────────────

/// Tracks state across an OpenAI→Anthropic streaming translation session.
#[derive(Debug, Default)]
pub struct AnthropicStreamState {
    pub block_index: usize,
    pub open_block: Option<String>, // "text", "thinking", or "tool_use"
    pub message_started: bool,
    pub model: String,
    /// Tracks tool call metadata by OpenAI tool_calls array index.
    pub tool_state: std::collections::HashMap<usize, (String, String)>, // index → (id, name)
}

impl AnthropicStreamState {
    /// Close the currently open block if any, returning the SSE event string.
    fn close_open_block(&mut self) -> Option<String> {
        if self.open_block.take().is_some() {
            let idx = self.block_index;
            self.block_index += 1;
            Some(format!(
                "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{idx}}}\n\n"
            ))
        } else {
            None
        }
    }
}

/// Translate an OpenAI SSE event into Anthropic SSE event(s).
/// Returns None if the event produces no output.
pub fn openai_to_anthropic_stream_event(
    event_type: &str,
    data: &str,
    state: &mut AnthropicStreamState,
) -> Option<String> {
    // Handle [DONE] signal
    if data.trim() == "[DONE]" {
        let mut out = String::new();
        if let Some(close) = state.close_open_block() {
            out.push_str(&close);
        }
        out.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        return Some(out);
    }

    // Only handle "message" type events (default SSE type for OpenAI)
    if event_type != "message" && !event_type.is_empty() {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_str(data).ok()?;

    // Extract model from first chunk
    if let Some(m) = parsed.get("model").and_then(|v| v.as_str()) {
        if state.model.is_empty() {
            state.model = m.to_string();
        }
    }

    let mut out = String::new();

    // Emit message_start on first chunk
    if !state.message_started {
        state.message_started = true;
        let id = parsed
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("msg_unknown");
        let msg_start = json!({
            "type": "message_start",
            "message": {
                "id": id.strip_prefix("chatcmpl-").unwrap_or(id),
                "type": "message",
                "role": "assistant",
                "model": &state.model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        });
        out.push_str(&format!("event: message_start\ndata: {msg_start}\n\n"));
    }

    let choice = parsed
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    let choice = match choice {
        Some(c) => c,
        None => {
            // Usage-only chunk (no choices)
            if let Some(usage) = parsed.get("usage") {
                let output_tokens = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let msg_delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": output_tokens}
                });
                if let Some(close) = state.close_open_block() {
                    out.push_str(&close);
                }
                out.push_str(&format!("event: message_delta\ndata: {msg_delta}\n\n"));
            }
            return if out.is_empty() { None } else { Some(out) };
        }
    };

    let delta = choice.get("delta").unwrap_or(&serde_json::Value::Null);
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

    // reasoning_content delta
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            if state.open_block.as_deref() != Some("thinking") {
                if let Some(close) = state.close_open_block() {
                    out.push_str(&close);
                }
                let start = json!({
                    "type": "content_block_start",
                    "index": state.block_index,
                    "content_block": {"type": "thinking", "thinking": ""}
                });
                out.push_str(&format!("event: content_block_start\ndata: {start}\n\n"));
                state.open_block = Some("thinking".into());
            }
            let delta_ev = json!({
                "type": "content_block_delta",
                "index": state.block_index,
                "delta": {"type": "thinking_delta", "thinking": reasoning}
            });
            out.push_str(&format!("event: content_block_delta\ndata: {delta_ev}\n\n"));
        }
    }

    // content delta
    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            if state.open_block.as_deref() != Some("text") {
                if let Some(close) = state.close_open_block() {
                    out.push_str(&close);
                }
                let start = json!({
                    "type": "content_block_start",
                    "index": state.block_index,
                    "content_block": {"type": "text", "text": ""}
                });
                out.push_str(&format!("event: content_block_start\ndata: {start}\n\n"));
                state.open_block = Some("text".into());
            }
            let delta_ev = json!({
                "type": "content_block_delta",
                "index": state.block_index,
                "delta": {"type": "text_delta", "text": content}
            });
            out.push_str(&format!("event: content_block_delta\ndata: {delta_ev}\n\n"));
        }
    }

    // tool_calls delta
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let is_new = !state.tool_state.contains_key(&idx);

            if is_new {
                // New tool call — close previous block, start new tool_use block
                if let Some(close) = state.close_open_block() {
                    out.push_str(&close);
                }
                let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                state
                    .tool_state
                    .insert(idx, (id.to_string(), name.to_string()));

                let start = json!({
                    "type": "content_block_start",
                    "index": state.block_index,
                    "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
                });
                out.push_str(&format!("event: content_block_start\ndata: {start}\n\n"));
                state.open_block = Some("tool_use".into());
            }

            // Emit arguments as input_json_delta
            if let Some(args) = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
            {
                if !args.is_empty() {
                    let delta_ev = json!({
                        "type": "content_block_delta",
                        "index": state.block_index,
                        "delta": {"type": "input_json_delta", "partial_json": args}
                    });
                    out.push_str(&format!("event: content_block_delta\ndata: {delta_ev}\n\n"));
                }
            }
        }
    }

    // finish_reason → message_delta
    if let Some(fr) = finish_reason {
        if let Some(close) = state.close_open_block() {
            out.push_str(&close);
        }
        let stop_reason = match fr {
            "stop" => "end_turn",
            "length" => "max_tokens",
            "tool_calls" | "function_call" => "tool_use",
            "content_filter" => "end_turn",
            _ => "end_turn",
        };
        let msg_delta = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": 0}
        });
        out.push_str(&format!("event: message_delta\ndata: {msg_delta}\n\n"));
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
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
        assert_eq!(
            result.get("system").unwrap().as_str().unwrap(),
            "You are helpful."
        );
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "user");
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(content[0].get("text").unwrap().as_str().unwrap(), "Hello");
    }

    #[test]
    fn test_translate_request_inserts_top_level_cache_control() {
        // OpenAI client → Anthropic upstream: the translator must auto-insert
        // a top-level ephemeral cache_control so Anthropic's automatic prompt
        // caching activates with no per-block surgery and no beta header.
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let result = translate_request(&input).unwrap();
        let cc = result.get("cache_control").expect("cache_control must be inserted");
        assert_eq!(
            cc.get("type").and_then(|v| v.as_str()),
            Some("ephemeral"),
            "automatic caching uses an ephemeral top-level breakpoint"
        );
    }

    #[test]
    fn test_anthropic_to_openai_request_strips_cache_control_and_signals() {
        // Anthropic body WITH a cache_control breakpoint on a message block.
        // The OpenAI body must NOT carry cache_control (no native equivalent),
        // and the signal must report had_cache_control = true so logging can
        // account for the requested cache.
        let input = json!({
            "model": "claude-3.5",
            "max_tokens": 100,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "cached turn", "cache_control": {"type": "ephemeral"}}
                ]
            }]
        });
        let (translated, had_cc) =
            anthropic_to_openai_request_with_cache_signal(&input).unwrap();
        assert!(had_cc, "had_cache_control must be true when a block carries it");
        assert!(
            translated.get("cache_control").is_none(),
            "translated OpenAI body must not carry cache_control, got: {translated}"
        );
        // The translated body should still carry the message text.
        let msgs = translated.get("messages").unwrap().as_array().unwrap();
        assert!(!msgs.is_empty());
    }

    #[test]
    fn test_anthropic_to_openai_request_no_cache_control_signal_when_absent() {
        // No breakpoint anywhere -> had_cache_control = false.
        let input = json!({
            "model": "claude-3.5",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "plain turn"}]
        });
        let (_translated, had_cc) =
            anthropic_to_openai_request_with_cache_signal(&input).unwrap();
        assert!(!had_cc, "had_cache_control must be false when no breakpoint is present");
    }

    #[test]
    fn test_translate_response_maps_cache_tokens_to_openai() {
        // Anth→OAI non-streaming: Anthropic cache_read/cache_creation must
        // surface as OpenAI prompt_tokens_details.cached_tokens, and
        // prompt_tokens must be the FULL prompt (non-cached + cached) so
        // OpenAI clients see an accurate total.
        let input = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 5
            }
        });
        let result = translate_response(&input).unwrap();
        let usage = result.get("usage").expect("usage");
        assert_eq!(
            usage.get("prompt_tokens").and_then(|v| v.as_u64()),
            Some(100 + 80 + 5),
            "prompt_tokens must be the full prompt (non-cached + cache_read + cache_creation)"
        );
        assert_eq!(
            usage.get("completion_tokens").and_then(|v| v.as_u64()),
            Some(20)
        );
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64());
        assert_eq!(
            cached,
            Some(80),
            "cached_tokens must map from Anthropic cache_read_input_tokens"
        );
    }

    #[test]
    fn test_translate_response_no_cache_tokens_still_emits_details() {
        // When the upstream reports no caching, cached_tokens must be 0 (not
        // absent), and prompt_tokens falls back to input_tokens.
        let input = json!({
            "id": "msg_2",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": 50, "output_tokens": 10}
        });
        let result = translate_response(&input).unwrap();
        let usage = result.get("usage").expect("usage");
        assert_eq!(usage.get("prompt_tokens").and_then(|v| v.as_u64()), Some(50));
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64());
        assert_eq!(cached, Some(0));
    }

    #[test]
    fn test_openai_to_anthropic_response_maps_cached_tokens() {
        // OAI→Anth non-streaming: OpenAI cached_tokens must surface as
        // Anthropic cache_read_input_tokens, with the invariant
        // cache_read + cache_creation + input_tokens == prompt_tokens held
        // (cache_creation = 0 since OpenAI has no creation concept).
        let input = json!({
            "id": "chatcmpl-x",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 130,
                "completion_tokens": 8,
                "total_tokens": 138,
                "prompt_tokens_details": {"cached_tokens": 90}
            }
        });
        let result = openai_to_anthropic_response(&input).unwrap();
        let usage = result.get("usage").expect("usage");
        let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(cache_read, 90, "cache_read_input_tokens maps from cached_tokens");
        assert_eq!(input_tokens, 40, "input_tokens is the non-cached portion (130 - 90)");
        assert_eq!(cache_creation, 0, "OpenAI has no cache-creation concept");
        assert_eq!(
            cache_read + cache_creation + input_tokens,
            130,
            "Anthropic input invariant must equal OpenAI prompt_tokens"
        );
    }

    #[test]
    fn test_translate_stream_event_accumulates_cache_tokens_across_events() {
        // Anthropic streaming reports input_tokens + cache_creation in
        // message_start and output_tokens + cache_read in message_delta. The
        // translator must combine both halves into the terminal OpenAI usage
        // chunk with cached_tokens set.
        let mut state = StreamTranslateState::default();
        let start = r#"{"type":"message_start","message":{"id":"msg_s","type":"message","role":"assistant","model":"claude","content":[],"stop_reason":null,"usage":{"input_tokens":100,"output_tokens":0,"cache_creation_input_tokens":12}}}"#;
        translate_stream_event("message_start", start, &mut state);
        let delta = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20,"cache_read_input_tokens":80}}"#;
        let out = translate_stream_event("message_delta", delta, &mut state);
        let out = out.expect("message_delta must emit a usage chunk");
        // prompt_tokens = input_tokens(100) + cache_creation(12) + cache_read(80) = 192
        assert!(
            out.contains("\"prompt_tokens\":192"),
            "prompt_tokens must combine all input/cache fields, got: {out}"
        );
        assert!(
            out.contains("\"cached_tokens\":80"),
            "cached_tokens must map from cache_read_input_tokens, got: {out}"
        );
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
            content[1]
                .get("source")
                .unwrap()
                .get("media_type")
                .unwrap()
                .as_str()
                .unwrap(),
            "image/png"
        );
        assert_eq!(
            content[1]
                .get("source")
                .unwrap()
                .get("data")
                .unwrap()
                .as_str()
                .unwrap(),
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
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "tool_use"
        );
        assert_eq!(content[0].get("id").unwrap().as_str().unwrap(), "call_1");
        assert_eq!(
            content[0].get("name").unwrap().as_str().unwrap(),
            "read_file"
        );
        assert_eq!(
            content[0]
                .get("input")
                .unwrap()
                .get("path")
                .unwrap()
                .as_str()
                .unwrap(),
            "/src"
        );
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
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "thinking"
        );
        assert_eq!(
            content[0].get("thinking").unwrap().as_str().unwrap(),
            "Thinking..."
        );
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
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "tool_result"
        );
        assert_eq!(
            content[1].get("type").unwrap().as_str().unwrap(),
            "tool_result"
        );
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
        assert_eq!(
            tools[0].get("name").unwrap().as_str().unwrap(),
            "get_weather"
        );
        assert_eq!(
            tools[0].get("description").unwrap().as_str().unwrap(),
            "Get weather"
        );
        assert!(tools[0].get("input_schema").is_some());
    }

    #[test]
    fn test_tool_choice_mapping() {
        // auto
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": "auto"});
        let result = translate_request(&input).unwrap();
        assert_eq!(
            result
                .get("tool_choice")
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .unwrap(),
            "auto"
        );

        // required → any
        let input = json!({"model": "gpt-4", "messages": [{"role":"user","content":"Hi"}], "tool_choice": "required"});
        let result = translate_request(&input).unwrap();
        assert_eq!(
            result
                .get("tool_choice")
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .unwrap(),
            "any"
        );

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
        assert_eq!(
            msg.get("reasoning_content").unwrap().as_str().unwrap(),
            "Let me think..."
        );
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
        assert_eq!(
            tool_calls[0].get("id").unwrap().as_str().unwrap(),
            "toolu_1"
        );
        assert_eq!(
            tool_calls[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "read_file"
        );
        let args: serde_json::Value = serde_json::from_str(
            tool_calls[0]
                .get("function")
                .unwrap()
                .get("arguments")
                .unwrap()
                .as_str()
                .unwrap(),
        )
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
            assert_eq!(
                fr, expected_openai,
                "stop_reason {anthropic} → {expected_openai}"
            );
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
        assert_eq!(
            usage.get("completion_tokens").unwrap().as_u64().unwrap(),
            50
        );
        assert_eq!(usage.get("total_tokens").unwrap().as_u64().unwrap(), 150);
    }

    // ── Error translation tests ────────────────────────────────────────

    #[test]
    fn test_error_translation() {
        let input =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Too many requests"}}"#;
        let result = translate_error(input, 529);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed
                .get("error")
                .unwrap()
                .get("message")
                .unwrap()
                .as_str()
                .unwrap(),
            "Too many requests"
        );
        assert_eq!(
            parsed
                .get("error")
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .unwrap(),
            "overloaded_error"
        );
        assert_eq!(
            parsed
                .get("error")
                .unwrap()
                .get("code")
                .unwrap()
                .as_u64()
                .unwrap(),
            529
        );
    }

    #[test]
    fn test_error_translation_malformed_body() {
        let result = translate_error("not json", 500);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed
                .get("error")
                .unwrap()
                .get("message")
                .unwrap()
                .as_str()
                .unwrap(),
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

    // ══════════════════════════════════════════════════════════════════════
    // Anthropic → OpenAI: Request translation tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_a2o_system_string() {
        let input = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "system": "You are helpful.",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "system");
        assert_eq!(
            msgs[0].get("content").unwrap().as_str().unwrap(),
            "You are helpful."
        );
    }

    #[test]
    fn test_a2o_system_block_array() {
        let input = json!({
            "model": "m",
            "max_tokens": 1024,
            "system": [{"type": "text", "text": "Part 1"}, {"type": "text", "text": "Part 2"}],
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(
            msgs[0].get("content").unwrap().as_str().unwrap(),
            "Part 1\n\nPart 2"
        );
    }

    #[test]
    fn test_a2o_user_text_string() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get("content").unwrap().as_str().unwrap(), "Hello");
    }

    #[test]
    fn test_a2o_user_text_blocks_joined() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "A"},
                {"type": "text", "text": "B"}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get("content").unwrap().as_str().unwrap(), "A\n\nB");
    }

    #[test]
    fn test_a2o_user_image_source() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let content = msgs[0].get("content").unwrap().as_array().unwrap();
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "image_url"
        );
        assert_eq!(
            content[0]
                .get("image_url")
                .unwrap()
                .get("url")
                .unwrap()
                .as_str()
                .unwrap(),
            "data:image/png;base64,abc123"
        );
    }

    #[test]
    fn test_a2o_user_tool_result() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "result data"}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "tool");
        assert_eq!(
            msgs[0].get("tool_call_id").unwrap().as_str().unwrap(),
            "toolu_1"
        );
        assert_eq!(
            msgs[0].get("content").unwrap().as_str().unwrap(),
            "result data"
        );
    }

    #[test]
    fn test_a2o_assistant_text() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "assistant", "content": [
                {"type": "text", "text": "Hello"}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "assistant");
        assert_eq!(msgs[0].get("content").unwrap().as_str().unwrap(), "Hello");
    }

    #[test]
    fn test_a2o_assistant_tool_use() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "/src"}}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        let tc = msgs[0].get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "toolu_1");
        assert_eq!(
            tc[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "read_file"
        );
        let args: serde_json::Value = serde_json::from_str(
            tc[0]
                .get("function")
                .unwrap()
                .get("arguments")
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args.get("path").unwrap().as_str().unwrap(), "/src");
    }

    #[test]
    fn test_a2o_assistant_thinking() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "Let me think"},
                {"type": "text", "text": "Answer"}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(
            msgs[0].get("reasoning_content").unwrap().as_str().unwrap(),
            "Let me think"
        );
        assert_eq!(msgs[0].get("content").unwrap().as_str().unwrap(), "Answer");
    }

    #[test]
    fn test_a2o_redacted_thinking_dropped() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "secret"},
                {"type": "text", "text": "Done"}
            ]}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        assert!(msgs[0].get("reasoning_content").is_none());
        assert_eq!(msgs[0].get("content").unwrap().as_str().unwrap(), "Done");
    }

    #[test]
    fn test_a2o_tool_definitions() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [{"name": "get_weather", "description": "Get weather", "input_schema": {"type": "object"}}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let tools = result.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools[0].get("type").unwrap().as_str().unwrap(), "function");
        let func = tools[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "get_weather");
        assert_eq!(
            func.get("parameters")
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .unwrap(),
            "object"
        );
    }

    #[test]
    fn test_a2o_tool_choice_mapping() {
        // auto
        let input = json!({"model": "m", "max_tokens": 1024, "messages": [{"role":"user","content":"Hi"}], "tool_choice": {"type": "auto"}});
        let r = anthropic_to_openai_request(&input).unwrap();
        assert_eq!(r.get("tool_choice").unwrap().as_str().unwrap(), "auto");

        // any → required
        let input = json!({"model": "m", "max_tokens": 1024, "messages": [{"role":"user","content":"Hi"}], "tool_choice": {"type": "any"}});
        let r = anthropic_to_openai_request(&input).unwrap();
        assert_eq!(r.get("tool_choice").unwrap().as_str().unwrap(), "required");

        // specific tool
        let input = json!({"model": "m", "max_tokens": 1024, "messages": [{"role":"user","content":"Hi"}], "tool_choice": {"type": "tool", "name": "my_fn"}});
        let r = anthropic_to_openai_request(&input).unwrap();
        let tc = r.get("tool_choice").unwrap();
        assert_eq!(tc.get("type").unwrap().as_str().unwrap(), "function");
        assert_eq!(
            tc.get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "my_fn"
        );
    }

    #[test]
    fn test_a2o_post_pass_reasoning_fix() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "text", "text": "yes"}
                ]},
                {"role": "user", "content": "ok"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "fn", "input": {}}
                ]}
            ]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        let msgs = result.get("messages").unwrap().as_array().unwrap();
        // Second assistant message should get reasoning_content: " "
        assert_eq!(
            msgs[2].get("reasoning_content").unwrap().as_str().unwrap(),
            " "
        );
    }

    #[test]
    fn test_a2o_stream_options_set() {
        let input = json!({
            "model": "m", "max_tokens": 1024, "stream": true,
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        assert!(result.get("stream").unwrap().as_bool().unwrap());
        assert!(result
            .get("stream_options")
            .unwrap()
            .get("include_usage")
            .unwrap()
            .as_bool()
            .unwrap());
    }

    #[test]
    fn test_a2o_fields_dropped() {
        let input = json!({
            "model": "m", "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hi"}],
            "top_k": 50,
            "metadata": {"user_id": "123"},
            "thinking": {"type": "enabled", "budget_tokens": 5000}
        });
        let result = anthropic_to_openai_request(&input).unwrap();
        assert!(result.get("top_k").is_none());
        assert!(result.get("metadata").is_none());
        assert!(result.get("thinking").is_none());
    }

    // ══════════════════════════════════════════════════════════════════════
    // Anthropic → OpenAI: Response translation tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_a2o_response_text() {
        let input = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let result = openai_to_anthropic_response(&input).unwrap();
        assert_eq!(result.get("id").unwrap().as_str().unwrap(), "abc");
        assert_eq!(result.get("type").unwrap().as_str().unwrap(), "message");
        let content = result.get("content").unwrap().as_array().unwrap();
        assert_eq!(content[0].get("type").unwrap().as_str().unwrap(), "text");
        assert_eq!(content[0].get("text").unwrap().as_str().unwrap(), "Hello");
        assert_eq!(
            result.get("stop_reason").unwrap().as_str().unwrap(),
            "end_turn"
        );
    }

    #[test]
    fn test_a2o_response_with_tool_calls() {
        let input = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{"index": 0, "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"/x\"}"}}]
            }, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let result = openai_to_anthropic_response(&input).unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "tool_use"
        );
        assert_eq!(content[0].get("id").unwrap().as_str().unwrap(), "call_1");
        assert_eq!(
            content[0]
                .get("input")
                .unwrap()
                .get("path")
                .unwrap()
                .as_str()
                .unwrap(),
            "/x"
        );
        assert_eq!(
            result.get("stop_reason").unwrap().as_str().unwrap(),
            "tool_use"
        );
    }

    #[test]
    fn test_a2o_response_with_reasoning() {
        let input = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "deepseek-r1",
            "choices": [{"index": 0, "message": {
                "role": "assistant", "content": "Answer", "reasoning_content": "Thinking..."
            }, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 50, "total_tokens": 60}
        });
        let result = openai_to_anthropic_response(&input).unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "thinking"
        );
        assert_eq!(
            content[0].get("thinking").unwrap().as_str().unwrap(),
            "Thinking..."
        );
        assert_eq!(content[1].get("type").unwrap().as_str().unwrap(), "text");
        assert_eq!(content[1].get("text").unwrap().as_str().unwrap(), "Answer");
    }

    #[test]
    fn test_a2o_response_finish_reason_mapping() {
        fn check(fr: &str, expected: &str) {
            let input = json!({
                "id": "x", "object": "chat.completion", "model": "m",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "x"}, "finish_reason": fr}],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
            });
            let r = openai_to_anthropic_response(&input).unwrap();
            assert_eq!(
                r.get("stop_reason").unwrap().as_str().unwrap(),
                expected,
                "{fr} → {expected}"
            );
        }
        check("stop", "end_turn");
        check("length", "max_tokens");
        check("tool_calls", "tool_use");
        check("function_call", "tool_use");
        check("content_filter", "end_turn");
    }

    #[test]
    fn test_a2o_response_usage_mapping() {
        let input = json!({
            "id": "x", "object": "chat.completion", "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "x"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
        });
        let r = openai_to_anthropic_response(&input).unwrap();
        let usage = r.get("usage").unwrap();
        assert_eq!(usage.get("input_tokens").unwrap().as_u64().unwrap(), 100);
        assert_eq!(usage.get("output_tokens").unwrap().as_u64().unwrap(), 50);
        assert!(usage.get("total_tokens").is_none());
    }

    // ══════════════════════════════════════════════════════════════════════
    // Anthropic → OpenAI: Error translation tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_a2o_error_valid_json() {
        let input = r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit","code":"rate_limit_exceeded"}}"#;
        let result = openai_to_anthropic_error(input, 429);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed.get("type").unwrap().as_str().unwrap(), "error");
        let err = parsed.get("error").unwrap();
        assert_eq!(
            err.get("message").unwrap().as_str().unwrap(),
            "Rate limit exceeded"
        );
        assert_eq!(
            err.get("type").unwrap().as_str().unwrap(),
            "rate_limit_error"
        );
    }

    #[test]
    fn test_a2o_error_malformed() {
        let result = openai_to_anthropic_error("not json", 500);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let err = parsed.get("error").unwrap();
        assert_eq!(err.get("message").unwrap().as_str().unwrap(), "not json");
        assert_eq!(err.get("type").unwrap().as_str().unwrap(), "api_error");
    }

    #[test]
    fn test_a2o_error_status_mapping() {
        let result = openai_to_anthropic_error(r#"{"error":{"message":"x"}}"#, 401);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed
                .get("error")
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .unwrap(),
            "authentication_error"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Anthropic → OpenAI: Streaming translation tests
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_a2o_stream_first_chunk_emits_message_start() {
        let mut state = AnthropicStreamState::default();
        let data = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state);
        assert!(result.is_some());
        let out = result.unwrap();
        assert!(out.contains("message_start"));
        assert!(out.contains("\"id\":\"abc\""));
        assert!(state.message_started);
        assert_eq!(state.model, "gpt-4o");
    }

    #[test]
    fn test_a2o_stream_content_delta() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            ..Default::default()
        };
        let data = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("content_block_start"));
        assert!(result.contains("\"type\":\"text\""));
        assert!(result.contains("text_delta"));
        assert!(result.contains("Hello"));
        assert_eq!(state.open_block.as_deref(), Some("text"));
    }

    #[test]
    fn test_a2o_stream_reasoning_delta() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            ..Default::default()
        };
        let data = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"reasoning_content":"Thinking..."},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("content_block_start"));
        assert!(result.contains("\"type\":\"thinking\""));
        assert!(result.contains("thinking_delta"));
        assert!(result.contains("Thinking..."));
        assert_eq!(state.open_block.as_deref(), Some("thinking"));
    }

    #[test]
    fn test_a2o_stream_tool_call_new() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            ..Default::default()
        };
        let data = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("content_block_start"));
        assert!(result.contains("tool_use"));
        assert!(result.contains("call_1"));
        assert!(result.contains("read_file"));
        assert_eq!(state.open_block.as_deref(), Some("tool_use"));
        assert!(state.tool_state.contains_key(&0));
    }

    #[test]
    fn test_a2o_stream_tool_call_arguments() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            open_block: Some("tool_use".into()),
            ..Default::default()
        };
        state.tool_state.insert(0, ("call_1".into(), "fn".into()));
        let data = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("input_json_delta"));
        assert!(result.contains("{\\\"path\\\":"));
    }

    #[test]
    fn test_a2o_stream_finish_reason() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            open_block: Some("text".into()),
            ..Default::default()
        };
        let data = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("content_block_stop"));
        assert!(result.contains("message_delta"));
        assert!(result.contains("end_turn"));
    }

    #[test]
    fn test_a2o_stream_done() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            ..Default::default()
        };
        let result = openai_to_anthropic_stream_event("message", "[DONE]", &mut state).unwrap();
        assert!(result.contains("message_stop"));
    }

    #[test]
    fn test_a2o_stream_block_transition() {
        let mut state = AnthropicStreamState {
            message_started: true,
            model: "m".into(),
            ..Default::default()
        };
        // First: reasoning
        let data = r#"{"id":"x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"reasoning_content":"think"},"finish_reason":null}]}"#;
        openai_to_anthropic_stream_event("message", data, &mut state);
        assert_eq!(state.open_block.as_deref(), Some("thinking"));
        assert_eq!(state.block_index, 0);

        // Then: content (should close thinking first)
        let data = r#"{"id":"x","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":null}]}"#;
        let result = openai_to_anthropic_stream_event("message", data, &mut state).unwrap();
        assert!(result.contains("content_block_stop"));
        assert!(result.contains("content_block_start"));
        assert_eq!(state.open_block.as_deref(), Some("text"));
        assert_eq!(state.block_index, 1);
    }
}
