use serde_json::json;
use tracing::debug;

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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(
            usage.get("prompt_tokens").and_then(|v| v.as_u64()),
            Some(50)
        );
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
        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(
            cache_read, 90,
            "cache_read_input_tokens maps from cached_tokens"
        );
        assert_eq!(
            input_tokens, 40,
            "input_tokens is the non-cached portion (130 - 90)"
        );
        assert_eq!(cache_creation, 0, "OpenAI has no cache-creation concept");
        assert_eq!(
            cache_read + cache_creation + input_tokens,
            130,
            "Anthropic input invariant must equal OpenAI prompt_tokens"
        );
    }
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
}
