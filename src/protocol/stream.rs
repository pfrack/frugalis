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
    /// Accumulated token usage from the terminal OpenAI usage chunk so that
    /// stream-close logging can capture per-request token counts into
    /// InferenceRecord.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
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

    /// Return the accumulated token counts as a tuple suitable for building
    /// a `UsageBreakdown`. Mirrors `StreamTranslateState::collected_usage()`.
    pub fn collected_usage(&self) -> (u64, u64, u64, u64) {
        (
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_creation_tokens,
        )
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
            // Usage-only chunk (no choices). OpenAI emits a terminal chunk
            // with usage when stream_options include_usage is set.
            if let Some(usage) = parsed.get("usage") {
                let completion = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let prompt = usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cached = usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                // Preserve the Anthropic semantic split: cache_read maps from
                // OpenAI cached_tokens; non-cached input = prompt - cached.
                state.input_tokens = prompt.saturating_sub(cached);
                state.output_tokens = completion;
                state.cache_read_tokens = cached;
                state.cache_creation_tokens = 0;
                let msg_delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": completion}
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

#[cfg(test)]
mod tests {
    use super::*;
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
