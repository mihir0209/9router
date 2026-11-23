use std::collections::HashMap;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::db::{ApiKey, ProviderConnection, ProviderNode};
use crate::routes::AppState;

// ============================================================================
// Data Structures
// ============================================================================

/// Parsed parameters for speech-to-text (STT) transcription requests.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionParams {
    pub file_bytes: Vec<u8>,
    pub filename: String,
    pub content_type: String,
    pub model: String,
    pub language: Option<String>,
    pub response_format: Option<String>,
    pub prompt: Option<String>,
    pub temperature: Option<f32>,
}

impl Default for TranscriptionParams {
    fn default() -> Self {
        Self {
            file_bytes: Vec::new(),
            filename: "audio.wav".to_string(),
            content_type: "audio/wav".to_string(),
            model: "whisper-1".to_string(),
            language: None,
            response_format: None,
            prompt: None,
            temperature: None,
        }
    }
}

/// Request body for text-to-speech (TTS) speech synthesis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioSpeechRequest {
    pub model: String,
    pub input: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(rename = "response_format", default)]
    pub response_format: Option<String>,
    #[serde(default)]
    pub speed: Option<f32>,
}

/// Audio provider execution classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProviderType {
    CloudflareAi,
    OpenAiCompatible,
    EdgeTts,
}

/// Resolved target endpoint and credentials for audio processing.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTarget {
    pub provider_type: AudioProviderType,
    pub endpoint_url: String,
    pub api_key: Option<String>,
    pub account_id: Option<String>,
    pub model: String,
    pub custom_headers: Option<HeaderMap>,
}

// ============================================================================
// Helper Utilities & Content Type Resolution
// ============================================================================

/// Resolve standard MIME content-type from format string.
pub fn resolve_audio_content_type(format: Option<&str>) -> &'static str {
    match format.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("wav") => "audio/wav",
        Some("opus") => "audio/opus",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("webm") => "audio/webm",
        Some("ogg") => "audio/ogg",
        Some("mp3") | Some("mpeg") | _ => "audio/mpeg",
    }
}

/// Construct a binary audio HTTP response with correct headers.
pub fn build_audio_response(bytes: Vec<u8>, content_type: &str) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(val) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, val);
    }
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );

    (StatusCode::OK, headers, bytes).into_response()
}

/// Build an error response in OpenAI format.
pub fn error_response(status: StatusCode, message: &str) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": "invalid_request_error",
            "code": status.as_u16()
        }
    });

    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body.to_string(),
    )
        .into_response()
}

// ============================================================================
// STT Form Parsing
// ============================================================================

/// Parse multipart form data from axum extractor into `TranscriptionParams`.
pub async fn parse_transcription_form(
    mut multipart: axum::extract::Multipart,
) -> Result<TranscriptionParams, (StatusCode, String)> {
    let mut params = TranscriptionParams::default();
    let mut file_found = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Malformed multipart form-data: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let fname = field.file_name().unwrap_or("audio.wav").to_string();
                let ctype = field.content_type().unwrap_or("audio/wav").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading audio bytes: {e}")))?;
                params.filename = fname;
                params.content_type = ctype;
                params.file_bytes = bytes.to_vec();
                file_found = true;
            }
            "model" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading model: {e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    params.model = trimmed.to_string();
                }
            }
            "language" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading language: {e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    params.language = Some(trimmed.to_string());
                }
            }
            "response_format" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading response_format: {e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    params.response_format = Some(trimmed.to_string());
                }
            }
            "prompt" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading prompt: {e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    params.prompt = Some(trimmed.to_string());
                }
            }
            "temperature" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed reading temperature: {e}")))?;
                if let Ok(t) = text.trim().parse::<f32>() {
                    params.temperature = Some(t);
                }
            }
            _ => {
                // Ignore other form fields
            }
        }
    }

    if !file_found || params.file_bytes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing required field: file".to_string()));
    }

    Ok(params)
}

/// Parse raw multipart body bytes given a boundary delimiter (useful for unit tests and direct decoding).
pub fn parse_multipart_raw(
    boundary: &str,
    body: &[u8],
) -> Result<TranscriptionParams, (StatusCode, String)> {
    let mut params = TranscriptionParams::default();
    let mut file_found = false;

    let delimiter = format!("--{}", boundary);
    let delim_bytes = delimiter.as_bytes();

    // Split body by boundary
    let mut start_idx = 0;
    while let Some(pos) = find_subsequence(&body[start_idx..], delim_bytes) {
        let part_start = start_idx + pos + delim_bytes.len();
        // check for closing "--" or "\r\n"
        if part_start + 2 <= body.len() && &body[part_start..part_start + 2] == b"--" {
            break;
        }

        let next_delim = find_subsequence(&body[part_start..], delim_bytes);
        let part_end = match next_delim {
            Some(p) => part_start + p,
            None => body.len(),
        };

        let part_data = &body[part_start..part_end];
        // Strip leading \r\n if present
        let part_content = if part_data.starts_with(b"\r\n") {
            &part_data[2..]
        } else {
            part_data
        };

        // Split headers and body at "\r\n\r\n"
        if let Some(header_end) = find_subsequence(part_content, b"\r\n\r\n") {
            let header_bytes = &part_content[..header_end];
            let content_bytes = &part_content[header_end + 4..];
            // Strip trailing \r\n
            let content = if content_bytes.ends_with(b"\r\n") {
                &content_bytes[..content_bytes.len() - 2]
            } else {
                content_bytes
            };

            let headers_str = String::from_utf8_lossy(header_bytes);
            let mut field_name = String::new();
            let mut file_name = String::new();
            let mut content_type = String::new();

            for line in headers_str.lines() {
                let line_trim = line.trim();
                let line_lower = line_trim.to_ascii_lowercase();
                if line_lower.starts_with("content-disposition:") {
                    if let Some(name_idx) = line_trim.find("name=\"") {
                        let rem = &line_trim[name_idx + 6..];
                        if let Some(end_quote) = rem.find('"') {
                            field_name = rem[..end_quote].to_string();
                        }
                    }
                    if let Some(fname_idx) = line_trim.find("filename=\"") {
                        let rem = &line_trim[fname_idx + 10..];
                        if let Some(end_quote) = rem.find('"') {
                            file_name = rem[..end_quote].to_string();
                        }
                    }
                } else if line_lower.starts_with("content-type:") {
                    let parts: Vec<&str> = line_trim.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        content_type = parts[1].trim().to_string();
                    }
                }
            }

            match field_name.as_str() {
                "file" => {
                    params.file_bytes = content.to_vec();
                    if !file_name.is_empty() {
                        params.filename = file_name;
                    }
                    if !content_type.is_empty() {
                        params.content_type = content_type;
                    }
                    file_found = true;
                }
                "model" => {
                    let val = String::from_utf8_lossy(content).trim().to_string();
                    if !val.is_empty() {
                        params.model = val;
                    }
                }
                "language" => {
                    let val = String::from_utf8_lossy(content).trim().to_string();
                    if !val.is_empty() {
                        params.language = Some(val);
                    }
                }
                "response_format" => {
                    let val = String::from_utf8_lossy(content).trim().to_string();
                    if !val.is_empty() {
                        params.response_format = Some(val);
                    }
                }
                "prompt" => {
                    let val = String::from_utf8_lossy(content).trim().to_string();
                    if !val.is_empty() {
                        params.prompt = Some(val);
                    }
                }
                "temperature" => {
                    let val = String::from_utf8_lossy(content).trim().to_string();
                    if let Ok(t) = val.parse::<f32>() {
                        params.temperature = Some(t);
                    }
                }
                _ => {}
            }
        }

        start_idx = part_end;
    }

    if !file_found || params.file_bytes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing required field: file".to_string()));
    }

    Ok(params)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

// ============================================================================
// Cloudflare AI Mappings
// ============================================================================

/// Normalize model string for Cloudflare Workers AI.
pub fn normalize_cloudflare_model(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed == "whisper-1" || trimmed == "whisper" {
        return "@cf/openai/whisper".to_string();
    }
    if trimmed.starts_with("cf/") {
        format!("@cf/{}", &trimmed[3..])
    } else if trimmed.starts_with("cfr/") {
        format!("@cf/{}", &trimmed[4..])
    } else if trimmed.starts_with("@cf/") {
        trimmed.to_string()
    } else {
        format!("@cf/{}", trimmed)
    }
}

/// Construct Cloudflare Workers AI run URL for speech-to-text.
pub fn build_cloudflare_stt_url(base_url: Option<&str>, account_id: &str, model: &str) -> String {
    let norm_model = normalize_cloudflare_model(model);
    if let Some(base) = base_url {
        let b = base.trim_end_matches('/');
        if b.contains("{model}") {
            return b
                .replace("{model}", &norm_model)
                .replace("{accountId}", account_id)
                .replace("{account_id}", account_id);
        }
        if b.contains("{accountId}") || b.contains("{account_id}") {
            let replaced = b
                .replace("{accountId}", account_id)
                .replace("{account_id}", account_id);
            return format!("{}/{}", replaced.trim_end_matches('/'), norm_model);
        }
        return format!("{}/{}", b, norm_model);
    }

    format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
        account_id, norm_model
    )
}

/// Map Cloudflare AI transcription response into OpenAI format or raw text.
pub fn map_cloudflare_stt_response(
    cf_val: &serde_json::Value,
    response_format: Option<&str>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), (StatusCode, String)> {
    // Check for Cloudflare error envelope
    if let Some(success) = cf_val.get("success").and_then(|v| v.as_bool()) {
        if !success {
            let err_msg = cf_val
                .get("errors")
                .and_then(|e| e.as_array())
                .and_then(|arr| arr.first())
                .and_then(|obj| obj.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Cloudflare AI transcription failed");
            return Err((StatusCode::BAD_GATEWAY, err_msg.to_string()));
        }
    }

    // Extract transcribed text from { result: { text: "..." } } or { text: "..." }
    let text = cf_val
        .get("result")
        .and_then(|r| r.get("text"))
        .or_else(|| cf_val.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );

    let format_lower = response_format.map(|s| s.trim().to_ascii_lowercase());
    match format_lower.as_deref() {
        Some("text") => {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            Ok((StatusCode::OK, headers, text.as_bytes().to_vec()))
        }
        Some("verbose_json") => {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            let obj = json!({
                "task": "transcribe",
                "language": "english",
                "duration": 0.0,
                "text": text,
                "segments": []
            });
            Ok((StatusCode::OK, headers, serde_json::to_vec(&obj).unwrap()))
        }
        _ => {
            // Default "json"
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            let obj = json!({ "text": text });
            Ok((StatusCode::OK, headers, serde_json::to_vec(&obj).unwrap()))
        }
    }
}

/// Construct Cloudflare Workers AI run URL for text-to-speech.
pub fn build_cloudflare_tts_url(base_url: Option<&str>, account_id: &str, model: &str) -> String {
    let norm_model = if model.trim().is_empty() {
        "@cf/suno/bark".to_string()
    } else {
        normalize_cloudflare_model(model)
    };

    if let Some(base) = base_url {
        let b = base.trim_end_matches('/');
        if b.contains("{model}") {
            return b
                .replace("{model}", &norm_model)
                .replace("{accountId}", account_id)
                .replace("{account_id}", account_id);
        }
        if b.contains("{accountId}") || b.contains("{account_id}") {
            let replaced = b
                .replace("{accountId}", account_id)
                .replace("{account_id}", account_id);
            return format!("{}/{}", replaced.trim_end_matches('/'), norm_model);
        }
        return format!("{}/{}", b, norm_model);
    }

    format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
        account_id, norm_model
    )
}

/// Construct Cloudflare AI TTS request payload.
pub fn build_cloudflare_tts_payload(req: &AudioSpeechRequest) -> serde_json::Value {
    json!({
        "prompt": req.input
    })
}

// ============================================================================
// OpenAI Compatible Mappings
// ============================================================================

/// Build target URL for OpenAI-compatible transcription endpoint.
pub fn build_openai_stt_url(base_url: Option<&str>) -> String {
    match base_url {
        Some(b) => {
            let trimmed = b.trim_end_matches('/');
            if trimmed.ends_with("/v1/audio/transcriptions")
                || trimmed.ends_with("/audio/transcriptions")
            {
                trimmed.to_string()
            } else if trimmed.ends_with("/v1") {
                format!("{}/audio/transcriptions", trimmed)
            } else {
                format!("{}/v1/audio/transcriptions", trimmed)
            }
        }
        None => "https://api.openai.com/v1/audio/transcriptions".to_string(),
    }
}

/// Build target URL for OpenAI-compatible speech endpoint.
pub fn build_openai_tts_url(base_url: Option<&str>) -> String {
    match base_url {
        Some(b) => {
            let trimmed = b.trim_end_matches('/');
            if trimmed.ends_with("/v1/audio/speech") || trimmed.ends_with("/audio/speech") {
                trimmed.to_string()
            } else if trimmed.ends_with("/v1") {
                format!("{}/audio/speech", trimmed)
            } else {
                format!("{}/v1/audio/speech", trimmed)
            }
        }
        None => "https://api.openai.com/v1/audio/speech".to_string(),
    }
}

/// Build OpenAI-compatible TTS JSON request payload.
pub fn build_openai_tts_payload(req: &AudioSpeechRequest) -> serde_json::Value {
    let voice = req.voice.as_deref().unwrap_or("alloy");
    let format = req.response_format.as_deref().unwrap_or("mp3");
    let speed = req.speed.unwrap_or(1.0);

    json!({
        "model": req.model,
        "input": req.input,
        "voice": voice,
        "response_format": format,
        "speed": speed
    })
}

/// Build reqwest multipart form data for forwarding to upstream STT.
pub fn build_openai_stt_multipart(params: &TranscriptionParams) -> reqwest::multipart::Form {
    let mut form = reqwest::multipart::Form::new();

    let part = reqwest::multipart::Part::bytes(params.file_bytes.clone())
        .file_name(params.filename.clone())
        .mime_str(&params.content_type)
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(params.file_bytes.clone()));

    form = form.part("file", part);
    form = form.text("model", params.model.clone());

    if let Some(ref lang) = params.language {
        form = form.text("language", lang.clone());
    }
    if let Some(ref prompt) = params.prompt {
        form = form.text("prompt", prompt.clone());
    }
    if let Some(ref fmt) = params.response_format {
        form = form.text("response_format", fmt.clone());
    }
    if let Some(temp) = params.temperature {
        form = form.text("temperature", temp.to_string());
    }

    form
}

// ============================================================================
// Target Resolution (STT & TTS)
// ============================================================================

/// Resolve upstream STT target based on requested model, provider nodes, and database connections.
pub fn resolve_stt_target(
    model: &str,
    provider_nodes: &[ProviderNode],
    connections: &[ProviderConnection],
) -> AudioTarget {
    let model_lower = model.to_ascii_lowercase();

    // 1. Check for Cloudflare model or provider prefix
    if model_lower.starts_with("@cf/")
        || model_lower.starts_with("cf/")
        || model_lower.starts_with("cfr/")
    {
        let cf_conn = connections
            .iter()
            .find(|c| c.is_active && (c.provider == "cloudflare-ai" || c.provider == "cloudflare"));

        let account_id = cf_conn
            .and_then(|c| {
                c.data
                    .extra
                    .get("providerSpecificData")
                    .and_then(|psd| psd.get("accountId"))
                    .or_else(|| c.data.extra.get("accountId"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| "1da22c19548115accf1b98ab7c562b2c".to_string());

        let base_url = cf_conn.and_then(|c| c.data.base_url.as_deref());
        let endpoint = build_cloudflare_stt_url(base_url, &account_id, model);

        return AudioTarget {
            provider_type: AudioProviderType::CloudflareAi,
            endpoint_url: endpoint,
            api_key: cf_conn.and_then(|c| c.data.api_key.clone()),
            account_id: Some(account_id),
            model: normalize_cloudflare_model(model),
            custom_headers: None,
        };
    }

    // 2. Check provider nodes (custom node prefixes)
    for node in provider_nodes {
        let prefix_slash = format!("{}/", node.prefix);
        if !node.prefix.is_empty() && (model.starts_with(&prefix_slash) || model == node.prefix) {
            let upstream_model = if model.len() > prefix_slash.len() {
                model[prefix_slash.len()..].to_string()
            } else {
                model.to_string()
            };

            let endpoint = build_openai_stt_url(Some(&node.base_url));
            let mut api_key = None;
            let mut custom_headers = None;

            if let Some(pc) = connections.iter().find(|c| {
                c.is_active
                    && (c.provider == node.id
                        || c.provider == node.prefix
                        || node.name.as_ref() == Some(&c.provider))
            }) {
                api_key = pc.data.api_key.clone();
                if let Some(hdrs) = &pc.data.headers {
                    let mut hmap = HeaderMap::new();
                    for (k, v) in hdrs {
                        if let (Ok(kn), Ok(kv)) = (
                            HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(v),
                        ) {
                            hmap.insert(kn, kv);
                        }
                    }
                    custom_headers = Some(hmap);
                }
            }

            if api_key.is_none() {
                if let Some(k) = node.data.get("apiKey").and_then(|v| v.as_str()) {
                    api_key = Some(k.to_string());
                }
            }

            return AudioTarget {
                provider_type: AudioProviderType::OpenAiCompatible,
                endpoint_url: endpoint,
                api_key,
                account_id: None,
                model: upstream_model,
                custom_headers,
            };
        }
    }

    // 3. Check active connections (cloudflare-ai, selfhosted-stt, openai, groq, etc.)
    if let Some(cf_conn) = connections
        .iter()
        .find(|c| c.is_active && c.provider == "cloudflare-ai")
    {
        // If the user did not explicitly request an external provider, prefer active Cloudflare AI
        if model.is_empty() || model == "whisper-1" || model == "whisper" {
            let account_id = cf_conn
                .data
                .extra
                .get("providerSpecificData")
                .and_then(|psd| psd.get("accountId"))
                .or_else(|| cf_conn.data.extra.get("accountId"))
                .and_then(|v| v.as_str())
                .unwrap_or("1da22c19548115accf1b98ab7c562b2c")
                .to_string();

            let endpoint = build_cloudflare_stt_url(
                cf_conn.data.base_url.as_deref(),
                &account_id,
                "@cf/openai/whisper",
            );

            return AudioTarget {
                provider_type: AudioProviderType::CloudflareAi,
                endpoint_url: endpoint,
                api_key: cf_conn.data.api_key.clone(),
                account_id: Some(account_id),
                model: "@cf/openai/whisper".to_string(),
                custom_headers: None,
            };
        }
    }

    // OpenAI or self-hosted connection
    if let Some(pc) = connections.iter().find(|c| {
        c.is_active
            && (c.provider == "openai"
                || c.provider == "selfhosted-stt"
                || c.provider == "groq"
                || c.provider == "deepgram")
    }) {
        let endpoint = build_openai_stt_url(pc.data.base_url.as_deref());
        return AudioTarget {
            provider_type: AudioProviderType::OpenAiCompatible,
            endpoint_url: endpoint,
            api_key: pc.data.api_key.clone(),
            account_id: None,
            model: model.to_string(),
            custom_headers: None,
        };
    }

    // 4. Fallback default
    AudioTarget {
        provider_type: AudioProviderType::OpenAiCompatible,
        endpoint_url: "https://api.openai.com/v1/audio/transcriptions".to_string(),
        api_key: None,
        account_id: None,
        model: model.to_string(),
        custom_headers: None,
    }
}

/// Resolve upstream TTS target based on requested speech parameters, provider nodes, and connections.
pub fn resolve_tts_target(
    req: &AudioSpeechRequest,
    provider_nodes: &[ProviderNode],
    connections: &[ProviderConnection],
) -> AudioTarget {
    let model_lower = req.model.to_ascii_lowercase();

    // 1. Cloudflare Workers AI TTS
    if model_lower.starts_with("@cf/")
        || model_lower.starts_with("cf/")
        || model_lower.starts_with("cfr/")
    {
        let cf_conn = connections
            .iter()
            .find(|c| c.is_active && (c.provider == "cloudflare-ai" || c.provider == "cloudflare"));

        let account_id = cf_conn
            .and_then(|c| {
                c.data
                    .extra
                    .get("providerSpecificData")
                    .and_then(|psd| psd.get("accountId"))
                    .or_else(|| c.data.extra.get("accountId"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| "1da22c19548115accf1b98ab7c562b2c".to_string());

        let base_url = cf_conn.and_then(|c| c.data.base_url.as_deref());
        let endpoint = build_cloudflare_tts_url(base_url, &account_id, &req.model);

        return AudioTarget {
            provider_type: AudioProviderType::CloudflareAi,
            endpoint_url: endpoint,
            api_key: cf_conn.and_then(|c| c.data.api_key.clone()),
            account_id: Some(account_id),
            model: normalize_cloudflare_model(&req.model),
            custom_headers: None,
        };
    }

    // 2. Edge-TTS (free Microsoft Edge neural TTS)
    let is_edge_model = model_lower.contains("edge-tts") || model_lower == "edge";
    let is_edge_voice = req
        .voice
        .as_deref()
        .map(|v| v.contains("Neural") || v.contains('-'))
        .unwrap_or(false);
    let edge_conn = connections
        .iter()
        .find(|c| c.is_active && c.provider == "edge-tts");

    if is_edge_model || is_edge_voice || edge_conn.is_some() {
        return AudioTarget {
            provider_type: AudioProviderType::EdgeTts,
            endpoint_url: "https://www.bing.com/tfettts".to_string(),
            api_key: None,
            account_id: None,
            model: req.model.clone(),
            custom_headers: None,
        };
    }

    // 3. Provider nodes (custom endpoints: Kokoro, MeloTTS, self-hosted, etc.)
    for node in provider_nodes {
        let prefix_slash = format!("{}/", node.prefix);
        if !node.prefix.is_empty()
            && (req.model.starts_with(&prefix_slash) || req.model == node.prefix)
        {
            let upstream_model = if req.model.len() > prefix_slash.len() {
                req.model[prefix_slash.len()..].to_string()
            } else {
                req.model.clone()
            };

            let endpoint = build_openai_tts_url(Some(&node.base_url));
            let mut api_key = None;
            let mut custom_headers = None;

            if let Some(pc) = connections.iter().find(|c| {
                c.is_active
                    && (c.provider == node.id
                        || c.provider == node.prefix
                        || node.name.as_ref() == Some(&c.provider))
            }) {
                api_key = pc.data.api_key.clone();
                if let Some(hdrs) = &pc.data.headers {
                    let mut hmap = HeaderMap::new();
                    for (k, v) in hdrs {
                        if let (Ok(kn), Ok(kv)) = (
                            HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(v),
                        ) {
                            hmap.insert(kn, kv);
                        }
                    }
                    custom_headers = Some(hmap);
                }
            }

            if api_key.is_none() {
                if let Some(k) = node.data.get("apiKey").and_then(|v| v.as_str()) {
                    api_key = Some(k.to_string());
                }
            }

            return AudioTarget {
                provider_type: AudioProviderType::OpenAiCompatible,
                endpoint_url: endpoint,
                api_key,
                account_id: None,
                model: upstream_model,
                custom_headers,
            };
        }
    }

    // 4. Active custom TTS connections (selfhosted-tts, kokoro, melotts) with explicit endpoint
    if let Some(pc) = connections.iter().find(|c| {
        c.is_active
            && (c.provider == "selfhosted-tts"
                || c.provider == "kokoro"
                || c.provider == "melotts")
            && c.data.base_url.is_some()
    }) {
        let endpoint = build_openai_tts_url(pc.data.base_url.as_deref());
        return AudioTarget {
            provider_type: AudioProviderType::OpenAiCompatible,
            endpoint_url: endpoint,
            api_key: pc.data.api_key.clone(),
            account_id: None,
            model: req.model.clone(),
            custom_headers: None,
        };
    }

    // 5. Active OpenAI connection with valid key
    if let Some(pc) = connections.iter().find(|c| {
        c.is_active
            && c.provider == "openai"
            && c.data.api_key.as_deref().unwrap_or("").starts_with("sk-")
    }) {
        let endpoint = build_openai_tts_url(pc.data.base_url.as_deref());
        return AudioTarget {
            provider_type: AudioProviderType::OpenAiCompatible,
            endpoint_url: endpoint,
            api_key: pc.data.api_key.clone(),
            account_id: None,
            model: req.model.clone(),
            custom_headers: None,
        };
    }

    // 6. Zero-config free neural TTS fallback (Edge-TTS)
    AudioTarget {
        provider_type: AudioProviderType::EdgeTts,
        endpoint_url: "https://www.bing.com/tfettts".to_string(),
        api_key: None,
        account_id: None,
        model: req.model.clone(),
        custom_headers: None,
    }
}

// ============================================================================
// Edge-TTS Synthesis (Free Neural TTS)
// ============================================================================

/// Synthesize audio via Microsoft Edge translation TTS endpoint.
pub async fn synthesize_edge_tts(
    client: &reqwest::Client,
    text: &str,
    voice: Option<&str>,
) -> Result<(Vec<u8>, &'static str), (StatusCode, String)> {
    let raw_voice = voice.unwrap_or("alloy");
    let voice_id = match raw_voice.to_ascii_lowercase().as_str() {
        "alloy" => "en-US-GuyNeural",
        "echo" => "en-US-AndrewNeural",
        "fable" => "en-GB-SoniaNeural",
        "onyx" => "en-US-BrianNeural",
        "nova" => "en-US-JennyNeural",
        "shimmer" => "en-US-AriaNeural",
        _ => raw_voice,
    };
    let parts: Vec<&str> = voice_id.split('-').collect();
    let xml_lang = if parts.len() >= 2 {
        format!("{}-{}", parts[0], parts[1])
    } else {
        "en-US".to_string()
    };
    let gender = if voice_id.to_ascii_lowercase().contains("male")
        && !voice_id.to_ascii_lowercase().contains("female")
    {
        "Male"
    } else {
        "Female"
    };

    // 1. Get translation tokens and cookies from bing.com/translator
    let translator_res = client
        .get("https://www.bing.com/translator")
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
        )
        .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Edge-TTS handshake failed: {e}")))?;

    let mut cookie_header = String::new();
    for val in translator_res.headers().get_all(header::SET_COOKIE) {
        if let Ok(cookie_str) = val.to_str() {
            if let Some(cookie) = cookie_str.split(';').next() {
                if !cookie_header.is_empty() {
                    cookie_header.push_str("; ");
                }
                cookie_header.push_str(cookie.trim());
            }
        }
    }

    let html = translator_res.text().await.unwrap_or_default();
    let (key, token) = extract_bing_tokens(&html).unwrap_or_else(|| {
        ("123".to_string(), "abc".to_string())
    });

    // 2. Synthesize via tfettts endpoint
    let escaped_text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let ssml = format!(
        "<speak version='1.0' xml:lang='{}'><voice xml:lang='{}' xml:gender='{}' name='{}'><prosody rate='0.00%'>{}</prosody></voice></speak>",
        xml_lang, xml_lang, gender, voice_id, escaped_text
    );

    let mut form_params = HashMap::new();
    form_params.insert("ssml", ssml);
    form_params.insert("token", token);
    form_params.insert("key", key);

    let mut req_builder = client
        .post("https://www.bing.com/tfettts?isVertical=1&&IG=1&IID=translator.5023&SFX=1")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "*/*")
        .header(header::ORIGIN, "https://www.bing.com")
        .header(header::REFERER, "https://www.bing.com/translator")
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
        )
        .form(&form_params);

    if !cookie_header.is_empty() {
        req_builder = req_builder.header(header::COOKIE, cookie_header);
    }

    let tts_res = req_builder
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Edge-TTS upstream failed: {e}")))?;

    if !tts_res.status().is_success() {
        let status = StatusCode::from_u16(tts_res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let err_text = tts_res.text().await.unwrap_or_default();
        return Err((status, format!("Edge-TTS error: {err_text}")));
    }

    let audio_bytes = tts_res
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Failed reading audio bytes: {e}")))?
        .to_vec();

    Ok((audio_bytes, "audio/mpeg"))
}

fn extract_bing_tokens(html: &str) -> Option<(String, String)> {
    let marker = "params_AbusePreventionHelper";
    if let Some(idx) = html.find(marker) {
        let snippet = &html[idx..std::cmp::min(idx + 300, html.len())];
        if let Some(bracket_start) = snippet.find('[') {
            if let Some(bracket_end) = snippet[bracket_start..].find(']') {
                let inner = &snippet[bracket_start + 1..bracket_start + bracket_end];
                let parts: Vec<&str> = inner.split(',').collect();
                if parts.len() >= 2 {
                    let key = parts[0].trim().trim_matches('"').to_string();
                    let token = parts[1].trim().trim_matches('"').to_string();
                    if !key.is_empty() && !token.is_empty() {
                        return Some((key, token));
                    }
                }
            }
        }
    }
    None
}

// ============================================================================
// Authentication Helper
// ============================================================================

/// Validate authentication for audio endpoints against database and server config.
pub fn check_audio_auth(headers: &HeaderMap, state: &AppState) -> Result<Option<ApiKey>, Response> {
    if let Some(raw_key) = auth::extract_api_key(headers) {
        match state.db.get_api_key_by_key(&raw_key) {
            Ok(Some(k)) => {
                if state.db.validate_key(&k).is_ok() {
                    return Ok(Some(k));
                } else {
                    return Err(error_response(StatusCode::UNAUTHORIZED, "Invalid API key"));
                }
            }
            _ => {
                return Err(error_response(StatusCode::UNAUTHORIZED, "Invalid API key"));
            }
        }
    }

    // Check if login is optional or internal session
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let require_login = settings
        .get("requireLogin")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !require_login
        || auth::verify_internal_auth(headers, &state.config.jwt_secret, &state.db).is_ok()
        || auth::extract_session_token(headers)
            .map(|tok| auth::verify_dashboard_jwt(&tok, &state.config.jwt_secret))
            .unwrap_or(false)
    {
        return Ok(None);
    }

    Err(error_response(StatusCode::UNAUTHORIZED, "API key required"))
}

// ============================================================================
// Axum HTTP Handlers
// ============================================================================

/// POST /v1/audio/transcriptions & /api/v1/audio/transcriptions
pub async fn transcriptions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: axum::extract::Multipart,
) -> Response {
    if let Err(auth_err) = check_audio_auth(&headers, &state) {
        return auth_err;
    }

    let params = match parse_transcription_form(multipart).await {
        Ok(p) => p,
        Err((status, msg)) => return error_response(status, &msg),
    };

    let connections = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let target = resolve_stt_target(&params.model, &provider_nodes, &connections);

    match target.provider_type {
        AudioProviderType::CloudflareAi => {
            let mut req_builder = state
                .proxy
                .client()
                .post(&target.endpoint_url)
                .header(
                    header::CONTENT_TYPE,
                    if !params.content_type.is_empty() {
                        &params.content_type
                    } else {
                        "application/octet-stream"
                    },
                );

            if let Some(token) = target.api_key.as_deref() {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let resp = match req_builder.body(params.file_bytes).send().await {
                Ok(r) => r,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!("Cloudflare AI transcription request failed: {e}"),
                    );
                }
            };

            if !resp.status().is_success() {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let err_text = resp.text().await.unwrap_or_default();
                return error_response(status, &format!("Cloudflare AI error: {err_text}"));
            }

            let cf_json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!("Invalid JSON from Cloudflare AI: {e}"),
                    );
                }
            };

            match map_cloudflare_stt_response(&cf_json, params.response_format.as_deref()) {
                Ok((status, resp_headers, body_bytes)) => (status, resp_headers, body_bytes).into_response(),
                Err((status, msg)) => error_response(status, &msg),
            }
        }
        AudioProviderType::OpenAiCompatible | AudioProviderType::EdgeTts => {
            let mut req_builder = state.proxy.client().post(&target.endpoint_url);

            if let Some(token) = target.api_key.as_deref() {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }
            if let Some(custom_hdrs) = target.custom_headers {
                for (k, v) in custom_hdrs {
                    if let Some(k) = k {
                        req_builder = req_builder.header(k, v);
                    }
                }
            }

            let form = build_openai_stt_multipart(&params);
            req_builder = req_builder.multipart(form);

            let resp = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!("Upstream STT request failed: {e}"),
                    );
                }
            };

            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let mut resp_headers = HeaderMap::new();
            if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
                resp_headers.insert(header::CONTENT_TYPE, ct.clone());
            } else {
                resp_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            resp_headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );

            let body_bytes = resp.bytes().await.unwrap_or_default().to_vec();
            (status, resp_headers, body_bytes).into_response()
        }
    }
}

/// POST /v1/audio/speech & /api/v1/audio/speech
pub async fn speech_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(auth_err) = check_audio_auth(&headers, &state) {
        return auth_err;
    }

    let req: AudioSpeechRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("Invalid JSON: {e}")),
    };

    if req.input.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "Missing required field: input");
    }

    let connections = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let target = resolve_tts_target(&req, &provider_nodes, &connections);
    let expected_content_type = resolve_audio_content_type(req.response_format.as_deref());

    match target.provider_type {
        AudioProviderType::CloudflareAi => {
            let payload = build_cloudflare_tts_payload(&req);
            let mut req_builder = state
                .proxy
                .client()
                .post(&target.endpoint_url)
                .header(header::CONTENT_TYPE, "application/json");

            if let Some(token) = target.api_key.as_deref() {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }

            let resp = match req_builder.json(&payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!("Cloudflare AI TTS request failed: {e}"),
                    );
                }
            };

            if !resp.status().is_success() {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let err_text = resp.text().await.unwrap_or_default();
                return error_response(status, &format!("Cloudflare AI TTS error: {err_text}"));
            }

            let bytes = resp.bytes().await.unwrap_or_default().to_vec();
            build_audio_response(bytes, expected_content_type)
        }
        AudioProviderType::EdgeTts => {
            match synthesize_edge_tts(
                state.proxy.client(),
                &req.input,
                req.voice.as_deref(),
            )
            .await
            {
                Ok((bytes, ct)) => build_audio_response(bytes, ct),
                Err((status, msg)) => error_response(status, &msg),
            }
        }
        AudioProviderType::OpenAiCompatible => {
            let payload = build_openai_tts_payload(&req);
            let mut req_builder = state
                .proxy
                .client()
                .post(&target.endpoint_url)
                .header(header::CONTENT_TYPE, "application/json");

            if let Some(token) = target.api_key.as_deref() {
                req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
            }
            if let Some(custom_hdrs) = target.custom_headers {
                for (k, v) in custom_hdrs {
                    if let Some(k) = k {
                        req_builder = req_builder.header(k, v);
                    }
                }
            }

            let resp = match req_builder.json(&payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Upstream TTS request error: {e}, falling back to Edge-TTS");
                    match synthesize_edge_tts(state.proxy.client(), &req.input, req.voice.as_deref()).await {
                        Ok((bytes, ct)) => return build_audio_response(bytes, ct),
                        Err(_) => {
                            return error_response(
                                StatusCode::BAD_GATEWAY,
                                &format!("Upstream TTS request failed: {e}"),
                            );
                        }
                    }
                }
            };

            if !resp.status().is_success() {
                let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let err_text = resp.text().await.unwrap_or_default();
                tracing::warn!("Upstream TTS returned error ({status}): {err_text}, falling back to Edge-TTS");
                match synthesize_edge_tts(state.proxy.client(), &req.input, req.voice.as_deref()).await {
                    Ok((bytes, ct)) => return build_audio_response(bytes, ct),
                    Err(_) => {
                        return error_response(status, &format!("Upstream TTS error: {err_text}"));
                    }
                }
            }

            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string)
                .unwrap_or_else(|| expected_content_type.to_string());

            let bytes = resp.bytes().await.unwrap_or_default().to_vec();
            build_audio_response(bytes, &ct)
        }
    }
}

/// OPTIONS /v1/audio/* & /api/v1/audio/*
pub async fn audio_options_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "POST, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
    )
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_audio_content_type() {
        assert_eq!(resolve_audio_content_type(Some("mp3")), "audio/mpeg");
        assert_eq!(resolve_audio_content_type(Some("mpeg")), "audio/mpeg");
        assert_eq!(resolve_audio_content_type(Some("wav")), "audio/wav");
        assert_eq!(resolve_audio_content_type(Some("opus")), "audio/opus");
        assert_eq!(resolve_audio_content_type(Some("aac")), "audio/aac");
        assert_eq!(resolve_audio_content_type(Some("flac")), "audio/flac");
        assert_eq!(resolve_audio_content_type(Some("webm")), "audio/webm");
        assert_eq!(resolve_audio_content_type(Some("ogg")), "audio/ogg");
        assert_eq!(resolve_audio_content_type(None), "audio/mpeg");
        assert_eq!(resolve_audio_content_type(Some("unknown")), "audio/mpeg");
    }

    #[test]
    fn test_build_audio_response_headers() {
        let sample_audio = vec![0x49, 0x44, 0x33, 0x03, 0x00, 0x00];
        let resp = build_audio_response(sample_audio, "audio/mpeg");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "audio/mpeg"
        );
        assert_eq!(
            resp.headers().get(header::CONTENT_LENGTH).unwrap(),
            "6"
        );
        assert_eq!(
            resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
    }

    #[test]
    fn test_multipart_raw_parsing_valid() {
        let boundary = "---------------------------974767299852498929531610575";
        let raw_body = format!(
            "--{b}\r\n\
             Content-Disposition: form-data; name=\"model\"\r\n\r\n\
             whisper-1\r\n\
             --{b}\r\n\
             Content-Disposition: form-data; name=\"language\"\r\n\r\n\
             en\r\n\
             --{b}\r\n\
             Content-Disposition: form-data; name=\"response_format\"\r\n\r\n\
             json\r\n\
             --{b}\r\n\
             Content-Disposition: form-data; name=\"temperature\"\r\n\r\n\
             0.2\r\n\
             --{b}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\n\
             RIFF....WAVEfmt \r\n\
             --{b}--\r\n",
            b = boundary
        );

        let parsed = parse_multipart_raw(boundary, raw_body.as_bytes()).expect("Must parse");
        assert_eq!(parsed.model, "whisper-1");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.response_format.as_deref(), Some("json"));
        assert_eq!(parsed.temperature, Some(0.2));
        assert_eq!(parsed.filename, "speech.wav");
        assert_eq!(parsed.content_type, "audio/wav");
        assert_eq!(parsed.file_bytes, b"RIFF....WAVEfmt ");
    }

    #[test]
    fn test_multipart_raw_parsing_missing_file() {
        let boundary = "boundary123";
        let raw_body = format!(
            "--{b}\r\n\
             Content-Disposition: form-data; name=\"model\"\r\n\r\n\
             whisper-1\r\n\
             --{b}--\r\n",
            b = boundary
        );

        let res = parse_multipart_raw(boundary, raw_body.as_bytes());
        assert!(res.is_err());
        let (status, msg) = res.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.contains("Missing required field: file"));
    }

    #[test]
    fn test_cloudflare_stt_url_and_model_normalization() {
        let norm1 = normalize_cloudflare_model("whisper-1");
        assert_eq!(norm1, "@cf/openai/whisper");

        let norm2 = normalize_cloudflare_model("cf/openai/whisper");
        assert_eq!(norm2, "@cf/openai/whisper");

        let norm3 = normalize_cloudflare_model("@cf/openai/whisper");
        assert_eq!(norm3, "@cf/openai/whisper");

        let url1 = build_cloudflare_stt_url(None, "acc123", "@cf/openai/whisper");
        assert_eq!(
            url1,
            "https://api.cloudflare.com/client/v4/accounts/acc123/ai/run/@cf/openai/whisper"
        );

        let url2 = build_cloudflare_stt_url(
            Some("https://custom.cloudflare.gateway/accounts/{accountId}/ai/run"),
            "acc123",
            "cf/openai/whisper",
        );
        assert_eq!(
            url2,
            "https://custom.cloudflare.gateway/accounts/acc123/ai/run/@cf/openai/whisper"
        );
    }

    #[test]
    fn test_map_cloudflare_stt_response_json() {
        let cf_payload = json!({
            "success": true,
            "errors": [],
            "messages": [],
            "result": {
                "text": "Hello world transcription"
            }
        });

        let (status, headers, body) =
            map_cloudflare_stt_response(&cf_payload, Some("json")).expect("Success");

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["text"], "Hello world transcription");
    }

    #[test]
    fn test_map_cloudflare_stt_response_text_format() {
        let cf_payload = json!({
            "result": {
                "text": "Plain text speech output"
            }
        });

        let (status, headers, body) =
            map_cloudflare_stt_response(&cf_payload, Some("text")).expect("Success");

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "Plain text speech output"
        );
    }

    #[test]
    fn test_map_cloudflare_stt_response_error() {
        let cf_payload = json!({
            "success": false,
            "errors": [
                { "code": 1000, "message": "Model not available" }
            ]
        });

        let res = map_cloudflare_stt_response(&cf_payload, None);
        assert!(res.is_err());
        let (status, msg) = res.unwrap_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(msg.contains("Model not available"));
    }

    #[test]
    fn test_openai_stt_and_tts_url_building() {
        assert_eq!(
            build_openai_stt_url(None),
            "https://api.openai.com/v1/audio/transcriptions"
        );
        assert_eq!(
            build_openai_stt_url(Some("http://localhost:8080")),
            "http://localhost:8080/v1/audio/transcriptions"
        );
        assert_eq!(
            build_openai_stt_url(Some("http://localhost:8080/v1")),
            "http://localhost:8080/v1/audio/transcriptions"
        );
        assert_eq!(
            build_openai_stt_url(Some("http://localhost:8080/v1/audio/transcriptions")),
            "http://localhost:8080/v1/audio/transcriptions"
        );

        assert_eq!(
            build_openai_tts_url(None),
            "https://api.openai.com/v1/audio/speech"
        );
        assert_eq!(
            build_openai_tts_url(Some("http://localhost:8880")),
            "http://localhost:8880/v1/audio/speech"
        );
    }

    #[test]
    fn test_build_openai_tts_payload() {
        let req = AudioSpeechRequest {
            model: "kokoro".to_string(),
            input: "Speech synthesis test".to_string(),
            voice: Some("af_heart".to_string()),
            response_format: Some("wav".to_string()),
            speed: Some(1.2),
        };

        let payload = build_openai_tts_payload(&req);
        assert_eq!(payload["model"], "kokoro");
        assert_eq!(payload["input"], "Speech synthesis test");
        assert_eq!(payload["voice"], "af_heart");
        assert_eq!(payload["response_format"], "wav");
        let speed_diff = (payload["speed"].as_f64().unwrap() - 1.2).abs();
        assert!(speed_diff < 1e-4);
    }

    #[test]
    fn test_resolve_stt_target_cloudflare() {
        let nodes = vec![];
        let conns = vec![ProviderConnection {
            id: "conn-1".to_string(),
            provider: "cloudflare-ai".to_string(),
            auth_type: "apikey".to_string(),
            name: None,
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                api_key: Some("cf-secret-token".to_string()),
                ..Default::default()
            },
            raw_data: "{}".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        }];

        let target = resolve_stt_target("@cf/openai/whisper", &nodes, &conns);
        assert_eq!(target.provider_type, AudioProviderType::CloudflareAi);
        assert_eq!(target.api_key.as_deref(), Some("cf-secret-token"));
        assert!(target.endpoint_url.contains("/ai/run/@cf/openai/whisper"));
    }

    #[test]
    fn test_resolve_tts_target_edge_tts() {
        let nodes = vec![];
        let conns = vec![];

        let req = AudioSpeechRequest {
            model: "edge-tts".to_string(),
            input: "Hello edge".to_string(),
            voice: Some("en-US-GuyNeural".to_string()),
            response_format: Some("mp3".to_string()),
            speed: None,
        };

        let target = resolve_tts_target(&req, &nodes, &conns);
        assert_eq!(target.provider_type, AudioProviderType::EdgeTts);
    }
}
