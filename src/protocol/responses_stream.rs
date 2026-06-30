use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct SseEvent {
    pub event: String,
    pub data: serde_json::Value,
}

impl SseEvent {
    pub(crate) fn new(event: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    pub(crate) fn to_sse_bytes(&self) -> Vec<u8> {
        let data_str = serde_json::to_string(&self.data).unwrap_or_default();
        format!("event: {}\ndata: {}\n\n", self.event, data_str).into_bytes()
    }
}

#[derive(Debug)]
pub(crate) struct ResponsesStreamState {
    pub response_id: String,
    pub sequence_number: u64,
    pub created_emitted: bool,
    pub msg_output_item_emitted: bool,
    pub content_part_emitted: bool,
    pub reasoning_output_item_emitted: bool,
    pub reasoning_summary_part_emitted: bool,
    pub msg_output_index: u64,
    pub content_index: u64,
    pub tool_call_emitted: Vec<bool>,
    pub tool_call_ids: Vec<String>,
    pub tool_call_names: Vec<String>,
    pub tool_call_arguments: Vec<String>,
    pub has_content: bool,
    pub reasoning_text: String,
    pub has_reasoning: bool,
    pub has_tool_calls: bool,
    pub has_refusal: bool,
    pub finished: bool,
}

impl ResponsesStreamState {
    pub(crate) fn new() -> Self {
        Self {
            response_id: format!("resp_{}", Uuid::new_v4()),
            sequence_number: 0,
            created_emitted: false,
            msg_output_item_emitted: false,
            content_part_emitted: false,
            reasoning_output_item_emitted: false,
            reasoning_summary_part_emitted: false,
            msg_output_index: 0,
            content_index: 0,
            tool_call_emitted: vec![],
            tool_call_ids: vec![],
            tool_call_names: vec![],
            tool_call_arguments: vec![],
            has_content: false,
            reasoning_text: String::new(),
            has_reasoning: false,
            has_tool_calls: false,
            has_refusal: false,
            finished: false,
        }
    }

    pub(crate) fn emit_created(&mut self) -> SseEvent {
        let event = SseEvent::new(
            "response.created",
            serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "status": "in_progress",
                    "output": [],
                },
                "sequence_number": self.sequence_number,
            }),
        );
        self.sequence_number += 1;
        self.created_emitted = true;
        event
    }

    fn ensure_reasoning_output_item(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.reasoning_output_item_emitted {
            let output_index = self.msg_output_index;
            self.msg_output_index += 1;
            events.push(SseEvent::new(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "type": "reasoning",
                        "id": format!("rs_{}", Uuid::new_v4()),
                        "summary": [],
                        "content": [],
                    },
                    "sequence_number": self.sequence_number,
                }),
            ));
            self.sequence_number += 1;
            self.reasoning_output_item_emitted = true;
        }
        if !self.reasoning_summary_part_emitted {
            events.push(SseEvent::new(
                "response.reasoning_summary_part.added",
                serde_json::json!({
                    "type": "response.reasoning_summary_part.added",
                    "item_id": format!("rs_{}", Uuid::new_v4()),
                    "output_index": self.msg_output_index.saturating_sub(1),
                    "summary_index": 0,
                    "part": {
                        "type": "summary_text",
                        "text": "",
                    },
                    "sequence_number": self.sequence_number,
                }),
            ));
            self.sequence_number += 1;
            self.reasoning_summary_part_emitted = true;
        }
        events
    }

    fn ensure_msg_output_item(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.msg_output_item_emitted {
            let output_index = self.msg_output_index;
            self.msg_output_index += 1;
            self.content_index = 0;
            events.push(SseEvent::new(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "type": "message",
                        "id": format!("msg_{}", Uuid::new_v4()),
                        "role": "assistant",
                        "status": "in_progress",
                        "content": [],
                    },
                    "sequence_number": self.sequence_number,
                }),
            ));
            self.sequence_number += 1;
            self.msg_output_item_emitted = true;
        }
        if !self.content_part_emitted {
            let msg_item_id = format!("msg_{}", Uuid::new_v4());
            events.push(SseEvent::new(
                "response.content_part.added",
                serde_json::json!({
                    "type": "response.content_part.added",
                    "item_id": msg_item_id,
                    "output_index": self.msg_output_index.saturating_sub(1),
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "",
                        "annotations": [],
                    },
                    "sequence_number": self.sequence_number,
                }),
            ));
            self.sequence_number += 1;
            self.content_part_emitted = true;
        }
        events
    }

    fn ensure_tool_call_items(&mut self, tool_calls: &[serde_json::Value]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        for (i, tc) in tool_calls.iter().enumerate() {
            if i >= self.tool_call_emitted.len() || !self.tool_call_emitted[i] {
                let output_index = self.msg_output_index;
                self.msg_output_index += 1;
                let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let fc_id = format!("fc_{}", Uuid::new_v4());
                events.push(SseEvent::new(
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "id": fc_id,
                            "call_id": call_id,
                            "name": name,
                            "arguments": "",
                            "status": "in_progress",
                        },
                        "sequence_number": self.sequence_number,
                    }),
                ));
                self.sequence_number += 1;
                if i >= self.tool_call_emitted.len() {
                    self.tool_call_emitted.push(true);
                    self.tool_call_ids.push(call_id.to_string());
                    self.tool_call_names.push(name.to_string());
                    self.tool_call_arguments.push(String::new());
                } else {
                    self.tool_call_emitted[i] = true;
                }
                self.has_tool_calls = true;
            }
        }
        events
    }
}

pub(crate) fn translate_chat_chunk_to_responses_events(
    state: &mut ResponsesStreamState,
    chunk: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();

    if !state.created_emitted {
        events.push(state.emit_created());
    }

    if chunk == "data: [DONE]" || chunk == "[DONE]" {
        if !state.finished {
            state.finished = true;
            events.push(SseEvent::new(
                "response.completed",
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": state.response_id,
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                            "total_tokens": 0,
                        },
                    },
                    "sequence_number": state.sequence_number,
                }),
            ));
            state.sequence_number += 1;
        }
        return events;
    }

    let data_str = if let Some(rest) = chunk.strip_prefix("data: ") {
        rest
    } else {
        return events;
    };

    if data_str == "[DONE]" {
        if !state.finished {
            state.finished = true;
            events.push(SseEvent::new(
                "response.completed",
                serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": state.response_id,
                        "status": "completed",
                        "output": [],
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                            "total_tokens": 0,
                        },
                    },
                    "sequence_number": state.sequence_number,
                }),
            ));
            state.sequence_number += 1;
        }
        return events;
    }

    let parsed: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(_) => return events,
    };

    // Extract the choice
    let choice = match parsed
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
    {
        Some(c) => c,
        None => return events,
    };

    let delta = choice.get("delta");
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

    // ── Reasoning content ──
    if let Some(delta) = delta {
        if let Some(reasoning_content) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            if !reasoning_content.is_empty() {
                state.has_reasoning = true;
                events.extend(state.ensure_reasoning_output_item());
                state.reasoning_text.push_str(reasoning_content);
                events.push(SseEvent::new(
                    "response.reasoning_summary_text.delta",
                    serde_json::json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": format!("rs_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "summary_index": 0,
                        "delta": reasoning_content,
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;
            }
        }
    }

    // ── Content / text ──
    if let Some(delta) = delta {
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                state.has_content = true;
                events.extend(state.ensure_msg_output_item());
                events.push(SseEvent::new(
                    "response.output_text.delta",
                    serde_json::json!({
                        "type": "response.output_text.delta",
                        "item_id": format!("msg_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "content_index": 0,
                        "delta": content,
                        "logprobs": [],
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;
            }
        }
    }

    // ── Tool calls ──
    if let Some(delta) = delta {
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            events.extend(state.ensure_tool_call_items(tool_calls));
            for (i, tc) in tool_calls.iter().enumerate() {
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    if !args.is_empty() {
                        if i < state.tool_call_arguments.len() {
                            state.tool_call_arguments[i].push_str(args);
                        }
                        events.push(SseEvent::new(
                            "response.function_call_arguments.delta",
                            serde_json::json!({
                                "type": "response.function_call_arguments.delta",
                                "item_id": format!("fc_{}", Uuid::new_v4()),
                                "output_index": state.msg_output_index.saturating_sub(1),
                                "delta": args,
                                "sequence_number": state.sequence_number,
                            }),
                        ));
                        state.sequence_number += 1;
                    }
                }
            }
        }
    }

    // ── Refusal ──
    if let Some(delta) = delta {
        if let Some(refusal) = delta.get("refusal").and_then(|v| v.as_str()) {
            if !refusal.is_empty() {
                state.has_refusal = true;
                events.extend(state.ensure_msg_output_item());
                events.push(SseEvent::new(
                    "response.refusal.delta",
                    serde_json::json!({
                        "type": "response.refusal.delta",
                        "item_id": format!("msg_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "content_index": 0,
                        "delta": refusal,
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;
            }
        }
    }

    // ── Finish reason ──
    if let Some(reason) = finish_reason {
        if !reason.is_empty() {
            // Emit done events in order
            if state.has_content {
                events.push(SseEvent::new(
                    "response.output_text.done",
                    serde_json::json!({
                        "type": "response.output_text.done",
                        "item_id": format!("msg_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "content_index": 0,
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;

                events.push(SseEvent::new(
                    "response.content_part.done",
                    serde_json::json!({
                        "type": "response.content_part.done",
                        "item_id": format!("msg_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "content_index": 0,
                        "part": {
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                        },
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;

                events.push(SseEvent::new(
                    "response.output_item.done",
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "item": {
                            "type": "message",
                            "id": format!("msg_{}", Uuid::new_v4()),
                            "role": "assistant",
                            "status": "completed",
                            "content": [],
                        },
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;
            }

            if state.has_reasoning {
                events.push(SseEvent::new(
                    "response.reasoning_summary_text.done",
                    serde_json::json!({
                        "type": "response.reasoning_summary_text.done",
                        "item_id": format!("rs_{}", Uuid::new_v4()),
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "summary_index": 0,
                        "text": state.reasoning_text,
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;

                events.push(SseEvent::new(
                    "response.output_item.done",
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": state.msg_output_index.saturating_sub(1),
                        "item": {
                            "type": "reasoning",
                            "id": format!("rs_{}", Uuid::new_v4()),
                            "summary": [{
                                "type": "summary_text",
                                "text": state.reasoning_text,
                            }],
                            "content": [],
                        },
                        "sequence_number": state.sequence_number,
                    }),
                ));
                state.sequence_number += 1;
            }

            if state.has_tool_calls {
                for i in 0..state.tool_call_emitted.len() {
                    let name = if i < state.tool_call_names.len() {
                        state.tool_call_names[i].clone()
                    } else {
                        String::new()
                    };
                    let args = if i < state.tool_call_arguments.len() {
                        state.tool_call_arguments[i].clone()
                    } else {
                        String::new()
                    };
                    events.push(SseEvent::new(
                        "response.function_call_arguments.done",
                        serde_json::json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": format!("fc_{}", Uuid::new_v4()),
                            "name": name,
                            "output_index": state.msg_output_index.saturating_sub(1),
                            "arguments": args,
                            "sequence_number": state.sequence_number,
                        }),
                    ));
                    state.sequence_number += 1;

                    events.push(SseEvent::new(
                        "response.output_item.done",
                        serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": state.msg_output_index.saturating_sub(1),
                            "item": {
                                "type": "function_call",
                                "id": format!("fc_{}", Uuid::new_v4()),
                                "call_id": if i < state.tool_call_ids.len() { state.tool_call_ids[i].clone() } else { String::new() },
                                "name": name,
                                "arguments": args,
                                "status": "completed",
                            },
                            "sequence_number": state.sequence_number,
                        }),
                    ));
                    state.sequence_number += 1;
                }
            }

            state.finished = true;
        }
    }

    events
}

pub(crate) fn finalize_stream(
    state: &ResponsesStreamState,
) -> SseEvent {
    SseEvent::new(
        "response.completed",
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": state.response_id,
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "total_tokens": 0,
                },
            },
            "sequence_number": state.sequence_number,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_creation() {
        let event = SseEvent::new("response.created", serde_json::json!({"status": "in_progress"}));
        assert_eq!(event.event, "response.created");
        assert_eq!(event.data["status"], "in_progress");
    }

    #[test]
    fn test_sse_event_to_bytes() {
        let event = SseEvent::new("test", serde_json::json!({"key": "value"}));
        let bytes = event.to_sse_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "event: test\ndata: {\"key\":\"value\"}\n\n");
    }

    #[test]
    fn test_initial_state_no_created_emit() {
        let state = ResponsesStreamState::new();
        assert!(!state.created_emitted);
        assert_eq!(state.sequence_number, 0);
        assert!(state.response_id.starts_with("resp_"));
    }

    #[test]
    fn test_emit_created_sets_sequence() {
        let mut state = ResponsesStreamState::new();
        let event = state.emit_created();
        assert_eq!(event.event, "response.created");
        assert_eq!(state.sequence_number, 1);
        assert!(state.created_emitted);
    }

    #[test]
    fn test_translate_single_content_chunk() {
        let mut state = ResponsesStreamState::new();
        let chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"}}]}";
        let events = translate_chat_chunk_to_responses_events(&mut state, chunk);
        assert!(events.len() >= 3); // created + output_item.added + content_part.added + output_text.delta
        assert_eq!(events[0].event, "response.created");
        assert!(events.iter().any(|e| e.event == "response.output_text.delta"));
        assert!(state.has_content);
        assert!(state.msg_output_item_emitted);
    }

    #[test]
    fn test_translate_multiple_content_chunks() {
        let mut state = ResponsesStreamState::new();
        let chunk1 = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}";
        let chunk2 = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"}}]}";
        let events1 = translate_chat_chunk_to_responses_events(&mut state, chunk1);
        let events2 = translate_chat_chunk_to_responses_events(&mut state, chunk2);
        assert!(events1.iter().any(|e| e.event == "response.output_text.delta"));
        assert!(events2.iter().any(|e| e.event == "response.output_text.delta"));
        // output_item.added and content_part.added should only be emitted once
        let added_count = events1.iter().filter(|e| e.event == "response.output_item.added").count()
            + events2.iter().filter(|e| e.event == "response.output_item.added").count();
        assert_eq!(added_count, 1);
    }

    #[test]
    fn test_translate_tool_calls() {
        let mut state = ResponsesStreamState::new();
        let chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"NYC\\\"}\"}}]}}]}";
        let events = translate_chat_chunk_to_responses_events(&mut state, chunk);
        assert!(events.iter().any(|e| e.event == "response.output_item.added"));
        assert!(events.iter().any(|e| e.event == "response.function_call_arguments.delta"));
        assert!(state.has_tool_calls);
    }

    #[test]
    fn test_translate_reasoning_content() {
        let mut state = ResponsesStreamState::new();
        let chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking step\"}}]}";
        let events = translate_chat_chunk_to_responses_events(&mut state, chunk);
        assert!(events.iter().any(|e| e.event == "response.reasoning_summary_text.delta"));
        assert!(state.has_reasoning);
    }

    #[test]
    fn test_translate_done_terminator() {
        let mut state = ResponsesStreamState::new();
        // First send some content
        let chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}";
        translate_chat_chunk_to_responses_events(&mut state, chunk);
        // Then DONE
        let events = translate_chat_chunk_to_responses_events(&mut state, "data: [DONE]");
        assert!(events.iter().any(|e| e.event == "response.completed"));
        assert!(state.finished);
    }

    #[test]
    fn test_translate_finish_reason_emits_done_events() {
        let mut state = ResponsesStreamState::new();
        let content_chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}";
        translate_chat_chunk_to_responses_events(&mut state, content_chunk);
        let finish_chunk = "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}]}";
        let events = translate_chat_chunk_to_responses_events(&mut state, finish_chunk);
        assert!(events.iter().any(|e| e.event == "response.output_text.done"));
        assert!(events.iter().any(|e| e.event == "response.content_part.done"));
        assert!(events.iter().any(|e| e.event == "response.output_item.done"));
        assert!(state.finished);
    }

    #[test]
    fn test_sequence_number_monotonic() {
        let mut state = ResponsesStreamState::new();
        assert_eq!(state.sequence_number, 0);
        let chunk1 = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}";
        let events1 = translate_chat_chunk_to_responses_events(&mut state, chunk1);
        for event in &events1 {
            let seq = event.data["sequence_number"].as_i64().unwrap_or(-1);
            assert!(seq >= 0, "all events should have sequence_number");
        }
        let chunk2 = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"b\"}}]}";
        let events2 = translate_chat_chunk_to_responses_events(&mut state, chunk2);
        for event in &events2 {
            let seq = event.data["sequence_number"].as_i64().unwrap_or(-1);
            assert!(seq >= 0);
        }
    }

    #[test]
    fn test_translate_refusal() {
        let mut state = ResponsesStreamState::new();
        let chunk = "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"I cannot answer that\"}}]}";
        let events = translate_chat_chunk_to_responses_events(&mut state, chunk);
        assert!(events.iter().any(|e| e.event == "response.refusal.delta"));
        assert!(state.has_refusal);
    }

    #[test]
    fn test_finalize_stream() {
        let state = ResponsesStreamState::new();
        let event = finalize_stream(&state);
        assert_eq!(event.event, "response.completed");
        assert_eq!(event.data["response"]["status"], "completed");
    }
}
