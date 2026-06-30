use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub(crate) struct ResponsesRequestExtras {
    pub instructions: Option<serde_json::Value>,
    pub model: Option<String>,
    pub tools: Option<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub reasoning: Option<serde_json::Value>,
    pub max_output_tokens: Option<serde_json::Value>,
    pub temperature: Option<serde_json::Value>,
    pub top_p: Option<serde_json::Value>,
    pub parallel_tool_calls: Option<bool>,
    pub metadata: Option<serde_json::Value>,
    pub service_tier: Option<serde_json::Value>,
    pub truncation: Option<serde_json::Value>,
    pub previous_response_id: Option<String>,
    pub store: Option<bool>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponsesRejection {
    pub status: u16,
    pub message: String,
}

impl ResponsesRejection {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

}

fn validate_input_item(item: &serde_json::Value, input_index: usize) -> Result<(), ResponsesRejection> {
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    // Accept Chat-style messages (no type, but with role) as valid
    if item_type.is_empty() && item.get("role").and_then(|v| v.as_str()).is_some() {
        return Ok(());
    }
    match item_type {
        "message" | "function_call" | "function_call_output" | "item_reference"
        | "agent_message" | "custom_tool_call" | "custom_tool_call_output"
        | "compaction_summary" => Ok(()),
        "web_search_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'web_search' is not available on this gateway (input item at index {})", input_index),
        )),
        "file_search_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'file_search' is not available on this gateway (input item at index {})", input_index),
        )),
        "computer_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'computer_use' is not available on this gateway (input item at index {})", input_index),
        )),
        "code_interpreter_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'code_interpreter' is not available on this gateway (input item at index {})", input_index),
        )),
        "image_generation_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'image_generation' is not available on this gateway (input item at index {})", input_index),
        )),
        "local_shell_call" | "local_shell_call_output" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'shell' is not available on this gateway (input item at index {})", input_index),
        )),
        "apply_patch_tool_call" | "apply_patch_tool_call_output" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: built-in tool 'apply_patch' is not available on this gateway (input item at index {})", input_index),
        )),
        "mcp_list_tools" | "mcp_approval_request" | "mcp_approval_response" | "mcp_tool_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: MCP tools are not available on this gateway (input item at index {})", input_index),
        )),
        "reasoning" => Ok(()),
        "tool_search_call" => Err(ResponsesRejection::bad_request(
            format!("Unsupported feature: tool search is not available on this gateway (input item at index {})", input_index),
        )),
        "" => Err(ResponsesRejection::bad_request(
            format!("Input item at index {} has no 'type' field", input_index),
        )),
        other => Err(ResponsesRejection::bad_request(
            format!("Unsupported input item type '{}' at index {}", other, input_index),
        )),
    }
}

fn input_item_to_chat_message(item: &serde_json::Value) -> Option<serde_json::Value> {
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    // Accept Chat-style messages (no type, but with role)
    if item_type.is_empty() && item.get("role").and_then(|v| v.as_str()).is_some() {
        return Some(serde_json::json!({
            "role": item.get("role").and_then(|v| v.as_str()).unwrap_or("user"),
            "content": item.get("content").unwrap_or(&serde_json::Value::Null),
        }));
    }
    match item_type {
        "message" => {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = item.get("content");
            Some(serde_json::json!({
                "role": role,
                "content": content.unwrap_or(&serde_json::Value::Null),
            }))
        }
        "function_call" => {
            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let content = serde_json::json!([
                {"type": "tool_use", "id": call_id, "name": name, "input": serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null)}
            ]);
            Some(serde_json::json!({
                "role": "assistant",
                "content": content,
            }))
        }
        "function_call_output" => {
            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
            let output = item.get("output").and_then(|v| v.as_str()).unwrap_or("");
            Some(serde_json::json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output,
            }))
        }
        "custom_tool_call" => {
            let call_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = item.get("arguments").unwrap_or(&serde_json::Value::Null);
            Some(serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(arguments).unwrap_or_default()
                    }
                }]
            }))
        }
        "custom_tool_call_output" => {
            let call_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let output = item.get("output").and_then(|v| v.as_str()).unwrap_or("");
            Some(serde_json::json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output,
            }))
        }
        "item_reference" => {
            // Item references are used by Codex for multi-turn but we can't
            // resolve them without a transcript store. Skip them silently.
            None
        }
        "reasoning" => {
            // Reasoning items from previous turns have no Chat equivalent.
            // Skip them — the model doesn't need to see them again.
            None
        }
        "compaction_summary" => {
            let summary_text = item.get("summary_text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if summary_text.is_empty() {
                None
            } else {
                Some(serde_json::json!({
                    "role": "system",
                    "content": format!("[Conversation summary: {}]", summary_text),
                }))
            }
        }
        "agent_message" => {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            Some(serde_json::json!({
                "role": role,
                "content": content,
            }))
        }
        _ => None,
    }
}

pub(crate) fn request_to_chat(
    body: &serde_json::Value,
) -> Result<(serde_json::Value, ResponsesRequestExtras), ResponsesRejection> {
    let mut extras = ResponsesRequestExtras::default();

    let input = body.get("input");
    let instructions = body.get("instructions");
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let tools = body.get("tools");
    let tool_choice = body.get("tool_choice");
    let reasoning = body.get("reasoning");

    if body.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(ResponsesRejection::bad_request(
            "Unsupported feature: 'background: true' is not available on this gateway",
        ));
    }

    if body.get("conversation").is_some() {
        return Err(ResponsesRejection::bad_request(
            "Unsupported feature: 'conversation' API is not available on this gateway; use 'previous_response_id' with the full transcript instead",
        ));
    }

    if body.get("prompt").is_some() {
        return Err(ResponsesRejection::bad_request(
            "Unsupported feature: 'prompt' field is not available on this gateway; use 'instructions' instead",
        ));
    }

    extras.previous_response_id = body
        .get("previous_response_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    extras.store = body.get("store").and_then(|v| v.as_bool());

    if extras.store == Some(true) {
        tracing::warn!("store=true has no effect on this gateway; transcripts are not persisted");
    }

    let reasoning_effort = reasoning
        .and_then(|r| r.get("effort"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(ref effort) = reasoning_effort {
        if effort != "none" {
            tracing::warn!(
                "reasoning.effort={} requested but fidelity is best-effort — Chat Completions has no first-class reasoning field",
                effort
            );
        }
    }
    extras.reasoning_effort = reasoning_effort;
    extras.reasoning = reasoning.cloned();

    // ── Build messages array ──
    let mut messages: Vec<serde_json::Value> = Vec::new();

    if let Some(instructions_val) = instructions {
        let instructions_content = match instructions_val {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(items) => {
                let mut parts: Vec<String> = Vec::new();
                for item in items {
                    if let Some(text) = item.get("content").and_then(|v| v.as_str()) {
                        parts.push(text.to_string());
                    }
                }
                parts.join("\n")
            }
            _ => String::new(),
        };
        if !instructions_content.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": instructions_content,
            }));
        }
    }

    match input {
        Some(serde_json::Value::String(s)) => {
            messages.push(serde_json::json!({
                "role": "user",
                "content": s,
            }));
        }
        Some(serde_json::Value::Array(items)) => {
            if items.len() > 1000 {
                return Err(ResponsesRejection::bad_request(
                    "'input' array exceeds maximum length of 1000 items",
                ));
            }
            for (idx, item) in items.iter().enumerate() {
                validate_input_item(item, idx)?;
                if let Some(msg) = input_item_to_chat_message(item) {
                    messages.push(msg);
                }
            }
        }
        Some(_) => {
            return Err(ResponsesRejection::bad_request(
                "'input' must be a string or an array of input items",
            ));
        }
        None => {
            return Err(ResponsesRejection::bad_request(
                "'input' is required",
            ));
        }
    }

    // ── Build chat body ──
    let mut chat_body: serde_json::Value = serde_json::json!({
        "messages": messages,
        "stream": stream,
    });

    if let Some(model) = body.get("model").and_then(|v| v.as_str()) {
        chat_body["model"] = serde_json::Value::String(model.to_string());
        extras.model = Some(model.to_string());
    }

    if let Some(max_output_tokens) = body.get("max_output_tokens") {
        chat_body["max_tokens"] = max_output_tokens.clone();
        extras.max_output_tokens = Some(max_output_tokens.clone());
    }

    if let Some(temperature) = body.get("temperature") {
        chat_body["temperature"] = temperature.clone();
        extras.temperature = Some(temperature.clone());
    }

    if let Some(top_p) = body.get("top_p") {
        chat_body["top_p"] = top_p.clone();
        extras.top_p = Some(top_p.clone());
    }

    if let Some(parallel_tool_calls) = body.get("parallel_tool_calls").and_then(|v| v.as_bool()) {
        chat_body["parallel_tool_calls"] = serde_json::Value::Bool(parallel_tool_calls);
        extras.parallel_tool_calls = Some(parallel_tool_calls);
    }

    if let Some(metadata) = body.get("metadata") {
        chat_body["metadata"] = metadata.clone();
        extras.metadata = Some(metadata.clone());
    }

    if let Some(service_tier) = body.get("service_tier") {
        chat_body["service_tier"] = service_tier.clone();
        extras.service_tier = Some(service_tier.clone());
    }

    // prompt_cache_key → user
    if let Some(prompt_cache_key) = body.get("prompt_cache_key").and_then(|v| v.as_str()) {
        chat_body["user"] = serde_json::Value::String(prompt_cache_key.to_string());
    }

    // text.format → response_format
    if let Some(text) = body.get("text") {
        if let Some(format) = text.get("format") {
            match format.get("type").and_then(|v| v.as_str()) {
                Some("text") => {}
                Some("json_object") => {
                    chat_body["response_format"] = serde_json::json!({"type": "json_object"});
                }
                Some("json_schema") => {
                    chat_body["response_format"] = format.clone();
                }
                Some("grammar") => {
                    return Err(ResponsesRejection::bad_request(
                        "Unsupported feature: 'text.format: grammar' is not available on this gateway",
                    ));
                }
                Some(other) => {
                    return Err(ResponsesRejection::bad_request(
                        format!("Unsupported feature: 'text.format' type '{}' is not available on this gateway", other),
                    ));
                }
                None => {}
            }
        }
    }

    // tools: filter to type: "function" only
    if let Some(tools_val) = tools {
        let supported_tools: Vec<serde_json::Value> = match tools_val {
            serde_json::Value::Array(arr) => {
                let mut result = Vec::new();
                for tool in arr {
                    let tool_type = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match tool_type {
                        "function" => result.push(tool.clone()),
                        _ => {
                            return Err(ResponsesRejection::bad_request(
                                format!("Unsupported tool type '{}': only 'function' tools are available on this gateway", tool_type),
                            ));
                        }
                    }
                }
                result
            }
            _ => {
                return Err(ResponsesRejection::bad_request(
                    "'tools' must be an array",
                ));
            }
        };
        if !supported_tools.is_empty() {
            chat_body["tools"] = serde_json::Value::Array(supported_tools);
        }
        extras.tools = Some(tools_val.clone());
    }

    // tool_choice: reduce to Chat's 4 shapes
    if let Some(tc) = tool_choice {
        match tc {
            serde_json::Value::String(s) => {
                match s.as_str() {
                    "auto" | "none" | "required" => {
                        chat_body["tool_choice"] = tc.clone();
                    }
                    other => {
                        return Err(ResponsesRejection::bad_request(
                            format!("Unsupported tool_choice '{}': only 'auto', 'none', 'required', or {{type:'function', function:{{name}}}} are available", other),
                        ));
                    }
                }
            }
            serde_json::Value::Object(map) => {
                match map.get("type").and_then(|v| v.as_str()) {
                    Some("function") => {
                        chat_body["tool_choice"] = tc.clone();
                    }
                    Some(allowed) => {
                        return Err(ResponsesRejection::bad_request(
                            format!("Unsupported tool_choice type '{}': only 'function' type is available on this gateway", allowed),
                        ));
                    }
                    None => {
                        return Err(ResponsesRejection::bad_request(
                            "tool_choice object must have a 'type' field",
                        ));
                    }
                }
            }
            _ => {
                return Err(ResponsesRejection::bad_request(
                    "tool_choice must be a string or an object",
                ));
            }
        }
        extras.tool_choice = Some(tc.clone());
    }

    if let Some(truncation) = body.get("truncation") {
        extras.truncation = Some(truncation.clone());
    }

    // Pass through remaining fields that Chat Completions understands
    for key in &["safety_identifier", "user"] {
        if let Some(val) = body.get(*key) {
            chat_body[key] = val.clone();
        }
    }

    extras.instructions = instructions.cloned();
    extras.model = body.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());

    Ok((chat_body, extras))
}

pub(crate) fn response_from_chat(
    chat_body: &serde_json::Value,
    extras: &ResponsesRequestExtras,
) -> Result<serde_json::Value, String> {
    let response_id = format!("resp_{}", Uuid::new_v4());
    let created_at = chrono::Utc::now().timestamp();

    let empty_vec = vec![];
    let choices = chat_body
        .get("choices")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_vec);
    let choice = choices.first();

    let finish_reason = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("stop");

    let message = choice.and_then(|c| c.get("message"));

    // ── Status ──
    let (status, incomplete_details): (String, Option<serde_json::Value>) = match finish_reason {
        "stop" | "tool_calls" => ("completed".to_string(), None),
        "length" => (
            "incomplete".to_string(),
            Some(serde_json::json!({"reason": "max_output_tokens"})),
        ),
        "content_filter" => (
            "incomplete".to_string(),
            Some(serde_json::json!({"reason": "content_filter"})),
        ),
        _ => ("completed".to_string(), None),
    };

    // ── Build output[] items ──
    let mut output: Vec<serde_json::Value> = Vec::new();

    if let Some(msg) = message {
        // reasoning_content (DeepSeek-style)
        let reasoning_content = msg.get("reasoning_content").and_then(|v| v.as_str());
        if let Some(reasoning_text) = reasoning_content {
            if !reasoning_text.is_empty() {
                output.push(serde_json::json!({
                    "type": "reasoning",
                    "id": format!("rs_{}", Uuid::new_v4()),
                    "summary": [{
                        "type": "summary_text",
                        "text": reasoning_text,
                    }],
                    "content": [],
                }));
            }
        }

        // content
        let content = msg.get("content").and_then(|v| v.as_str());
        let msg_id = format!("msg_{}", Uuid::new_v4());

        if let Some(text) = content {
            if !text.is_empty() {
                output.push(serde_json::json!({
                    "type": "message",
                    "id": msg_id,
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    }],
                }));
            }
        }

        // tool_calls
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let arguments = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                output.push(serde_json::json!({
                    "type": "function_call",
                    "id": format!("fc_{}", Uuid::new_v4()),
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments,
                    "status": "completed",
                }));
            }
        }

        // refusal
        if let Some(refusal) = msg.get("refusal").and_then(|v| v.as_str()) {
            if !refusal.is_empty() && output.is_empty() {
                output.push(serde_json::json!({
                    "type": "message",
                    "id": format!("msg_{}", Uuid::new_v4()),
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "refusal",
                        "refusal": refusal,
                    }],
                }));
            }
        }
    }

    // ── output_text convenience ──
    let output_text: String = output
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(|v| v.as_str())?;
            if item_type == "message" {
                item.get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| {
                        arr.iter().find_map(|part| {
                            if part.get("type") == Some(&serde_json::Value::String("output_text".to_string()))
                            {
                                part.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                    })
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ── Usage ──
    let usage = chat_body.get("usage");
    let prompt_tokens = usage.and_then(|u| u.get("prompt_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);
    let cached_tokens = usage
        .and_then(|u| u.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion_tokens = usage.and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);

    let input_tokens = (prompt_tokens.saturating_sub(cached_tokens)).max(0) as i64;
    let total_tokens = input_tokens + completion_tokens;

    // ── Build response ──
    let mut resp = serde_json::json!({
        "id": response_id,
        "object": "response",
        "status": status,
        "created_at": created_at,
        "completed_at": created_at,
        "output": output,
        "output_text": output_text,
        "model": serde_json::Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "input_tokens_details": {
                "cached_tokens": cached_tokens,
            },
            "output_tokens": completion_tokens,
            "output_tokens_details": {
                "reasoning_tokens": 0,
            },
            "total_tokens": total_tokens,
        },
        "instructions": serde_json::Value::Null,
        "tools": serde_json::Value::Null,
        "tool_choice": serde_json::Value::Null,
        "parallel_tool_calls": true,
        "temperature": serde_json::Value::Null,
        "top_p": serde_json::Value::Null,
        "max_output_tokens": serde_json::Value::Null,
        "reasoning": serde_json::Value::Null,
        "metadata": serde_json::Value::Null,
        "truncation": "disabled",
        "previous_response_id": serde_json::Value::Null,
        "service_tier": serde_json::Value::Null,
        "incomplete_details": serde_json::Value::Null,
        "error": serde_json::Value::Null,
    });

    if let Some(reason) = incomplete_details {
        resp["status"] = serde_json::Value::String("incomplete".to_string());
        resp["incomplete_details"] = reason;
    }

    if let Some(model) = &extras.model {
        resp["model"] = serde_json::Value::String(model.clone());
    }

    if let Some(prev) = &extras.previous_response_id {
        resp["previous_response_id"] = serde_json::Value::String(prev.clone());
    }

    if let Some(tc) = &extras.tool_choice {
        resp["tool_choice"] = tc.clone();
    }

    if let Some(tools) = &extras.tools {
        resp["tools"] = tools.clone();
    }

    if let Some(reasoning) = &extras.reasoning {
        resp["reasoning"] = reasoning.clone();
    }

    if let Some(max_tokens) = &extras.max_output_tokens {
        resp["max_output_tokens"] = max_tokens.clone();
    }

    if let Some(temp) = &extras.temperature {
        resp["temperature"] = temp.clone();
    }

    if let Some(tp) = &extras.top_p {
        resp["top_p"] = tp.clone();
    }

    if let Some(pct) = extras.parallel_tool_calls {
        resp["parallel_tool_calls"] = serde_json::Value::Bool(pct);
    }

    if let Some(meta) = &extras.metadata {
        resp["metadata"] = meta.clone();
    }

    if let Some(st) = &extras.service_tier {
        resp["service_tier"] = st.clone();
    }

    if let Some(trunc) = &extras.truncation {
        resp["truncation"] = trunc.clone();
    }

    if let Some(instructions) = &extras.instructions {
        resp["instructions"] = instructions.clone();
    }

    Ok(resp)
}

pub(crate) fn wrap_error_as_responses(status: u16, message: &str) -> serde_json::Value {
    let error_code = match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        404 => "not_found",
        415 => "unsupported_media_type",
        429 => "rate_limit_exceeded",
        502 => "server_error",
        503 => "server_error",
        _ => "api_error",
    };
    let resp_id = format!("resp_{}", Uuid::new_v4());
    serde_json::json!({
        "id": resp_id,
        "object": "response",
        "status": "failed",
        "created_at": chrono::Utc::now().timestamp(),
        "completed_at": serde_json::Value::Null,
        "error": {
            "code": error_code,
            "message": message,
            "param": null,
        },
        "output": [],
        "output_text": "",
        "usage": {
            "input_tokens": 0,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 0,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 0,
        },
    })
}

pub(crate) fn map_upstream_error_to_responses(status: u16, upstream_body: &str) -> serde_json::Value {
    let parsed = serde_json::from_str::<serde_json::Value>(upstream_body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or(upstream_body);
    wrap_error_as_responses(status, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("valid JSON")
    }

    // ── request_to_chat: basic ──

    #[test]
    fn test_request_to_chat_minimal() {
        let body = parse_json(r#"{"model":"gpt-4o","input":"hello"}"#);
        let (chat, extras) = request_to_chat(&body).unwrap();
        assert_eq!(chat["model"], "gpt-4o");
        assert_eq!(chat["messages"][0]["role"], "user");
        assert_eq!(chat["messages"][0]["content"], "hello");
        assert_eq!(chat["stream"], false);
        assert!(extras.model.is_some());
    }

    #[test]
    fn test_request_to_chat_with_input_items() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"role":"user","content":"hello"}]}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["messages"][0]["role"], "user");
        assert_eq!(chat["messages"][0]["content"], "hello");
    }

    #[test]
    fn test_request_to_chat_with_instructions() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hi","instructions":"be helpful"}"#,
        );
        let (chat, extras) = request_to_chat(&body).unwrap();
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], "be helpful");
        assert_eq!(chat["messages"][1]["role"], "user");
        assert_eq!(extras.instructions, Some(serde_json::Value::String("be helpful".to_string())));
    }

    #[test]
    fn test_request_to_chat_with_instructions_array() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hi","instructions":[{"role":"developer","content":"be nice"}]}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], "be nice");
    }

    #[test]
    fn test_request_to_chat_with_max_output_tokens() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","max_output_tokens":4096}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["max_tokens"], 4096);
    }

    #[test]
    fn test_request_to_chat_stream_true() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","stream":true}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["stream"], true);
    }

    #[test]
    fn test_request_to_chat_prompt_cache_key_to_user() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","prompt_cache_key":"my-key"}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["user"], "my-key");
    }

    #[test]
    fn test_request_to_chat_text_format_json_object() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","text":{"format":{"type":"json_object"}}}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["response_format"]["type"], "json_object");
    }

    #[test]
    fn test_request_to_chat_text_format_json_schema() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","text":{"format":{"type":"json_schema","schema":{"type":"object"}}}}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["response_format"]["type"], "json_schema");
    }

    #[test]
    fn test_request_to_chat_with_tools() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","tools":[{"type":"function","function":{"name":"get_weather","parameters":{}}}]}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_request_to_chat_with_tool_choice_auto() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","tool_choice":"auto"}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["tool_choice"], "auto");
    }

    #[test]
    fn test_request_to_chat_with_tool_choice_function() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","tool_choice":{"type":"function","function":{"name":"get_weather"}}}"#,
        );
        let (chat, _) = request_to_chat(&body).unwrap();
        assert_eq!(chat["tool_choice"]["type"], "function");
    }

    #[test]
    fn test_request_to_chat_reasoning_extracted() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","reasoning":{"effort":"medium","summary":"auto"}}"#,
        );
        let (chat, extras) = request_to_chat(&body).unwrap();
        assert!(chat.get("reasoning").is_none(), "reasoning should not be in chat body");
        assert_eq!(extras.reasoning_effort, Some("medium".to_string()));
        assert!(extras.reasoning.is_some());
    }

    #[test]
    fn test_request_to_chat_reasoning_effort_fidelity_warning() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","reasoning":{"effort":"medium"}}"#,
        );
        let (chat, extras) = request_to_chat(&body).unwrap();
        assert_eq!(extras.reasoning_effort, Some("medium".to_string()));
        assert!(chat.get("reasoning").is_none());
    }

    #[test]
    fn test_request_to_chat_store_true_warns() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","store":true}"#,
        );
        let (_, extras) = request_to_chat(&body).unwrap();
        assert_eq!(extras.store, Some(true));
    }

    // ── request_to_chat: rejections ──

    #[test]
    fn test_request_to_chat_rejects_web_search_tool() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"web_search_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("web_search"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_code_interpreter() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"code_interpreter_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("code_interpreter"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_background_true() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","background":true}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("background"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_conversation() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","conversation":{"id":"conv_123"}}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("conversation"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_prompt_field() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","prompt":"do it"}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("prompt"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_grammar_format() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hello","text":{"format":{"type":"grammar"}}}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("grammar"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_file_search() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"file_search_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("file_search"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_computer_call() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"computer_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("computer_use"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_image_generation() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"image_generation_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("image_generation"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_shell_tools() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"local_shell_call","id":"sh_1","name":"bash"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("shell"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_apply_patch() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"apply_patch_tool_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("apply_patch"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_mcp_tools() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":[{"type":"mcp_tool_call"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("MCP"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_non_function_tool() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hi","tools":[{"type":"web_search"}]}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("web_search"));
        assert_eq!(err.status, 400);
    }

    #[test]
    fn test_request_to_chat_rejects_invalid_tool_choice() {
        let body = parse_json(
            r#"{"model":"gpt-4o","input":"hi","tool_choice":{"type":"allowed","allowed_tools":[{"type":"web_search"}]}}"#,
        );
        let err = request_to_chat(&body).unwrap_err();
        assert!(err.message.contains("allowed"));
        assert_eq!(err.status, 400);
    }

    // ── response_from_chat: basic ──

    #[test]
    fn test_response_from_chat_simple_text() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"Hello there"}}],
            "usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":2}}
        }"#,
        );
        let extras = ResponsesRequestExtras {
            model: Some("gpt-4o".to_string()),
            ..Default::default()
        };
        let resp = response_from_chat(&chat, &extras).unwrap();
        assert_eq!(resp["object"], "response");
        assert!(resp["id"].as_str().unwrap().starts_with("resp_"));
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["model"], "gpt-4o");
        assert_eq!(resp["output"][0]["content"][0]["text"], "Hello there");
        assert_eq!(resp["usage"]["input_tokens"], 8);
        assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 2);
        assert_eq!(resp["usage"]["output_tokens"], 3);
    }

    #[test]
    fn test_response_from_chat_with_tool_calls() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"NYC\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}
        }"#,
        );
        let resp = response_from_chat(&chat, &ResponsesRequestExtras::default()).unwrap();
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["output"][0]["type"], "function_call");
        assert_eq!(resp["output"][0]["name"], "get_weather");
        assert_eq!(resp["output"][0]["arguments"], "{\"city\":\"NYC\"}");
    }

    #[test]
    fn test_response_from_chat_finish_reason_length() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"length","message":{"role":"assistant","content":"truncated"}}],
            "usage":{"prompt_tokens":10,"completion_tokens":200}
        }"#,
        );
        let resp = response_from_chat(&chat, &ResponsesRequestExtras::default()).unwrap();
        assert_eq!(resp["status"], "incomplete");
        assert_eq!(resp["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn test_response_from_chat_finish_reason_content_filter() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"content_filter","message":{"role":"assistant","content":"sorry"}}],
            "usage":{"prompt_tokens":10,"completion_tokens":1}
        }"#,
        );
        let resp = response_from_chat(&chat, &ResponsesRequestExtras::default()).unwrap();
        assert_eq!(resp["status"], "incomplete");
        assert_eq!(resp["incomplete_details"]["reason"], "content_filter");
    }

    #[test]
    fn test_response_from_chat_usage_saturating_sub() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}],
            "usage":{"prompt_tokens":5,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":10}}
        }"#,
        );
        let resp = response_from_chat(&chat, &ResponsesRequestExtras::default()).unwrap();
        assert_eq!(resp["usage"]["input_tokens"], 0);
        assert_eq!(resp["usage"]["total_tokens"], 1);
    }

    #[test]
    fn test_response_from_chat_echoes_request_fields() {
        let chat = parse_json(
            r#"{
            "id":"chatcmpl-abc",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"ok"}}],
            "usage":{"prompt_tokens":5,"completion_tokens":1}
        }"#,
        );
        let extras = ResponsesRequestExtras {
            model: Some("gpt-4o".to_string()),
            instructions: Some(serde_json::Value::String("be nice".to_string())),
            tool_choice: Some(serde_json::Value::String("auto".to_string())),
            max_output_tokens: Some(serde_json::Value::Number(serde_json::Number::from(4096))),
            parallel_tool_calls: Some(true),
            ..Default::default()
        };
        let resp = response_from_chat(&chat, &extras).unwrap();
        assert_eq!(resp["instructions"], "be nice");
        assert_eq!(resp["tool_choice"], "auto");
        assert_eq!(resp["max_output_tokens"], 4096);
        assert_eq!(resp["parallel_tool_calls"], true);
    }

    #[test]
    fn test_wrap_error_as_responses_bad_request() {
        let err = wrap_error_as_responses(400, "something went wrong");
        assert_eq!(err["status"], "failed");
        assert_eq!(err["error"]["code"], "invalid_request_error");
        assert_eq!(err["error"]["message"], "something went wrong");
        assert!(err["id"].as_str().unwrap().starts_with("resp_"));
    }

    #[test]
    fn test_wrap_error_as_responses_rate_limit() {
        let err = wrap_error_as_responses(429, "too many");
        assert_eq!(err["error"]["code"], "rate_limit_exceeded");
    }

    #[test]
    fn test_wrap_error_as_responses_server_error() {
        let err = wrap_error_as_responses(502, "upstream failed");
        assert_eq!(err["error"]["code"], "server_error");
    }
}
