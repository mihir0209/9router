use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{ApiKey, ProviderConnection};
use crate::proxy::{self, UpstreamConnection};
use crate::routes::{
    authenticate, estimate_chat_tokens, record_completion_usage_and_details,
    resolve_usage_buffer_tokens, wrap_byte_stream_with_usage, AppState,
};
use crate::sse::SseParser;
use crate::translator::{
    claude_to_openai_response, openai_to_claude_request, ChatCompletionRequest,
    ChatCompletionResponse, ChatFunction, ChatMessage, ChatTool, ChatToolCall,
    ClaudeMessageResponse, ContentPart, FunctionCall, ImageUrl, MessageContent,
};

const MAX_RESPONSES_CALL_ID_LEN: usize = 64;
static CALL_ID_SEQ: AtomicU64 = AtomicU64::new(1);
static RESP_ID_SEQ: AtomicU64 = AtomicU64::new(1);

const CHAT_ONLY_PARAMETERS: &[&str] = &[
    "audio",
    "frequency_penalty",
    "logit_bias",
    "modalities",
    "n",
    "prediction",
    "presence_penalty",
    "seed",
    "stop",
];

// ============================================================================
// OpenAI Responses API Schemas
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning_summary: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub store: Option<bool>,
    #[serde(default)]
    pub include: Option<Vec<String>>,
    #[serde(default)]
    pub text: Option<Value>,
    #[serde(default)]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub verbosity: Option<String>,
    #[serde(default)]
    pub logprobs: Option<bool>,
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
    #[serde(default)]
    pub truncation: Option<String>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesApiResponse {
    pub id: String,
    pub object: String,
    pub created_at: i64,
    pub status: String,
    pub model: String,
    pub output: Vec<Value>,
    pub output_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
    pub background: bool,
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResponsesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<InputTokensDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct InputTokensDetails {
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ResponsesConversionContext {
    pub model: String,
    pub custom_tool_names: HashSet<String>,
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponsesSseEvent {
    pub event: &'static str,
    pub data: Value,
}

// ============================================================================
// Request Normalization & Conversion Helpers
// ============================================================================

fn next_response_id() -> String {
    let ts = chrono::Utc::now().timestamp_millis();
    let seq = RESP_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("resp_{}_{}", ts, seq)
}

pub fn clamp_responses_call_id(id: Option<&str>) -> String {
    match id {
        Some(raw) if !raw.is_empty() => {
            if raw.len() > MAX_RESPONSES_CALL_ID_LEN {
                raw[..MAX_RESPONSES_CALL_ID_LEN].to_string()
            } else {
                raw.to_string()
            }
        }
        _ => {
            let ts = chrono::Utc::now().timestamp_millis();
            let seq = CALL_ID_SEQ.fetch_add(1, Ordering::Relaxed);
            format!("call_{}_{}", ts, seq)
        }
    }
}

pub fn coerce_responses_arguments(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "{}".to_string(),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return "{}".to_string();
            }
            if serde_json::from_str::<Value>(s).is_ok() {
                s.clone()
            } else {
                "{}".to_string()
            }
        }
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    }
}

pub fn coerce_responses_output(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|c| {
                if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                    t.to_string()
                } else {
                    serde_json::to_string(c).unwrap_or_default()
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn to_responses_text_format(response_format: &Value) -> Option<Value> {
    let obj = response_format.as_object()?;
    if obj.get("type").and_then(|v| v.as_str()) != Some("json_schema") {
        return Some(response_format.clone());
    }
    let mut out = serde_json::Map::new();
    out.insert("type".to_string(), json!("json_schema"));
    if let Some(schema) = obj.get("json_schema").and_then(|v| v.as_object()) {
        for (k, v) in schema {
            out.insert(k.clone(), v.clone());
        }
    }
    Some(Value::Object(out))
}

/// Canonicalize legacy Chat Completions aliases to the current Responses API shape.
pub fn normalize_responses_request(body: &mut Value) {
    let Some(map) = body.as_object_mut() else {
        return;
    };

    if !map.contains_key("max_output_tokens") || map.get("max_output_tokens") == Some(&Value::Null) {
        let fallback = map
            .get("max_completion_tokens")
            .cloned()
            .filter(|v| !v.is_null())
            .or_else(|| map.get("max_tokens").cloned().filter(|v| !v.is_null()));
        if let Some(v) = fallback {
            map.insert("max_output_tokens".to_string(), v);
        } else {
            map.remove("max_output_tokens");
        }
    }
    map.remove("max_completion_tokens");
    map.remove("max_tokens");

    let effort = map.remove("reasoning_effort");
    let summary = map.remove("reasoning_summary");
    if effort.is_some() || summary.is_some() {
        let mut reasoning = map
            .get("reasoning")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if !reasoning.contains_key("effort") {
            if let Some(e) = effort.filter(|v| !v.is_null()) {
                reasoning.insert("effort".to_string(), e);
            }
        }
        if !reasoning.contains_key("summary") {
            if let Some(s) = summary.filter(|v| !v.is_null()) {
                reasoning.insert("summary".to_string(), s);
            }
        }
        if !reasoning.is_empty() {
            map.insert("reasoning".to_string(), Value::Object(reasoning));
        }
    }

    let verbosity = map.remove("verbosity");
    let response_format = map.remove("response_format");
    if verbosity.is_some() || response_format.is_some() {
        let mut text = map
            .get("text")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if !text.contains_key("verbosity") {
            if let Some(v) = verbosity.filter(|v| !v.is_null()) {
                text.insert("verbosity".to_string(), v);
            }
        }
        if !text.contains_key("format") {
            if let Some(rf) = response_format.as_ref().and_then(to_responses_text_format) {
                text.insert("format".to_string(), rf);
            }
        }
        if !text.is_empty() {
            map.insert("text".to_string(), Value::Object(text));
        }
    }

    if map.get("logprobs") == Some(&Value::Bool(true)) {
        let mut include = map
            .get("include")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let target = Value::String("message.output_text.logprobs".to_string());
        if !include.contains(&target) {
            include.push(target);
        }
        map.insert("include".to_string(), Value::Array(include));
    }
    map.remove("logprobs");

    if let Some(tc) = map.get_mut("tool_choice") {
        if tc.get("type").and_then(|v| v.as_str()) == Some("function") {
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(str::to_string)
            {
                *tc = json!({ "type": "function", "name": name });
            }
        }
    }

    for param in CHAT_ONLY_PARAMETERS {
        map.remove(*param);
    }

    if let Some(so) = map.get("stream_options").and_then(|v| v.as_object()) {
        if let Some(inc) = so.get("include_obfuscation").cloned() {
            map.insert(
                "stream_options".to_string(),
                json!({ "include_obfuscation": inc }),
            );
        } else {
            map.remove("stream_options");
        }
    }
}

/// Normalize Responses API `input` (string or array) to an array of items.
pub fn normalize_responses_input(input: &Value) -> Option<Vec<Value>> {
    match input {
        Value::String(s) => {
            let text = if s.trim().is_empty() { "..." } else { s.as_str() };
            Some(vec![json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            })])
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                Some(vec![json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "..." }]
                })])
            } else {
                Some(arr.clone())
            }
        }
        _ => None,
    }
}

fn normalize_tool_parameters(params: Option<&Value>) -> Value {
    match params {
        None | Some(Value::Null) => json!({ "type": "object", "properties": {} }),
        Some(Value::Object(obj)) => {
            if obj.get("type").and_then(|v| v.as_str()) == Some("object")
                && !obj.contains_key("properties")
            {
                let mut cloned = obj.clone();
                cloned.insert("properties".to_string(), json!({}));
                Value::Object(cloned)
            } else {
                Value::Object(obj.clone())
            }
        }
        Some(other) => other.clone(),
    }
}

fn convert_message_content(raw: Option<&Value>) -> Option<MessageContent> {
    let val = raw?;
    match val {
        Value::String(s) => Some(MessageContent::Text(s.clone())),
        Value::Array(arr) => {
            let mut parts = Vec::with_capacity(arr.len());
            for item in arr {
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_type {
                    "input_text" | "output_text" | "text" => {
                        let text = item
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        parts.push(ContentPart {
                            part_type: "text".to_string(),
                            text: Some(text),
                            image_url: None,
                        });
                    }
                    "input_image" | "image_url" => {
                        let url = item
                            .get("image_url")
                            .and_then(|v| {
                                v.as_str()
                                    .map(str::to_string)
                                    .or_else(|| v.get("url").and_then(|u| u.as_str()).map(str::to_string))
                            })
                            .or_else(|| item.get("file_id").and_then(|v| v.as_str()).map(str::to_string))
                            .unwrap_or_default();
                        let detail = item
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .or_else(|| Some("auto".to_string()));
                        parts.push(ContentPart {
                            part_type: "image_url".to_string(),
                            text: None,
                            image_url: Some(ImageUrl { url, detail }),
                        });
                    }
                    _ => {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                            parts.push(ContentPart {
                                part_type: "text".to_string(),
                                text: Some(t.to_string()),
                                image_url: None,
                            });
                        }
                    }
                }
            }
            Some(MessageContent::Parts(parts))
        }
        Value::Null => None,
        other => Some(MessageContent::Text(other.to_string())),
    }
}

fn convert_responses_tool_choice(tc: &Value) -> Value {
    if tc.get("type").and_then(|v| v.as_str()) == Some("function") {
        if let Some(name) = tc.get("name").and_then(|v| v.as_str()) {
            return json!({
                "type": "function",
                "function": { "name": name }
            });
        }
    }
    tc.clone()
}

fn convert_text_format_to_response_format(text_val: &Value) -> Option<Value> {
    let format = text_val.get("format")?;
    let ftype = format.get("type").and_then(|v| v.as_str())?;
    if ftype == "json_schema" && format.get("json_schema").is_none() {
        let mut inner = format.as_object()?.clone();
        inner.remove("type");
        Some(json!({
            "type": "json_schema",
            "json_schema": Value::Object(inner)
        }))
    } else {
        Some(format.clone())
    }
}

/// Convert an OpenAI Responses API request into an internal `ChatCompletionRequest`.
pub fn responses_to_chat_request(
    req: &ResponsesRequest,
) -> Result<(ChatCompletionRequest, ResponsesConversionContext), String> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut custom_tool_names: HashSet<String> = HashSet::new();
    let mut additional_tools: Vec<Value> = Vec::new();

    if let Some(ref instr) = req.instructions {
        if !instr.is_empty() {
            messages.push(ChatMessage::system(instr.clone()));
        }
    }

    if let Some(ref prev_id) = req.previous_response_id {
        if !prev_id.trim().is_empty() {
            // Preserve previous_response_id hint in metadata if needed; upstream stateless
            // chat completions cannot resolve server-stored IDs directly, so we record it in ctx.
        }
    }

    if let Some(ref raw_input) = req.input {
        let items = normalize_responses_input(raw_input)
            .ok_or_else(|| "Invalid 'input' field: expected string or array".to_string())?;

        let mut current_assistant_msg: Option<ChatMessage> = None;

        for item in items {
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("role").and_then(|_| Some("message")));

            match item_type {
                Some("message") => {
                    if let Some(asst) = current_assistant_msg.take() {
                        messages.push(asst);
                    }
                    let role = item
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("user")
                        .to_string();
                    let content = convert_message_content(item.get("content"));
                    messages.push(ChatMessage {
                        role,
                        content,
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                Some("function_call") | Some("custom_tool_call") => {
                    let Some(name) = item.get("name").and_then(|v| v.as_str()).map(str::trim) else {
                        continue;
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let is_custom = item_type == Some("custom_tool_call");
                    if is_custom {
                        custom_tool_names.insert(name.to_string());
                    }

                    let call_id =
                        clamp_responses_call_id(item.get("call_id").and_then(|v| v.as_str()));
                    let arguments = if is_custom {
                        let raw_in = item
                            .get("input")
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| serde_json::to_string(v).unwrap_or_default())
                            })
                            .unwrap_or_default();
                        json!({ "input": raw_in }).to_string()
                    } else {
                        coerce_responses_arguments(item.get("arguments"))
                    };

                    let asst = current_assistant_msg.get_or_insert_with(|| ChatMessage {
                        role: "assistant".to_string(),
                        content: None,
                        name: None,
                        tool_calls: Some(Vec::new()),
                        tool_call_id: None,
                    });
                    asst.tool_calls.get_or_insert_with(Vec::new).push(ChatToolCall {
                        id: call_id,
                        tool_type: "function".to_string(),
                        function: FunctionCall {
                            name: name.to_string(),
                            arguments,
                        },
                    });
                }
                Some("function_call_output") | Some("custom_tool_call_output") => {
                    if let Some(asst) = current_assistant_msg.take() {
                        messages.push(asst);
                    }
                    let call_id =
                        clamp_responses_call_id(item.get("call_id").and_then(|v| v.as_str()));
                    let output_str = coerce_responses_output(item.get("output"));
                    messages.push(ChatMessage::tool(output_str, call_id));
                }
                Some("additional_tools") => {
                    if let Some(arr) = item.get("tools").and_then(|v| v.as_array()) {
                        additional_tools.extend(arr.iter().cloned());
                    }
                }
                Some("reasoning") | _ => {
                    // Reasoning items and unknown display-only items are skipped.
                }
            }
        }

        if let Some(asst) = current_assistant_msg.take() {
            messages.push(asst);
        }
    } else if messages.is_empty() {
        // Ensure at least one message if neither instructions nor input was supplied
        messages.push(ChatMessage::user("..."));
    }

    let mut raw_tools: Vec<Value> = req.tools.clone().unwrap_or_default();
    raw_tools.extend(additional_tools);

    let converted_tools: Vec<ChatTool> = raw_tools
        .into_iter()
        .filter_map(|tool| {
            if let Some(func) = tool.get("function") {
                let name = func.get("name")?.as_str()?.trim();
                if name.is_empty() {
                    return None;
                }
                return Some(ChatTool {
                    tool_type: "function".to_string(),
                    function: ChatFunction {
                        name: name.to_string(),
                        description: func
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        parameters: func.get("parameters").cloned(),
                    },
                });
            }

            let name = tool.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let ttype = tool.get("type").and_then(|v| v.as_str()).unwrap_or("function");
            if ttype == "custom" {
                custom_tool_names.insert(name.to_string());
                let desc = tool
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let syntax = tool
                    .get("format")
                    .and_then(|f| f.get("syntax"))
                    .and_then(|v| v.as_str());
                let def = tool
                    .get("format")
                    .and_then(|f| f.get("definition"))
                    .and_then(|v| v.as_str());
                let mut desc_parts = Vec::new();
                if !desc.is_empty() {
                    desc_parts.push(desc.to_string());
                }
                if let Some(s) = syntax {
                    desc_parts.push(s.to_string());
                }
                if let Some(d) = def {
                    desc_parts.push(d.to_string());
                }
                return Some(ChatTool {
                    tool_type: "function".to_string(),
                    function: ChatFunction {
                        name: name.to_string(),
                        description: Some(desc_parts.join("\n\n")),
                        parameters: Some(json!({
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string",
                                    "description": "Raw freeform input for this custom tool"
                                }
                            },
                            "required": ["input"],
                            "additionalProperties": false
                        })),
                    },
                });
            }

            Some(ChatTool {
                tool_type: "function".to_string(),
                function: ChatFunction {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    parameters: Some(normalize_tool_parameters(tool.get("parameters"))),
                },
            })
        })
        .collect();

    let max_tokens = req
        .max_output_tokens
        .or(req.max_completion_tokens)
        .or(req.max_tokens);

    let response_format = req
        .response_format
        .clone()
        .or_else(|| req.text.as_ref().and_then(convert_text_format_to_response_format));

    let chat_req = ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        top_p: req.top_p,
        n: None,
        stream: req.stream,
        stop: None,
        max_tokens,
        max_completion_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: req.user.clone(),
        tools: if converted_tools.is_empty() {
            None
        } else {
            Some(converted_tools)
        },
        tool_choice: req.tool_choice.as_ref().map(convert_responses_tool_choice),
        response_format,
        metadata: req.metadata.clone(),
    };

    let ctx = ResponsesConversionContext {
        model: req.model.clone(),
        custom_tool_names,
        previous_response_id: req.previous_response_id.clone(),
    };

    Ok((chat_req, ctx))
}

// ============================================================================
// Context Compaction (/v1/responses/compact)
// ============================================================================

fn extract_item_summary_text(item: &Value) -> String {
    let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match itype {
        "function_call" | "custom_tool_call" => {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
            format!("called `{}`", name)
        }
        "function_call_output" | "custom_tool_call_output" => {
            let out = coerce_responses_output(item.get("output"));
            let snippet: String = out.chars().take(120).collect();
            format!("tool output: {}", snippet.replace('\n', " "))
        }
        _ => {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let text = match item.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            let snippet: String = text.chars().take(160).collect();
            format!("{}: {}", role, snippet.replace('\n', " "))
        }
    }
}

/// Compact Responses API `input` items cleanly:
/// 1. Drops display-only `reasoning` items.
/// 2. Truncates oversized `function_call_output` / `custom_tool_call_output` payloads.
/// 3. If items exceed `max_items`, summarizes older middle turns into a concise context
///    message while preserving leading system/developer messages and the most recent
///    `keep_recent` items (adjusting boundary so `function_call` / `function_call_output`
///    pairs are never split).
pub fn compact_responses_input(
    items: &[Value],
    max_items: usize,
    max_output_chars: usize,
) -> Vec<Value> {
    let mut filtered: Vec<Value> = Vec::with_capacity(items.len());

    for item in items {
        let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if itype == "reasoning" {
            continue;
        }
        let mut cloned = item.clone();
        if itype == "function_call_output" || itype == "custom_tool_call_output" {
            let out = coerce_responses_output(cloned.get("output"));
            if out.len() > max_output_chars {
                let half = max_output_chars / 2;
                let head: String = out.chars().take(half).collect();
                let tail: String = out
                    .chars()
                    .rev()
                    .take(half)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let compacted = format!(
                    "{}\n... [compacted {} chars] ...\n{}",
                    head,
                    out.len().saturating_sub(max_output_chars),
                    tail
                );
                if let Some(obj) = cloned.as_object_mut() {
                    obj.insert("output".to_string(), Value::String(compacted));
                }
            }
        }
        filtered.push(cloned);
    }

    if filtered.len() <= max_items || max_items < 4 {
        return filtered;
    }

    // Preserve leading system/developer messages
    let mut head_end = 0;
    while head_end < filtered.len() {
        let role = filtered[head_end].get("role").and_then(|v| v.as_str());
        if matches!(role, Some("system") | Some("developer")) {
            head_end += 1;
        } else {
            break;
        }
    }

    let keep_recent = (max_items.saturating_sub(head_end)).max(2);
    let mut tail_start = filtered.len().saturating_sub(keep_recent);
    if tail_start <= head_end {
        return filtered;
    }

    // Never start the kept tail on an orphaned function_call_output
    while tail_start > head_end {
        let t = filtered[tail_start]
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if t == "function_call_output" || t == "custom_tool_call_output" {
            tail_start -= 1;
        } else {
            break;
        }
    }

    if tail_start <= head_end + 1 {
        return filtered;
    }

    let middle = &filtered[head_end..tail_start];
    let mut summary_lines = Vec::with_capacity(middle.len().min(12));
    for item in middle.iter().take(10) {
        let line = extract_item_summary_text(item);
        if !line.trim().is_empty() {
            summary_lines.push(format!("- {}", line));
        }
    }
    if middle.len() > 10 {
        summary_lines.push(format!("- ... ({} more earlier items)", middle.len() - 10));
    }

    let summary_msg = json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": format!("[Compacted earlier conversation ({} items)]:\n{}", middle.len(), summary_lines.join("\n"))
        }]
    });

    let mut out = Vec::with_capacity(head_end + 1 + (filtered.len() - tail_start));
    out.extend_from_slice(&filtered[..head_end]);
    out.push(summary_msg);
    out.extend_from_slice(&filtered[tail_start..]);
    out
}

/// Compacts a `ResponsesRequest` in place and returns approximate bytes saved.
pub fn compact_responses_request(req: &mut ResponsesRequest) -> usize {
    let Some(ref mut input) = req.input else {
        return 0;
    };
    let Some(items) = normalize_responses_input(input) else {
        return 0;
    };
    let before_bytes = serde_json::to_vec(&items).map(|b| b.len()).unwrap_or(0);
    let compacted = compact_responses_input(&items, 16, 4000);
    let after_bytes = serde_json::to_vec(&compacted).map(|b| b.len()).unwrap_or(0);
    *input = Value::Array(compacted);
    before_bytes.saturating_sub(after_bytes)
}

// ============================================================================
// Non-Streaming Response Translation (Chat Completions -> Responses API)
// ============================================================================

fn extract_custom_tool_input(args: &str) -> String {
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(args) {
        if let Some(Value::String(s)) = map.get("input") {
            return s.clone();
        }
    }
    args.to_string()
}

pub fn chat_to_responses_response(
    resp: &ChatCompletionResponse,
    ctx: &ResponsesConversionContext,
    buffer_tokens: u64,
) -> ResponsesApiResponse {
    let resp_id = if resp.id.starts_with("resp_") {
        resp.id.clone()
    } else if resp.id.is_empty() {
        next_response_id()
    } else {
        format!("resp_{}", resp.id)
    };

    let mut output: Vec<Value> = Vec::new();
    let mut output_text_parts: Vec<String> = Vec::new();

    if let Some(choice) = resp.choices.first() {
        if let Some(ref content) = choice.message.content {
            if !content.is_empty() || choice.message.tool_calls.is_none() {
                output_text_parts.push(content.clone());
                output.push(json!({
                    "id": format!("msg_{}_0", resp_id),
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": content,
                        "annotations": [],
                        "logprobs": []
                    }]
                }));
            }
        } else if choice.message.tool_calls.is_none() {
            output.push(json!({
                "id": format!("msg_{}_0", resp_id),
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "",
                    "annotations": [],
                    "logprobs": []
                }]
            }));
        }

        if let Some(ref tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let is_custom = ctx.custom_tool_names.contains(&tc.function.name);
                if is_custom {
                    let input_str = extract_custom_tool_input(&tc.function.arguments);
                    output.push(json!({
                        "id": format!("ctc_{}", tc.id),
                        "type": "custom_tool_call",
                        "status": "completed",
                        "call_id": tc.id,
                        "name": tc.function.name,
                        "input": input_str
                    }));
                } else {
                    output.push(json!({
                        "id": format!("fc_{}", tc.id),
                        "type": "function_call",
                        "status": "completed",
                        "call_id": tc.id,
                        "name": tc.function.name,
                        "arguments": tc.function.arguments
                    }));
                }
            }
        }
    }

    let usage = resp.usage.as_ref().map(|u| {
        let out_tokens = u.completion_tokens.saturating_add(buffer_tokens as u32);
        let cached = u
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .map(|c| InputTokensDetails {
                cached_tokens: c as u32,
            });
        ResponsesUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: out_tokens,
            total_tokens: u.prompt_tokens.saturating_add(out_tokens),
            input_tokens_details: cached,
            output_tokens_details: None,
        }
    });

    ResponsesApiResponse {
        id: resp_id,
        object: "response".to_string(),
        created_at: resp.created,
        status: "completed".to_string(),
        model: if resp.model.is_empty() {
            ctx.model.clone()
        } else {
            resp.model.clone()
        },
        output,
        output_text: output_text_parts.join(""),
        usage,
        background: false,
        error: None,
        previous_response_id: ctx.previous_response_id.clone(),
    }
}

pub fn chat_json_to_responses_response(
    val: &Value,
    ctx: &ResponsesConversionContext,
    buffer_tokens: u64,
) -> Result<ResponsesApiResponse, String> {
    // If upstream already returned a Responses API object, pass through with normalization
    if val.get("object").and_then(|v| v.as_str()) == Some("response") && val.get("output").is_some()
    {
        let mut resp: ResponsesApiResponse =
            serde_json::from_value(val.clone()).map_err(|e| e.to_string())?;
        if resp.output_text.is_empty() {
            resp.output_text = extract_output_text_from_items(&resp.output);
        }
        return Ok(resp);
    }

    let chat_resp: ChatCompletionResponse =
        serde_json::from_value(val.clone()).map_err(|e| e.to_string())?;
    Ok(chat_to_responses_response(&chat_resp, ctx, buffer_tokens))
}

fn extract_output_text_from_items(items: &[Value]) -> String {
    let mut out = String::new();
    for item in items {
        if item.get("type").and_then(|v| v.as_str()) == Some("message") {
            if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    out
}

// ============================================================================
// Streaming SSE Translation (Chat Completions Chunks -> Responses API SSE)
// ============================================================================

#[derive(Debug, Default)]
pub struct ResponsesStreamState {
    pub started: bool,
    pub completed_sent: bool,
    pub completion_pending: bool,
    pub response_id: String,
    pub model: String,
    pub created: i64,
    pub seq: u64,
    pub in_thinking: bool,
    pub reasoning_id: Option<String>,
    pub reasoning_index: u32,
    pub reasoning_done: bool,
    pub reasoning_buf: String,
    pub msg_item_added: HashMap<u32, bool>,
    pub msg_content_added: HashMap<u32, bool>,
    pub msg_item_done: HashMap<u32, bool>,
    pub msg_text_buf: HashMap<u32, String>,
    pub func_names: HashMap<u32, String>,
    pub func_call_ids: HashMap<u32, String>,
    pub func_item_added: HashMap<u32, bool>,
    pub func_item_done: HashMap<u32, bool>,
    pub func_args_buf: HashMap<u32, String>,
    pub completed_output_items: BTreeMap<u32, Value>,
    pub responses_usage: Option<Value>,
    pub custom_tool_names: HashSet<String>,
}

impl ResponsesStreamState {
    pub fn new(model: impl Into<String>, custom_tool_names: HashSet<String>) -> Self {
        Self {
            response_id: next_response_id(),
            model: model.into(),
            created: chrono::Utc::now().timestamp(),
            custom_tool_names,
            ..Default::default()
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn emit(&mut self, event: &'static str, mut data: Value) -> ResponsesSseEvent {
        let seq = self.next_seq();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("sequence_number".to_string(), json!(seq));
        }
        ResponsesSseEvent { event, data }
    }
}

fn to_responses_usage_value(usage: &Value) -> Option<Value> {
    let obj = usage.as_object()?;
    let input_tokens = obj
        .get("input_tokens")
        .or_else(|| obj.get("prompt_tokens"))
        .and_then(|v| v.as_u64())?;
    let output_tokens = obj
        .get("output_tokens")
        .or_else(|| obj.get("completion_tokens"))
        .and_then(|v| v.as_u64())?;
    if input_tokens + output_tokens == 0 {
        return None;
    }
    let mut out = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens
    });
    if let Some(cached) = obj
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .or_else(|| {
            obj.get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
        })
        .and_then(|v| v.as_u64())
    {
        out["input_tokens_details"] = json!({ "cached_tokens": cached });
    }
    if let Some(reasoning) = obj
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .or_else(|| {
            obj.get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
        })
        .and_then(|v| v.as_u64())
    {
        out["output_tokens_details"] = json!({ "reasoning_tokens": reasoning });
    }
    Some(out)
}

fn start_reasoning(state: &mut ResponsesStreamState, idx: u32, events: &mut Vec<ResponsesSseEvent>) {
    if state.reasoning_id.is_none() {
        let rid = format!("rs_{}_{}", state.response_id, idx);
        state.reasoning_id = Some(rid.clone());
        state.reasoning_index = idx;

        events.push(state.emit(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": idx,
                "item": { "id": rid, "type": "reasoning", "summary": [] }
            }),
        ));
        events.push(state.emit(
            "response.reasoning_summary_part.added",
            json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": rid,
                "output_index": idx,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" }
            }),
        ));
    }
}

fn emit_reasoning_delta(
    state: &mut ResponsesStreamState,
    text: &str,
    events: &mut Vec<ResponsesSseEvent>,
) {
    if text.is_empty() {
        return;
    }
    let Some(rid) = state.reasoning_id.clone() else {
        return;
    };
    let idx = state.reasoning_index;
    state.reasoning_buf.push_str(text);
    events.push(state.emit(
        "response.reasoning_summary_text.delta",
        json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": rid,
            "output_index": idx,
            "summary_index": 0,
            "delta": text
        }),
    ));
}

fn close_reasoning(state: &mut ResponsesStreamState, events: &mut Vec<ResponsesSseEvent>) {
    let Some(rid) = state.reasoning_id.clone() else {
        return;
    };
    if state.reasoning_done {
        return;
    }
    state.reasoning_done = true;
    let idx = state.reasoning_index;
    let buf = state.reasoning_buf.clone();

    events.push(state.emit(
        "response.reasoning_summary_text.done",
        json!({
            "type": "response.reasoning_summary_text.done",
            "item_id": rid,
            "output_index": idx,
            "summary_index": 0,
            "text": buf
        }),
    ));
    events.push(state.emit(
        "response.reasoning_summary_part.done",
        json!({
            "type": "response.reasoning_summary_part.done",
            "item_id": rid,
            "output_index": idx,
            "summary_index": 0,
            "part": { "type": "summary_text", "text": buf }
        }),
    ));
    let item = json!({
        "id": rid,
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": buf }]
    });
    events.push(state.emit(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": idx,
            "item": item
        }),
    ));
    state.completed_output_items.insert(idx, item);
}

fn emit_text_content(
    state: &mut ResponsesStreamState,
    idx: u32,
    content: &str,
    events: &mut Vec<ResponsesSseEvent>,
) {
    let msg_id = format!("msg_{}_{}", state.response_id, idx);
    if !*state.msg_item_added.get(&idx).unwrap_or(&false) {
        state.msg_item_added.insert(idx, true);
        events.push(state.emit(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": idx,
                "item": { "id": msg_id, "type": "message", "content": [], "role": "assistant" }
            }),
        ));
    }
    if !*state.msg_content_added.get(&idx).unwrap_or(&false) {
        state.msg_content_added.insert(idx, true);
        events.push(state.emit(
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "item_id": msg_id,
                "output_index": idx,
                "content_index": 0,
                "part": { "type": "output_text", "annotations": [], "logprobs": [], "text": "" }
            }),
        ));
    }

    events.push(state.emit(
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "item_id": msg_id,
            "output_index": idx,
            "content_index": 0,
            "delta": content,
            "logprobs": []
        }),
    ));

    state
        .msg_text_buf
        .entry(idx)
        .or_default()
        .push_str(content);
}

fn close_message(state: &mut ResponsesStreamState, idx: u32, events: &mut Vec<ResponsesSseEvent>) {
    if !*state.msg_item_added.get(&idx).unwrap_or(&false)
        || *state.msg_item_done.get(&idx).unwrap_or(&false)
    {
        return;
    }
    state.msg_item_done.insert(idx, true);
    let full_text = state.msg_text_buf.get(&idx).cloned().unwrap_or_default();
    let msg_id = format!("msg_{}_{}", state.response_id, idx);

    events.push(state.emit(
        "response.output_text.done",
        json!({
            "type": "response.output_text.done",
            "item_id": msg_id,
            "output_index": idx,
            "content_index": 0,
            "text": full_text,
            "logprobs": []
        }),
    ));
    events.push(state.emit(
        "response.content_part.done",
        json!({
            "type": "response.content_part.done",
            "item_id": msg_id,
            "output_index": idx,
            "content_index": 0,
            "part": { "type": "output_text", "annotations": [], "logprobs": [], "text": full_text }
        }),
    ));

    let item = json!({
        "id": msg_id,
        "type": "message",
        "status": "completed",
        "content": [{ "type": "output_text", "annotations": [], "logprobs": [], "text": full_text }],
        "role": "assistant"
    });
    events.push(state.emit(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": idx,
            "item": item
        }),
    ));
    state.completed_output_items.insert(idx, item);
}

fn emit_tool_call(
    state: &mut ResponsesStreamState,
    tc: &Value,
    events: &mut Vec<ResponsesSseEvent>,
) {
    let tc_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if let Some(name) = tc
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
    {
        state.func_names.insert(tc_idx, name.to_string());
    }
    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
        state.func_call_ids.insert(tc_idx, id.to_string());
    }

    let call_id = state.func_call_ids.get(&tc_idx).cloned();
    let func_name = state.func_names.get(&tc_idx).cloned();

    if !*state.func_item_added.get(&tc_idx).unwrap_or(&false) {
        if let (Some(cid), Some(fname)) = (&call_id, &func_name) {
            state.func_item_added.insert(tc_idx, true);
            let is_custom = state.custom_tool_names.contains(fname);
            let item = if is_custom {
                json!({
                    "id": format!("ctc_{}", cid),
                    "type": "custom_tool_call",
                    "input": "",
                    "call_id": cid,
                    "name": fname
                })
            } else {
                json!({
                    "id": format!("fc_{}", cid),
                    "type": "function_call",
                    "arguments": "",
                    "call_id": cid,
                    "name": fname
                })
            };
            events.push(state.emit(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": tc_idx,
                    "item": item
                }),
            ));
        }
    }

    if let Some(args_delta) = tc
        .get("function")
        .and_then(|f| f.get("arguments"))
        .and_then(|v| v.as_str())
    {
        if !args_delta.is_empty() {
            let is_custom = func_name
                .as_deref()
                .map(|n| state.custom_tool_names.contains(n))
                .unwrap_or(false);
            if *state.func_item_added.get(&tc_idx).unwrap_or(&false) && !is_custom {
                if let Some(ref cid) = call_id {
                    events.push(state.emit(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": format!("fc_{}", cid),
                            "output_index": tc_idx,
                            "delta": args_delta
                        }),
                    ));
                }
            }
            state
                .func_args_buf
                .entry(tc_idx)
                .or_default()
                .push_str(args_delta);
        }
    }
}

fn close_tool_call(
    state: &mut ResponsesStreamState,
    idx: u32,
    events: &mut Vec<ResponsesSseEvent>,
) {
    let Some(cid) = state.func_call_ids.get(&idx).cloned() else {
        return;
    };
    if *state.func_item_done.get(&idx).unwrap_or(&false) {
        return;
    }
    state.func_item_done.insert(idx, true);

    let args = state
        .func_args_buf
        .get(&idx)
        .cloned()
        .unwrap_or_else(|| "{}".to_string());
    let fname = state.func_names.get(&idx).cloned().unwrap_or_default();
    let is_custom = state.custom_tool_names.contains(&fname);

    let item = if is_custom {
        let input = extract_custom_tool_input(&args);
        events.push(state.emit(
            "response.custom_tool_call_input.delta",
            json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": format!("ctc_{}", cid),
                "output_index": idx,
                "delta": input
            }),
        ));
        events.push(state.emit(
            "response.custom_tool_call_input.done",
            json!({
                "type": "response.custom_tool_call_input.done",
                "item_id": format!("ctc_{}", cid),
                "output_index": idx,
                "input": input
            }),
        ));
        json!({
            "id": format!("ctc_{}", cid),
            "type": "custom_tool_call",
            "status": "completed",
            "input": input,
            "call_id": cid,
            "name": fname
        })
    } else {
        events.push(state.emit(
            "response.function_call_arguments.done",
            json!({
                "type": "response.function_call_arguments.done",
                "item_id": format!("fc_{}", cid),
                "output_index": idx,
                "arguments": args
            }),
        ));
        json!({
            "id": format!("fc_{}", cid),
            "type": "function_call",
            "status": "completed",
            "arguments": args,
            "call_id": cid,
            "name": fname
        })
    };

    events.push(state.emit(
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": idx,
            "item": item
        }),
    ));
    state.completed_output_items.insert(idx, item);
}

fn send_completed(state: &mut ResponsesStreamState, events: &mut Vec<ResponsesSseEvent>) {
    if state.completed_sent {
        return;
    }
    state.completed_sent = true;
    let output: Vec<Value> = state.completed_output_items.values().cloned().collect();
    let output_text = extract_output_text_from_items(&output);

    let mut resp = json!({
        "id": state.response_id,
        "object": "response",
        "created_at": state.created,
        "status": "completed",
        "model": state.model,
        "background": false,
        "error": null,
        "output": output,
        "output_text": output_text
    });
    if let Some(ref usage) = state.responses_usage {
        resp["usage"] = usage.clone();
    }

    events.push(state.emit(
        "response.completed",
        json!({
            "type": "response.completed",
            "response": resp
        }),
    ));
}

fn flush_stream_events(state: &mut ResponsesStreamState) -> Vec<ResponsesSseEvent> {
    if state.completed_sent || !state.started {
        return Vec::new();
    }
    let mut events = Vec::new();
    let msg_indices: Vec<u32> = state.msg_item_added.keys().copied().collect();
    for idx in msg_indices {
        close_message(state, idx, &mut events);
    }
    close_reasoning(state, &mut events);
    let tc_indices: Vec<u32> = state.func_call_ids.keys().copied().collect();
    for idx in tc_indices {
        close_tool_call(state, idx, &mut events);
    }
    send_completed(state, &mut events);
    events
}

/// Translate a single OpenAI Chat Completions streaming chunk (`None` to flush at EOF/[DONE])
/// into OpenAI Responses API SSE events.
pub fn translate_chat_chunk_to_responses_events(
    chunk: Option<&Value>,
    state: &mut ResponsesStreamState,
) -> Vec<ResponsesSseEvent> {
    let Some(chunk) = chunk else {
        return flush_stream_events(state);
    };

    if let Some(usage) = chunk.get("usage") {
        if let Some(ru) = to_responses_usage_value(usage) {
            state.responses_usage = Some(ru);
        }
    }

    let choices = chunk.get("choices").and_then(|v| v.as_array());
    if choices.map(|c| c.is_empty()).unwrap_or(true) {
        if state.completion_pending && state.responses_usage.is_some() {
            return flush_stream_events(state);
        }
        return Vec::new();
    }

    let mut events = Vec::new();
    let choice = &choices.unwrap()[0];
    let idx = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

    if !state.started {
        state.started = true;
        if let Some(cid) = chunk.get("id").and_then(|v| v.as_str()) {
            if !cid.is_empty() {
                state.response_id = if cid.starts_with("resp_") {
                    cid.to_string()
                } else {
                    format!("resp_{}", cid)
                };
            }
        }
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            if !m.is_empty() {
                state.model = m.to_string();
            }
        }

        events.push(state.emit(
            "response.created",
            json!({
                "type": "response.created",
                "response": {
                    "id": state.response_id,
                    "object": "response",
                    "created_at": state.created,
                    "status": "in_progress",
                    "model": state.model,
                    "background": false,
                    "error": null,
                    "output": []
                }
            }),
        ));
        events.push(state.emit(
            "response.in_progress",
            json!({
                "type": "response.in_progress",
                "response": {
                    "id": state.response_id,
                    "object": "response",
                    "created_at": state.created,
                    "status": "in_progress",
                    "model": state.model
                }
            }),
        ));
    }

    // Reasoning content
    let reasoning_text = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !reasoning_text.is_empty() {
        start_reasoning(state, idx, &mut events);
        emit_reasoning_delta(state, reasoning_text, &mut events);
    }

    // Text content (including <think>...</think> tag handling)
    if let Some(raw_content) = delta.get("content").and_then(|v| v.as_str()) {
        if !raw_content.is_empty() {
            let mut content = raw_content.to_string();
            if content.contains("<think>") {
                state.in_thinking = true;
                content = content.replace("<think>", "");
                start_reasoning(state, idx, &mut events);
            }
            if content.contains("</think>") {
                let mut parts = content.splitn(2, "</think>");
                let think_part = parts.next().unwrap_or("");
                let rest = parts.next().unwrap_or("");
                if !think_part.is_empty() {
                    emit_reasoning_delta(state, think_part, &mut events);
                }
                close_reasoning(state, &mut events);
                state.in_thinking = false;
                content = rest.to_string();
            }
            if state.in_thinking {
                if !content.is_empty() {
                    emit_reasoning_delta(state, &content, &mut events);
                }
                return events;
            }
            if !content.is_empty() {
                close_reasoning(state, &mut events);
                emit_text_content(state, idx, &content, &mut events);
            }
        }
    }

    // Tool calls
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        if !tool_calls.is_empty() {
            close_reasoning(state, &mut events);
            close_message(state, idx, &mut events);
            for tc in tool_calls {
                emit_tool_call(state, tc, &mut events);
            }
        }
    }

    // Finish reason
    if choice
        .get("finish_reason")
        .map(|v| !v.is_null())
        .unwrap_or(false)
    {
        let msg_indices: Vec<u32> = state.msg_item_added.keys().copied().collect();
        for i in msg_indices {
            close_message(state, i, &mut events);
        }
        close_reasoning(state, &mut events);
        let tc_indices: Vec<u32> = state.func_call_ids.keys().copied().collect();
        for i in tc_indices {
            close_tool_call(state, i, &mut events);
        }

        if state.responses_usage.is_some() {
            send_completed(state, &mut events);
        } else {
            state.completion_pending = true;
        }
    }

    events
}

fn to_axum_sse_event(ev: ResponsesSseEvent) -> Event {
    let data = serde_json::to_string(&ev.data).unwrap_or_else(|_| "{}".to_string());
    Event::default().event(ev.event).data(data)
}

pub fn chat_sse_to_responses_sse_stream<S, E>(
    byte_stream: S,
    model: String,
    custom_tool_names: HashSet<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = async_stream::stream! {
        tokio::pin!(byte_stream);
        let mut parser = SseParser::new();
        let mut state = ResponsesStreamState::new(model, custom_tool_names);

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for ev in parser.push(&text) {
                        let trimmed = ev.data.trim();
                        if trimmed == "[DONE]" {
                            for rev in translate_chat_chunk_to_responses_events(None, &mut state) {
                                yield Ok(to_axum_sse_event(rev));
                            }
                            continue;
                        }
                        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                            // If upstream already emits Responses API SSE events, pass through
                            if ev.event.as_deref().map(|e| e.starts_with("response.")).unwrap_or(false)
                                || val.get("type").and_then(|v| v.as_str()).map(|t| t.starts_with("response.")).unwrap_or(false)
                            {
                                let mut out = Event::default().data(ev.data);
                                if let Some(name) = ev.event {
                                    out = out.event(name);
                                }
                                yield Ok(out);
                                continue;
                            }
                            for rev in translate_chat_chunk_to_responses_events(Some(&val), &mut state) {
                                yield Ok(to_axum_sse_event(rev));
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading upstream SSE stream for /v1/responses: {}", err);
                    break;
                }
            }
        }

        if let Some(ev) = parser.flush() {
            let trimmed = ev.data.trim();
            if trimmed != "[DONE]" {
                if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                    for rev in translate_chat_chunk_to_responses_events(Some(&val), &mut state) {
                        yield Ok(to_axum_sse_event(rev));
                    }
                }
            }
        }
        for rev in translate_chat_chunk_to_responses_events(None, &mut state) {
            yield Ok(to_axum_sse_event(rev));
        }
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

pub fn claude_sse_to_responses_sse_stream<S, E>(
    byte_stream: S,
    model: String,
    custom_tool_names: HashSet<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mapped = async_stream::stream! {
        tokio::pin!(byte_stream);
        let mut parser = SseParser::new();
        let mut state = ResponsesStreamState::new(model.clone(), custom_tool_names);
        let mut msg_id = state.response_id.clone();
        let mut tool_idx: u32 = 0;
        let mut input_tokens: u64 = 0;

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for ev in parser.push(&text) {
                        let Ok(val) = serde_json::from_str::<Value>(&ev.data) else {
                            continue;
                        };
                        let etype = ev.event.as_deref().or_else(|| val.get("type").and_then(|v| v.as_str())).unwrap_or("");
                        match etype {
                            "message_start" => {
                                if let Some(id) = val.pointer("/message/id").and_then(|v| v.as_str()) {
                                    msg_id = id.to_string();
                                }
                                if let Some(it) = val.pointer("/message/usage/input_tokens").and_then(|v| v.as_u64()) {
                                    input_tokens = it;
                                }
                                let chunk = json!({
                                    "id": msg_id,
                                    "model": state.model,
                                    "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": null }]
                                });
                                for rev in translate_chat_chunk_to_responses_events(Some(&chunk), &mut state) {
                                    yield Ok(to_axum_sse_event(rev));
                                }
                            }
                            "content_block_start" => {
                                if val.pointer("/content_block/type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    let cid = val.pointer("/content_block/id").and_then(|v| v.as_str()).unwrap_or("");
                                    let name = val.pointer("/content_block/name").and_then(|v| v.as_str()).unwrap_or("");
                                    let chunk = json!({
                                        "id": msg_id,
                                        "model": state.model,
                                        "choices": [{
                                            "index": 0,
                                            "delta": {
                                                "tool_calls": [{
                                                    "index": tool_idx,
                                                    "id": cid,
                                                    "type": "function",
                                                    "function": { "name": name, "arguments": "" }
                                                }]
                                            },
                                            "finish_reason": null
                                        }]
                                    });
                                    for rev in translate_chat_chunk_to_responses_events(Some(&chunk), &mut state) {
                                        yield Ok(to_axum_sse_event(rev));
                                    }
                                    tool_idx += 1;
                                }
                            }
                            "content_block_delta" => {
                                let dtype = val.pointer("/delta/type").and_then(|v| v.as_str()).unwrap_or("");
                                let chunk = match dtype {
                                    "text_delta" => {
                                        let t = val.pointer("/delta/text").and_then(|v| v.as_str()).unwrap_or("");
                                        Some(json!({
                                            "id": msg_id,
                                            "model": state.model,
                                            "choices": [{ "index": 0, "delta": { "content": t }, "finish_reason": null }]
                                        }))
                                    }
                                    "thinking_delta" => {
                                        let t = val.pointer("/delta/thinking").and_then(|v| v.as_str()).unwrap_or("");
                                        Some(json!({
                                            "id": msg_id,
                                            "model": state.model,
                                            "choices": [{ "index": 0, "delta": { "reasoning_content": t }, "finish_reason": null }]
                                        }))
                                    }
                                    "input_json_delta" => {
                                        let p = val.pointer("/delta/partial_json").and_then(|v| v.as_str()).unwrap_or("");
                                        let active_idx = tool_idx.saturating_sub(1);
                                        Some(json!({
                                            "id": msg_id,
                                            "model": state.model,
                                            "choices": [{
                                                "index": 0,
                                                "delta": {
                                                    "tool_calls": [{ "index": active_idx, "function": { "arguments": p } }]
                                                },
                                                "finish_reason": null
                                            }]
                                        }))
                                    }
                                    _ => None,
                                };
                                if let Some(c) = chunk {
                                    for rev in translate_chat_chunk_to_responses_events(Some(&c), &mut state) {
                                        yield Ok(to_axum_sse_event(rev));
                                    }
                                }
                            }
                            "message_delta" => {
                                let output_tokens = val.pointer("/usage/output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                let stop_reason = val.pointer("/delta/stop_reason").and_then(|v| v.as_str()).unwrap_or("stop");
                                let finish = if stop_reason == "tool_use" { "tool_calls" } else { "stop" };
                                let chunk = json!({
                                    "id": msg_id,
                                    "model": state.model,
                                    "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
                                    "usage": { "prompt_tokens": input_tokens, "completion_tokens": output_tokens }
                                });
                                for rev in translate_chat_chunk_to_responses_events(Some(&chunk), &mut state) {
                                    yield Ok(to_axum_sse_event(rev));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Error reading Claude SSE stream for /v1/responses: {}", err);
                    break;
                }
            }
        }
        for rev in translate_chat_chunk_to_responses_events(None, &mut state) {
            yield Ok(to_axum_sse_event(rev));
        }
    };

    Sse::new(mapped).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// ============================================================================
// Provider Candidate Resolution & Execution
// ============================================================================

fn populate_connection_parameters(conn: &mut UpstreamConnection, pc: &ProviderConnection) {
    conn.provider = pc.provider.clone();
    conn.base_url = pc.data.base_url.clone();
    conn.api_key = pc.data.api_key.clone();

    let psd = pc
        .data
        .extra
        .get("providerSpecificData")
        .and_then(|v| v.as_object());
    if pc.provider.eq_ignore_ascii_case("azure") {
        if conn.base_url.is_none() {
            conn.base_url = psd
                .and_then(|p| p.get("azureEndpoint"))
                .or_else(|| pc.data.extra.get("azureEndpoint"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
        conn.deployment = psd
            .and_then(|p| p.get("deployment"))
            .or_else(|| pc.data.extra.get("deployment"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        conn.api_version = psd
            .and_then(|p| p.get("apiVersion"))
            .or_else(|| pc.data.extra.get("apiVersion"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
    } else if pc.provider.eq_ignore_ascii_case("bedrock")
        || pc.provider.eq_ignore_ascii_case("bedrock-xai")
    {
        conn.region = psd
            .and_then(|p| p.get("region"))
            .or_else(|| pc.data.extra.get("region"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
    }
}

fn resolve_target_for_candidate(
    candidate: &str,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[ProviderConnection],
) -> (UpstreamConnection, String) {
    for node in provider_nodes {
        let mut matched_prefix: Option<String> = None;
        if !node.prefix.is_empty() {
            let prefix_slash = format!("{}/", node.prefix);
            if candidate.starts_with(&prefix_slash) || candidate == node.prefix {
                matched_prefix = Some(prefix_slash);
            }
        }
        if matched_prefix.is_none() {
            if let Some(ref name) = node.name {
                let name_slash = format!("{}/", name);
                if candidate
                    .to_lowercase()
                    .starts_with(&name_slash.to_lowercase())
                {
                    matched_prefix = Some(candidate[..name_slash.len()].to_string());
                }
            }
        }
        if matched_prefix.is_none() {
            let id_slash = format!("{}/", node.id);
            if candidate.starts_with(&id_slash) {
                matched_prefix = Some(id_slash);
            }
        }

        if let Some(prefix_slash) = matched_prefix {
            let upstream_model = if candidate.len() > prefix_slash.len() {
                candidate[prefix_slash.len()..].to_string()
            } else {
                candidate.to_string()
            };
            let mut conn = UpstreamConnection::default();
            conn.provider = node.name.clone().unwrap_or_else(|| node.prefix.clone());
            conn.base_url = Some(node.base_url.clone());

            if let Some(mapping_val) = node
                .data
                .get("schemaMapping")
                .or_else(|| node.data.get("schema_mapping"))
            {
                if let Ok(mapping) =
                    serde_json::from_value::<crate::custom_adapter::CustomSchemaMapping>(
                        mapping_val.clone(),
                    )
                {
                    conn.schema_mapping = Some(mapping);
                }
            } else if node.node_type.as_deref() == Some("custom") {
                if let Ok(mapping) =
                    serde_json::from_value::<crate::custom_adapter::CustomSchemaMapping>(
                        node.data.clone(),
                    )
                {
                    conn.schema_mapping = Some(mapping);
                }
            }

            if let Some(pc) = all_connections.iter().find(|c| {
                c.is_active
                    && (c.provider == node.id
                        || c.provider == node.prefix
                        || node.name.as_ref() == Some(&c.provider))
            }) {
                conn.api_key = pc.data.api_key.clone();
                if let Some(hdrs) = &pc.data.headers {
                    let mut hmap = HeaderMap::new();
                    for (k, v) in hdrs {
                        if let (Ok(kn), Ok(kv)) = (
                            header::HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(v),
                        ) {
                            hmap.insert(kn, kv);
                        }
                    }
                    conn.custom_headers = Some(hmap);
                }
            }
            if conn.api_key.is_none() {
                if let Some(k) = node.data.get("apiKey").and_then(|v| v.as_str()) {
                    conn.api_key = Some(k.to_string());
                }
            }
            return (conn, upstream_model);
        }
    }

    if let Some((p_prefix, m_part)) = candidate.split_once('/') {
        if let Some(pc) = all_connections
            .iter()
            .find(|c| c.is_active && c.provider.eq_ignore_ascii_case(p_prefix))
        {
            let mut conn = UpstreamConnection::default();
            populate_connection_parameters(&mut conn, pc);
            return (conn, m_part.to_string());
        }
        let mut conn = UpstreamConnection::default();
        conn.provider = p_prefix.to_string();
        return (conn, m_part.to_string());
    }

    if let Some(pc) = all_connections
        .iter()
        .find(|c| c.is_active && c.data.default_model.as_deref() == Some(candidate))
    {
        let mut conn = UpstreamConnection::default();
        populate_connection_parameters(&mut conn, pc);
        return (conn, candidate.to_string());
    }

    let (provider, target_model) = if candidate.starts_with("claude") {
        ("anthropic".to_string(), candidate.to_string())
    } else if candidate.contains("gemini") {
        ("vertex".to_string(), candidate.to_string())
    } else if candidate.starts_with("gpt-")
        || candidate.starts_with("o1")
        || candidate.starts_with("o3")
        || candidate.starts_with("o4")
    {
        ("openai".to_string(), candidate.to_string())
    } else {
        ("unknown".to_string(), candidate.to_string())
    };

    let mut conn = UpstreamConnection::default();
    conn.provider = provider.clone();
    if let Some(pc) = all_connections
        .iter()
        .find(|c| c.is_active && c.provider.eq_ignore_ascii_case(&provider))
    {
        populate_connection_parameters(&mut conn, pc);
    }
    (conn, target_model)
}

#[allow(clippy::too_many_arguments)]
async fn execute_candidate_responses(
    candidate: &str,
    chat_req: &ChatCompletionRequest,
    ctx: &ResponsesConversionContext,
    is_stream: bool,
    endpoint: &str,
    state: &AppState,
    api_key: &ApiKey,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[ProviderConnection],
) -> Result<Response, (StatusCode, String)> {
    let start_time = Instant::now();
    let (conn, target_model) =
        resolve_target_for_candidate(candidate, provider_nodes, all_connections);

    let mut clean_req = chat_req.clone();
    clean_req.model = target_model.clone();

    if conn.api_key.is_none() && conn.base_url.is_none() {
        state.circuit_breaker.record_failure(candidate);
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "No active connection or API key configured for provider '{}'.",
                conn.provider
            ),
        ));
    }

    let buffer = resolve_usage_buffer_tokens(&state.config);

    if conn.provider == "anthropic" {
        let claude_req = openai_to_claude_request(&clean_req);
        match state.proxy.forward_claude_messages(&claude_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                if !status.is_success() {
                    let err_text = upstream_res.text().await.unwrap_or_default();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("[{}] {}", status, err_text)));
                }
                state.circuit_breaker.record_success(&conn.provider);
                state.circuit_breaker.record_success(candidate);

                if is_stream {
                    let stream = wrap_byte_stream_with_usage(
                        upstream_res.bytes_stream(),
                        state.clone(),
                        api_key.clone(),
                        endpoint.to_string(),
                        Some(conn.provider.clone()),
                        target_model.clone(),
                        None,
                        Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                        start_time,
                    );
                    let sse = claude_sse_to_responses_sse_stream(
                        stream,
                        target_model,
                        ctx.custom_tool_names.clone(),
                    );
                    return Ok(sse.into_response());
                }

                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                let claude_resp: ClaudeMessageResponse = serde_json::from_slice(&body_bytes)
                    .map_err(|e| {
                        (
                            StatusCode::BAD_GATEWAY,
                            format!("Invalid Claude response JSON: {}", e),
                        )
                    })?;

                let prompt_tokens = claude_resp.usage.input_tokens as i64;
                let completion_tokens = claude_resp.usage.output_tokens as i64;
                let cached_tokens = claude_resp.usage.cache_read_input_tokens.unwrap_or(0) as i64;

                let openai_resp = claude_to_openai_response(&claude_resp);
                let resp_obj = chat_to_responses_response(&openai_resp, ctx, buffer);
                let resp_val = serde_json::to_value(&resp_obj).ok();

                record_completion_usage_and_details(
                    state,
                    api_key,
                    endpoint,
                    Some(&conn.provider),
                    &target_model,
                    None,
                    "ok",
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    start_time.elapsed().as_millis() as u64,
                    Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                    resp_val.as_ref(),
                );

                return Ok((
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    Json(resp_obj),
                )
                    .into_response());
            }
            Err(e) => {
                state.circuit_breaker.record_failure(&conn.provider);
                state.circuit_breaker.record_failure(candidate);
                return Err((StatusCode::BAD_GATEWAY, e.to_string()));
            }
        }
    }

    match state.proxy.forward_chat_completions(&clean_req, &conn).await {
        Ok(upstream_res) => {
            let status = upstream_res.status();
            if !status.is_success() {
                let err_text = upstream_res.text().await.unwrap_or_default();
                state.circuit_breaker.record_status(&conn.provider, status);
                state.circuit_breaker.record_status(candidate, status);
                return Err((status, format!("[{}] {}", status, err_text)));
            }
            state.circuit_breaker.record_success(&conn.provider);
            state.circuit_breaker.record_success(candidate);

            if is_stream {
                let stream = wrap_byte_stream_with_usage(
                    upstream_res.bytes_stream(),
                    state.clone(),
                    api_key.clone(),
                    endpoint.to_string(),
                    Some(conn.provider.clone()),
                    target_model.clone(),
                    None,
                    Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                    start_time,
                );
                let sse = chat_sse_to_responses_sse_stream(
                    stream,
                    target_model,
                    ctx.custom_tool_names.clone(),
                );
                return Ok(sse.into_response());
            }

            let body_bytes = upstream_res.bytes().await.unwrap_or_default();
            let raw_val: Value = serde_json::from_slice(&body_bytes).map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Invalid upstream JSON response: {}", e),
                )
            })?;
            let unwrapped = raw_val.get("result").unwrap_or(&raw_val);

            let resp_obj = chat_json_to_responses_response(unwrapped, ctx, buffer)
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

            let (prompt_tokens, completion_tokens, cached_tokens) = resp_obj
                .usage
                .as_ref()
                .map(|u| {
                    (
                        u.input_tokens as i64,
                        u.output_tokens.saturating_sub(buffer as u32) as i64,
                        u.input_tokens_details
                            .as_ref()
                            .map(|d| d.cached_tokens as i64)
                            .unwrap_or(0),
                    )
                })
                .unwrap_or((0, 0, 0));

            let resp_val = serde_json::to_value(&resp_obj).ok();
            record_completion_usage_and_details(
                state,
                api_key,
                endpoint,
                Some(&conn.provider),
                &target_model,
                None,
                "ok",
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                start_time.elapsed().as_millis() as u64,
                Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                resp_val.as_ref(),
            );

            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                Json(resp_obj),
            )
                .into_response())
        }
        Err(e) => {
            state.circuit_breaker.record_failure(&conn.provider);
            state.circuit_breaker.record_failure(candidate);
            Err((StatusCode::BAD_GATEWAY, e.to_string()))
        }
    }
}

// ============================================================================
// Axum HTTP Handlers
// ============================================================================

fn responses_error_response(status: StatusCode, message: String, error_type: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "message": message,
                "type": error_type
            }
        })),
    )
        .into_response()
}

pub async fn responses_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => return auth_err,
    };
    handle_responses_inner(state, body, api_key, "/v1/responses", false).await
}

pub async fn responses_compact_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => return auth_err,
    };
    handle_responses_inner(state, body, api_key, "/v1/responses/compact", true).await
}

async fn handle_responses_inner(
    state: AppState,
    body: Bytes,
    api_key: ApiKey,
    endpoint: &str,
    compact_mode: bool,
) -> Response {
    let start_time = Instant::now();

    let mut raw_val: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return responses_error_response(
                StatusCode::BAD_REQUEST,
                format!("Invalid JSON request body: {}", e),
                "invalid_request_error",
            );
        }
    };

    normalize_responses_request(&mut raw_val);

    let mut req: ResponsesRequest = match serde_json::from_value(raw_val) {
        Ok(r) => r,
        Err(e) => {
            return responses_error_response(
                StatusCode::BAD_REQUEST,
                format!("Invalid Responses API request: {}", e),
                "invalid_request_error",
            );
        }
    };

    if compact_mode {
        let saved = compact_responses_request(&mut req);
        if saved > 0 {
            tracing::info!("[responses/compact] compacted input items, saved {} bytes", saved);
        }
    }

    let (mut chat_req, ctx) = match responses_to_chat_request(&req) {
        Ok(pair) => pair,
        Err(e) => {
            return responses_error_response(
                StatusCode::BAD_REQUEST,
                e,
                "invalid_request_error",
            );
        }
    };

    let rtk_saved = crate::token_saver::compress_chat_request(&mut chat_req);
    if rtk_saved > 0 {
        tracing::info!("[RTK] compressed responses tool results, saved {} bytes", rtk_saved);
    }

    let (_, clean_model) = proxy::strip_provider_prefix(&req.model);
    let estimated_tokens = estimate_chat_tokens(&chat_req);
    if let Err(resp) = state
        .rate_limiter
        .check_model_rate_limit(&clean_model, estimated_tokens)
    {
        return resp;
    }

    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return responses_error_response(
            StatusCode::FORBIDDEN,
            e.to_string(),
            "permission_denied",
        );
    }

    let is_stream = req.stream.unwrap_or(false);
    let candidates = state.db.resolve_combo_chain(&req.model);
    let combo = state.db.get_combo_by_name(&req.model).ok().flatten();
    let (combo_strategy, combo_name) = match &combo {
        Some(c) => (
            crate::combo_router::ComboStrategy::parse(
                c.strategy.as_deref().unwrap_or("fallback"),
            ),
            Some(c.name.clone()),
        ),
        None => (crate::combo_router::ComboStrategy::Fallback, None),
    };

    let ordered_candidates = state.combo_tracker.order_candidates(
        combo_name.as_deref().unwrap_or(&req.model),
        &candidates,
        combo_strategy,
    );

    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let all_connections = state.db.load_provider_connections().unwrap_or_default();
    let mut last_error = String::new();
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;

    for candidate in &ordered_candidates {
        let (conn, _) =
            resolve_target_for_candidate(candidate, &provider_nodes, &all_connections);
        if state.circuit_breaker.is_open(&conn.provider)
            || state.circuit_breaker.is_open(candidate)
        {
            continue;
        }
        let _in_flight = state.combo_tracker.track_in_flight(candidate);
        match execute_candidate_responses(
            candidate,
            &chat_req,
            &ctx,
            is_stream,
            endpoint,
            &state,
            &api_key,
            &provider_nodes,
            &all_connections,
        )
        .await
        {
            Ok(resp) => {
                if let Some(ref cn) = combo_name {
                    state.combo_tracker.record_success(cn, candidate);
                }
                return resp;
            }
            Err((status, err)) => {
                state
                    .combo_tracker
                    .record_error_with_status(candidate, status);
                last_status = status;
                last_error = err;
            }
        }
    }

    if last_error.is_empty() {
        last_error = "All candidates tripped or circuit breaker is open".to_string();
    }

    record_completion_usage_and_details(
        &state,
        &api_key,
        endpoint,
        None,
        &req.model,
        None,
        "error",
        0,
        0,
        0,
        start_time.elapsed().as_millis() as u64,
        Some(&serde_json::to_value(&chat_req).unwrap_or_default()),
        Some(&json!({ "error": &last_error })),
    );

    responses_error_response(
        last_status,
        format!(
            "All {} candidate(s) failed for '{}': {}",
            candidates.len(),
            req.model,
            last_error
        ),
        "combo_routing_error",
    )
}

// ============================================================================
// Unit Tests & Translation Latency Benchmark
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::{ChatChoice, ChatResponseMessage, UsageInfo};

    #[test]
    fn test_normalize_responses_request_aliases() {
        let mut body = json!({
            "model": "gpt-5.4",
            "input": "Hello world",
            "max_completion_tokens": 512,
            "reasoning_effort": "high",
            "reasoning_summary": "auto",
            "verbosity": "low",
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "out", "schema": { "type": "object" } }
            },
            "tool_choice": { "type": "function", "function": { "name": "get_weather" } },
            "logprobs": true,
            "frequency_penalty": 0.5,
            "seed": 42
        });

        normalize_responses_request(&mut body);

        assert_eq!(body["max_output_tokens"], 512);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["text"]["verbosity"], "low");
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["name"], "out");
        assert_eq!(body["tool_choice"], json!({ "type": "function", "name": "get_weather" }));
        assert!(body["include"]
            .as_array()
            .unwrap()
            .contains(&json!("message.output_text.logprobs")));
        assert!(body.get("frequency_penalty").is_none());
        assert!(body.get("seed").is_none());
    }

    #[test]
    fn test_responses_to_chat_request_string_and_items() {
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "gpt-5.4",
            "instructions": "You are helpful.",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "Check weather" }] },
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking..." }] },
                { "type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": { "city": "NYC" } },
                { "type": "function_call_output", "call_id": "call_1", "output": { "temp": 72 } }
            ],
            "tools": [
                { "type": "function", "name": "get_weather", "description": "Get weather", "parameters": { "type": "object" } },
                { "type": "request_user_input" },
                { "type": "custom", "name": "exec", "description": "Run shell" }
            ],
            "tool_choice": { "type": "function", "name": "get_weather" },
            "max_output_tokens": 1024
        }))
        .unwrap();

        let (chat, ctx) = responses_to_chat_request(&req).unwrap();
        assert_eq!(chat.messages.len(), 4);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[1].role, "user");
        assert_eq!(chat.messages[2].role, "assistant");
        assert_eq!(
            chat.messages[2].tool_calls.as_ref().unwrap()[0].function.name,
            "get_weather"
        );
        assert_eq!(chat.messages[3].role, "tool");
        assert_eq!(chat.messages[3].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(chat.max_tokens, Some(1024));

        // Hosted nameless tool filtered out, custom + function kept
        let tools = chat.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 2);
        assert!(ctx.custom_tool_names.contains("exec"));
        assert_eq!(
            chat.tool_choice,
            Some(json!({ "type": "function", "function": { "name": "get_weather" } }))
        );
    }

    #[test]
    fn test_chat_to_responses_response_text_and_tools() {
        let mut custom_tools = HashSet::new();
        custom_tools.insert("exec".to_string());
        let ctx = ResponsesConversionContext {
            model: "gpt-5.4".to_string(),
            custom_tool_names: custom_tools,
            previous_response_id: Some("resp_prev".to_string()),
        };

        let chat_resp = ChatCompletionResponse {
            id: "chatcmpl-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1700000000,
            model: "gpt-5.4".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: Some("Running command".to_string()),
                    tool_calls: Some(vec![
                        ChatToolCall {
                            id: "call_abc".to_string(),
                            tool_type: "function".to_string(),
                            function: FunctionCall {
                                name: "exec".to_string(),
                                arguments: r#"{"input":"ls -la"}"#.to_string(),
                            },
                        },
                        ChatToolCall {
                            id: "call_def".to_string(),
                            tool_type: "function".to_string(),
                            function: FunctionCall {
                                name: "read_file".to_string(),
                                arguments: r#"{"path":"a.txt"}"#.to_string(),
                            },
                        },
                    ]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: Some(UsageInfo {
                prompt_tokens: 100,
                completion_tokens: 25,
                total_tokens: 125,
                prompt_tokens_details: Some(json!({ "cached_tokens": 20 })),
            }),
            response_schema: None,
        };

        let out = chat_to_responses_response(&chat_resp, &ctx, 5);
        assert_eq!(out.id, "resp_chatcmpl-123");
        assert_eq!(out.object, "response");
        assert_eq!(out.status, "completed");
        assert_eq!(out.output_text, "Running command");
        assert_eq!(out.output.len(), 3);
        assert_eq!(out.output[0]["type"], "message");
        assert_eq!(out.output[1]["type"], "custom_tool_call");
        assert_eq!(out.output[1]["input"], "ls -la");
        assert_eq!(out.output[2]["type"], "function_call");
        assert_eq!(out.previous_response_id.as_deref(), Some("resp_prev"));
        let usage = out.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.total_tokens, 130);
        assert_eq!(usage.input_tokens_details.unwrap().cached_tokens, 20);
    }

    #[test]
    fn test_streaming_chat_chunks_to_responses_sse() {
        let mut state = ResponsesStreamState::new("gpt-5.4", HashSet::new());
        let mut all_events = Vec::new();

        let chunks = vec![
            json!({
                "id": "c1",
                "model": "gpt-5.4",
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "<think>plan</think>Hello" }, "finish_reason": null }]
            }),
            json!({
                "id": "c1",
                "model": "gpt-5.4",
                "choices": [{ "index": 0, "delta": { "content": " world" }, "finish_reason": null }]
            }),
            json!({
                "id": "c1",
                "model": "gpt-5.4",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
            }),
            json!({
                "id": "c1",
                "model": "gpt-5.4",
                "choices": [],
                "usage": { "prompt_tokens": 12, "completion_tokens": 4 }
            }),
        ];

        for c in &chunks {
            all_events.extend(translate_chat_chunk_to_responses_events(Some(c), &mut state));
        }
        all_events.extend(translate_chat_chunk_to_responses_events(None, &mut state));

        let names: Vec<&str> = all_events.iter().map(|e| e.event).collect();
        assert!(names.contains(&"response.created"));
        assert!(names.contains(&"response.in_progress"));
        assert!(names.contains(&"response.reasoning_summary_text.delta"));
        assert!(names.contains(&"response.output_item.added"));
        assert!(names.contains(&"response.content_part.added"));
        assert!(names.contains(&"response.output_text.delta"));
        assert!(names.contains(&"response.output_text.done"));
        assert!(names.contains(&"response.completed"));

        let completed = all_events
            .iter()
            .find(|e| e.event == "response.completed")
            .unwrap();
        assert_eq!(completed.data["response"]["output_text"], "Hello world");
        assert_eq!(completed.data["response"]["usage"]["input_tokens"], 12);
        assert_eq!(completed.data["response"]["usage"]["output_tokens"], 4);
    }

    #[test]
    fn test_compact_responses_input_preserves_pairs_and_drops_reasoning() {
        let mut items = vec![json!({ "type": "message", "role": "system", "content": "sys" })];
        for i in 0..10 {
            items.push(json!({ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "r" }] }));
            items.push(json!({ "type": "message", "role": "user", "content": format!("msg {}", i) }));
            items.push(json!({ "type": "function_call", "call_id": format!("c{}", i), "name": "tool", "arguments": "{}" }));
            items.push(json!({ "type": "function_call_output", "call_id": format!("c{}", i), "output": "x".repeat(500) }));
        }

        let compacted = compact_responses_input(&items, 8, 100);
        // No reasoning items remain
        assert!(!compacted.iter().any(|it| it["type"] == "reasoning"));
        // System message preserved at index 0
        assert_eq!(compacted[0]["role"], "system");
        // Compacted summary inserted at index 1
        assert!(compacted[1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Compacted earlier conversation"));
        // Tool output truncated
        let last_out = compacted.last().unwrap()["output"].as_str().unwrap();
        assert!(last_out.contains("[compacted "));
    }

    #[test]
    fn bench_responses_translation_latency() {
        let raw = json!({
            "model": "gpt-5.4",
            "instructions": "Be concise.",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "Explain Rust ownership." }] },
                { "type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": "{\"topic\":\"ownership\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "Ownership rules summary" }
            ],
            "tools": [{ "type": "function", "name": "lookup", "parameters": { "type": "object" } }],
            "max_output_tokens": 512
        });

        let chat_resp = ChatCompletionResponse {
            id: "chatcmpl-bench".to_string(),
            object: "chat.completion".to_string(),
            created: 1700000000,
            model: "gpt-5.4".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: Some("Each value in Rust has an owner.".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(UsageInfo {
                prompt_tokens: 40,
                completion_tokens: 10,
                total_tokens: 50,
                prompt_tokens_details: None,
            }),
            response_schema: None,
        };

        let chunk = json!({
            "id": "c1",
            "model": "gpt-5.4",
            "choices": [{ "index": 0, "delta": { "content": "token " }, "finish_reason": null }]
        });

        let iters = 1000usize;
        let mut durations_ns = Vec::with_capacity(iters);

        for _ in 0..iters {
            let t0 = Instant::now();
            let mut body = raw.clone();
            normalize_responses_request(&mut body);
            let parsed: ResponsesRequest = serde_json::from_value(body).unwrap();
            let (chat_req, ctx) = responses_to_chat_request(&parsed).unwrap();
            let _ = chat_req.messages.len();
            let _resp = chat_to_responses_response(&chat_resp, &ctx, 0);
            let mut st = ResponsesStreamState::new("gpt-5.4", HashSet::new());
            let _evs = translate_chat_chunk_to_responses_events(Some(&chunk), &mut st);
            durations_ns.push(t0.elapsed().as_nanos());
        }

        durations_ns.sort_unstable();
        let sum_ns: u128 = durations_ns.iter().sum();
        let avg_us = (sum_ns as f64 / iters as f64) / 1000.0;
        let p50_us = durations_ns[iters / 2] as f64 / 1000.0;
        let p99_us = durations_ns[(iters * 99) / 100] as f64 / 1000.0;
        println!(
            "[BENCH] Responses API full round-trip (normalize + req + resp + stream chunk) over {} iters: avg={:.2}us p50={:.2}us p99={:.2}us",
            iters, avg_us, p50_us, p99_us
        );
        assert!(avg_us < 1000.0, "Translation latency exceeded 1ms: {}us", avg_us);
    }
}
