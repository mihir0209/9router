use std::collections::HashSet;
use std::time::Instant;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth;
use crate::db::{ApiKey, Database, ProviderConnection};
use crate::routes::{authenticate, chat_completions_handler, record_completion_usage_and_details, AppState};

const GEMINI_NATIVE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const IMAGE_BLOCK_TOKEN_ESTIMATE: u64 = 1600;
const DOCUMENT_BINARY_TOKEN_ESTIMATE: u64 = 1500;

// ============================================================================
// Gemini REST Schemas
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContent {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub response_modalities: Option<Vec<String>>,
    #[serde(default)]
    pub response_mime_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentRequest {
    #[serde(default)]
    pub contents: Vec<GeminiContent>,
    #[serde(default)]
    pub system_instruction: Option<GeminiContent>,
    #[serde(default)]
    pub generation_config: Option<GeminiGenerationConfig>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_config: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    pub content: GeminiContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub index: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u32,
    #[serde(default)]
    pub candidates_token_count: u32,
    #[serde(default)]
    pub total_token_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thoughts_token_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentResponse {
    pub candidates: Vec<GeminiCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiAction {
    GenerateContent,
    StreamGenerateContent,
    CountTokens,
}

impl GeminiAction {
    pub fn as_suffix(&self) -> &'static str {
        match self {
            GeminiAction::GenerateContent => ":generateContent",
            GeminiAction::StreamGenerateContent => ":streamGenerateContent",
            GeminiAction::CountTokens => ":countTokens",
        }
    }

    pub fn is_stream(&self) -> bool {
        matches!(self, GeminiAction::StreamGenerateContent)
    }
}

// ============================================================================
// Anthropic Token Counting (POST /v1/messages/count_tokens)
// ============================================================================

/// Recursively counts character length across arbitrary JSON values for token estimation.
fn count_value_chars(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::String(s) => s.chars().count(),
        Value::Number(n) => n.to_string().len(),
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::Array(arr) => arr.iter().map(count_value_chars).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| k.chars().count() + count_value_chars(v))
            .sum(),
    }
}

/// Counts text characters and discrete media tokens for a single Anthropic content block.
fn count_anthropic_block(block: &Value) -> (usize, u64) {
    match block {
        Value::Null => (0, 0),
        Value::String(s) => (s.chars().count(), 0),
        Value::Object(map) => {
            let block_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    let chars = map.get("text").map(count_value_chars).unwrap_or(0);
                    (chars, 0)
                }
                "thinking" => {
                    let chars = map.get("thinking").map(count_value_chars).unwrap_or(0);
                    (chars, 0)
                }
                "tool_use" => {
                    let name_chars = map.get("name").map(count_value_chars).unwrap_or(0);
                    let input_chars = map.get("input").map(count_value_chars).unwrap_or(0);
                    (name_chars + input_chars + 12, 0)
                }
                "tool_result" => {
                    let content_chars = map.get("content").map(count_value_chars).unwrap_or(0);
                    (content_chars + 8, 0)
                }
                "image" => (0, IMAGE_BLOCK_TOKEN_ESTIMATE),
                "document" => {
                    let title_chars = map.get("title").map(count_value_chars).unwrap_or(0);
                    let context_chars = map.get("context").map(count_value_chars).unwrap_or(0);
                    if let Some(source) = map.get("source").and_then(|v| v.as_object()) {
                        let src_type = source.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let media_type = source
                            .get("media_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if src_type == "text" || media_type == "text/plain" {
                            let data_chars = source.get("data").map(count_value_chars).unwrap_or(0);
                            return (title_chars + context_chars + data_chars, 0);
                        }
                        if src_type == "content" {
                            let inner_chars = source.get("content").map(count_value_chars).unwrap_or(0);
                            return (title_chars + context_chars + inner_chars, 0);
                        }
                    }
                    (title_chars + context_chars, DOCUMENT_BINARY_TOKEN_ESTIMATE)
                }
                _ => (count_value_chars(block), 0),
            }
        }
        other => (count_value_chars(other), 0),
    }
}

/// Estimates Anthropic `input_tokens` across `system`, `messages`, and `tools`.
pub fn estimate_anthropic_input_tokens(body: &Value) -> u64 {
    let mut total_chars: usize = 0;
    let mut media_tokens: u64 = 0;

    if let Some(system) = body.get("system") {
        match system {
            Value::String(s) => total_chars += s.chars().count(),
            Value::Array(parts) => {
                for part in parts {
                    let (c, m) = count_anthropic_block(part);
                    total_chars += c;
                    media_tokens += m;
                }
            }
            other => total_chars += count_value_chars(other),
        }
    }

    if let Some(tools) = body.get("tools") {
        total_chars += count_value_chars(tools);
    }

    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if let Some(role) = msg.get("role").and_then(|v| v.as_str()) {
                total_chars += role.len() + 4;
            }
            if let Some(content) = msg.get("content") {
                match content {
                    Value::String(s) => total_chars += s.chars().count(),
                    Value::Array(blocks) => {
                        for block in blocks {
                            let (c, m) = count_anthropic_block(block);
                            total_chars += c;
                            media_tokens += m;
                        }
                    }
                    other => total_chars += count_value_chars(other),
                }
            }
        }
    }

    let text_tokens = if total_chars == 0 {
        0
    } else {
        (total_chars as u64).div_ceil(4)
    };
    text_tokens + media_tokens
}

fn should_delegate_count_tokens_upstream(state: &AppState) -> bool {
    if std::env::var("COUNT_TOKENS_UPSTREAM")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    state
        .db
        .load_settings()
        .ok()
        .and_then(|s| s.get("countTokensUpstream").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

fn connection_access_token(conn: &ProviderConnection) -> Option<&str> {
    conn.data
        .extra
        .get("accessToken")
        .or_else(|| conn.data.extra.get("access_token"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

async fn try_upstream_anthropic_count_tokens(
    state: &AppState,
    body: &Value,
) -> Option<u64> {
    let conns = state.db.load_provider_connections().ok()?;
    let conn = conns.into_iter().find(|c| {
        c.is_active
            && c.provider.eq_ignore_ascii_case("anthropic")
            && (c.data.api_key.is_some() || connection_access_token(c).is_some())
    })?;

    let base = conn
        .data
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com")
        .trim_end_matches('/');
    let url = format!("{}/v1/messages/count_tokens", base);

    let mut req = state
        .proxy
        .client()
        .post(&url)
        .timeout(std::time::Duration::from_secs(5))
        .header(header::CONTENT_TYPE, "application/json")
        .header("anthropic-version", "2023-06-01");

    if let Some(key) = &conn.data.api_key {
        req = req.header("x-api-key", key);
    } else if let Some(token) = connection_access_token(&conn) {
        req = req.header(header::AUTHORIZATION, format!("Bearer {}", token));
    }

    let resp = req.json(body).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: Value = resp.json().await.ok()?;
    json.get("input_tokens").and_then(|v| v.as_u64())
}

/// `POST /v1/messages/count_tokens` — Anthropic token counting endpoint.
pub async fn count_tokens_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }

    if let Err(auth_err) = authenticate(&headers, &state.db) {
        return auth_err;
    }

    let parsed: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(v) if v.is_object() => v,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                ],
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": "Request body must be a JSON object"
                    }
                })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                ],
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("Invalid JSON request body: {}", e)
                    }
                })),
            )
                .into_response();
        }
    };

    let input_tokens = if should_delegate_count_tokens_upstream(&state) {
        match try_upstream_anthropic_count_tokens(&state, &parsed).await {
            Some(tokens) => tokens,
            None => estimate_anthropic_input_tokens(&parsed),
        }
    } else {
        estimate_anthropic_input_tokens(&parsed)
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "input_tokens": input_tokens })),
    )
        .into_response()
}

// ============================================================================
// Gemini Authentication & Path Parsing Helpers
// ============================================================================

/// Extracts a Gemini client API key from `Authorization: Bearer`, `x-goog-api-key`,
/// `x-api-key`, `api-key`, or `?key=` query parameter.
pub fn extract_gemini_api_key(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(k) = auth::extract_api_key(headers) {
        return Some(k);
    }
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "key" && !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn gemini_error_response(status: StatusCode, grpc_status: &str, message: impl Into<String>) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({
            "error": {
                "code": status.as_u16(),
                "message": message.into(),
                "status": grpc_status
            }
        })),
    )
        .into_response()
}

fn authenticate_gemini(headers: &HeaderMap, uri: &Uri, db: &Database) -> Result<ApiKey, Response> {
    if let Some(key_str) = extract_gemini_api_key(headers, uri) {
        if let Ok(Some(k)) = db.get_api_key_by_key(&key_str) {
            match db.validate_key(&k) {
                Ok(()) => return Ok(k),
                Err(crate::db::KeyValidationError::RateLimited {
                    limit,
                    window_seconds,
                }) => {
                    let mut res = gemini_error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "RESOURCE_EXHAUSTED",
                        format!("Rate limit exceeded: max {} requests per {}s.", limit, window_seconds),
                    );
                    res.headers_mut()
                        .insert(header::RETRY_AFTER, "1".parse().unwrap());
                    return Err(res);
                }
                Err(_) => {
                    return Err(gemini_error_response(
                        StatusCode::UNAUTHORIZED,
                        "UNAUTHENTICATED",
                        "Invalid or expired API key.",
                    ));
                }
            }
        }
        return Err(gemini_error_response(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "Invalid API key.",
        ));
    }

    if let Ok(settings) = db.load_settings() {
        if let Some(false) = settings.get("requireApiKey").and_then(|v| v.as_bool()) {
            return Ok(ApiKey {
                id: "no-auth-default".to_string(),
                key: "no-auth".to_string(),
                name: Some("Local Default".to_string()),
                ..ApiKey::default()
            });
        }
    }

    Err(gemini_error_response(
        StatusCode::UNAUTHORIZED,
        "UNAUTHENTICATED",
        "API key required. Provide via 'Authorization: Bearer <key>', 'x-goog-api-key', or '?key=<key>'.",
    ))
}

/// Parses `/v1beta/models/{*path}` into `(model_name, GeminiAction)`.
pub fn parse_gemini_model_action(raw_path: &str) -> Result<(String, GeminiAction), String> {
    let trimmed = raw_path.trim().trim_start_matches('/');
    let without_prefix = trimmed.strip_prefix("models/").unwrap_or(trimmed);

    let (model_part, action) = if let Some(m) = without_prefix.strip_suffix(":streamGenerateContent") {
        (m, GeminiAction::StreamGenerateContent)
    } else if let Some(m) = without_prefix.strip_suffix(":generateContent") {
        (m, GeminiAction::GenerateContent)
    } else if let Some(m) = without_prefix.strip_suffix(":countTokens") {
        (m, GeminiAction::CountTokens)
    } else {
        return Err(format!(
            "Unsupported or missing Gemini action in path '{}'. Expected :generateContent, :streamGenerateContent, or :countTokens.",
            raw_path
        ));
    };

    let model = model_part.trim_matches('/');
    if model.is_empty() || model.contains("..") {
        return Err(format!("Invalid model identifier in path '{}'.", raw_path));
    }

    Ok((model.to_string(), action))
}

// ============================================================================
// GET /v1beta/models & GET /v1beta/models/{*path}
// ============================================================================

/// `GET /v1beta/models` — Returns Gemini-format models list filtered by active connections and key permissions.
pub async fn v1beta_models_list_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }

    let api_key = match authenticate_gemini(&headers, &uri, &state.db) {
        Ok(k) => k,
        Err(auth_res) => return auth_res,
    };

    let mut models = Vec::new();
    let mut seen = HashSet::new();

    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let active_conns: Vec<_> = all_conns.into_iter().filter(|c| c.is_active).collect();
    let empty_active = active_conns.is_empty();

    let has_google = active_conns
        .iter()
        .any(|c| matches!(c.provider.to_ascii_lowercase().as_str(), "gemini" | "google" | "vertex"));
    let has_openai = active_conns.iter().any(|c| c.provider.eq_ignore_ascii_case("openai"));
    let has_anthropic = active_conns.iter().any(|c| c.provider.eq_ignore_ascii_case("anthropic"));
    let has_deepseek = active_conns.iter().any(|c| c.provider.eq_ignore_ascii_case("deepseek"));

    let display_names = state.db.load_model_display_names().unwrap_or_default();
    let disabled_models = state.db.load_disabled_models().unwrap_or_default();
    let custom_models = state.db.load_custom_models().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let mut push_model = |raw_name: &str, check_id: &str, display_name: &str, desc: &str| {
        if state.db.check_model_allowed(&api_key, check_id).is_err() {
            return;
        }
        let canonical = if raw_name.starts_with("models/") {
            raw_name.to_string()
        } else {
            format!("models/{}", raw_name)
        };
        if seen.insert(canonical.clone()) {
            models.push(json!({
                "name": canonical,
                "version": "1.0",
                "displayName": display_name,
                "description": desc,
                "inputTokenLimit": 128000,
                "outputTokenLimit": 8192,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
            }));
        }
    };

    // 1. Standard Gemini models when Google/Gemini/Vertex is active (or no connections configured yet)
    if has_google || empty_active {
        let gemini_catalog = [
            ("gemini-2.5-pro", "Gemini 2.5 Pro"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash"),
            ("gemini-2.0-flash", "Gemini 2.0 Flash"),
            ("gemini-3.8-flash", "Gemini 3.8 Flash"),
            ("gemini-3.8-flash-medium", "Gemini 3.8 Flash Medium"),
            ("gemini-3.8-flash-high", "Gemini 3.8 Flash High"),
            ("gemini-4-carbon", "Gemini 4 Carbon"),
        ];
        for (id, default_disp) in gemini_catalog {
            let disp = display_names.get(id).map(|s| s.as_str()).unwrap_or(default_disp);
            push_model(id, id, disp, &format!("Gemini model: {}", disp));
        }
    }

    // 2. Other active provider catalog models exposed under models/<provider>/<id>
    let other_catalog = [
        ("openai", "gpt-4o", "GPT-4o", has_openai),
        ("openai", "gpt-4o-mini", "GPT-4o Mini", has_openai),
        ("anthropic", "claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet", has_anthropic),
        ("anthropic", "claude-3-5-haiku-20241022", "Claude 3.5 Haiku", has_anthropic),
        ("deepseek", "deepseek-chat", "DeepSeek Chat", has_deepseek),
    ];
    for (prov, id, default_disp, enabled) in other_catalog {
        if enabled {
            let disp = display_names.get(id).map(|s| s.as_str()).unwrap_or(default_disp);
            let namespaced = format!("{}/{}", prov, id);
            push_model(&namespaced, id, disp, &format!("{} model: {}", prov, disp));
        }
    }

    // 3. Combos
    if let Ok(combos) = state.db.load_combos() {
        for combo in combos {
            push_model(
                &combo.name,
                &combo.name,
                &combo.name,
                &format!("Combo: {}", combo.name),
            );
        }
    }

    // 4. Custom models on active provider nodes
    for node in &provider_nodes {
        let has_active_conn = active_conns.iter().any(|c| {
            c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider)
        });
        if !has_active_conn && !empty_active {
            continue;
        }
        let disabled_set: HashSet<&str> = disabled_models
            .get(&node.id)
            .or_else(|| disabled_models.get(&node.prefix))
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        for cm in &custom_models {
            let pa = cm
                .get("providerAlias")
                .or_else(|| cm.get("provider"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if pa != node.id && pa != node.prefix && node.name.as_deref() != Some(pa) {
                continue;
            }
            let raw_id = cm.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
            if raw_id.is_empty() || disabled_set.contains(raw_id) {
                continue;
            }
            let disp = cm
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(raw_id);
            let namespaced = if node.prefix.is_empty() {
                raw_id.to_string()
            } else {
                format!("{}/{}", node.prefix, raw_id)
            };
            push_model(&namespaced, raw_id, disp, &format!("Custom model: {}", disp));
        }
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "models": models })),
    )
        .into_response()
}

/// `GET /v1beta/models/{*path}` — Returns metadata for a single Gemini model.
pub async fn v1beta_model_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(model_path): Path<String>,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }

    let api_key = match authenticate_gemini(&headers, &uri, &state.db) {
        Ok(k) => k,
        Err(auth_res) => return auth_res,
    };

    let clean_model = model_path.trim_start_matches('/').strip_prefix("models/").unwrap_or(model_path.trim_start_matches('/'));
    if let Err(e) = state.db.check_model_allowed(&api_key, clean_model) {
        return gemini_error_response(StatusCode::FORBIDDEN, "PERMISSION_DENIED", e.to_string());
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({
            "name": format!("models/{}", clean_model),
            "version": "1.0",
            "displayName": clean_model,
            "description": format!("Model: {}", clean_model),
            "inputTokenLimit": 128000,
            "outputTokenLimit": 8192,
            "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
        })),
    )
        .into_response()
}

// ============================================================================
// Gemini Token Estimation & Request/Response Translation
// ============================================================================

fn count_gemini_parts(parts: &[GeminiPart]) -> (usize, u64) {
    let mut chars: usize = 0;
    let mut media_tokens: u64 = 0;
    for p in parts {
        if let Some(t) = &p.text {
            chars += t.chars().count();
        }
        if p.inline_data.is_some() {
            media_tokens += IMAGE_BLOCK_TOKEN_ESTIMATE;
        }
        if let Some(fc) = &p.function_call {
            chars += count_value_chars(fc);
        }
        if let Some(fr) = &p.function_response {
            chars += count_value_chars(fr);
        }
    }
    (chars, media_tokens)
}

/// Estimates prompt tokens for a Gemini `GenerateContentRequest`.
pub fn estimate_gemini_tokens(req: &GeminiGenerateContentRequest) -> u64 {
    let mut chars: usize = 0;
    let mut media_tokens: u64 = 0;

    if let Some(sys) = &req.system_instruction {
        let (c, m) = count_gemini_parts(&sys.parts);
        chars += c;
        media_tokens += m;
    }
    for c in &req.contents {
        chars += c.role.as_deref().map(str::len).unwrap_or(4) + 4;
        let (pc, pm) = count_gemini_parts(&c.parts);
        chars += pc;
        media_tokens += pm;
    }
    if let Some(tools) = &req.tools {
        for t in tools {
            chars += count_value_chars(t);
        }
    }

    let text_tokens = if chars == 0 {
        0
    } else {
        (chars as u64).div_ceil(4)
    };
    (text_tokens + media_tokens).max(1)
}

/// Converts a Gemini `GenerateContentRequest` into OpenAI chat-completions JSON.
pub fn gemini_request_to_openai_json(
    req: &GeminiGenerateContentRequest,
    model: &str,
    stream: bool,
) -> Value {
    let mut messages = Vec::new();

    if let Some(sys) = &req.system_instruction {
        let sys_text: String = sys
            .parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        if !sys_text.is_empty() {
            messages.push(json!({ "role": "system", "content": sys_text }));
        }
    }

    for content in &req.contents {
        let role = match content.role.as_deref() {
            Some("model") => "assistant",
            _ => "user",
        };

        let mut text_segments = Vec::new();
        let mut content_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut tool_results = Vec::new();

        for part in &content.parts {
            if let Some(txt) = &part.text {
                text_segments.push(txt.as_str());
            }
            if let Some(inline) = &part.inline_data {
                let mime = inline
                    .get("mimeType")
                    .or_else(|| inline.get("mime_type"))
                    .and_then(|v| v.as_str());
                let data = inline.get("data").and_then(|v| v.as_str());
                if let (Some(mime), Some(data)) = (mime, data) {
                    content_parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", mime, data)
                        }
                    }));
                }
            }
            if let Some(fc) = &part.function_call {
                let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": format!("call_{}", tool_calls.len()),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string())
                    }
                }));
            }
            if let Some(fr) = &part.function_response {
                let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                let resp_val = fr.get("response").cloned().unwrap_or(Value::Null);
                let resp_str = if let Some(s) = resp_val.as_str() {
                    s.to_string()
                } else {
                    serde_json::to_string(&resp_val).unwrap_or_default()
                };
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": format!("call_{}", name),
                    "name": name,
                    "content": resp_str
                }));
            }
        }

        let joined_text = text_segments.join("\n");
        if !tool_calls.is_empty() {
            let mut msg = json!({
                "role": "assistant",
                "tool_calls": tool_calls
            });
            if !joined_text.is_empty() {
                msg["content"] = json!(joined_text);
            }
            messages.push(msg);
        } else if !content_parts.is_empty() {
            if !joined_text.is_empty() {
                content_parts.insert(0, json!({ "type": "text", "text": joined_text }));
            }
            messages.push(json!({
                "role": role,
                "content": content_parts
            }));
        } else if !joined_text.is_empty() {
            messages.push(json!({
                "role": role,
                "content": joined_text
            }));
        }

        for tr in tool_results {
            messages.push(tr);
        }
    }

    let mut payload = json!({
        "model": model,
        "messages": messages,
        "stream": stream
    });

    if let Some(cfg) = &req.generation_config {
        if let Some(max_tokens) = cfg.max_output_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temp) = cfg.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(top_p) = cfg.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(stops) = &cfg.stop_sequences {
            payload["stop"] = json!(stops);
        }
    }

    if let Some(tools) = &req.tools {
        let mut openai_tools = Vec::new();
        for tool in tools {
            if let Some(decls) = tool.get("functionDeclarations").and_then(|v| v.as_array()) {
                for decl in decls {
                    if let Some(name) = decl.get("name").and_then(|v| v.as_str()) {
                        let mut func = json!({ "name": name });
                        if let Some(desc) = decl.get("description") {
                            func["description"] = desc.clone();
                        }
                        if let Some(params) = decl.get("parameters") {
                            func["parameters"] = params.clone();
                        }
                        openai_tools.push(json!({
                            "type": "function",
                            "function": func
                        }));
                    }
                }
            }
        }
        if !openai_tools.is_empty() {
            payload["tools"] = json!(openai_tools);
        }
    }

    payload
}

// ============================================================================
// Native Gemini Upstream Forwarding & Translated Fallback
// ============================================================================

fn find_native_gemini_connection<'a>(
    connections: &'a [ProviderConnection],
    model: &str,
) -> Option<&'a ProviderConnection> {
    // Only route directly to Google Generative Language API when model is unprefixed
    // or explicitly prefixed with `gemini/` or `google/`.
    if let Some((prefix, _)) = model.split_once('/') {
        if !prefix.eq_ignore_ascii_case("gemini") && !prefix.eq_ignore_ascii_case("google") {
            return None;
        }
    }

    connections.iter().find(|c| {
        c.is_active
            && (c.provider.eq_ignore_ascii_case("gemini") || c.provider.eq_ignore_ascii_case("google"))
            && (c.data.api_key.is_some() || connection_access_token(c).is_some())
    })
}

fn normalize_native_gemini_model(model: &str) -> &str {
    let m = model.strip_prefix("models/").unwrap_or(model);
    if let Some(rest) = m.strip_prefix("gemini/") {
        rest
    } else if let Some(rest) = m.strip_prefix("google/") {
        rest
    } else {
        m
    }
}

async fn forward_native_gemini_upstream(
    state: &AppState,
    api_key: &ApiKey,
    conn: &ProviderConnection,
    uri: &Uri,
    model: &str,
    action: &GeminiAction,
    raw_body: &[u8],
    start_time: Instant,
) -> Option<Response> {
    let model_id = normalize_native_gemini_model(model);
    let base = conn
        .data
        .base_url
        .as_deref()
        .unwrap_or(GEMINI_NATIVE_BASE_URL)
        .trim_end_matches('/');
    let mut url = format!("{}/{}{}", base, model_id, action.as_suffix());

    if let Some(q) = uri.query() {
        let filtered: Vec<&str> = q
            .split('&')
            .filter(|pair| !pair.starts_with("key="))
            .collect();
        if !filtered.is_empty() {
            url.push('?');
            url.push_str(&filtered.join("&"));
        }
    }

    let mut req = state
        .proxy
        .client()
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(k) = &conn.data.api_key {
        req = req.header("x-goog-api-key", k);
    } else if let Some(tok) = connection_access_token(conn) {
        req = req.header(header::AUTHORIZATION, format!("Bearer {}", tok));
    }

    let upstream_resp = match req.body(raw_body.to_vec()).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("[v1beta] Native Gemini upstream failed: {}", e);
            return None;
        }
    };

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream_resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if action.is_stream() && status.is_success() {
        let stream = upstream_resp.bytes_stream().map(|res| {
            res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        });
        record_completion_usage_and_details(
            state,
            api_key,
            "/v1beta/models",
            Some(&conn.provider),
            model_id,
            Some(&conn.id),
            "ok",
            0,
            0,
            0,
            start_time.elapsed().as_millis() as u64,
            None,
            None,
        );
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Body::from_stream(stream))
            .ok();
    }

    let bytes = match upstream_resp.bytes().await {
        Ok(b) => b,
        Err(_) => return None,
    };

    if status.is_success() {
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let usage = parsed.get("usageMetadata");
        let pt = usage
            .and_then(|u| u.get("promptTokenCount"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let ct = usage
            .and_then(|u| u.get("candidatesTokenCount"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        record_completion_usage_and_details(
            state,
            api_key,
            "/v1beta/models",
            Some(&conn.provider),
            model_id,
            Some(&conn.id),
            "ok",
            pt,
            ct,
            0,
            start_time.elapsed().as_millis() as u64,
            None,
            Some(&parsed),
        );
    }

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::from(bytes))
        .ok()
}

fn map_openai_finish_reason(fr: Option<&str>) -> &'static str {
    match fr {
        Some("length") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        _ => "STOP",
    }
}

async fn transform_openai_json_to_gemini(resp: Response, model_name: &str) -> Response {
    let (parts, body) = resp.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }

    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return gemini_error_response(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e.to_string());
        }
    };

    let openai_res: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return (parts.status, bytes).into_response(),
    };

    if openai_res.get("candidates").is_some() {
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(openai_res),
        )
            .into_response();
    }

    let choice = openai_res
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let mut gemini_parts = Vec::new();

    if let Some(ch) = choice {
        let msg = ch.get("message");
        if let Some(reasoning) = msg
            .and_then(|m| m.get("reasoning_content"))
            .and_then(|r| r.as_str())
        {
            if !reasoning.is_empty() {
                gemini_parts.push(GeminiPart {
                    text: Some(reasoning.to_string()),
                    thought: Some(true),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                });
            }
        }
        if let Some(content_text) = msg.and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
            if !content_text.is_empty() || msg.and_then(|m| m.get("tool_calls")).is_none() {
                gemini_parts.push(GeminiPart {
                    text: Some(content_text.to_string()),
                    thought: None,
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                });
            }
        }
        if let Some(tool_calls) = msg.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
            for tc in tool_calls {
                if let Some(func) = tc.get("function") {
                    let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let args_str = func
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let args: Value =
                        serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                    gemini_parts.push(GeminiPart {
                        text: None,
                        thought: None,
                        inline_data: None,
                        function_call: Some(json!({ "name": name, "args": args })),
                        function_response: None,
                    });
                }
            }
        }
    }

    let finish_reason = map_openai_finish_reason(
        choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str()),
    );

    let usage_metadata = openai_res.get("usage").map(|u| {
        let pt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let ct = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let tt = u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let thought = u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .map(|t| t as u32);

        GeminiUsageMetadata {
            prompt_token_count: pt,
            candidates_token_count: ct,
            total_token_count: tt,
            thoughts_token_count: thought,
        }
    });

    let gemini_resp = GeminiGenerateContentResponse {
        candidates: vec![GeminiCandidate {
            content: GeminiContent {
                role: Some("model".to_string()),
                parts: gemini_parts,
            },
            finish_reason: Some(finish_reason.to_string()),
            index: 0,
        }],
        usage_metadata,
        model_version: Some(
            openai_res
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or(model_name)
                .to_string(),
        ),
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(gemini_resp),
    )
        .into_response()
}

async fn transform_openai_sse_to_gemini_sse(resp: Response, model_name: String) -> Response {
    let (parts, body) = resp.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }

    let body_stream = body.into_data_stream();
    let output_stream = async_stream::stream! {
        let mut pinned = body_stream;
        let mut buffer = String::new();

        while let Some(chunk_result) = pinned.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(_) => break,
            };
            let text = match std::str::from_utf8(&chunk) {
                Ok(s) => s,
                Err(_) => continue,
            };
            buffer.push_str(text);

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer.drain(..=pos);

                if !line.starts_with("data:") {
                    continue;
                }
                let data = line["data:".len()..].trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let choice = match parsed
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                {
                    Some(c) => c,
                    None => continue,
                };

                let delta = choice.get("delta");
                let finish_reason = choice.get("finish_reason").and_then(|f| f.as_str());
                let mut parts_vec = Vec::new();

                if let Some(thought) = delta
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(|r| r.as_str())
                {
                    if !thought.is_empty() {
                        parts_vec.push(json!({ "text": thought, "thought": true }));
                    }
                }
                if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        parts_vec.push(json!({ "text": content }));
                    }
                }

                if parts_vec.is_empty() && finish_reason.is_none() {
                    continue;
                }

                let mut candidate = json!({
                    "content": {
                        "role": "model",
                        "parts": if parts_vec.is_empty() { vec![json!({ "text": "" })] } else { parts_vec }
                    },
                    "index": 0
                });

                if let Some(fr) = finish_reason {
                    candidate["finishReason"] = json!(map_openai_finish_reason(Some(fr)));
                }

                let mut chunk_obj = json!({ "candidates": [candidate] });
                if let Some(usage) = parsed.get("usage") {
                    chunk_obj["usageMetadata"] = json!({
                        "promptTokenCount": usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "candidatesTokenCount": usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "totalTokenCount": usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    });
                    chunk_obj["modelVersion"] = json!(
                        parsed.get("model").and_then(|v| v.as_str()).unwrap_or(&model_name)
                    );
                }

                let out_line = format!(
                    "data: {}\r\n\r\n",
                    serde_json::to_string(&chunk_obj).unwrap_or_default()
                );
                yield Ok::<_, std::io::Error>(axum::body::Bytes::from(out_line));
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::from_stream(output_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// POST /v1beta/models/{*path}
// ============================================================================

/// `POST /v1beta/models/{*path}` — Handles `:generateContent`, `:streamGenerateContent`, and `:countTokens`.
pub async fn v1beta_models_action_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(raw_path): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = state.rate_limiter.check_ip_headers(&headers) {
        return resp;
    }

    let start_time = Instant::now();
    let api_key = match authenticate_gemini(&headers, &uri, &state.db) {
        Ok(k) => k,
        Err(auth_res) => return auth_res,
    };

    let (target_model, action) = match parse_gemini_model_action(&raw_path) {
        Ok(parsed) => parsed,
        Err(msg) => return gemini_error_response(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", msg),
    };

    let clean_model = target_model
        .split_once('/')
        .map(|(_, m)| m)
        .unwrap_or(&target_model);

    if let Err(e) = state.db.check_model_allowed(&api_key, clean_model) {
        return gemini_error_response(StatusCode::FORBIDDEN, "PERMISSION_DENIED", e.to_string());
    }

    let gemini_req: GeminiGenerateContentRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return gemini_error_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                format!("Invalid JSON request body: {}", e),
            );
        }
    };

    if action == GeminiAction::CountTokens {
        let total_tokens = estimate_gemini_tokens(&gemini_req);
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({ "totalTokens": total_tokens })),
        )
            .into_response();
    }

    let estimated_tokens = estimate_gemini_tokens(&gemini_req);
    if let Err(resp) = state
        .rate_limiter
        .check_model_rate_limit(clean_model, estimated_tokens)
    {
        return resp;
    }

    // 1. Prefer direct native Gemini upstream if an active Gemini/Google connection exists
    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let is_combo = state
        .db
        .get_combo_by_name(&target_model)
        .ok()
        .flatten()
        .is_some();

    if !is_combo {
        if let Some(native_conn) = find_native_gemini_connection(&all_conns, &target_model) {
            if let Some(resp) = forward_native_gemini_upstream(
                &state,
                &api_key,
                native_conn,
                &uri,
                &target_model,
                &action,
                &body,
                start_time,
            )
            .await
            {
                return resp;
            }
        }
    }

    // 2. Fallback / multi-provider translation through chat_completions_handler
    let openai_payload = gemini_request_to_openai_json(&gemini_req, &target_model, action.is_stream());
    let mut chat_headers = headers.clone();
    chat_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    if !chat_headers.contains_key(header::AUTHORIZATION) {
        if let Ok(val) = format!("Bearer {}", api_key.key).parse() {
            chat_headers.insert(header::AUTHORIZATION, val);
        }
    }

    let payload_bytes = axum::body::Bytes::from(serde_json::to_vec(&openai_payload).unwrap_or_default());
    let internal_resp = chat_completions_handler(State(state), chat_headers, payload_bytes).await;

    if action.is_stream() {
        transform_openai_sse_to_gemini_sse(internal_resp, target_model).await
    } else {
        transform_openai_json_to_gemini(internal_resp, &target_model).await
    }
}

// ============================================================================
// Unit Tests & Latency Benchmarks
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;
    use std::time::Instant;

    fn test_app_state() -> (AppState, String) {
        let db = Database::open_in_memory().expect("open in-memory db");
        let raw_key = "sk_test_v1beta_123".to_string();
        let api_key = ApiKey {
            id: "key-v1beta-test".to_string(),
            name: Some("v1beta-test".to_string()),
            key: raw_key.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            ..ApiKey::default()
        };
        db.save_api_key(&api_key).expect("save api key");

        let config = Arc::new(Config::default());
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let console_buffer = crate::routes::ConsoleLogBuffer::new();
        let rate_limiter = Arc::new(crate::ratelimit::RateLimiter::from_config(&config));

        let state = AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer,
            rate_limiter,
        };
        (state, raw_key)
    }

    #[test]
    fn test_estimate_anthropic_input_tokens_all_block_types() {
        let payload = json!({
            "model": "claude-3-5-sonnet-20241022",
            "system": [
                { "type": "text", "text": "You are a helpful coding assistant." }
            ],
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get weather for a city",
                    "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } }
                }
            ],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Check the weather in Tokyo and inspect this image." },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                        { "type": "document", "title": "Spec", "source": { "type": "text", "media_type": "text/plain", "data": "Hello document text" } }
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": { "city": "Tokyo" } }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "tu_1", "content": "22C sunny" }
                    ]
                }
            ]
        });

        let tokens = estimate_anthropic_input_tokens(&payload);
        // Should include IMAGE_BLOCK_TOKEN_ESTIMATE (1600) + text/tool/system tokens
        assert!(tokens > 1650 && tokens < 1800, "unexpected token estimate: {}", tokens);
    }

    #[test]
    fn test_parse_gemini_model_action() {
        assert_eq!(
            parse_gemini_model_action("gemini-2.5-flash:generateContent").unwrap(),
            ("gemini-2.5-flash".to_string(), GeminiAction::GenerateContent)
        );
        assert_eq!(
            parse_gemini_model_action("models/gemini/gemini-2.5-pro:streamGenerateContent").unwrap(),
            ("gemini/gemini-2.5-pro".to_string(), GeminiAction::StreamGenerateContent)
        );
        assert_eq!(
            parse_gemini_model_action("anthropic/claude-3-5-sonnet-20241022:countTokens").unwrap(),
            ("anthropic/claude-3-5-sonnet-20241022".to_string(), GeminiAction::CountTokens)
        );
        assert!(parse_gemini_model_action("../secret:generateContent").is_err());
        assert!(parse_gemini_model_action("gemini-2.5-flash").is_err());
    }

    #[test]
    fn test_gemini_request_to_openai_json_translation() {
        let req: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "systemInstruction": { "parts": [{ "text": "Be concise." }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "Hi" }] },
                { "role": "model", "parts": [{ "functionCall": { "name": "ping", "args": { "x": 1 } } }] },
                { "role": "user", "parts": [{ "functionResponse": { "name": "ping", "response": { "ok": true } } }] }
            ],
            "generationConfig": { "temperature": 0.25, "maxOutputTokens": 512 },
            "tools": [{ "functionDeclarations": [{ "name": "ping", "description": "Ping", "parameters": { "type": "object" } }] }]
        }))
        .expect("deserialize");

        let openai = gemini_request_to_openai_json(&req, "gemini-2.5-flash", true);
        assert_eq!(openai["model"], "gemini-2.5-flash");
        assert_eq!(openai["stream"], true);
        assert_eq!(openai["max_tokens"], 512);
        assert_eq!(openai["messages"][0]["role"], "system");
        assert_eq!(openai["messages"][1]["role"], "user");
        assert_eq!(openai["messages"][2]["role"], "assistant");
        assert_eq!(openai["messages"][2]["tool_calls"][0]["function"]["name"], "ping");
        assert_eq!(openai["messages"][3]["role"], "tool");
        assert_eq!(openai["tools"][0]["function"]["name"], "ping");
    }

    #[tokio::test]
    async fn test_count_tokens_handler_auth_and_response() {
        let (state, key) = test_app_state();

        // Missing auth -> 401
        let unauth = count_tokens_handler(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from_static(br#"{"model":"claude-3-5-sonnet-20241022","messages":[]}"#),
        )
        .await;
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        // Valid auth -> 200 with input_tokens
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", key.parse().unwrap());
        let ok_resp = count_tokens_handler(
            State(state),
            headers,
            axum::body::Bytes::from_static(
                br#"{"model":"claude-3-5-sonnet-20241022","system":"sys","messages":[{"role":"user","content":"hello world"}]}"#,
            ),
        )
        .await;
        assert_eq!(ok_resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(ok_resp.into_body(), 65536).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(val["input_tokens"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_v1beta_models_list_and_count_tokens_action() {
        let (state, key) = test_app_state();
        let uri: Uri = format!("/v1beta/models?key={}", key).parse().unwrap();

        let list_resp = v1beta_models_list_handler(
            State(state.clone()),
            HeaderMap::new(),
            uri,
        )
        .await;
        assert_eq!(list_resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(list_resp.into_body(), 65536).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = val["models"].as_array().expect("models array");
        assert!(!arr.is_empty());
        assert!(arr.iter().any(|m| m["name"] == "models/gemini-2.5-flash"));

        // Test :countTokens via x-goog-api-key
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-api-key", key.parse().unwrap());
        let action_uri: Uri = "/v1beta/models/gemini-2.5-flash:countTokens".parse().unwrap();
        let count_resp = v1beta_models_action_handler(
            State(state),
            headers,
            action_uri,
            Path("gemini-2.5-flash:countTokens".to_string()),
            axum::body::Bytes::from_static(br#"{"contents":[{"role":"user","parts":[{"text":"Count these tokens please."}]}]}"#),
        )
        .await;
        assert_eq!(count_resp.status(), StatusCode::OK);
        let cbytes = axum::body::to_bytes(count_resp.into_body(), 65536).await.unwrap();
        let cval: Value = serde_json::from_slice(&cbytes).unwrap();
        assert!(cval["totalTokens"].as_u64().unwrap() > 0);
    }

    #[test]
    fn bench_count_tokens_and_gemini_translation_latency() {
        let claude_payload = json!({
            "model": "claude-3-5-sonnet-20241022",
            "system": "You are a Rust systems engineer.",
            "messages": [
                { "role": "user", "content": "Explain zero-copy parsing in Rust with examples." }
            ]
        });

        let iters = 10_000u32;
        let t0 = Instant::now();
        let mut acc = 0u64;
        for _ in 0..iters {
            acc += estimate_anthropic_input_tokens(&claude_payload);
        }
        let elapsed = t0.elapsed();
        assert!(acc > 0);
        let ns_per_op = elapsed.as_nanos() / (iters as u128);
        eprintln!(
            "[BENCH] estimate_anthropic_input_tokens: {} iters in {:?} ({} ns/op)",
            iters, elapsed, ns_per_op
        );
        assert!(ns_per_op < 100_000, "token estimation too slow: {} ns/op", ns_per_op);

        let gemini_req: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "systemInstruction": { "parts": [{ "text": "Be concise." }] },
            "contents": [{ "role": "user", "parts": [{ "text": "Hello Gemini" }] }]
        }))
        .unwrap();

        let t1 = Instant::now();
        let mut translated_len = 0usize;
        for _ in 0..iters {
            let out = gemini_request_to_openai_json(&gemini_req, "gemini-2.5-flash", false);
            translated_len += out["messages"].as_array().map(|a| a.len()).unwrap_or(0);
        }
        let g_elapsed = t1.elapsed();
        assert!(translated_len > 0);
        let g_ns_per_op = g_elapsed.as_nanos() / (iters as u128);
        eprintln!(
            "[BENCH] gemini_request_to_openai_json: {} iters in {:?} ({} ns/op)",
            iters, g_elapsed, g_ns_per_op
        );
        assert!(g_ns_per_op < 100_000, "gemini translation too slow: {} ns/op", g_ns_per_op);
    }
}
