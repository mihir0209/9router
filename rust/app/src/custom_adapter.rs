use std::convert::Infallible;
use std::time::Duration;

use async_stream::stream;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::sse::SseParser;
use crate::translator::{
    ChatChoice, ChatCompletionChunk, ChatCompletionResponse, ChatResponseMessage, ChunkChoice,
    ChunkDelta, ClaudeBlock, ClaudeMessageResponse, ClaudeUsage, UsageInfo,
};

pub const TAG_OPENAI_COMPATIBLE: &str = "openai-compatible";
pub const TAG_ANTHROPIC_COMPATIBLE: &str = "anthropic-compatible";

/// Schema mapping definition for arbitrary / non-standard provider outputs.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomSchemaMapping {
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub response_text_path: Option<String>,
    #[serde(default)]
    pub prompt_tokens_path: Option<String>,
    #[serde(default)]
    pub completion_tokens_path: Option<String>,
    #[serde(default)]
    pub stream_event: Option<String>,
    #[serde(default)]
    pub stream_text_path: Option<String>,
    #[serde(default)]
    pub stream_done_event: Option<String>,
    #[serde(default)]
    pub stream_done_flag: Option<String>,
    #[serde(default)]
    pub implementation_tags: Vec<String>,
}

/// Robust JSON path extractor navigating dotted keys, array indices (`a.b[0].c`, `[2][1][0]`),
/// and transparently decoding stringified JSON payloads embedded in string properties.
pub fn extract_json_value(root: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    if path.is_empty() {
        return Some(root.clone());
    }
    let mut current = root.clone();
    for part in path.split('.') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // Auto-decode stringified JSON if current is a JSON-encoded string
        if let serde_json::Value::String(s) = &current {
            let trimmed = s.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    current = parsed;
                }
            }
        }

        if let Some(bracket_pos) = part.find('[') {
            let field_name = &part[..bracket_pos];
            if !field_name.is_empty() {
                current = current.get(field_name)?.clone();
                // Auto-decode stringified JSON on intermediate property
                if let serde_json::Value::String(s) = &current {
                    let trimmed = s.trim();
                    if trimmed.starts_with('{') || trimmed.starts_with('[') {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                            current = parsed;
                        }
                    }
                }
            }
            let mut remaining = &part[bracket_pos..];
            while let Some(start) = remaining.find('[') {
                let end = remaining.find(']')?;
                let idx_str = &remaining[start + 1..end];
                let idx: usize = idx_str.parse().ok()?;
                current = current.get(idx)?.clone();
                remaining = &remaining[end + 1..];
            }
        } else {
            current = current.get(part)?.clone();
        }
    }
    Some(current)
}

/// Backwards-compatible reference extractor where no stringified unescaping is required.
pub fn extract_json_path<'a>(
    root: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for part in path.split('.') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(bracket_pos) = part.find('[') {
            let field_name = &part[..bracket_pos];
            if !field_name.is_empty() {
                current = current.get(field_name)?;
            }
            let mut remaining = &part[bracket_pos..];
            while let Some(start) = remaining.find('[') {
                let end = remaining.find(']')?;
                let idx_str = &remaining[start + 1..end];
                let idx: usize = idx_str.parse().ok()?;
                current = current.get(idx)?;
                remaining = &remaining[end + 1..];
            }
        } else {
            current = current.get(part)?;
        }
    }
    Some(current)
}

/// Extracts text string from a JSON value at given path (handles plain strings, numbers, and stringified JSON).
pub fn extract_text_by_path(root: &serde_json::Value, path: &str) -> Option<String> {
    let val = extract_json_value(root, path)?;
    match val {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Object(o) => o
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        _ => None,
    }
}

/// Extracts integer from a JSON value at given path.
pub fn extract_i64_by_path(root: &serde_json::Value, path: &str) -> Option<i64> {
    let val = extract_json_value(root, path)?;
    match val {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Extracts boolean flag from a JSON value at given path.
pub fn extract_bool_by_path(root: &serde_json::Value, path: &str) -> Option<bool> {
    let val = extract_json_value(root, path)?;
    match val {
        serde_json::Value::Bool(b) => Some(b),
        serde_json::Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Transforms an arbitrary custom JSON response into a standard OpenAI ChatCompletionResponse.
pub fn transform_custom_to_openai_response(
    raw: &serde_json::Value,
    mapping: &CustomSchemaMapping,
    model: &str,
) -> Result<ChatCompletionResponse, String> {
    let text_path = mapping.response_text_path.as_deref().unwrap_or("text");
    let content_text = extract_text_by_path(raw, text_path).ok_or_else(|| {
        format!(
            "Failed to extract text at path '{}' from custom response: {}",
            text_path, raw
        )
    })?;

    let prompt_tokens = mapping
        .prompt_tokens_path
        .as_deref()
        .and_then(|p| extract_i64_by_path(raw, p))
        .unwrap_or(10) as u32;

    let completion_tokens = mapping
        .completion_tokens_path
        .as_deref()
        .and_then(|p| extract_i64_by_path(raw, p))
        .unwrap_or(25) as u32;

    Ok(ChatCompletionResponse {
        id: format!("chatcmpl-custom-{}", chrono::Utc::now().timestamp_millis()),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatResponseMessage {
                role: "assistant".to_string(),
                content: Some(content_text),
                tool_calls: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(UsageInfo {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_tokens_details: None,
        }),
    })
}

/// Transforms an arbitrary custom JSON response into a standard Claude ClaudeMessageResponse.
pub fn transform_custom_to_claude_response(
    raw: &serde_json::Value,
    mapping: &CustomSchemaMapping,
    model: &str,
) -> Result<ClaudeMessageResponse, String> {
    let text_path = mapping.response_text_path.as_deref().unwrap_or("text");
    let content_text = extract_text_by_path(raw, text_path).ok_or_else(|| {
        format!(
            "Failed to extract text at path '{}' from custom response: {}",
            text_path, raw
        )
    })?;

    let prompt_tokens = mapping
        .prompt_tokens_path
        .as_deref()
        .and_then(|p| extract_i64_by_path(raw, p))
        .unwrap_or(10) as u32;

    let completion_tokens = mapping
        .completion_tokens_path
        .as_deref()
        .and_then(|p| extract_i64_by_path(raw, p))
        .unwrap_or(25) as u32;

    Ok(ClaudeMessageResponse {
        id: format!("msg-custom-{}", chrono::Utc::now().timestamp_millis()),
        message_type: "message".to_string(),
        role: "assistant".to_string(),
        model: model.to_string(),
        content: vec![ClaudeBlock::Text {
            text: content_text,
            cache_control: None,
        }],
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: ClaudeUsage {
            input_tokens: prompt_tokens,
            output_tokens: completion_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    })
}

/// Converts a custom upstream SSE stream into a standard OpenAI SSE stream.
pub fn custom_sse_to_openai_stream<S, E>(
    byte_stream: S,
    mapping: CustomSchemaMapping,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = stream! {
        let mut pinned = Box::pin(byte_stream);
        let mut parser = SseParser::new();
        let chunk_id = format!("chatcmpl-custom-{}", chrono::Utc::now().timestamp_millis());
        let text_path = mapping.stream_text_path.as_deref().unwrap_or("atom_chunk");

        while let Some(chunk_res) = pinned.next().await {
            match chunk_res {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parser.push(&text);

                    for ev in events {
                        let event_name = ev.event.as_deref().unwrap_or("");

                        // Check termination by event name
                        if let Some(done_ev) = &mapping.stream_done_event {
                            if event_name == done_ev {
                                let final_chunk = ChatCompletionChunk {
                                    id: chunk_id.clone(),
                                    object: "chat.completion.chunk".to_string(),
                                    created: chrono::Utc::now().timestamp(),
                                    model: model.clone(),
                                    choices: vec![ChunkChoice {
                                        index: 0,
                                        delta: ChunkDelta::default(),
                                        finish_reason: Some("stop".to_string()),
                                    }],
                                    usage: None,
                                };
                                if let Ok(serialized) = serde_json::to_string(&final_chunk) {
                                    yield Ok(Event::default().data(serialized));
                                }
                                yield Ok(Event::default().data("[DONE]"));
                                return;
                            }
                        }

                        // Parse data JSON
                        if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                            // Check termination by JSON flag
                            if let Some(done_flag) = &mapping.stream_done_flag {
                                if extract_bool_by_path(&json_data, done_flag) == Some(true) {
                                    let final_chunk = ChatCompletionChunk {
                                        id: chunk_id.clone(),
                                        object: "chat.completion.chunk".to_string(),
                                        created: chrono::Utc::now().timestamp(),
                                        model: model.clone(),
                                        choices: vec![ChunkChoice {
                                            index: 0,
                                            delta: ChunkDelta::default(),
                                            finish_reason: Some("stop".to_string()),
                                        }],
                                        usage: None,
                                    };
                                    if let Ok(serialized) = serde_json::to_string(&final_chunk) {
                                        yield Ok(Event::default().data(serialized));
                                    }
                                    yield Ok(Event::default().data("[DONE]"));
                                    return;
                                }
                            }

                            // Match target stream event if specified
                            if let Some(target_ev) = &mapping.stream_event {
                                if !event_name.is_empty() && event_name != target_ev {
                                    continue;
                                }
                            }

                            // Extract text delta
                            if let Some(delta_text) = extract_text_by_path(&json_data, text_path) {
                                if !delta_text.is_empty() {
                                    let chunk = ChatCompletionChunk {
                                        id: chunk_id.clone(),
                                        object: "chat.completion.chunk".to_string(),
                                        created: chrono::Utc::now().timestamp(),
                                        model: model.clone(),
                                        choices: vec![ChunkChoice {
                                            index: 0,
                                            delta: ChunkDelta {
                                                content: Some(delta_text),
                                                ..Default::default()
                                            },
                                            finish_reason: None,
                                        }],
                                        usage: None,
                                    };
                                    if let Ok(serialized) = serde_json::to_string(&chunk) {
                                        yield Ok(Event::default().data(serialized));
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading custom SSE stream: {}", err);
                    break;
                }
            }
        }

        // Emit final [DONE] if stream closed without explicit done event
        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Converts a custom upstream SSE stream into a standard Claude/Anthropic SSE stream.
pub fn custom_sse_to_claude_stream<S, E>(
    byte_stream: S,
    mapping: CustomSchemaMapping,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = stream! {
        let mut pinned = Box::pin(byte_stream);
        let mut parser = SseParser::new();
        let message_id = format!("msg-custom-{}", chrono::Utc::now().timestamp_millis());
        let text_path = mapping.stream_text_path.as_deref().unwrap_or("atom_chunk");

        // 1. Emit message_start
        let msg_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 12, "output_tokens": 1 }
            }
        });
        yield Ok(Event::default().event("message_start").data(msg_start.to_string()));

        // 2. Emit content_block_start
        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        });
        yield Ok(Event::default().event("content_block_start").data(block_start.to_string()));

        let mut output_tokens = 0;

        'stream_loop: while let Some(chunk_res) = pinned.next().await {
            match chunk_res {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parser.push(&text);

                    for ev in events {
                        let event_name = ev.event.as_deref().unwrap_or("");

                        // Check termination
                        let mut is_done = false;
                        if let Some(done_ev) = &mapping.stream_done_event {
                            if event_name == done_ev {
                                is_done = true;
                            }
                        }

                        if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                            if let Some(done_flag) = &mapping.stream_done_flag {
                                if extract_bool_by_path(&json_data, done_flag) == Some(true) {
                                    is_done = true;
                                }
                            }

                            if is_done {
                                break 'stream_loop;
                            }

                            if let Some(target_ev) = &mapping.stream_event {
                                if !event_name.is_empty() && event_name != target_ev {
                                    continue;
                                }
                            }

                            if let Some(delta_text) = extract_text_by_path(&json_data, text_path) {
                                if !delta_text.is_empty() {
                                    output_tokens += 1;
                                    let delta_event = serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": { "type": "text_delta", "text": delta_text }
                                    });
                                    yield Ok(Event::default().event("content_block_delta").data(delta_event.to_string()));
                                }
                            }
                        }

                        if is_done {
                            break 'stream_loop;
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading custom SSE stream for Claude: {}", err);
                    break 'stream_loop;
                }
            }
        }

        // 3. Emit content_block_stop
        let block_stop = serde_json::json!({
            "type": "content_block_stop",
            "index": 0
        });
        yield Ok(Event::default().event("content_block_stop").data(block_stop.to_string()));

        // 4. Emit message_delta
        let msg_delta = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "output_tokens": output_tokens }
        });
        yield Ok(Event::default().event("message_delta").data(msg_delta.to_string()));

        // 5. Emit message_stop
        let msg_stop = serde_json::json!({
            "type": "message_stop"
        });
        yield Ok(Event::default().event("message_stop").data(msg_stop.to_string()));
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Synthesizes an OpenAI SSE stream from a complete ChatCompletionResponse (for non-streaming custom providers).
pub fn synthesize_openai_stream(
    completion: ChatCompletionResponse,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mapped = stream! {
        let chunk_id = completion.id.clone();
        let content = completion.choices.first().and_then(|c| c.message.content.clone()).unwrap_or_default();

        let chunk1 = ChatCompletionChunk {
            id: chunk_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: completion.created,
            model: model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: Some(content),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        if let Ok(serialized) = serde_json::to_string(&chunk1) {
            yield Ok(Event::default().data(serialized));
        }

        let chunk2 = ChatCompletionChunk {
            id: chunk_id,
            object: "chat.completion.chunk".to_string(),
            created: completion.created,
            model,
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta::default(),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        };
        if let Ok(serialized) = serde_json::to_string(&chunk2) {
            yield Ok(Event::default().data(serialized));
        }

        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(mapped)
}

/// Synthesizes an Anthropic/Claude SSE stream from a complete ClaudeMessageResponse (for non-streaming custom providers).
pub fn synthesize_claude_stream(
    response: ClaudeMessageResponse,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mapped = stream! {
        let message_id = response.id.clone();
        let text = response.content.iter().filter_map(|b| match b {
            ClaudeBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect::<Vec<_>>().join("\n");
        let output_tokens = response.usage.output_tokens;

        let msg_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": response.usage.input_tokens, "output_tokens": 1 }
            }
        });
        yield Ok(Event::default().event("message_start").data(msg_start.to_string()));

        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" }
        });
        yield Ok(Event::default().event("content_block_start").data(block_start.to_string()));

        let delta_event = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": text }
        });
        yield Ok(Event::default().event("content_block_delta").data(delta_event.to_string()));

        let block_stop = serde_json::json!({
            "type": "content_block_stop",
            "index": 0
        });
        yield Ok(Event::default().event("content_block_stop").data(block_stop.to_string()));

        let msg_delta = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "output_tokens": output_tokens }
        });
        yield Ok(Event::default().event("message_delta").data(msg_delta.to_string()));

        let msg_stop = serde_json::json!({
            "type": "message_stop"
        });
        yield Ok(Event::default().event("message_stop").data(msg_stop.to_string()));
    };

    Sse::new(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_deeply_nested_worst_schema() {
        let payload = serde_json::json!({
            "telemetry_protocol": "ABYSS_V666",
            "hyper_matrix": {
                "capsules": [
                    {
                        "quantum_strata": {
                            "glyph_payload": {
                                "unfolded_utterance": "Quantum Wormhole Verified"
                            },
                            "chronos_id": "alien-seq-98124"
                        }
                    }
                ],
                "resource_quanta": {
                    "ingress_flux": 20,
                    "egress_flux": 186
                }
            },
            "singularity_status": "ENTROPY_OK"
        });

        let extracted = extract_text_by_path(
            &payload,
            "hyper_matrix.capsules[0].quantum_strata.glyph_payload.unfolded_utterance",
        );
        assert_eq!(extracted.as_deref(), Some("Quantum Wormhole Verified"));

        let ingress = extract_i64_by_path(&payload, "hyper_matrix.resource_quanta.ingress_flux");
        assert_eq!(ingress, Some(20));

        let egress = extract_i64_by_path(&payload, "hyper_matrix.resource_quanta.egress_flux");
        assert_eq!(egress, Some(186));
    }

    #[test]
    fn test_extract_flattened_delimited_worst_schema() {
        let payload = serde_json::json!({
            "SYS:::PROTOCOL:::SPEC": "FLAT_DELIMITED_V1",
            "RESOURCE:::INBOUND:::QUANTA": 14,
            "RESOURCE:::OUTBOUND:::QUANTA": 56,
            "PAYLOAD:::RAW:::UTTERANCE": "Flat Delimited Verified Successfully"
        });

        let extracted = extract_text_by_path(&payload, "PAYLOAD:::RAW:::UTTERANCE");
        assert_eq!(extracted.as_deref(), Some("Flat Delimited Verified Successfully"));

        let tokens = extract_i64_by_path(&payload, "RESOURCE:::OUTBOUND:::QUANTA");
        assert_eq!(tokens, Some(56));
    }

    #[test]
    fn test_extract_positional_tuple_worst_schema() {
        let payload = serde_json::json!([
            "MAGIC_ARRAY_V9",
            200,
            [
                ["stats", 18, 64],
                ["content", "text_plain", ["stream_atom", "Positional Tuple AST Verified"]],
                ["digest", "0xdeadbeef"]
            ],
            "EOX"
        ]);

        let extracted = extract_text_by_path(&payload, "[2][1][2][1]");
        assert_eq!(extracted.as_deref(), Some("Positional Tuple AST Verified"));

        let tokens = extract_i64_by_path(&payload, "[2][0][2]");
        assert_eq!(tokens, Some(64));
    }

    #[test]
    fn test_extract_stringified_nested_blob_worst_schema() {
        let payload = serde_json::json!({
            "status_code": 200,
            "envelope_type": "STRINGIFIED_NESTED_PAYLOAD",
            "raw_stringified_blob": "{\"target_utterance\":{\"text_block\":\"Double Serialized Escaped String Verified\"},\"inbound_units\":18,\"outbound_units\":64}"
        });

        let extracted = extract_text_by_path(&payload, "raw_stringified_blob.target_utterance.text_block");
        assert_eq!(extracted.as_deref(), Some("Double Serialized Escaped String Verified"));

        let tokens = extract_i64_by_path(&payload, "raw_stringified_blob.outbound_units");
        assert_eq!(tokens, Some(64));
    }

    #[test]
    fn test_custom_to_openai_and_claude_transforms() {
        let payload = serde_json::json!({
            "hyper_matrix": {
                "capsules": [{
                    "quantum_strata": {
                        "glyph_payload": {
                            "unfolded_utterance": "Hello From Deep Space"
                        }
                    }
                }],
                "resource_quanta": { "ingress_flux": 15, "egress_flux": 40 }
            }
        });

        let mapping = CustomSchemaMapping {
            response_text_path: Some(
                "hyper_matrix.capsules[0].quantum_strata.glyph_payload.unfolded_utterance"
                    .to_string(),
            ),
            prompt_tokens_path: Some("hyper_matrix.resource_quanta.ingress_flux".to_string()),
            completion_tokens_path: Some("hyper_matrix.resource_quanta.egress_flux".to_string()),
            ..Default::default()
        };

        let openai_resp =
            transform_custom_to_openai_response(&payload, &mapping, "worst/k2-horizon").unwrap();
        assert_eq!(
            openai_resp.choices[0].message.content.as_deref(),
            Some("Hello From Deep Space")
        );
        assert_eq!(openai_resp.usage.unwrap().prompt_tokens, 15);

        let claude_resp =
            transform_custom_to_claude_response(&payload, &mapping, "worst/k2-horizon").unwrap();
        if let ClaudeBlock::Text { text, .. } = &claude_resp.content[0] {
            assert_eq!(text, "Hello From Deep Space");
        } else {
            panic!("Expected text block");
        }
        assert_eq!(claude_resp.usage.input_tokens, 15);
    }
}
