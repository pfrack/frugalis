use serde_json::json;

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


#[cfg(test)]
mod tests {
    use super::*;
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
            let cc = result
                .get("cache_control")
                .expect("cache_control must be inserted");
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
            let (translated, had_cc) = anthropic_to_openai_request_with_cache_signal(&input).unwrap();
            assert!(
                had_cc,
                "had_cache_control must be true when a block carries it"
            );
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
            let (_translated, had_cc) = anthropic_to_openai_request_with_cache_signal(&input).unwrap();
            assert!(
                !had_cc,
                "had_cache_control must be false when no breakpoint is present"
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
}
