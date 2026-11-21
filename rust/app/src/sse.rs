use std::convert::Infallible;
use std::time::Duration;

use async_stream::stream;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::translator::{
    ChatCompletionChunk, ChunkChoice, ChunkDelta, ChunkFunctionCall, ChunkToolCall, UsageInfo,
};

// ============================================================================
// SSE Parser
// ============================================================================

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ServerSentEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

pub struct SseParser {
    buffer: String,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Appends incoming text chunk and extracts all fully formed SSE events.
    pub fn push(&mut self, chunk: &str) -> Vec<ServerSentEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();

        while let Some(pos) = find_event_boundary(&self.buffer) {
            let event_str = &self.buffer[..pos];
            if let Some(ev) = parse_single_event(event_str) {
                events.push(ev);
            }
            // Advance past the boundary (\r\n\r\n or \n\n)
            let skip = if self.buffer[pos..].starts_with("\r\n\r\n") {
                4
            } else if self.buffer[pos..].starts_with("\n\n") {
                2
            } else {
                1
            };
            self.buffer.drain(..pos + skip);
        }

        events
    }

    /// Flush any remaining buffered event
    pub fn flush(&mut self) -> Option<ServerSentEvent> {
        if self.buffer.trim().is_empty() {
            self.buffer.clear();
            return None;
        }
        let event_str = std::mem::take(&mut self.buffer);
        parse_single_event(&event_str)
    }
}

fn find_event_boundary(buffer: &str) -> Option<usize> {
    if let Some(pos) = buffer.find("\n\n") {
        return Some(pos);
    }
    if let Some(pos) = buffer.find("\r\n\r\n") {
        return Some(pos);
    }
    None
}

fn parse_single_event(block: &str) -> Option<ServerSentEvent> {
    let mut event_name: Option<String> = None;
    let mut data_lines = Vec::new();
    let mut id = None;
    let mut retry = None;
    let mut has_content = false;

    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            // Comment or empty line
            continue;
        }
        has_content = true;

        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("id:") {
            id = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("retry:") {
            retry = rest.trim().parse().ok();
        }
    }

    if !has_content && data_lines.is_empty() {
        return None;
    }

    Some(ServerSentEvent {
        event: event_name,
        data: data_lines.join("\n"),
        id,
        retry,
    })
}

// ============================================================================
// Zero-Copy Pass-Through Stream
// ============================================================================

/// Pass-through streamer for OpenAI-compatible providers.
/// Preserves chunks as raw SSE data payloads without JSON serialization overhead.
pub fn passthrough_sse_stream<S, E>(
    byte_stream: S,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = stream! {
        let mut pinned = Box::pin(byte_stream);
        let mut parser = SseParser::new();

        while let Some(chunk_res) = pinned.next().await {
            match chunk_res {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parser.push(&text);
                    for ev in events {
                        let mut event = Event::default().data(ev.data);
                        if let Some(name) = ev.event {
                            event = event.event(name);
                        }
                        if let Some(id) = ev.id {
                            event = event.id(id);
                        }
                        yield Ok(event);
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading passthrough SSE stream: {}", err);
                    break;
                }
            }
        }

        if let Some(remaining) = parser.flush() {
            let mut event = Event::default().data(remaining.data);
            if let Some(name) = remaining.event {
                event = event.event(name);
            }
            yield Ok(event);
        }
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// ============================================================================
// Translated Streams (Claude <-> OpenAI)
// ============================================================================

#[derive(Default)]
pub struct ClaudeToOpenAiStreamState {
    pub message_id: String,
    pub model: String,
    pub tool_call_index: u32,
    pub thinking_block_active: bool,
}

/// Translates a single Claude SSE event into one or more OpenAI format SSE events.
pub fn translate_claude_event_to_openai(
    ev: &ServerSentEvent,
    state: &mut ClaudeToOpenAiStreamState,
) -> Vec<Event> {
    let mut out = Vec::new();
    let event_type = ev.event.as_deref().unwrap_or("");

    let json_data: serde_json::Value = match serde_json::from_str(&ev.data) {
        Ok(v) => v,
        Err(_) => return out,
    };

    match event_type {
        "message_start" => {
            if let Some(msg) = json_data.get("message") {
                state.message_id = msg
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("msg_stream")
                    .to_string();
                state.model = msg
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude")
                    .to_string();

                let chunk = ChatCompletionChunk {
                    id: format!("chatcmpl-{}", state.message_id),
                    object: "chat.completion.chunk".to_string(),
                    created: chrono::Utc::now().timestamp(),
                    model: state.model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: ChunkDelta {
                            role: Some("assistant".to_string()),
                            content: None,
                            reasoning_content: None,
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                if let Ok(serialized) = serde_json::to_string(&chunk) {
                    out.push(Event::default().data(serialized));
                }
            }
        }
        "content_block_start" => {
            if let Some(block) = json_data.get("content_block") {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if block_type == "tool_use" {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let idx = json_data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    state.tool_call_index = idx;

                    let chunk = ChatCompletionChunk {
                        id: format!("chatcmpl-{}", state.message_id),
                        object: "chat.completion.chunk".to_string(),
                        created: chrono::Utc::now().timestamp(),
                        model: state.model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: None,
                                content: None,
                                reasoning_content: None,
                                tool_calls: Some(vec![ChunkToolCall {
                                    index: idx,
                                    id: Some(id.to_string()),
                                    tool_type: Some("function".to_string()),
                                    function: Some(ChunkFunctionCall {
                                        name: Some(name.to_string()),
                                        arguments: Some(String::new()),
                                    }),
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    if let Ok(serialized) = serde_json::to_string(&chunk) {
                        out.push(Event::default().data(serialized));
                    }
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = json_data.get("delta") {
                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if delta_type == "text_delta" {
                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                        let chunk = ChatCompletionChunk {
                            id: format!("chatcmpl-{}", state.message_id),
                            object: "chat.completion.chunk".to_string(),
                            created: chrono::Utc::now().timestamp(),
                            model: state.model.clone(),
                            choices: vec![ChunkChoice {
                                index: 0,
                                delta: ChunkDelta {
                                    role: None,
                                    content: Some(text.to_string()),
                                    reasoning_content: None,
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        };
                        if let Ok(serialized) = serde_json::to_string(&chunk) {
                            out.push(Event::default().data(serialized));
                        }
                    }
                } else if delta_type == "input_json_delta" {
                    if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                        let chunk = ChatCompletionChunk {
                            id: format!("chatcmpl-{}", state.message_id),
                            object: "chat.completion.chunk".to_string(),
                            created: chrono::Utc::now().timestamp(),
                            model: state.model.clone(),
                            choices: vec![ChunkChoice {
                                index: 0,
                                delta: ChunkDelta {
                                    role: None,
                                    content: None,
                                    reasoning_content: None,
                                    tool_calls: Some(vec![ChunkToolCall {
                                        index: state.tool_call_index,
                                        id: None,
                                        tool_type: None,
                                        function: Some(ChunkFunctionCall {
                                            name: None,
                                            arguments: Some(partial.to_string()),
                                        }),
                                    }]),
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        };
                        if let Ok(serialized) = serde_json::to_string(&chunk) {
                            out.push(Event::default().data(serialized));
                        }
                    }
                } else if delta_type == "thinking_delta" {
                    if let Some(thinking) = delta.get("thinking").and_then(|v| v.as_str()) {
                        let chunk = ChatCompletionChunk {
                            id: format!("chatcmpl-{}", state.message_id),
                            object: "chat.completion.chunk".to_string(),
                            created: chrono::Utc::now().timestamp(),
                            model: state.model.clone(),
                            choices: vec![ChunkChoice {
                                index: 0,
                                delta: ChunkDelta {
                                    role: None,
                                    content: None,
                                    reasoning_content: Some(thinking.to_string()),
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        };
                        if let Ok(serialized) = serde_json::to_string(&chunk) {
                            out.push(Event::default().data(serialized));
                        }
                    }
                }
            }
        }
        "message_delta" => {
            let finish_reason = json_data
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|s| s.as_str())
                .map(|r| match r {
                    "end_turn" => "stop",
                    "tool_use" => "tool_calls",
                    "max_tokens" => "length",
                    other => other,
                })
                .unwrap_or("stop");

            let usage = json_data.get("usage").map(|u| {
                let out_tok = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                UsageInfo {
                    prompt_tokens: 0,
                    completion_tokens: out_tok,
                    total_tokens: out_tok,
                    prompt_tokens_details: None,
                }
            });

            let chunk = ChatCompletionChunk {
                id: format!("chatcmpl-{}", state.message_id),
                object: "chat.completion.chunk".to_string(),
                created: chrono::Utc::now().timestamp(),
                model: state.model.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta::default(),
                    finish_reason: Some(finish_reason.to_string()),
                }],
                usage,
            };
            if let Ok(serialized) = serde_json::to_string(&chunk) {
                out.push(Event::default().data(serialized));
            }
        }
        "message_stop" => {
            out.push(Event::default().data("[DONE]"));
        }
        _ => {}
    }

    out
}

/// Translates an incoming Claude SSE byte stream to an OpenAI format SSE stream.
pub fn claude_to_openai_sse_stream<S, E>(
    byte_stream: S,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = stream! {
        let mut pinned = Box::pin(byte_stream);
        let mut parser = SseParser::new();
        let mut state = ClaudeToOpenAiStreamState::default();

        while let Some(chunk_res) = pinned.next().await {
            match chunk_res {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parser.push(&text);
                    for ev in events {
                        let openai_events = translate_claude_event_to_openai(&ev, &mut state);
                        for oe in openai_events {
                            yield Ok(oe);
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading Claude SSE stream: {}", err);
                    break;
                }
            }
        }
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

#[derive(Default)]
pub struct OpenAiToClaudeStreamState {
    pub message_id: String,
    pub model: String,
    pub started: bool,
    pub block_started: bool,
}

/// Translates a single OpenAI SSE chunk into Claude SSE events.
pub fn translate_openai_chunk_to_claude(
    data_str: &str,
    state: &mut OpenAiToClaudeStreamState,
) -> Vec<Event> {
    let mut out = Vec::new();
    let trimmed = data_str.trim();

    if trimmed == "[DONE]" {
        if state.block_started {
            out.push(
                Event::default()
                    .event("content_block_stop")
                    .data(serde_json::json!({"type": "content_block_stop", "index": 0}).to_string()),
            );
            state.block_started = false;
        }
        out.push(
            Event::default()
                .event("message_delta")
                .data(serde_json::json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": 0}
                }).to_string()),
        );
        out.push(
            Event::default()
                .event("message_stop")
                .data(serde_json::json!({"type": "message_stop"}).to_string()),
        );
        return out;
    }

    let chunk: ChatCompletionChunk = match serde_json::from_str(trimmed) {
        Ok(c) => c,
        Err(_) => return out,
    };

    if !state.started {
        state.started = true;
        state.message_id = chunk.id.strip_prefix("chatcmpl-").unwrap_or(&chunk.id).to_string();
        state.model = chunk.model.clone();

        out.push(
            Event::default().event("message_start").data(
                serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": format!("msg_{}", state.message_id),
                        "type": "message",
                        "role": "assistant",
                        "model": state.model,
                        "content": [],
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                })
                .to_string(),
            ),
        );
    }

    if let Some(first_choice) = chunk.choices.first() {
        if let Some(content) = &first_choice.delta.content {
            if !content.is_empty() {
                if !state.block_started {
                    state.block_started = true;
                    out.push(
                        Event::default().event("content_block_start").data(
                            serde_json::json!({
                                "type": "content_block_start",
                                "index": 0,
                                "content_block": {"type": "text", "text": ""}
                            })
                            .to_string(),
                        ),
                    );
                }

                out.push(
                    Event::default().event("content_block_delta").data(
                        serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": content}
                        })
                        .to_string(),
                    ),
                );
            }
        }

        if let Some(finish_reason) = &first_choice.finish_reason {
            if state.block_started {
                out.push(
                    Event::default()
                        .event("content_block_stop")
                        .data(serde_json::json!({"type": "content_block_stop", "index": 0}).to_string()),
                );
                state.block_started = false;
            }

            let stop_reason = match finish_reason.as_str() {
                "stop" => "end_turn",
                "tool_calls" => "tool_use",
                "length" => "max_tokens",
                other => other,
            };

            let out_tokens = chunk.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0);

            out.push(
                Event::default().event("message_delta").data(
                    serde_json::json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                        "usage": {"output_tokens": out_tokens}
                    })
                    .to_string(),
                ),
            );

            out.push(
                Event::default()
                    .event("message_stop")
                    .data(serde_json::json!({"type": "message_stop"}).to_string()),
            );
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_parser_multiple_events() {
        let mut parser = SseParser::new();
        let stream_part1 = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n";
        let events1 = parser.push(stream_part1);
        assert_eq!(events1.len(), 1);
        assert_eq!(events1[0].event.as_deref(), Some("message_start"));
        assert!(events1[0].data.contains("msg_1"));

        // Split chunk across push calls
        let part2 = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\"";
        let events2 = parser.push(part2);
        assert_eq!(events2.len(), 0);

        let part3 = ",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
        let events3 = parser.push(part3);
        assert_eq!(events3.len(), 1);
        assert_eq!(events3[0].event.as_deref(), Some("content_block_delta"));
        assert!(events3[0].data.contains("Hi"));
    }

    #[test]
    fn test_claude_to_openai_chunk_translation() {
        let mut state = ClaudeToOpenAiStreamState::default();

        let ev_start = ServerSentEvent {
            event: Some("message_start".to_string()),
            data: serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": "msg_abc123",
                    "model": "claude-3-5-sonnet"
                }
            })
            .to_string(),
            id: None,
            retry: None,
        };
        let out_start = translate_claude_event_to_openai(&ev_start, &mut state);
        assert_eq!(out_start.len(), 1);
        assert_eq!(state.message_id, "msg_abc123");

        let ev_delta = ServerSentEvent {
            event: Some("content_block_delta".to_string()),
            data: serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": "Hello, world!"
                }
            })
            .to_string(),
            id: None,
            retry: None,
        };
        let out_delta = translate_claude_event_to_openai(&ev_delta, &mut state);
        assert_eq!(out_delta.len(), 1);

        let ev_stop = ServerSentEvent {
            event: Some("message_stop".to_string()),
            data: serde_json::json!({"type": "message_stop"}).to_string(),
            id: None,
            retry: None,
        };
        let out_stop = translate_claude_event_to_openai(&ev_stop, &mut state);
        assert_eq!(out_stop.len(), 1);
    }

    #[test]
    fn test_openai_to_claude_chunk_translation() {
        let mut state = OpenAiToClaudeStreamState::default();

        let chunk = ChatCompletionChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1000,
            model: "gpt-4o".to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: Some("Greetings".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let data = serde_json::to_string(&chunk).unwrap();
        let events = translate_openai_chunk_to_claude(&data, &mut state);
        assert!(events.len() >= 2); // message_start, content_block_start, content_block_delta

        // [DONE]
        let done_events = translate_openai_chunk_to_claude("[DONE]", &mut state);
        assert!(done_events.len() >= 2); // message_delta, message_stop
    }
}
