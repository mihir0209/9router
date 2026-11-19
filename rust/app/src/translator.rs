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
// Bi-directional Conversions (OpenAI <-> Claude)
// ============================================================================

/// Converts an OpenAI ChatCompletionRequest into a ClaudeMessageRequest.
pub fn openai_to_claude_request(req: &ChatCompletionRequest) -> ClaudeMessageRequest {
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
        metadata: None,
        stop_sequences,
        stream: req.stream,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        tools,
        tool_choice: req.tool_choice.clone(),
    }
}

/// Converts a ClaudeMessageRequest into an OpenAI ChatCompletionRequest.
pub fn claude_to_openai_request(req: &ClaudeMessageRequest) -> ChatCompletionRequest {
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
        response_format: None,
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
}
