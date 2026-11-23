use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::db::{ApiKey, Database};
use crate::routes::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<serde_json::Value>,
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
    pub stop_sequences: Option<Vec<String>>,
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
    pub tools: Option<Vec<serde_json::Value>>,
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

fn authenticate_gemini(
    headers: &HeaderMap,
    uri: &Uri,
    db: &Database,
) -> Result<ApiKey, Response> {
    let mut raw_key = auth::extract_api_key(headers);

    if raw_key.is_none() {
        if let Some(query) = uri.query() {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "key" && !v.is_empty() {
                        raw_key = Some(v.to_string());
                        break;
                    }
                }
            }
        }
    }

    if let Some(key_str) = raw_key {
        if let Ok(Some(k)) = db.get_api_key_by_key(&key_str) {
            if db.validate_key(&k).is_ok() {
                return Ok(k);
            }
        }
    }

    // Check if requireApiKey is false in settings
    if let Ok(settings) = db.load_settings() {
        if let Some(req) = settings.get("requireApiKey").and_then(|v| v.as_bool()) {
            if !req {
                return Ok(ApiKey {
                    id: "no-auth-default".to_string(),
                    key: "no-auth".to_string(),
                    name: Some("Local Default".to_string()),
                    ..ApiKey::default()
                });
            }
        }
    }

    Err((
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "code": 401,
                "message": "API key required. Provide via 'Authorization: Bearer <key>', 'x-goog-api-key', or '?key=<key>'.",
                "status": "UNAUTHENTICATED"
            }
        })),
    ).into_response())
}

pub async fn gemini_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if let Err(auth_res) = authenticate_gemini(&headers, &uri, &state.db) {
        return auth_res;
    }

    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let active_conns: Vec<_> = all_conns.into_iter().filter(|c| c.is_active).collect();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let mut add_model = |name: &str, display_name: &str| {
        let canonical_name = if name.starts_with("models/") {
            name.to_string()
        } else {
            format!("models/{}", name)
        };
        if seen.insert(canonical_name.clone()) {
            models.push(json!({
                "name": canonical_name,
                "version": "1.0",
                "displayName": display_name,
                "description": format!("Model: {}", display_name),
                "inputTokenLimit": 128000,
                "outputTokenLimit": 8192,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
            }));
        }
    };

    // Standard Gemini models
    add_model("gemini-2.5-pro", "Gemini 2.5 Pro");
    add_model("gemini-2.5-flash", "Gemini 2.5 Flash");
    add_model("gemini-3.8-flash", "Gemini 3.8 Flash");
    add_model("gemini-3.8-flash-medium", "Gemini 3.8 Flash Medium");
    add_model("gemini-3.8-flash-high", "Gemini 3.8 Flash High");
    add_model("gemini-4-carbon", "Gemini 4 Carbon");

    // Combos
    if let Ok(combos) = state.db.load_combos() {
        for combo in combos {
            add_model(&combo.name, &combo.name);
        }
    }

    // Connected custom provider models
    for node in &provider_nodes {
        let has_conn = active_conns.iter().any(|c| c.provider == node.id || c.provider == node.prefix);
        if !has_conn && !active_conns.is_empty() {
            continue;
        }
        let prefix = &node.prefix;
        if prefix == "h" || node.name.as_deref() == Some("HCNSEC") {
            add_model("h/Qwen3.8-Flash-Next", "Qwen3.8-Flash-Next");
            add_model("h/hy4", "hy4");
            add_model("h/DeepSeek-V4.1-Flash", "DeepSeek-V4.1-Flash");
        }
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "models": models })),
    ).into_response()
}

pub async fn gemini_model_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(model_path): Path<String>,
) -> Response {
    if let Err(auth_res) = authenticate_gemini(&headers, &uri, &state.db) {
        return auth_res;
    }

    let clean_model = model_path.strip_prefix("models/").unwrap_or(&model_path);
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
    ).into_response()
}

pub async fn gemini_action_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(raw_path): Path<String>,
    Json(gemini_req): Json<GeminiGenerateContentRequest>,
) -> Response {
    let api_key = match authenticate_gemini(&headers, &uri, &state.db) {
        Ok(k) => k,
        Err(auth_res) => return auth_res,
    };

    let (model_candidate, is_stream) = if raw_path.contains(":streamGenerateContent") {
        (raw_path.replace(":streamGenerateContent", ""), true)
    } else if raw_path.contains(":generateContent") {
        (raw_path.replace(":generateContent", ""), false)
    } else {
        (raw_path.clone(), false)
    };

    let target_model = model_candidate
        .strip_prefix("models/")
        .unwrap_or(&model_candidate)
        .to_string();

    let mut messages = Vec::new();

    // 1. System instruction
    if let Some(sys) = gemini_req.system_instruction {
        let mut sys_text = String::new();
        for p in sys.parts {
            if let Some(t) = p.text {
                if !sys_text.is_empty() {
                    sys_text.push('\n');
                }
                sys_text.push_str(&t);
            }
        }
        if !sys_text.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": sys_text
            }));
        }
    }

    // 2. Contents
    for c in gemini_req.contents {
        let role = match c.role.as_deref() {
            Some("model") => "assistant",
            _ => "user",
        };

        let mut part_contents = Vec::new();
        let mut text_buf = String::new();

        for p in c.parts {
            if let Some(txt) = p.text {
                if !text_buf.is_empty() {
                    text_buf.push('\n');
                }
                text_buf.push_str(&txt);
            }
            if let Some(inline) = p.inline_data {
                if let (Some(mime), Some(data)) = (
                    inline.get("mimeType").and_then(|v| v.as_str()),
                    inline.get("data").and_then(|v| v.as_str()),
                ) {
                    part_contents.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", mime, data)
                        }
                    }));
                }
            }
        }

        if !text_buf.is_empty() {
            if part_contents.is_empty() {
                messages.push(json!({
                    "role": role,
                    "content": text_buf
                }));
            } else {
                part_contents.insert(0, json!({
                    "type": "text",
                    "text": text_buf
                }));
                messages.push(json!({
                    "role": role,
                    "content": part_contents
                }));
            }
        } else if !part_contents.is_empty() {
            messages.push(json!({
                "role": role,
                "content": part_contents
            }));
        }
    }

    let mut openai_payload = json!({
        "model": target_model,
        "messages": messages,
        "stream": is_stream,
    });

    if let Some(cfg) = gemini_req.generation_config {
        if let Some(max_tokens) = cfg.max_output_tokens {
            openai_payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temp) = cfg.temperature {
            openai_payload["temperature"] = json!(temp);
        }
        if let Some(top_p) = cfg.top_p {
            openai_payload["top_p"] = json!(top_p);
        }
        if let Some(stops) = cfg.stop_sequences {
            openai_payload["stop"] = json!(stops);
        }
    }

    let mut chat_headers = headers.clone();
    chat_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    if !chat_headers.contains_key(header::AUTHORIZATION) {
        if let Ok(val) = format!("Bearer {}", api_key.key).parse() {
            chat_headers.insert(header::AUTHORIZATION, val);
        }
    }

    let payload_bytes = axum::body::Bytes::from(serde_json::to_vec(&openai_payload).unwrap_or_default());
    let internal_resp = crate::routes::chat_completions_handler(
        State(state),
        chat_headers,
        payload_bytes,
    ).await;

    if is_stream {
        transform_stream_to_gemini(internal_resp, target_model).await
    } else {
        transform_json_to_gemini(internal_resp, target_model).await
    }
}

async fn transform_json_to_gemini(resp: Response, model_name: String) -> Response {
    let (parts, body) = resp.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }

    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "code": 500, "message": e.to_string() } })),
            ).into_response();
        }
    };

    let openai_res: serde_json::Value = match serde_json::from_slice(&bytes) {
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
        ).into_response();
    }

    let choice = openai_res.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first());
    let mut gemini_parts = Vec::new();

    if let Some(ch) = choice {
        if let Some(reasoning) = ch.get("message").and_then(|m| m.get("reasoning_content")).and_then(|r| r.as_str()) {
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
        let content_text = ch.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("");
        gemini_parts.push(GeminiPart {
            text: Some(content_text.to_string()),
            thought: None,
            inline_data: None,
            function_call: None,
            function_response: None,
        });
    }

    let finish_reason = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str())
        .map(|f| match f {
            "stop" => "STOP",
            "length" => "MAX_TOKENS",
            "content_filter" => "SAFETY",
            _ => "STOP",
        })
        .unwrap_or("STOP");

    let usage = openai_res.get("usage");
    let usage_metadata = usage.map(|u| {
        let pt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let ct = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let tt = u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let thought = u.get("completion_tokens_details")
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
        model_version: Some(openai_res.get("model").and_then(|v| v.as_str()).unwrap_or(&model_name).to_string()),
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(gemini_resp),
    ).into_response()
}

async fn transform_stream_to_gemini(resp: Response, model_name: String) -> Response {
    let (parts, body) = resp.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }

    let body_stream = body.into_data_stream();
    let model_clone = model_name.clone();

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

                let parsed: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let choice = match parsed.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) {
                    Some(c) => c,
                    None => continue,
                };

                let delta = choice.get("delta");
                let finish_reason = choice.get("finish_reason").and_then(|f| f.as_str());

                let mut parts = Vec::new();
                if let Some(thought) = delta.and_then(|d| d.get("reasoning_content")).and_then(|r| r.as_str()) {
                    if !thought.is_empty() {
                        parts.push(json!({ "text": thought, "thought": true }));
                    }
                }
                if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        parts.push(json!({ "text": content }));
                    }
                }

                if parts.is_empty() && finish_reason.is_none() {
                    continue;
                }

                let mut candidate = json!({
                    "content": {
                        "role": "model",
                        "parts": if parts.is_empty() { vec![json!({ "text": "" })] } else { parts }
                    },
                    "index": 0
                });

                if let Some(fr) = finish_reason {
                    let gemini_fr = match fr {
                        "stop" => "STOP",
                        "length" => "MAX_TOKENS",
                        "content_filter" => "SAFETY",
                        _ => "STOP",
                    };
                    candidate["finishReason"] = json!(gemini_fr);
                }

                let mut chunk_obj = json!({
                    "candidates": [candidate]
                });

                if let Some(usage) = parsed.get("usage") {
                    let pt = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let ct = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let tt = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    chunk_obj["usageMetadata"] = json!({
                        "promptTokenCount": pt,
                        "candidatesTokenCount": ct,
                        "totalTokenCount": tt
                    });
                    chunk_obj["modelVersion"] = json!(parsed.get("model").and_then(|v| v.as_str()).unwrap_or(&model_clone));
                }

                let out_line = format!("data: {}\r\n\r\n", serde_json::to_string(&chunk_obj).unwrap_or_default());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_request_deserialization() {
        let raw_json = r#"{
            "contents": [
                {
                    "role": "user",
                    "parts": [{"text": "Hello world"}]
                }
            ],
            "systemInstruction": {
                "parts": [{"text": "You are a helpful assistant"}]
            },
            "generationConfig": {
                "temperature": 0.7,
                "maxOutputTokens": 2048,
                "topP": 0.95
            }
        }"#;

        let req: GeminiGenerateContentRequest = serde_json::from_str(raw_json).expect("deserialize Gemini request");
        assert_eq!(req.contents.len(), 1);
        assert_eq!(req.contents[0].parts[0].text.as_deref(), Some("Hello world"));
        assert_eq!(req.system_instruction.unwrap().parts[0].text.as_deref(), Some("You are a helpful assistant"));
        assert_eq!(req.generation_config.as_ref().unwrap().max_output_tokens, Some(2048));
    }

    #[test]
    fn test_gemini_response_serialization() {
        let resp = GeminiGenerateContentResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![
                        GeminiPart {
                            text: Some("Reasoning step".to_string()),
                            thought: Some(true),
                            inline_data: None,
                            function_call: None,
                            function_response: None,
                        },
                        GeminiPart {
                            text: Some("Final answer".to_string()),
                            thought: None,
                            inline_data: None,
                            function_call: None,
                            function_response: None,
                        },
                    ],
                },
                finish_reason: Some("STOP".to_string()),
                index: 0,
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 15,
                candidates_token_count: 42,
                total_token_count: 57,
                thoughts_token_count: Some(10),
            }),
            model_version: Some("gemini-2.5-pro".to_string()),
        };

        let val = serde_json::to_value(&resp).expect("serialize response");
        assert_eq!(val["candidates"][0]["content"]["role"], "model");
        assert_eq!(val["candidates"][0]["content"]["parts"][0]["text"], "Reasoning step");
        assert_eq!(val["candidates"][0]["content"]["parts"][0]["thought"], true);
        assert_eq!(val["candidates"][0]["content"]["parts"][1]["text"], "Final answer");
        assert_eq!(val["candidates"][0]["finishReason"], "STOP");
        assert_eq!(val["usageMetadata"]["promptTokenCount"], 15);
        assert_eq!(val["usageMetadata"]["candidatesTokenCount"], 42);
        assert_eq!(val["usageMetadata"]["totalTokenCount"], 57);
        assert_eq!(val["usageMetadata"]["thoughtsTokenCount"], 10);
    }
}
