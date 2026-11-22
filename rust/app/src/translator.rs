use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

// ============================================================================
// OpenAI Schemas
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopCondition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(MessageContent::Text(text.into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(MessageContent::Text(text.into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text(text.into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(content.into())),
            name: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<ImageUrl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StopCondition {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ChatFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatFunction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatResponseMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatResponseMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: ChunkDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkToolCall {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ChunkFunctionCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChunkFunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

// ============================================================================
// Claude (Anthropic) Schemas
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeMessageRequest {
    pub model: String,
    pub messages: Vec<ClaudeMessage>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<ClaudeSystem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ClaudeTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ClaudeSystem {
    Single(String),
    Multiple(Vec<ClaudeSystemPart>),
}

impl ClaudeSystem {
    pub fn as_text(&self) -> String {
        match self {
            ClaudeSystem::Single(s) => s.clone(),
            ClaudeSystem::Multiple(parts) => parts
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeSystemPart {
    #[serde(rename = "type")]
    pub part_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeMessage {
    pub role: String,
    pub content: ClaudeContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ClaudeContent {
    Single(String),
    Blocks(Vec<ClaudeBlock>),
}

impl ClaudeContent {
    pub fn as_text(&self) -> String {
        match self {
            ClaudeContent::Single(s) => s.clone(),
            ClaudeContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ClaudeBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClaudeBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<serde_json::Value>,
    },
    #[serde(rename = "image")]
    Image {
        source: ClaudeImageSource,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeMessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub role: String,
    pub model: String,
    pub content: Vec<ClaudeBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    pub usage: ClaudeUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClaudeUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

// ============================================================================
// Gemini Schemas
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GeminiInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiInlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiFunctionCall {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiFunctionResponse {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<GeminiCandidate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates_token_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_token_count: Option<u32>,
}

// ============================================================================
// Structured Output Modes & Schema Transformation
// ============================================================================

pub const STRUCTURED_OUTPUT_MODE_AUTO: &str = "auto";
pub const STRUCTURED_OUTPUT_MODE_PASSTHROUGH: &str = "passthrough";
pub const STRUCTURED_OUTPUT_MODE_FALLBACK: &str = "fallback";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StructuredOutputMode {
    #[default]
    Auto,
    Passthrough,
    Fallback,
}

impl std::str::FromStr for StructuredOutputMode {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().trim() {
            "passthrough" => Ok(StructuredOutputMode::Passthrough),
            "fallback" => Ok(StructuredOutputMode::Fallback),
            _ => Ok(StructuredOutputMode::Auto),
        }
    }
}

impl fmt::Display for StructuredOutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StructuredOutputMode::Auto => write!(f, "auto"),
            StructuredOutputMode::Passthrough => write!(f, "passthrough"),
            StructuredOutputMode::Fallback => write!(f, "fallback"),
        }
    }
}

pub fn resolve_structured_output_mode(explicit: Option<&str>) -> StructuredOutputMode {
    if let Some(s) = explicit {
        return s.parse().unwrap_or_default();
    }
    if let Ok(val) = std::env::var("STRUCTURED_OUTPUT_MODE") {
        return val.parse().unwrap_or_default();
    }
    StructuredOutputMode::Auto
}

pub fn build_json_schema_prompt(schema_json: &str) -> String {
    format!(
        "You must respond with valid JSON that strictly follows this JSON schema:\n```json\n{}\n```\nRespond ONLY with the JSON object, no other text.",
        schema_json
    )
}

pub fn normalize_openai_strict_schema(schema: &serde_json::Value) -> serde_json::Value {
    if !schema.is_object() {
        return schema.clone();
    }
    let mut copy = schema.clone();
    if let Some(obj) = copy.as_object_mut() {
        let is_object_type = obj.get("type").and_then(|t| t.as_str()) == Some("object")
            || obj.contains_key("properties");
        if is_object_type {
            if !obj.contains_key("additionalProperties") {
                obj.insert("additionalProperties".to_string(), serde_json::Value::Bool(false));
            }
            if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                let prop_keys: Vec<String> = props.keys().cloned().collect();
                let mut req_set = BTreeSet::new();
                if let Some(req_arr) = obj.get("required").and_then(|r| r.as_array()) {
                    for item in req_arr {
                        if let Some(s) = item.as_str() {
                            req_set.insert(s.to_string());
                        }
                    }
                }
                for k in prop_keys {
                    req_set.insert(k);
                }
                obj.insert(
                    "required".to_string(),
                    serde_json::Value::Array(
                        req_set.into_iter().map(serde_json::Value::String).collect(),
                    ),
                );

                if let Some(props_mut) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                    for (_k, v) in props_mut.iter_mut() {
                        *v = normalize_openai_strict_schema(v);
                    }
                }
            }
        }

        if let Some(items) = obj.get_mut("items") {
            if items.is_object() {
                *items = normalize_openai_strict_schema(items);
            } else if let Some(arr) = items.as_array_mut() {
                for item in arr {
                    *item = normalize_openai_strict_schema(item);
                }
            }
        }

        for def_key in &["$defs", "definitions"] {
            if let Some(defs) = obj.get_mut(*def_key).and_then(|d| d.as_object_mut()) {
                for (_k, v) in defs.iter_mut() {
                    *v = normalize_openai_strict_schema(v);
                }
            }
        }

        for comb_key in &["anyOf", "oneOf", "allOf"] {
            if let Some(comb) = obj.get_mut(*comb_key).and_then(|c| c.as_array_mut()) {
                for item in comb {
                    *item = normalize_openai_strict_schema(item);
                }
            }
        }
    }
    copy
}

pub fn downgrade_json_schema_to_json_object(req: &mut ChatCompletionRequest) {
    if let Some(rf) = &req.response_format {
        let is_json_schema = rf.get("type").and_then(|t| t.as_str()) == Some("json_schema");
        if is_json_schema {
            let schema_val = rf
                .get("json_schema")
                .and_then(|js| js.get("schema"))
                .or_else(|| rf.get("schema"));
            if let Some(schema) = schema_val {
                let schema_str = serde_json::to_string_pretty(schema).unwrap_or_default();
                let prompt = build_json_schema_prompt(&schema_str);

                if let Some(sys_msg) = req.messages.iter_mut().find(|m| m.role == "system") {
                    let new_content = match &sys_msg.content {
                        Some(MessageContent::Text(t)) => format!("{}\n\n{}", t, prompt),
                        Some(MessageContent::Parts(parts)) => {
                            let text = parts
                                .iter()
                                .filter_map(|p| p.text.as_deref())
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!("{}\n\n{}", text, prompt)
                        }
                        None => prompt,
                    };
                    sys_msg.content = Some(MessageContent::Text(new_content));
                } else {
                    req.messages.insert(0, ChatMessage::system(prompt));
                }
            }
            req.response_format = Some(serde_json::json!({ "type": "json_object" }));
        }
    }
}

pub fn apply_structured_output_mode(
    req: &mut ChatCompletionRequest,
    mode: StructuredOutputMode,
) {
    match mode {
        StructuredOutputMode::Passthrough => {
            // Preserve raw response schemas verbatim
        }
        StructuredOutputMode::Auto | StructuredOutputMode::Fallback => {
            // Run JSON schema conversion / downgrade
            downgrade_json_schema_to_json_object(req);
        }
    }
}

// ============================================================================
// Kiro/Kimi Tool Call Repair & Buffering
// ============================================================================

pub const DEFAULT_KIRO_TOOL_CALL_REPAIR_BUFFER_MAX_BYTES: usize = 8 * 1024 * 1024; // 8MB

pub fn resolve_kiro_tool_call_repair() -> bool {
    std::env::var("KIRO_TOOL_CALL_REPAIR")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

pub fn resolve_kiro_tool_call_repair_buffer_max_bytes() -> usize {
    std::env::var("KIRO_TOOL_CALL_REPAIR_BUFFER_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_KIRO_TOOL_CALL_REPAIR_BUFFER_MAX_BYTES)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRepairError {
    BufferExceeded(String),
    InvalidTool(String),
    MissingInput(String),
}

impl fmt::Display for ToolRepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolRepairError::BufferExceeded(msg) => write!(f, "{}", msg),
            ToolRepairError::InvalidTool(msg) => write!(f, "{}", msg),
            ToolRepairError::MissingInput(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for ToolRepairError {}

fn strip_trailing_commas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len {
        let ch = chars[i];
        if escaped {
            result.push(ch);
            escaped = false;
            i += 1;
            continue;
        }
        if ch == '\\' && in_string {
            result.push(ch);
            escaped = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            result.push(ch);
            i += 1;
            continue;
        }
        if !in_string && ch == ',' {
            let mut j = i + 1;
            while j < len && chars[j].is_whitespace() {
                j += 1;
            }
            if j < len && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

pub fn repair_json_string(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok("{}".to_string());
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }

    let candidate_stripped = strip_trailing_commas(trimmed);
    if serde_json::from_str::<serde_json::Value>(&candidate_stripped).is_ok() {
        return Ok(candidate_stripped);
    }

    let mut candidate = candidate_stripped;
    let mut in_string = false;
    let mut escaped = false;
    let mut stack = Vec::new();

    for ch in candidate.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            if in_string {
                escaped = true;
            }
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            match ch {
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' => {
                    if stack.last() == Some(&'}') {
                        stack.pop();
                    }
                }
                ']' => {
                    if stack.last() == Some(&']') {
                        stack.pop();
                    }
                }
                _ => {}
            }
        }
    }

    if in_string {
        candidate.push('"');
    }

    let trimmed_c = candidate.trim_end();
    if trimmed_c.ends_with(',') {
        candidate = trimmed_c[..trimmed_c.len() - 1].to_string();
    }

    while let Some(closing) = stack.pop() {
        let cur = candidate.trim_end();
        if cur.ends_with(',') {
            candidate = cur[..cur.len() - 1].to_string();
        }
        candidate.push(closing);
    }

    let candidate_final = strip_trailing_commas(&candidate);
    if serde_json::from_str::<serde_json::Value>(&candidate_final).is_ok() {
        return Ok(candidate_final);
    }
    candidate = candidate_final;

    if let Some(colon_pos) = candidate.rfind(':') {
        let after = candidate[colon_pos + 1..].trim();
        if after.is_empty() || after == "}" || after == "]" {
            let mut fixed = candidate[..colon_pos + 1].to_string();
            fixed.push_str("null");
            let mut s = Vec::new();
            let mut in_s = false;
            let mut esc = false;
            for ch in fixed.chars() {
                if esc { esc = false; continue; }
                if ch == '\\' { if in_s { esc = true; } continue; }
                if ch == '"' { in_s = !in_s; continue; }
                if !in_s {
                    if ch == '{' { s.push('}'); }
                    else if ch == '[' { s.push(']'); }
                    else if ch == '}' || ch == ']' { s.pop(); }
                }
            }
            while let Some(c) = s.pop() {
                fixed.push(c);
            }
            if serde_json::from_str::<serde_json::Value>(&fixed).is_ok() {
                return Ok(fixed);
            }
        }
    }

    Err(format!("Unable to repair fragmented JSON: {}", raw))
}

#[derive(Debug, Clone, Default)]
pub struct BufferedToolCall {
    pub id: String,
    pub name: String,
    pub input_chunks: Vec<String>,
    pub input_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct KiroToolCallRepairBuffer {
    pub repair_enabled: bool,
    pub max_bytes: usize,
    buffered_bytes: usize,
    tools: BTreeMap<String, BufferedToolCall>,
    pub dropped_tools: usize,
}

impl Default for KiroToolCallRepairBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroToolCallRepairBuffer {
    pub fn new() -> Self {
        Self {
            repair_enabled: resolve_kiro_tool_call_repair(),
            max_bytes: resolve_kiro_tool_call_repair_buffer_max_bytes(),
            buffered_bytes: 0,
            tools: BTreeMap::new(),
            dropped_tools: 0,
        }
    }

    pub fn with_limits(repair_enabled: bool, max_bytes: usize) -> Self {
        Self {
            repair_enabled,
            max_bytes,
            buffered_bytes: 0,
            tools: BTreeMap::new(),
            dropped_tools: 0,
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffered_bytes
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn clear(&mut self) {
        self.tools.clear();
        self.buffered_bytes = 0;
    }

    pub fn append_tool_fragment(
        &mut self,
        tool_id: &str,
        tool_name: Option<&str>,
        chunk: &str,
    ) -> Result<(), ToolRepairError> {
        let chunk_bytes = chunk.as_bytes().len();
        if self.buffered_bytes + chunk_bytes > self.max_bytes {
            return Err(ToolRepairError::BufferExceeded(
                "kiro_integrity_buffer_exceeded: Kiro tool call buffer limit exceeded".to_string(),
            ));
        }

        let entry = self
            .tools
            .entry(tool_id.to_string())
            .or_insert_with(|| BufferedToolCall {
                id: tool_id.to_string(),
                name: tool_name.unwrap_or_default().to_string(),
                input_chunks: Vec::new(),
                input_bytes: 0,
            });

        if let Some(name) = tool_name {
            if entry.name.is_empty() {
                entry.name = name.to_string();
            }
        }

        entry.input_chunks.push(chunk.to_string());
        entry.input_bytes += chunk_bytes;
        self.buffered_bytes += chunk_bytes;
        Ok(())
    }

    pub fn finalize_tool_call(&mut self, tool_id: &str) -> Result<ChatToolCall, ToolRepairError> {
        let tool = self
            .tools
            .remove(tool_id)
            .ok_or_else(|| ToolRepairError::InvalidTool(format!("Tool {} not found in buffer", tool_id)))?;

        self.buffered_bytes = self.buffered_bytes.saturating_sub(tool.input_bytes);

        let joined = tool.input_chunks.join("");
        let final_arguments = if self.repair_enabled {
            repair_json_string(&joined).unwrap_or(joined)
        } else {
            joined
        };

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&final_arguments) {
            if tool.name == "tool_call" {
                if let Some(actual_name) = parsed.get("name").and_then(|n| n.as_str()) {
                    let inner_args = parsed.get("arguments").cloned().unwrap_or(serde_json::Value::Null);
                    let inner_args_str = if inner_args.is_string() {
                        inner_args.as_str().unwrap().to_string()
                    } else {
                        serde_json::to_string(&inner_args).unwrap_or_default()
                    };
                    return Ok(ChatToolCall {
                        id: tool.id,
                        tool_type: "function".to_string(),
                        function: FunctionCall {
                            name: actual_name.to_string(),
                            arguments: inner_args_str,
                        },
                    });
                }
            }

            Ok(ChatToolCall {
                id: tool.id,
                tool_type: "function".to_string(),
                function: FunctionCall {
                    name: tool.name,
                    arguments: final_arguments,
                },
            })
        } else if self.repair_enabled {
            Err(ToolRepairError::InvalidTool(format!(
                "invalid_kiro_tool_call: unable to parse repaired JSON arguments: {}",
                final_arguments
            )))
        } else {
            Err(ToolRepairError::InvalidTool(format!(
                "invalid_kiro_tool_call: fragmented JSON tool call without repair: {}",
                final_arguments
            )))
        }
    }

    pub fn finalize_all(&mut self) -> Result<Vec<ChatToolCall>, ToolRepairError> {
        let ids: Vec<String> = self.tools.keys().cloned().collect();
        let mut calls = Vec::new();
        for id in ids {
            match self.finalize_tool_call(&id) {
                Ok(call) => calls.push(call),
                Err(err) => {
                    self.dropped_tools += 1;
                    if self.tools.is_empty() && calls.is_empty() {
                        return Err(err);
                    }
                }
            }
        }
        self.clear();
        Ok(calls)
    }
}

// ============================================================================
// Cursor Empty Turn Retries and Tail Parser
// ============================================================================

pub const DEFAULT_CURSOR_EMPTY_TURN_MAX_RETRIES: usize = 3;
pub const DEFAULT_CURSOR_TAIL_ONLY_THRESHOLD: usize = 80;
pub const DEFAULT_CURSOR_EMPTY_TURN_RETRY_DELAY_MS: u64 = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRetryConfig {
    pub max_retries: usize,
    pub tail_only_threshold: usize,
    pub retry_delay_ms: u64,
}

impl Default for CursorRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_CURSOR_EMPTY_TURN_MAX_RETRIES,
            tail_only_threshold: DEFAULT_CURSOR_TAIL_ONLY_THRESHOLD,
            retry_delay_ms: DEFAULT_CURSOR_EMPTY_TURN_RETRY_DELAY_MS,
        }
    }
}

impl CursorRetryConfig {
    pub fn load_from_env() -> Self {
        let max_retries = std::env::var("CURSOR_EMPTY_TURN_MAX_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CURSOR_EMPTY_TURN_MAX_RETRIES);
        let tail_only_threshold = std::env::var("CURSOR_TAIL_ONLY_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CURSOR_TAIL_ONLY_THRESHOLD);
        let retry_delay_ms = std::env::var("CURSOR_EMPTY_TURN_RETRY_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CURSOR_EMPTY_TURN_RETRY_DELAY_MS);

        Self {
            max_retries,
            tail_only_threshold,
            retry_delay_ms,
        }
    }
}

pub fn should_retry_cursor_empty_turn(message_count: usize, threshold: usize) -> bool {
    message_count >= threshold
}

pub fn tail_only_cursor_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return Vec::new();
    }
    let system_messages: Vec<ChatMessage> = messages
        .iter()
        .filter(|m| {
            m.role == "system"
                && !m
                    .content
                    .as_ref()
                    .map(|c| c.as_text().contains("[9router]"))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();

    let chat_messages: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| m.role != "system")
        .collect();

    if chat_messages.is_empty() {
        return system_messages;
    }

    let mut anchor = chat_messages.len() - 1;
    for (i, m) in chat_messages.iter().enumerate().rev() {
        if m.role == "user" || m.role == "tool" {
            anchor = i;
            break;
        }
    }

    let tail_start = if anchor > 0 && chat_messages[anchor - 1].role == "assistant" {
        anchor - 1
    } else {
        anchor
    };

    let tail = &chat_messages[tail_start..];

    let note = ChatMessage::system(format!(
        "[9router] Long session: {} messages trimmed to the latest turn. Answer the current request; re-read files if needed.",
        messages.len()
    ));

    let mut result = system_messages;
    result.push(note);
    for m in tail {
        result.push((*m).clone());
    }
    result
}

pub fn tail_only_claude_messages(messages: &[ClaudeMessage]) -> Vec<ClaudeMessage> {
    if messages.is_empty() {
        return Vec::new();
    }
    let mut anchor = messages.len() - 1;
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role == "user" {
            anchor = i;
            break;
        }
    }
    let tail_start = if anchor > 0 && messages[anchor - 1].role == "assistant" {
        anchor - 1
    } else {
        anchor
    };

    let tail = &messages[tail_start..];
    let note = ClaudeMessage {
        role: "user".to_string(),
        content: ClaudeContent::Single(format!(
            "[9router] Long session: {} messages trimmed to the latest turn. Answer the current request; re-read files if needed.",
            messages.len()
        )),
    };
    let mut result = vec![note];
    for m in tail {
        result.push((*m).clone());
    }
    result
}

#[derive(Debug, Clone)]
pub struct CursorTurnRetryManager {
    pub config: CursorRetryConfig,
    pub attempts: usize,
    pub message_count: usize,
}

impl CursorTurnRetryManager {
    pub fn new(config: CursorRetryConfig, message_count: usize) -> Self {
        Self {
            config,
            attempts: 0,
            message_count,
        }
    }

    pub fn should_retry(&self, turn_empty: bool, output_started: bool) -> bool {
        turn_empty
            && !output_started
            && self.attempts < self.config.max_retries
            && should_retry_cursor_empty_turn(self.message_count, self.config.tail_only_threshold)
    }

    pub fn record_attempt(&mut self) -> usize {
        self.attempts += 1;
        self.attempts
    }

    pub fn prepare_retry_messages(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        tail_only_cursor_messages(messages)
    }
}

// ============================================================================
// Bi-directional Conversions (OpenAI <-> Claude)
// ============================================================================

/// Converts an OpenAI ChatCompletionRequest into a ClaudeMessageRequest using resolved StructuredOutputMode.
pub fn openai_to_claude_request(req: &ChatCompletionRequest) -> ClaudeMessageRequest {
    openai_to_claude_request_with_mode(req, resolve_structured_output_mode(None))
}

/// Converts an OpenAI ChatCompletionRequest into a ClaudeMessageRequest with explicit StructuredOutputMode.
pub fn openai_to_claude_request_with_mode(
    req: &ChatCompletionRequest,
    mode: StructuredOutputMode,
) -> ClaudeMessageRequest {
    let mut system_parts = Vec::new();
    let mut non_system_messages = Vec::new();

    for msg in &req.messages {
        if msg.role == "system" {
            if let Some(content) = &msg.content {
                system_parts.push(content.as_text());
            }
        } else {
            non_system_messages.push(msg);
        }
    }

    let mut metadata = req.metadata.clone();

    // Structured output handling:
    if let Some(rf) = &req.response_format {
        let is_json_schema = rf.get("type").and_then(|t| t.as_str()) == Some("json_schema");
        if is_json_schema {
            match mode {
                StructuredOutputMode::Passthrough => {
                    // When mode is passthrough, preserve raw response schemas
                    let mut meta_map = match metadata.take() {
                        Some(serde_json::Value::Object(m)) => m,
                        _ => serde_json::Map::new(),
                    };
                    meta_map.insert("response_format".to_string(), rf.clone());
                    metadata = Some(serde_json::Value::Object(meta_map));
                }
                StructuredOutputMode::Auto | StructuredOutputMode::Fallback => {
                    // When auto or fallback, run JSON schema conversion: inject schema prompt into system prompt
                    let schema_val = rf
                        .get("json_schema")
                        .and_then(|js| js.get("schema"))
                        .or_else(|| rf.get("schema"));
                    if let Some(schema) = schema_val {
                        let normalized = normalize_openai_strict_schema(schema);
                        let schema_str = serde_json::to_string_pretty(&normalized)
                            .unwrap_or_else(|_| schema.to_string());
                        let prompt = build_json_schema_prompt(&schema_str);
                        system_parts.push(prompt);
                    }
                }
            }
        }
    }

    let system = if !system_parts.is_empty() {
        Some(ClaudeSystem::Single(system_parts.join("\n\n")))
    } else {
        None
    };

    let mut claude_messages: Vec<ClaudeMessage> = Vec::new();

    for msg in non_system_messages {
        let (role, mut blocks) = match msg.role.as_str() {
            "assistant" => {
                let mut b = Vec::new();
                if let Some(content) = &msg.content {
                    let text = content.as_text();
                    if !text.is_empty() {
                        b.push(ClaudeBlock::Text {
                            text,
                            cache_control: None,
                        });
                    }
                }
                if let Some(calls) = &msg.tool_calls {
                    for call in calls {
                        let input = serde_json::from_str(&call.function.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        b.push(ClaudeBlock::ToolUse {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            input,
                        });
                    }
                }
                ("assistant", b)
            }
            "tool" => {
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                let content_val = msg.content.as_ref().map(|c| {
                    serde_json::Value::String(c.as_text())
                });
                let b = vec![ClaudeBlock::ToolResult {
                    tool_use_id,
                    content: content_val,
                    is_error: None,
                }];
                ("user", b)
            }
            _ => {
                // user or other
                let mut b = Vec::new();
                if let Some(content) = &msg.content {
                    match content {
                        MessageContent::Text(t) => {
                            b.push(ClaudeBlock::Text {
                                text: t.clone(),
                                cache_control: None,
                            });
                        }
                        MessageContent::Parts(parts) => {
                            for part in parts {
                                if part.part_type == "text" {
                                    if let Some(text) = &part.text {
                                        b.push(ClaudeBlock::Text {
                                            text: text.clone(),
                                            cache_control: None,
                                        });
                                    }
                                } else if part.part_type == "image_url" {
                                    if let Some(img) = &part.image_url {
                                        if let Some((media_type, data)) = parse_data_uri(&img.url) {
                                            b.push(ClaudeBlock::Image {
                                                source: ClaudeImageSource {
                                                    source_type: "base64".to_string(),
                                                    media_type,
                                                    data,
                                                },
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                ("user", b)
            }
        };

        if blocks.is_empty() {
            continue;
        }

        // Merge adjacent same-role messages to comply with Anthropic alternating turn rules
        if let Some(last) = claude_messages.last_mut() {
            if last.role == role {
                match &mut last.content {
                    ClaudeContent::Single(prev_text) => {
                        let mut new_blocks = vec![ClaudeBlock::Text {
                            text: prev_text.clone(),
                            cache_control: None,
                        }];
                        new_blocks.append(&mut blocks);
                        last.content = ClaudeContent::Blocks(new_blocks);
                    }
                    ClaudeContent::Blocks(prev_blocks) => {
                        prev_blocks.append(&mut blocks);
                    }
                }
                continue;
            }
        }

        claude_messages.push(ClaudeMessage {
            role: role.to_string(),
            content: ClaudeContent::Blocks(blocks),
        });
    }

    let tools = req.tools.as_ref().map(|list| {
        list.iter()
            .map(|t| ClaudeTool {
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                input_schema: t
                    .function
                    .parameters
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"})),
            })
            .collect()
    });

    let stop_sequences = req.stop.as_ref().map(|s| match s {
        StopCondition::Single(str_val) => vec![str_val.clone()],
        StopCondition::Multiple(vec_val) => vec_val.clone(),
    });

    let max_tokens = req
        .max_tokens
        .or(req.max_completion_tokens)
        .unwrap_or(4096);

    ClaudeMessageRequest {
        model: req.model.clone(),
        messages: claude_messages,
        max_tokens,
        system,
        metadata,
        stop_sequences,
        stream: req.stream,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        tools,
        tool_choice: req.tool_choice.clone(),
    }
}

/// Converts a ClaudeMessageRequest into an OpenAI ChatCompletionRequest using resolved StructuredOutputMode.
pub fn claude_to_openai_request(req: &ClaudeMessageRequest) -> ChatCompletionRequest {
    claude_to_openai_request_with_mode(req, resolve_structured_output_mode(None))
}

/// Converts a ClaudeMessageRequest into an OpenAI ChatCompletionRequest with explicit StructuredOutputMode.
pub fn claude_to_openai_request_with_mode(
    req: &ClaudeMessageRequest,
    mode: StructuredOutputMode,
) -> ChatCompletionRequest {
    let mut messages = Vec::new();

    if let Some(system) = &req.system {
        let text = system.as_text();
        if !text.is_empty() {
            messages.push(ChatMessage::system(text));
        }
    }

    for msg in &req.messages {
        match &msg.content {
            ClaudeContent::Single(text) => {
                messages.push(ChatMessage {
                    role: msg.role.clone(),
                    content: Some(MessageContent::Text(text.clone())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            ClaudeContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();

                for block in blocks {
                    match block {
                        ClaudeBlock::Text { text, .. } => {
                            text_parts.push(text.clone());
                        }
                        ClaudeBlock::Image { source } => {
                            let url = format!("data:{};base64,{}", source.media_type, source.data);
                            // We can represent as a part if needed, or text placeholder
                            text_parts.push(format!("[image: {}]", url));
                        }
                        ClaudeBlock::ToolUse { id, name, input } => {
                            tool_calls.push(ChatToolCall {
                                id: id.clone(),
                                tool_type: "function".to_string(),
                                function: FunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        ClaudeBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            let content_str = match content {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                                None => String::new(),
                            };
                            tool_results.push((tool_use_id.clone(), content_str));
                        }
                    }
                }

                // If tool results exist, push each as a separate tool message
                for (id, content_str) in tool_results {
                    messages.push(ChatMessage::tool(content_str, id));
                }

                if !text_parts.is_empty() || !tool_calls.is_empty() {
                    let content = if text_parts.is_empty() {
                        None
                    } else {
                        Some(MessageContent::Text(text_parts.join("\n")))
                    };

                    let tc = if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    };

                    messages.push(ChatMessage {
                        role: msg.role.clone(),
                        content,
                        name: None,
                        tool_calls: tc,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    let tools = req.tools.as_ref().map(|list| {
        list.iter()
            .map(|t| ChatTool {
                tool_type: "function".to_string(),
                function: ChatFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: Some(t.input_schema.clone()),
                },
            })
            .collect()
    });

    let stop = req
        .stop_sequences
        .as_ref()
        .map(|seqs| StopCondition::Multiple(seqs.clone()));

    let response_format = if let Some(meta) = &req.metadata {
        if let Some(rf) = meta.get("response_format") {
            match mode {
                StructuredOutputMode::Passthrough => {
                    // When mode is passthrough, preserve raw response schemas
                    Some(rf.clone())
                }
                StructuredOutputMode::Auto | StructuredOutputMode::Fallback => {
                    // When auto or fallback, run JSON schema conversion
                    let mut norm_rf = rf.clone();
                    if let Some(schema) = norm_rf
                        .get_mut("json_schema")
                        .and_then(|js| js.get_mut("schema"))
                    {
                        *schema = normalize_openai_strict_schema(schema);
                    }
                    Some(norm_rf)
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        top_p: req.top_p,
        n: None,
        stream: req.stream,
        stop,
        max_tokens: Some(req.max_tokens),
        max_completion_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools,
        tool_choice: req.tool_choice.clone(),
        response_format,
        metadata: req.metadata.clone(),
    }
}

/// Converts a ClaudeMessageResponse into an OpenAI ChatCompletionResponse.
pub fn claude_to_openai_response(res: &ClaudeMessageResponse) -> ChatCompletionResponse {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &res.content {
        match block {
            ClaudeBlock::Text { text, .. } => {
                text_parts.push(text.clone());
            }
            ClaudeBlock::ToolUse { id, name, input } => {
                tool_calls.push(ChatToolCall {
                    id: id.clone(),
                    tool_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(input).unwrap_or_default(),
                    },
                });
            }
            _ => {}
        }
    }

    let finish_reason = match res.stop_reason.as_deref() {
        Some("end_turn") => Some("stop".to_string()),
        Some("tool_use") => Some("tool_calls".to_string()),
        Some("max_tokens") => Some("length".to_string()),
        Some("stop_sequence") => Some("stop".to_string()),
        other => other.map(|s| s.to_string()),
    };

    let prompt_tokens = res.usage.input_tokens
        + res.usage.cache_creation_input_tokens.unwrap_or(0)
        + res.usage.cache_read_input_tokens.unwrap_or(0);
    let completion_tokens = res.usage.output_tokens;
    let total_tokens = prompt_tokens + completion_tokens;

    let content = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join("\n"))
    };

    let tc = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    };

    ChatCompletionResponse {
        id: format!("chatcmpl-{}", res.id),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: res.model.clone(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatResponseMessage {
                role: "assistant".to_string(),
                content,
                tool_calls: tc,
            },
            finish_reason,
        }],
        usage: Some(UsageInfo {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_tokens_details: None,
        }),
        response_schema: res.response_schema.clone(),
    }
}

/// Converts an OpenAI ChatCompletionResponse into a ClaudeMessageResponse.
pub fn openai_to_claude_response(res: &ChatCompletionResponse) -> ClaudeMessageResponse {
    let mut content = Vec::new();
    let mut stop_reason = None;

    if let Some(first_choice) = res.choices.first() {
        if let Some(text) = &first_choice.message.content {
            if !text.is_empty() {
                content.push(ClaudeBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                });
            }
        }
        if let Some(tool_calls) = &first_choice.message.tool_calls {
            for call in tool_calls {
                let input = serde_json::from_str(&call.function.arguments)
                    .unwrap_or(serde_json::Value::Null);
                content.push(ClaudeBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    input,
                });
            }
        }
        stop_reason = match first_choice.finish_reason.as_deref() {
            Some("stop") => Some("end_turn".to_string()),
            Some("tool_calls") => Some("tool_use".to_string()),
            Some("length") => Some("max_tokens".to_string()),
            other => other.map(|s| s.to_string()),
        };
    }

    let usage = if let Some(u) = &res.usage {
        ClaudeUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }
    } else {
        ClaudeUsage::default()
    };

    let id = res
        .id
        .strip_prefix("chatcmpl-")
        .unwrap_or(&res.id)
        .to_string();

    ClaudeMessageResponse {
        id: format!("msg_{}", id),
        message_type: "message".to_string(),
        role: "assistant".to_string(),
        model: res.model.clone(),
        content,
        stop_reason,
        stop_sequence: None,
        usage,
        response_schema: res.response_schema.clone(),
    }
}

fn parse_data_uri(uri: &str) -> Option<(String, String)> {
    if !uri.starts_with("data:") {
        return None;
    }
    let rest = &uri[5..];
    let (header, data) = rest.split_once(',')?;
    let (mime, encoding) = header.split_once(';')?;
    if encoding == "base64" {
        Some((mime.to_string(), data.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_to_claude_request_basic() {
        let openai_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                ChatMessage::system("You are a helpful assistant."),
                ChatMessage::user("Hello Claude!"),
            ],
            temperature: Some(0.7),
            max_tokens: Some(2048),
            ..Default::default()
        };

        let claude_req = openai_to_claude_request(&openai_req);
        assert_eq!(claude_req.model, "gpt-4o");
        assert_eq!(
            claude_req.system.unwrap().as_text(),
            "You are a helpful assistant."
        );
        assert_eq!(claude_req.messages.len(), 1);
        assert_eq!(claude_req.messages[0].role, "user");
        assert_eq!(claude_req.messages[0].content.as_text(), "Hello Claude!");
        assert_eq!(claude_req.max_tokens, 2048);
        assert_eq!(claude_req.temperature, Some(0.7));
    }

    #[test]
    fn test_claude_to_openai_request_with_tools() {
        let claude_req = ClaudeMessageRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![
                ClaudeMessage {
                    role: "user".to_string(),
                    content: ClaudeContent::Single("What's the weather?".to_string()),
                },
                ClaudeMessage {
                    role: "assistant".to_string(),
                    content: ClaudeContent::Blocks(vec![ClaudeBlock::ToolUse {
                        id: "call_123".to_string(),
                        name: "get_weather".to_string(),
                        input: serde_json::json!({"location": "San Francisco"}),
                    }]),
                },
                ClaudeMessage {
                    role: "user".to_string(),
                    content: ClaudeContent::Blocks(vec![ClaudeBlock::ToolResult {
                        tool_use_id: "call_123".to_string(),
                        content: Some(serde_json::json!("65 degrees and sunny")),
                        is_error: None,
                    }]),
                },
            ],
            max_tokens: 1024,
            system: Some(ClaudeSystem::Single("Be concise.".to_string())),
            metadata: None,
            stop_sequences: None,
            stream: Some(false),
            temperature: Some(0.5),
            top_p: None,
            top_k: None,
            tools: Some(vec![ClaudeTool {
                name: "get_weather".to_string(),
                description: Some("Get current weather".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    }
                }),
            }]),
            tool_choice: None,
        };

        let openai_req = claude_to_openai_request(&claude_req);
        assert_eq!(openai_req.model, "claude-3-5-sonnet");
        assert_eq!(openai_req.messages[0].role, "system");
        assert_eq!(openai_req.messages[0].content.as_ref().unwrap().as_text(), "Be concise.");
        assert_eq!(openai_req.messages[1].role, "user");
        assert_eq!(openai_req.messages[2].role, "assistant");
        let tc = openai_req.messages[2].tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].id, "call_123");
        assert_eq!(tc[0].function.name, "get_weather");
        assert_eq!(openai_req.messages[3].role, "tool");
        assert_eq!(openai_req.messages[3].tool_call_id.as_deref(), Some("call_123"));

        let tools = openai_req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "get_weather");
    }

    #[test]
    fn test_bidirectional_response_conversion() {
        let claude_resp = ClaudeMessageResponse {
            id: "12345".to_string(),
            message_type: "message".to_string(),
            role: "assistant".to_string(),
            model: "claude-3-5-sonnet".to_string(),
            content: vec![ClaudeBlock::Text {
                text: "Hello there!".to_string(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: ClaudeUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
            response_schema: None,
        };

        let openai_resp = claude_to_openai_response(&claude_resp);
        assert_eq!(openai_resp.id, "chatcmpl-12345");
        assert_eq!(openai_resp.choices[0].message.content.as_deref(), Some("Hello there!"));
        assert_eq!(openai_resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(openai_resp.usage.as_ref().unwrap().prompt_tokens, 10);
        assert_eq!(openai_resp.usage.as_ref().unwrap().completion_tokens, 5);

        // Convert back
        let back_to_claude = openai_to_claude_response(&openai_resp);
        assert_eq!(back_to_claude.id, "msg_12345");
        assert_eq!(back_to_claude.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(back_to_claude.content.len(), 1);
        if let ClaudeBlock::Text { text, .. } = &back_to_claude.content[0] {
            assert_eq!(text, "Hello there!");
        } else {
            panic!("Expected text block");
        }
    }

    #[test]
    fn test_structured_output_mode_resolution() {
        assert_eq!(
            resolve_structured_output_mode(Some("passthrough")),
            StructuredOutputMode::Passthrough
        );
        assert_eq!(
            resolve_structured_output_mode(Some("PASSTHROUGH")),
            StructuredOutputMode::Passthrough
        );
        assert_eq!(
            resolve_structured_output_mode(Some("fallback")),
            StructuredOutputMode::Fallback
        );
        assert_eq!(
            resolve_structured_output_mode(Some("auto")),
            StructuredOutputMode::Auto
        );
        assert_eq!(
            resolve_structured_output_mode(Some("unknown_mode")),
            StructuredOutputMode::Auto
        );
    }

    #[test]
    fn test_normalize_openai_strict_schema() {
        let raw = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "nested": {
                    "type": "object",
                    "properties": {
                        "count": { "type": "integer" }
                    }
                }
            },
            "required": ["name"]
        });

        let normalized = normalize_openai_strict_schema(&raw);
        assert_eq!(normalized["additionalProperties"], false);
        let req = normalized["required"].as_array().unwrap();
        assert!(req.contains(&serde_json::json!("name")));
        assert!(req.contains(&serde_json::json!("nested")));

        let nested = &normalized["properties"]["nested"];
        assert_eq!(nested["additionalProperties"], false);
        assert!(nested["required"].as_array().unwrap().contains(&serde_json::json!("count")));
    }

    #[test]
    fn test_apply_structured_output_mode_passthrough_and_fallback() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } }
        });

        // 1. Passthrough: preserves raw response schemas
        let mut req_passthrough = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("weather")],
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": "weather_spec", "schema": schema.clone() }
            })),
            ..Default::default()
        };
        apply_structured_output_mode(&mut req_passthrough, StructuredOutputMode::Passthrough);
        assert_eq!(
            req_passthrough.response_format.as_ref().unwrap()["type"],
            "json_schema"
        );
        assert_eq!(req_passthrough.messages.len(), 1);

        // 2. Fallback: downgrades to json_object and injects prompt into system message
        let mut req_fallback = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("weather")],
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": "weather_spec", "schema": schema }
            })),
            ..Default::default()
        };
        apply_structured_output_mode(&mut req_fallback, StructuredOutputMode::Fallback);
        assert_eq!(
            req_fallback.response_format.as_ref().unwrap()["type"],
            "json_object"
        );
        assert_eq!(req_fallback.messages.len(), 2);
        assert_eq!(req_fallback.messages[0].role, "system");
        assert!(req_fallback.messages[0].content.as_ref().unwrap().as_text().contains("strictly follows this JSON schema"));
    }

    #[test]
    fn test_openai_to_claude_structured_output_passthrough() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "supplier": { "type": "string" } }
        });

        let req = ChatCompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![ChatMessage::user("hi")],
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": "supplier_schema", "schema": schema.clone() }
            })),
            ..Default::default()
        };

        let claude_req = openai_to_claude_request_with_mode(&req, StructuredOutputMode::Passthrough);
        assert!(claude_req.system.is_none());
        assert!(claude_req.metadata.is_some());
        let meta = claude_req.metadata.as_ref().unwrap();
        assert_eq!(meta["response_format"]["type"], "json_schema");
        assert_eq!(meta["response_format"]["json_schema"]["name"], "supplier_schema");

        // Convert back to openai
        let back_openai = claude_to_openai_request_with_mode(&claude_req, StructuredOutputMode::Passthrough);
        assert_eq!(back_openai.response_format.unwrap()["type"], "json_schema");
    }

    #[test]
    fn test_openai_to_claude_structured_output_auto_and_fallback() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "order_id": { "type": "number" } }
        });

        let req = ChatCompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![ChatMessage::user("give me order")],
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": "order_schema", "schema": schema }
            })),
            ..Default::default()
        };

        // Auto mode runs JSON schema conversion
        let claude_auto = openai_to_claude_request_with_mode(&req, StructuredOutputMode::Auto);
        let sys_text = claude_auto.system.as_ref().unwrap().as_text();
        assert!(sys_text.contains("strictly follows this JSON schema"));
        assert!(sys_text.contains("\"additionalProperties\": false"));

        // Fallback mode runs JSON schema conversion
        let claude_fallback = openai_to_claude_request_with_mode(&req, StructuredOutputMode::Fallback);
        let fallback_sys = claude_fallback.system.as_ref().unwrap().as_text();
        assert!(fallback_sys.contains("strictly follows this JSON schema"));
    }

    #[test]
    fn test_kiro_tool_call_repair_buffer_streaming_fragments() {
        let mut buffer = KiroToolCallRepairBuffer::new();
        assert_eq!(buffer.max_bytes, DEFAULT_KIRO_TOOL_CALL_REPAIR_BUFFER_MAX_BYTES);
        assert_eq!(buffer.buffered_bytes(), 0);

        // Feed fragments of a tool call
        buffer
            .append_tool_fragment("call_1", Some("get_stock"), "{\"sym")
            .unwrap();
        buffer
            .append_tool_fragment("call_1", None, "bol\": \"GOOG\", ")
            .unwrap();
        buffer
            .append_tool_fragment("call_1", None, "\"currency\": \"USD\"}")
            .unwrap();

        assert!(buffer.buffered_bytes() > 0);

        let tool_call = buffer.finalize_tool_call("call_1").unwrap();
        assert_eq!(tool_call.id, "call_1");
        assert_eq!(tool_call.function.name, "get_stock");
        let parsed: serde_json::Value = serde_json::from_str(&tool_call.function.arguments).unwrap();
        assert_eq!(parsed["symbol"], "GOOG");
        assert_eq!(parsed["currency"], "USD");
        assert_eq!(buffer.buffered_bytes(), 0);
    }

    #[test]
    fn test_kiro_tool_call_repair_unclosed_json_variants() {
        let mut buffer = KiroToolCallRepairBuffer::new();

        // 1. Truncated closing brace
        buffer
            .append_tool_fragment("t1", Some("search"), "{\"query\": \"deep space\"")
            .unwrap();
        let call1 = buffer.finalize_tool_call("t1").unwrap();
        let parsed1: serde_json::Value = serde_json::from_str(&call1.function.arguments).unwrap();
        assert_eq!(parsed1["query"], "deep space");

        // 2. Trailing comma before EOF
        buffer
            .append_tool_fragment("t2", Some("lookup"), "{\"id\": 42,}")
            .unwrap();
        let call2 = buffer.finalize_tool_call("t2").unwrap();
        let parsed2: serde_json::Value = serde_json::from_str(&call2.function.arguments).unwrap();
        assert_eq!(parsed2["id"], 42);

        // 3. Unclosed array inside object
        buffer
            .append_tool_fragment("t3", Some("batch"), "{\"items\": [\"a\", \"b\"")
            .unwrap();
        let call3 = buffer.finalize_tool_call("t3").unwrap();
        let parsed3: serde_json::Value = serde_json::from_str(&call3.function.arguments).unwrap();
        assert_eq!(parsed3["items"], serde_json::json!(["a", "b"]));

        // 4. Unclosed string literal
        buffer
            .append_tool_fragment("t4", Some("echo"), "{\"msg\": \"hello world")
            .unwrap();
        let call4 = buffer.finalize_tool_call("t4").unwrap();
        let parsed4: serde_json::Value = serde_json::from_str(&call4.function.arguments).unwrap();
        assert_eq!(parsed4["msg"], "hello world");
    }

    #[test]
    fn test_kiro_tool_call_repair_nested_mcp() {
        let mut buffer = KiroToolCallRepairBuffer::new();
        // Kiro nested MCP format: name="tool_call", input={"name": "filesystem_read", "arguments": {"path": "file.txt"}}
        buffer
            .append_tool_fragment(
                "call_mcp",
                Some("tool_call"),
                "{\"name\": \"filesystem_read\", \"arguments\": {\"path\": \"/src/main.rs\"}}",
            )
            .unwrap();

        let call = buffer.finalize_tool_call("call_mcp").unwrap();
        assert_eq!(call.function.name, "filesystem_read");
        let parsed: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(parsed["path"], "/src/main.rs");
    }

    #[test]
    fn test_kiro_tool_call_repair_buffer_exceeded() {
        let mut buffer = KiroToolCallRepairBuffer::with_limits(true, 30);
        let chunk = "0123456789012345678901234567891"; // 31 bytes
        let res = buffer.append_tool_fragment("tool_large", Some("fn"), chunk);
        assert!(res.is_err());
        match res.unwrap_err() {
            ToolRepairError::BufferExceeded(msg) => {
                assert!(msg.contains("kiro_integrity_buffer_exceeded"));
            }
            other => panic!("Expected BufferExceeded, got {:?}", other),
        }
    }

    #[test]
    fn test_kiro_tool_call_repair_disabled() {
        let mut buffer = KiroToolCallRepairBuffer::with_limits(false, 1024);
        buffer
            .append_tool_fragment("call_broken", Some("fn"), "{\"unclosed_key\":")
            .unwrap();
        let res = buffer.finalize_tool_call("call_broken");
        assert!(res.is_err());
    }

    #[test]
    fn test_cursor_tail_only_messages() {
        let mut messages = vec![ChatMessage::system("be helpful")];
        for i in 0..90 {
            messages.push(ChatMessage::user(format!("m{}", i)));
        }
        messages.push(ChatMessage::assistant("working on it"));
        messages.push(ChatMessage::tool("result", "call_1"));
        messages.push(ChatMessage::user("final ask"));
        assert_eq!(messages.len(), 94);

        let trimmed = tail_only_cursor_messages(&messages);
        assert_eq!(trimmed.len(), 3);
        assert_eq!(trimmed[0].role, "system");
        assert_eq!(trimmed[0].content.as_ref().unwrap().as_text(), "be helpful");
        assert_eq!(trimmed[1].role, "system");
        assert!(trimmed[1].content.as_ref().unwrap().as_text().contains("94 messages trimmed"));
        assert_eq!(trimmed[2].role, "user");
        assert_eq!(trimmed[2].content.as_ref().unwrap().as_text(), "final ask");

        // Idempotency check: re-trimming does not duplicate [9router] notes
        let re_trimmed = tail_only_cursor_messages(&trimmed);
        let note_count = re_trimmed
            .iter()
            .filter(|m| m.content.as_ref().map(|c| c.as_text().contains("[9router]")).unwrap_or(false))
            .count();
        assert_eq!(note_count, 1);
    }

    #[test]
    fn test_cursor_empty_turn_retries_and_tunables() {
        let config = CursorRetryConfig::default();
        assert_eq!(config.max_retries, DEFAULT_CURSOR_EMPTY_TURN_MAX_RETRIES);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.tail_only_threshold, DEFAULT_CURSOR_TAIL_ONLY_THRESHOLD);
        assert_eq!(config.tail_only_threshold, 80);
        assert_eq!(config.retry_delay_ms, DEFAULT_CURSOR_EMPTY_TURN_RETRY_DELAY_MS);
        assert_eq!(config.retry_delay_ms, 400);

        // Verification of threshold check
        assert!(!should_retry_cursor_empty_turn(50, 80));
        assert!(should_retry_cursor_empty_turn(80, 80));
        assert!(should_retry_cursor_empty_turn(100, 80));

        // Test retry manager loop
        let mut manager = CursorTurnRetryManager::new(config, 90);

        // Attempt 0: empty turn with no output started -> should retry
        assert!(manager.should_retry(true, false));
        assert_eq!(manager.record_attempt(), 1);

        // Attempt 1: should retry
        assert!(manager.should_retry(true, false));
        assert_eq!(manager.record_attempt(), 2);

        // Attempt 2: should retry
        assert!(manager.should_retry(true, false));
        assert_eq!(manager.record_attempt(), 3);

        // Attempt 3: max retries reached (3) -> should NOT retry
        assert!(!manager.should_retry(true, false));

        // If output had already started, it should never retry
        let fresh_manager = CursorTurnRetryManager::new(CursorRetryConfig::default(), 90);
        assert!(!fresh_manager.should_retry(true, true));
    }
}
