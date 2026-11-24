use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::db::{ProviderConnection, ProviderNode};
use crate::routes::AppState;

// ============================================================================
// Request & Response Data Structures
// ============================================================================

/// Request body for /v1/images/generations and /api/v1/images/generations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageGenerationRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default)]
    pub response_format: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Standard OpenAI image generation response structure
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageGenerationResponse {
    pub created: i64,
    pub data: Vec<ImageData>,
}

/// Single image item within OpenAI image generation response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

/// Query parameters for image generation endpoints (e.g. ?response_format=binary)
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImageGenerationQuery {
    pub response_format: Option<String>,
}

/// Resolved target upstream provider and model
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedImageProvider {
    Cloudflare {
        url: String,
        api_key: Option<String>,
        model: String,
    },
    OpenAICompatible {
        url: String,
        api_key: Option<String>,
        model: String,
        headers: Option<HeaderMap>,
    },
}

/// Result of executing an image generation request
#[derive(Debug)]
pub enum ImageGenerationExecutionResult {
    Binary {
        bytes: Vec<u8>,
        content_type: &'static str,
    },
    Json(ImageGenerationResponse),
}

/// Internal execution error representation
#[derive(Debug)]
pub struct ImageGenerationError {
    pub status: StatusCode,
    pub message: String,
    pub error_type: String,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Detects whether binary image output was requested via body, query parameter, or Accept header
pub fn is_binary_output_requested(
    body_format: Option<&str>,
    query_format: Option<&str>,
    accept_header: Option<&str>,
) -> bool {
    if let Some(fmt) = body_format {
        if fmt.eq_ignore_ascii_case("binary")
            || fmt.eq_ignore_ascii_case("raw")
            || fmt.eq_ignore_ascii_case("image")
        {
            return true;
        }
    }
    if let Some(q_fmt) = query_format {
        if q_fmt.eq_ignore_ascii_case("binary")
            || q_fmt.eq_ignore_ascii_case("raw")
            || q_fmt.eq_ignore_ascii_case("image")
        {
            return true;
        }
    }
    if let Some(accept) = accept_header {
        let lower = accept.to_lowercase();
        if lower.starts_with("image/")
            || (lower.contains("image/")
                && !lower.contains("text/html")
                && !lower.contains("application/json"))
        {
            return true;
        }
    }
    false
}

/// Parses "1024x1024" or "512x512" into width and height dimensions
pub fn parse_size_to_dimensions(size: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = size.split('x').collect();
    if parts.len() == 2 {
        if let (Ok(w), Ok(h)) = (parts[0].trim().parse::<u32>(), parts[1].trim().parse::<u32>()) {
            return Some((w, h));
        }
    }
    None
}

/// Inspects leading magic bytes to identify image MIME type
pub fn detect_image_mime_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else {
        "image/png"
    }
}

/// Checks if model or provider designates Cloudflare Workers AI
pub fn is_cloudflare_model_or_provider(model: Option<&str>, provider: Option<&str>) -> bool {
    if let Some(p) = provider {
        if p.eq_ignore_ascii_case("cloudflare")
            || p.eq_ignore_ascii_case("cloudflare-ai")
            || p.eq_ignore_ascii_case("cf")
        {
            return true;
        }
    }
    if let Some(m) = model {
        if m.starts_with("@cf/") || m.starts_with("cf/") || m.starts_with("cfr/") {
            return true;
        }
    }
    false
}

/// Normalizes model name into Cloudflare model identifier format (e.g. `@cf/...`)
pub fn normalize_cloudflare_model(model: Option<&str>) -> String {
    match model {
        Some(m) => {
            let trimmed = m.trim();
            // Map deprecated or generic models to the active Flux model
            if trimmed.contains("stable-diffusion-xl")
                || trimmed.contains("sd-xl")
                || trimmed.contains("dall-e")
                || trimmed == "flux"
                || trimmed == "default"
                || trimmed.is_empty()
            {
                return "@cf/black-forest-labs/flux-1-schnell".to_string();
            }
            if trimmed.starts_with("cf/") {
                format!("@{}", trimmed)
            } else if trimmed.starts_with("cfr/") {
                format!("@cf/{}", &trimmed[4..])
            } else if trimmed.starts_with("@cf/") {
                trimmed.to_string()
            } else if trimmed.contains('/') {
                format!("@cf/{}", trimmed)
            } else {
                format!("@cf/black-forest-labs/{}", trimmed)
            }
        }
        None => "@cf/black-forest-labs/flux-1-schnell".to_string(),
    }
}

/// Builds JSON payload for Cloudflare Workers AI Text-to-Image models
pub fn build_cloudflare_image_payload(req: &ImageGenerationRequest) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("prompt".to_string(), Value::String(req.prompt.clone()));

    if let Some(ref size_str) = req.size {
        if let Some((w, h)) = parse_size_to_dimensions(size_str) {
            map.insert("width".to_string(), Value::Number(w.into()));
            map.insert("height".to_string(), Value::Number(h.into()));
        }
    }

    // Pass through optional parameters supported by Workers AI
    for key in &[
        "num_steps",
        "steps",
        "guidance",
        "seed",
        "strength",
        "negative_prompt",
    ] {
        if let Some(val) = req.extra.get(*key) {
            map.insert(key.to_string(), val.clone());
        }
    }

    if let Some(val) = req.extra.get("image").or_else(|| req.extra.get("image_b64")) {
        map.insert("image_b64".to_string(), val.clone());
    }
    if let Some(val) = req
        .extra
        .get("mask")
        .or_else(|| req.extra.get("mask_image"))
    {
        map.insert("mask_b64".to_string(), val.clone());
    }

    Value::Object(map)
}

/// Constructs Cloudflare AI run URL
pub fn build_cloudflare_run_url(
    base_url: Option<&str>,
    account_id: Option<&str>,
    model: &str,
) -> String {
    let acc = account_id.unwrap_or("1da22c19548115accf1b98ab7c562b2c");
    if let Some(b) = base_url {
        let trimmed = b.trim_end_matches('/');
        // Avoid routing through OpenAI chat compat endpoint for Workers AI image generation
        if !trimmed.ends_with("/compat") {
            if trimmed.contains("/ai/run") {
                return format!("{}/{}", trimmed, model);
            }
            if trimmed.contains("{model}") {
                return trimmed.replace("{model}", model).replace("{accountId}", acc);
            }
        }
    }
    format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
        acc, model
    )
}

/// Normalizes Cloudflare response (JSON object or queued response) into standard OpenAI format
pub fn normalize_cloudflare_response(val: &Value) -> Result<ImageGenerationResponse, String> {
    let now = Utc::now().timestamp();

    // 1. If already OpenAI format { created, data: [...] }
    if let Some(data_arr) = val.get("data").and_then(|d| d.as_array()) {
        let created = val.get("created").and_then(|c| c.as_i64()).unwrap_or(now);
        let mut items = Vec::new();
        for item in data_arr {
            let b64 = item
                .get("b64_json")
                .and_then(|s| s.as_str())
                .map(ToString::to_string);
            let url = item
                .get("url")
                .and_then(|s| s.as_str())
                .map(ToString::to_string);
            let revised = item
                .get("revised_prompt")
                .and_then(|s| s.as_str())
                .map(ToString::to_string);
            if b64.is_some() || url.is_some() {
                items.push(ImageData {
                    b64_json: b64,
                    url,
                    revised_prompt: revised,
                });
            }
        }
        if !items.is_empty() {
            return Ok(ImageGenerationResponse {
                created,
                data: items,
            });
        }
    }

    // 2. Search in "result" or top-level
    let result_obj = val.get("result").unwrap_or(val);

    // Check for queued responses: result.responses[...].result
    let effective_result = if let Some(responses) = result_obj.get("responses").and_then(|r| r.as_array()) {
        responses
            .iter()
            .find_map(|item| {
                if item.get("success").and_then(|s| s.as_bool()).unwrap_or(true) {
                    item.get("result")
                } else {
                    None
                }
            })
            .unwrap_or(result_obj)
    } else {
        result_obj
    };

    // Extract image string representation
    let img_str_opt = if let Some(s) = effective_result.as_str() {
        Some(s)
    } else if let Some(s) = effective_result.get("image").and_then(|v| v.as_str()) {
        Some(s)
    } else if let Some(s) = effective_result
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|item| item.get("b64_json"))
        .and_then(|v| v.as_str())
    {
        Some(s)
    } else if let Some(s) = effective_result
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|item| item.get("url"))
        .and_then(|v| v.as_str())
    {
        Some(s)
    } else if let Some(s) = val.get("image").and_then(|v| v.as_str()) {
        Some(s)
    } else {
        None
    };

    if let Some(raw_str) = img_str_opt {
        let (b64, url) = if raw_str.starts_with("http://") || raw_str.starts_with("https://") {
            (None, Some(raw_str.to_string()))
        } else if let Some(idx) = raw_str.find(";base64,") {
            (Some(raw_str[idx + 8..].to_string()), None)
        } else {
            (Some(raw_str.to_string()), None)
        };

        return Ok(ImageGenerationResponse {
            created: now,
            data: vec![ImageData {
                b64_json: b64,
                url,
                revised_prompt: None,
            }],
        });
    }

    Err(format!(
        "Could not extract image data from Cloudflare response: {}",
        val
    ))
}

/// Encodes binary image bytes into OpenAI ImageGenerationResponse with base64 data
pub fn binary_bytes_to_openai_response(bytes: &[u8]) -> ImageGenerationResponse {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    ImageGenerationResponse {
        created: Utc::now().timestamp(),
        data: vec![ImageData {
            b64_json: Some(b64),
            url: None,
            revised_prompt: None,
        }],
    }
}

/// Decodes base64 string from OpenAI response into raw binary image bytes
pub fn extract_binary_bytes_from_response(
    resp: &ImageGenerationResponse,
) -> Option<(Vec<u8>, &'static str)> {
    if let Some(first) = resp.data.first() {
        if let Some(ref b64) = first.b64_json {
            let clean = if let Some(idx) = b64.find(";base64,") {
                &b64[idx + 8..]
            } else {
                b64.as_str()
            };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(clean.trim()) {
                let mime = detect_image_mime_type(&bytes);
                return Some((bytes, mime));
            }
        }
    }
    None
}

/// Constructs OpenAI-compatible image generation request payload
pub fn build_openai_image_payload(req: &ImageGenerationRequest, target_model: &str) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("model".to_string(), Value::String(target_model.to_string()));
    map.insert("prompt".to_string(), Value::String(req.prompt.clone()));

    if let Some(n) = req.n {
        map.insert("n".to_string(), Value::Number(n.into()));
    }
    if let Some(ref size) = req.size {
        map.insert("size".to_string(), Value::String(size.clone()));
    }
    if let Some(ref quality) = req.quality {
        map.insert("quality".to_string(), Value::String(quality.clone()));
    }
    if let Some(ref resp_fmt) = req.response_format {
        if resp_fmt.eq_ignore_ascii_case("binary") {
            map.insert(
                "response_format".to_string(),
                Value::String("b64_json".to_string()),
            );
        } else {
            map.insert(
                "response_format".to_string(),
                Value::String(resp_fmt.clone()),
            );
        }
    }
    if let Some(ref style) = req.style {
        map.insert("style".to_string(), Value::String(style.clone()));
    }

    // Copy remaining extra fields
    for (k, v) in &req.extra {
        if !map.contains_key(k) {
            map.insert(k.clone(), v.clone());
        }
    }

    Value::Object(map)
}

/// Builds upstream OpenAI image endpoint URL
pub fn build_openai_images_url(base_url: Option<&str>) -> String {
    let base = base_url
        .unwrap_or("https://api.openai.com")
        .trim_end_matches('/');
    if base.ends_with("/images/generations") {
        base.to_string()
    } else if base.ends_with("/v1") || base.ends_with("/compat") || base.ends_with("/v2") {
        format!("{}/images/generations", base)
    } else {
        format!("{}/v1/images/generations", base)
    }
}

fn build_headers_from_map(hdrs: Option<&HashMap<String, String>>) -> Option<HeaderMap> {
    hdrs.map(|map| {
        let mut hm = HeaderMap::new();
        for (k, v) in map {
            if let (Ok(kn), Ok(kv)) = (
                header::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                hm.insert(kn, kv);
            }
        }
        hm
    })
}

fn build_headers_from_json(val: Option<&Value>) -> Option<HeaderMap> {
    val.and_then(|v| v.as_object()).map(|obj| {
        let mut hm = HeaderMap::new();
        for (k, v) in obj {
            if let Some(str_val) = v.as_str() {
                if let (Ok(kn), Ok(kv)) = (
                    header::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(str_val),
                ) {
                    hm.insert(kn, kv);
                }
            }
        }
        hm
    })
}

/// Resolves provider target connection from candidate model, configured database connections and nodes
pub fn resolve_image_target(
    candidate_model: Option<&str>,
    connections: &[ProviderConnection],
    nodes: &[ProviderNode],
) -> ResolvedImageProvider {
    let model_str = candidate_model.unwrap_or("").trim();

    // 1. Cloudflare Workers AI detection
    let is_cf = is_cloudflare_model_or_provider(
        if model_str.is_empty() {
            None
        } else {
            Some(model_str)
        },
        None,
    );

    if is_cf {
        let normalized_model = normalize_cloudflare_model(if model_str.is_empty() {
            None
        } else {
            Some(model_str)
        });

        let cf_conn = connections
            .iter()
            .find(|c| c.is_active && (c.provider == "cloudflare-ai" || c.provider == "cloudflare"));
        let cf_node = nodes
            .iter()
            .find(|n| n.prefix == "cfr" || n.prefix == "cf" || n.id == "cloudflare-ai");

        let api_key = cf_conn
            .and_then(|c| c.data.api_key.clone())
            .or_else(|| {
                cf_node.and_then(|n| {
                    n.data
                        .get("apiKey")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                })
            });

        let account_id = cf_conn.and_then(|c| {
            c.data
                .extra
                .get("providerSpecificData")
                .and_then(|psd| psd.get("accountId"))
                .and_then(|aid| aid.as_str())
                .map(ToString::to_string)
        });

        let base_url = cf_node
            .map(|n| n.base_url.clone())
            .or_else(|| cf_conn.and_then(|c| c.data.base_url.clone()));

        let url = build_cloudflare_run_url(
            base_url.as_deref(),
            account_id.as_deref(),
            &normalized_model,
        );

        return ResolvedImageProvider::Cloudflare {
            url,
            api_key,
            model: normalized_model,
        };
    }

    // 2. Check for explicit provider prefix (e.g. "openai/dall-e-3", "stability/sdxl", "custom/flux")
    if let Some((p_prefix, m_part)) = model_str.split_once('/') {
        if let Some(node) = nodes.iter().find(|n| {
            n.prefix.eq_ignore_ascii_case(p_prefix)
                || n.id.eq_ignore_ascii_case(p_prefix)
                || n.name
                    .as_deref()
                    .is_some_and(|nm| nm.eq_ignore_ascii_case(p_prefix))
        }) {
            let api_key = node
                .data
                .get("apiKey")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let url = build_openai_images_url(Some(&node.base_url));
            let custom_headers = build_headers_from_json(node.data.get("customHeaders"));
            return ResolvedImageProvider::OpenAICompatible {
                url,
                api_key,
                model: m_part.to_string(),
                headers: custom_headers,
            };
        }

        if let Some(pc) = connections
            .iter()
            .find(|c| c.is_active && c.provider.eq_ignore_ascii_case(p_prefix))
        {
            let url = build_openai_images_url(pc.data.base_url.as_deref());
            let custom_headers = build_headers_from_map(pc.data.headers.as_ref());
            return ResolvedImageProvider::OpenAICompatible {
                url,
                api_key: pc.data.api_key.clone(),
                model: m_part.to_string(),
                headers: custom_headers,
            };
        }
    }

    // 3. Match by defaultModel
    if !model_str.is_empty() {
        if let Some(pc) = connections
            .iter()
            .find(|c| c.is_active && c.data.default_model.as_deref() == Some(model_str))
        {
            let url = build_openai_images_url(pc.data.base_url.as_deref());
            let custom_headers = build_headers_from_map(pc.data.headers.as_ref());
            return ResolvedImageProvider::OpenAICompatible {
                url,
                api_key: pc.data.api_key.clone(),
                model: model_str.to_string(),
                headers: custom_headers,
            };
        }
    }

    // 4. Default OpenAI connection or Cloudflare AI fallback
    let openai_conn = connections
        .iter()
        .find(|c| c.is_active && c.provider.eq_ignore_ascii_case("openai") && c.data.api_key.as_deref().unwrap_or("").starts_with("sk-"));

    if openai_conn.is_none() {
        if let Some(cf_conn) = connections
            .iter()
            .find(|c| c.is_active && (c.provider == "cloudflare-ai" || c.provider == "cloudflare"))
        {
            let account_id = cf_conn
                .data
                .extra
                .get("providerSpecificData")
                .and_then(|psd| psd.get("accountId"))
                .and_then(|aid| aid.as_str())
                .unwrap_or("1da22c19548115accf1b98ab7c562b2c");
            let norm_model = normalize_cloudflare_model(Some(model_str));
            let url = build_cloudflare_run_url(None, Some(account_id), &norm_model);
            return ResolvedImageProvider::Cloudflare {
                url,
                api_key: cf_conn.data.api_key.clone(),
                model: norm_model,
            };
        }
    }

    let raw_openai = connections
        .iter()
        .find(|c| c.is_active && c.provider.eq_ignore_ascii_case("openai"));
    let target_model = if model_str.is_empty() {
        "dall-e-3"
    } else {
        model_str
    };
    let base_url = raw_openai.and_then(|c| c.data.base_url.clone());
    let api_key = raw_openai.and_then(|c| c.data.api_key.clone());
    let custom_headers = raw_openai.and_then(|c| build_headers_from_map(c.data.headers.as_ref()));

    ResolvedImageProvider::OpenAICompatible {
        url: build_openai_images_url(base_url.as_deref()),
        api_key,
        model: target_model.to_string(),
        headers: custom_headers,
    }
}

/// Executes upstream HTTP image generation and parses response
pub async fn execute_image_generation(
    client: &reqwest::Client,
    provider: &ResolvedImageProvider,
    req: &ImageGenerationRequest,
    binary_output: bool,
) -> Result<ImageGenerationExecutionResult, ImageGenerationError> {
    match provider {
        ResolvedImageProvider::Cloudflare {
            url,
            api_key,
            model: _,
        } => {
            let payload = build_cloudflare_image_payload(req);
            let mut builder = client.post(url).json(&payload);
            if let Some(token) = api_key {
                if !token.is_empty() {
                    builder = builder.bearer_auth(token);
                }
            }

            let resp = builder.send().await.map_err(|e| ImageGenerationError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Cloudflare upstream network error: {}", e),
                error_type: "upstream_error".to_string(),
            })?;

            let status = resp.status();
            if !status.is_success() {
                let err_text = resp.text().await.unwrap_or_default();
                return Err(ImageGenerationError {
                    status: StatusCode::from_u16(status.as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY),
                    message: format!("Cloudflare AI error (status {}): {}", status, err_text),
                    error_type: "upstream_error".to_string(),
                });
            }

            let content_type = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_lowercase();

            let bytes = resp.bytes().await.map_err(|e| ImageGenerationError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed to read Cloudflare response body: {}", e),
                error_type: "upstream_error".to_string(),
            })?;

            if content_type.starts_with("image/") {
                if binary_output {
                    let mime = detect_image_mime_type(&bytes);
                    return Ok(ImageGenerationExecutionResult::Binary {
                        bytes: bytes.to_vec(),
                        content_type: mime,
                    });
                } else {
                    let mut norm_resp = binary_bytes_to_openai_response(&bytes);
                    if req.response_format.as_deref() == Some("url") {
                        if let Some(item) = norm_resp.data.first_mut() {
                            if let Some(b64) = item.b64_json.take() {
                                item.url = Some(format!(
                                    "data:{};base64,{}",
                                    detect_image_mime_type(&bytes),
                                    b64
                                ));
                            }
                        }
                    }
                    return Ok(ImageGenerationExecutionResult::Json(norm_resp));
                }
            }

            // Cloudflare returned JSON
            let json_val: Value =
                serde_json::from_slice(&bytes).map_err(|e| ImageGenerationError {
                    status: StatusCode::BAD_GATEWAY,
                    message: format!("Invalid JSON response from Cloudflare: {}", e),
                    error_type: "upstream_error".to_string(),
                })?;

            let mut norm_resp =
                normalize_cloudflare_response(&json_val).map_err(|e| ImageGenerationError {
                    status: StatusCode::BAD_GATEWAY,
                    message: e,
                    error_type: "upstream_error".to_string(),
                })?;

            if req.response_format.as_deref() == Some("url") {
                for item in &mut norm_resp.data {
                    if item.url.is_none() && item.b64_json.is_some() {
                        let b64 = item.b64_json.take().unwrap();
                        item.url = Some(format!("data:image/png;base64,{}", b64));
                    }
                }
            }

            if binary_output {
                if let Some((raw_bytes, mime)) = extract_binary_bytes_from_response(&norm_resp) {
                    return Ok(ImageGenerationExecutionResult::Binary {
                        bytes: raw_bytes,
                        content_type: mime,
                    });
                } else {
                    return Err(ImageGenerationError {
                        status: StatusCode::BAD_GATEWAY,
                        message: "Cloudflare response did not contain valid base64 image data for binary extraction".to_string(),
                        error_type: "upstream_error".to_string(),
                    });
                }
            }

            Ok(ImageGenerationExecutionResult::Json(norm_resp))
        }

        ResolvedImageProvider::OpenAICompatible {
            url,
            api_key,
            model,
            headers,
        } => {
            let payload = build_openai_image_payload(req, model);
            let mut builder = client.post(url).json(&payload);
            if let Some(token) = api_key {
                if !token.is_empty() {
                    builder = builder.bearer_auth(token);
                }
            }
            if let Some(hdrs) = headers {
                builder = builder.headers(hdrs.clone());
            }

            let resp = builder.send().await.map_err(|e| ImageGenerationError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("OpenAI upstream network error: {}", e),
                error_type: "upstream_error".to_string(),
            })?;

            let status = resp.status();
            if !status.is_success() {
                let err_text = resp.text().await.unwrap_or_default();
                return Err(ImageGenerationError {
                    status: StatusCode::from_u16(status.as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY),
                    message: format!(
                        "Upstream image provider error (status {}): {}",
                        status, err_text
                    ),
                    error_type: "upstream_error".to_string(),
                });
            }

            let content_type = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_lowercase();

            let bytes = resp.bytes().await.map_err(|e| ImageGenerationError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("Failed to read upstream response body: {}", e),
                error_type: "upstream_error".to_string(),
            })?;

            if content_type.starts_with("image/") {
                if binary_output {
                    let mime = detect_image_mime_type(&bytes);
                    return Ok(ImageGenerationExecutionResult::Binary {
                        bytes: bytes.to_vec(),
                        content_type: mime,
                    });
                } else {
                    let norm_resp = binary_bytes_to_openai_response(&bytes);
                    return Ok(ImageGenerationExecutionResult::Json(norm_resp));
                }
            }

            let json_val: Value =
                serde_json::from_slice(&bytes).map_err(|e| ImageGenerationError {
                    status: StatusCode::BAD_GATEWAY,
                    message: format!("Invalid JSON response from upstream: {}", e),
                    error_type: "upstream_error".to_string(),
                })?;

            let norm_resp: ImageGenerationResponse = serde_json::from_value(json_val.clone())
                .or_else(|_| normalize_cloudflare_response(&json_val))
                .map_err(|e| ImageGenerationError {
                    status: StatusCode::BAD_GATEWAY,
                    message: format!(
                        "Could not parse upstream image response into OpenAI format: {}",
                        e
                    ),
                    error_type: "upstream_error".to_string(),
                })?;

            if binary_output {
                if let Some((raw_bytes, mime)) = extract_binary_bytes_from_response(&norm_resp) {
                    return Ok(ImageGenerationExecutionResult::Binary {
                        bytes: raw_bytes,
                        content_type: mime,
                    });
                } else if let Some(first_url) =
                    norm_resp.data.first().and_then(|d| d.url.as_deref())
                {
                    let img_get = client.get(first_url).send().await.map_err(|e| {
                        ImageGenerationError {
                            status: StatusCode::BAD_GATEWAY,
                            message: format!("Failed to fetch image from URL {}: {}", first_url, e),
                            error_type: "upstream_error".to_string(),
                        }
                    })?;
                    if img_get.status().is_success() {
                        let img_bytes = img_get.bytes().await.map_err(|e| ImageGenerationError {
                            status: StatusCode::BAD_GATEWAY,
                            message: format!("Failed to read fetched image bytes: {}", e),
                            error_type: "upstream_error".to_string(),
                        })?;
                        let mime = detect_image_mime_type(&img_bytes);
                        return Ok(ImageGenerationExecutionResult::Binary {
                            bytes: img_bytes.to_vec(),
                            content_type: mime,
                        });
                    }
                }

                return Err(ImageGenerationError {
                    status: StatusCode::BAD_GATEWAY,
                    message: "Upstream response did not contain image data for binary extraction"
                        .to_string(),
                    error_type: "upstream_error".to_string(),
                });
            }

            Ok(ImageGenerationExecutionResult::Json(norm_resp))
        }
    }
}

/// Converts execution result into Axum HTTP Response
pub fn create_image_response(result: ImageGenerationExecutionResult) -> Response {
    match result {
        ImageGenerationExecutionResult::Binary {
            bytes,
            content_type,
        } => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
                (
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("inline; filename=\"image.png\""),
                ),
                (
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_static("*"),
                ),
            ],
            bytes,
        )
            .into_response(),

        ImageGenerationExecutionResult::Json(resp) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                ),
                (
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_static("*"),
                ),
            ],
            Json(resp),
        )
            .into_response(),
    }
}

/// Creates a standardized error response in OpenAI API format
pub fn create_error_response(status: StatusCode, message: &str, error_type: &str) -> Response {
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            ),
        ],
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": null
            }
        })),
    )
        .into_response()
}

// ============================================================================
// Axum HTTP Handlers
// ============================================================================

/// Native Axum handler for POST /v1/images/generations and /api/v1/images/generations
pub async fn image_generations_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ImageGenerationQuery>,
    body: Bytes,
) -> Response {
    // 1. Authenticate caller
    let api_key = match crate::routes::authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => return auth_err,
    };

    // 2. Parse request JSON body
    let req: ImageGenerationRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return create_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Invalid JSON request body: {}", e),
                "invalid_request_error",
            );
        }
    };

    // 3. Validate required prompt field
    if req.prompt.trim().is_empty() {
        return create_error_response(
            StatusCode::BAD_REQUEST,
            "Missing required field: prompt",
            "invalid_request_error",
        );
    }

    // 4. Verify model permission against API key
    let requested_model = req.model.as_deref().unwrap_or("dall-e-3");
    let (_, clean_model) = crate::proxy::strip_provider_prefix(requested_model);
    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return create_error_response(
            StatusCode::FORBIDDEN,
            &format!("Model not allowed: {}", e),
            "permission_error",
        );
    }

    // 5. Binary format detection
    let accept_header = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());
    let binary_output = is_binary_output_requested(
        req.response_format.as_deref(),
        query.response_format.as_deref(),
        accept_header,
    );

    // 6. Resolve provider connection from state/DB
    let connections = state.db.load_provider_connections().unwrap_or_default();
    let nodes = state.db.load_provider_nodes().unwrap_or_default();
    let provider = resolve_image_target(req.model.as_deref(), &connections, &nodes);

    // 7. Execute request
    match execute_image_generation(state.proxy.client(), &provider, &req, binary_output).await {
        Ok(result) => create_image_response(result),
        Err(err) => create_error_response(err.status, &err.message, &err.error_type),
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_request_all_fields() {
        let json_data = r#"{
            "model": "dall-e-3",
            "prompt": "A serene lake surrounded by mountains at dawn",
            "n": 1,
            "size": "1024x1024",
            "quality": "hd",
            "response_format": "b64_json",
            "style": "vivid",
            "output_format": "png",
            "seed": 42
        }"#;

        let req: ImageGenerationRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(req.model.as_deref(), Some("dall-e-3"));
        assert_eq!(
            req.prompt,
            "A serene lake surrounded by mountains at dawn"
        );
        assert_eq!(req.n, Some(1));
        assert_eq!(req.size.as_deref(), Some("1024x1024"));
        assert_eq!(req.quality.as_deref(), Some("hd"));
        assert_eq!(req.response_format.as_deref(), Some("b64_json"));
        assert_eq!(req.style.as_deref(), Some("vivid"));
        assert_eq!(req.output_format.as_deref(), Some("png"));
        assert_eq!(req.extra.get("seed"), Some(&Value::Number(42.into())));
    }

    #[test]
    fn test_parse_image_request_minimal() {
        let json_data = r#"{
            "prompt": "Simple sketch of a cat"
        }"#;

        let req: ImageGenerationRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(req.model, None);
        assert_eq!(req.prompt, "Simple sketch of a cat");
        assert_eq!(req.n, None);
        assert_eq!(req.size, None);
        assert_eq!(req.quality, None);
        assert_eq!(req.response_format, None);
        assert_eq!(req.style, None);
        assert!(req.extra.is_empty());
    }

    #[test]
    fn test_parse_image_request_invalid_json() {
        let invalid_json = "{ invalid_json }";
        let res: Result<ImageGenerationRequest, _> = serde_json::from_str(invalid_json);
        assert!(res.is_err());
    }

    #[test]
    fn test_size_to_dimensions_parsing() {
        assert_eq!(parse_size_to_dimensions("1024x1024"), Some((1024, 1024)));
        assert_eq!(parse_size_to_dimensions("512x768"), Some((512, 768)));
        assert_eq!(parse_size_to_dimensions(" 1920x1080 "), Some((1920, 1080)));
        assert_eq!(parse_size_to_dimensions("invalid"), None);
        assert_eq!(parse_size_to_dimensions("1024"), None);
        assert_eq!(parse_size_to_dimensions(""), None);
    }

    #[test]
    fn test_detect_image_mime_type() {
        assert_eq!(
            detect_image_mime_type(b"\x89PNG\r\n\x1a\n...random data..."),
            "image/png"
        );
        assert_eq!(
            detect_image_mime_type(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]),
            "image/jpeg"
        );
        assert_eq!(detect_image_mime_type(b"GIF89a\x01\x00\x01\x00"), "image/gif");
        assert_eq!(
            detect_image_mime_type(b"RIFF\x20\x00\x00\x00WEBPVP8 ..."),
            "image/webp"
        );
        assert_eq!(detect_image_mime_type(b"BM\x36\x00\x00\x00"), "image/bmp");
        assert_eq!(detect_image_mime_type(b"unknown bytes"), "image/png");
    }

    #[test]
    fn test_is_binary_output_requested() {
        // Body format
        assert!(is_binary_output_requested(Some("binary"), None, None));
        assert!(is_binary_output_requested(Some("raw"), None, None));
        assert!(is_binary_output_requested(Some("image"), None, None));
        assert!(!is_binary_output_requested(Some("b64_json"), None, None));
        assert!(!is_binary_output_requested(Some("url"), None, None));

        // Query param
        assert!(is_binary_output_requested(None, Some("binary"), None));
        assert!(is_binary_output_requested(None, Some("raw"), None));
        assert!(!is_binary_output_requested(None, Some("b64_json"), None));

        // Accept header
        assert!(is_binary_output_requested(None, None, Some("image/*")));
        assert!(is_binary_output_requested(None, None, Some("image/png")));
        assert!(is_binary_output_requested(None, None, Some("image/jpeg")));
        assert!(!is_binary_output_requested(
            None,
            None,
            Some("application/json")
        ));
        assert!(!is_binary_output_requested(
            None,
            None,
            Some("text/html,application/xhtml+xml")
        ));
    }

    #[test]
    fn test_cloudflare_model_detection() {
        assert!(is_cloudflare_model_or_provider(
            Some("@cf/black-forest-labs/flux-1-schnell"),
            None
        ));
        assert!(is_cloudflare_model_or_provider(
            Some("cf/black-forest-labs/flux-1-schnell"),
            None
        ));
        assert!(is_cloudflare_model_or_provider(
            Some("cfr/stabilityai/sdxl"),
            None
        ));
        assert!(is_cloudflare_model_or_provider(
            None,
            Some("cloudflare-ai")
        ));
        assert!(is_cloudflare_model_or_provider(None, Some("cloudflare")));
        assert!(is_cloudflare_model_or_provider(None, Some("cf")));

        assert!(!is_cloudflare_model_or_provider(Some("dall-e-3"), None));
        assert!(!is_cloudflare_model_or_provider(
            Some("stability/sd3"),
            Some("stability")
        ));
    }

    #[test]
    fn test_cloudflare_model_normalization() {
        assert_eq!(
            normalize_cloudflare_model(Some("@cf/black-forest-labs/flux-1-schnell")),
            "@cf/black-forest-labs/flux-1-schnell"
        );
        assert_eq!(
            normalize_cloudflare_model(Some("cf/black-forest-labs/flux-1-schnell")),
            "@cf/black-forest-labs/flux-1-schnell"
        );
        assert_eq!(
            normalize_cloudflare_model(Some("cfr/bytedance/sdxl-lightning")),
            "@cf/bytedance/sdxl-lightning"
        );
        assert_eq!(
            normalize_cloudflare_model(Some("black-forest-labs/flux-1-schnell")),
            "@cf/black-forest-labs/flux-1-schnell"
        );
        assert_eq!(
            normalize_cloudflare_model(None),
            "@cf/black-forest-labs/flux-1-schnell"
        );
    }

    #[test]
    fn test_build_cloudflare_image_payload() {
        let req = ImageGenerationRequest {
            model: Some("@cf/black-forest-labs/flux-1-schnell".to_string()),
            prompt: "A cybernetic falcon flying through neon city".to_string(),
            n: Some(1),
            size: Some("1024x768".to_string()),
            quality: None,
            response_format: None,
            style: None,
            output_format: None,
            extra: {
                let mut m = HashMap::new();
                m.insert("num_steps".to_string(), Value::Number(25.into()));
                m.insert("seed".to_string(), Value::Number(12345.into()));
                m.insert(
                    "negative_prompt".to_string(),
                    Value::String("blurry, low quality".to_string()),
                );
                m
            },
        };

        let payload = build_cloudflare_image_payload(&req);
        assert_eq!(
            payload.get("prompt"),
            Some(&Value::String(
                "A cybernetic falcon flying through neon city".to_string()
            ))
        );
        assert_eq!(payload.get("width"), Some(&Value::Number(1024.into())));
        assert_eq!(payload.get("height"), Some(&Value::Number(768.into())));
        assert_eq!(payload.get("num_steps"), Some(&Value::Number(25.into())));
        assert_eq!(payload.get("seed"), Some(&Value::Number(12345.into())));
        assert_eq!(
            payload.get("negative_prompt"),
            Some(&Value::String("blurry, low quality".to_string()))
        );
    }

    #[test]
    fn test_build_cloudflare_run_url() {
        // Default account ID
        let url = build_cloudflare_run_url(
            None,
            None,
            "@cf/black-forest-labs/flux-1-schnell",
        );
        assert_eq!(
            url,
            "https://api.cloudflare.com/client/v4/accounts/1da22c19548115accf1b98ab7c562b2c/ai/run/@cf/black-forest-labs/flux-1-schnell"
        );

        // Custom account ID
        let url_custom = build_cloudflare_run_url(
            None,
            Some("my-account-999"),
            "@cf/stabilityai/sdxl",
        );
        assert_eq!(
            url_custom,
            "https://api.cloudflare.com/client/v4/accounts/my-account-999/ai/run/@cf/stabilityai/sdxl"
        );

        // Custom base URL with /ai/run
        let url_base = build_cloudflare_run_url(
            Some("https://gateway.ai.cloudflare.com/v1/acc/gw/workers-ai/ai/run"),
            None,
            "@cf/black-forest-labs/flux-1-schnell",
        );
        assert_eq!(
            url_base,
            "https://gateway.ai.cloudflare.com/v1/acc/gw/workers-ai/ai/run/@cf/black-forest-labs/flux-1-schnell"
        );
    }

    #[test]
    fn test_normalize_cloudflare_response_result_image() {
        let cf_json = json!({
            "result": {
                "image": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            },
            "success": true,
            "errors": [],
            "messages": []
        });

        let norm = normalize_cloudflare_response(&cf_json).unwrap();
        assert_eq!(norm.data.len(), 1);
        assert_eq!(
            norm.data[0].b64_json.as_deref(),
            Some("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        );
        assert_eq!(norm.data[0].url, None);
    }

    #[test]
    fn test_normalize_cloudflare_response_direct_b64() {
        let cf_json = json!({
            "result": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
        });

        let norm = normalize_cloudflare_response(&cf_json).unwrap();
        assert_eq!(norm.data.len(), 1);
        assert_eq!(
            norm.data[0].b64_json.as_deref(),
            Some("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        );
    }

    #[test]
    fn test_normalize_cloudflare_response_queued() {
        let cf_json = json!({
            "result": {
                "responses": [
                    {
                        "success": true,
                        "result": {
                            "image": "queued_b64_image_content"
                        }
                    }
                ]
            }
        });

        let norm = normalize_cloudflare_response(&cf_json).unwrap();
        assert_eq!(norm.data.len(), 1);
        assert_eq!(
            norm.data[0].b64_json.as_deref(),
            Some("queued_b64_image_content")
        );
    }

    #[test]
    fn test_normalize_cloudflare_response_data_uri() {
        let cf_json = json!({
            "result": {
                "image": "data:image/png;base64,data_uri_content_here"
            }
        });

        let norm = normalize_cloudflare_response(&cf_json).unwrap();
        assert_eq!(norm.data.len(), 1);
        assert_eq!(
            norm.data[0].b64_json.as_deref(),
            Some("data_uri_content_here")
        );
    }

    #[test]
    fn test_normalize_cloudflare_response_already_openai() {
        let openai_json = json!({
            "created": 1700000000,
            "data": [
                {
                    "b64_json": "existing_b64_json_string"
                }
            ]
        });

        let norm = normalize_cloudflare_response(&openai_json).unwrap();
        assert_eq!(norm.created, 1700000000);
        assert_eq!(norm.data.len(), 1);
        assert_eq!(
            norm.data[0].b64_json.as_deref(),
            Some("existing_b64_json_string")
        );
    }

    #[test]
    fn test_binary_bytes_to_openai_response() {
        let bytes = b"fake image bytes content";
        let resp = binary_bytes_to_openai_response(bytes);
        assert_eq!(resp.data.len(), 1);
        let b64 = resp.data[0].b64_json.as_ref().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn test_extract_binary_bytes_from_response() {
        let bytes = b"\x89PNG\r\n\x1a\nfake png data";
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let resp = ImageGenerationResponse {
            created: 12345,
            data: vec![ImageData {
                b64_json: Some(b64),
                url: None,
                revised_prompt: None,
            }],
        };

        let (extracted_bytes, mime) = extract_binary_bytes_from_response(&resp).unwrap();
        assert_eq!(extracted_bytes, bytes);
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn test_build_openai_image_payload() {
        let req = ImageGenerationRequest {
            model: Some("dall-e-3".to_string()),
            prompt: "A futuristic city in the clouds".to_string(),
            n: Some(1),
            size: Some("1024x1024".to_string()),
            quality: Some("hd".to_string()),
            response_format: Some("b64_json".to_string()),
            style: Some("vivid".to_string()),
            output_format: None,
            extra: HashMap::new(),
        };

        let payload = build_openai_image_payload(&req, "dall-e-3");
        assert_eq!(payload.get("model"), Some(&Value::String("dall-e-3".to_string())));
        assert_eq!(
            payload.get("prompt"),
            Some(&Value::String("A futuristic city in the clouds".to_string()))
        );
        assert_eq!(payload.get("n"), Some(&Value::Number(1.into())));
        assert_eq!(
            payload.get("size"),
            Some(&Value::String("1024x1024".to_string()))
        );
        assert_eq!(payload.get("quality"), Some(&Value::String("hd".to_string())));
        assert_eq!(
            payload.get("response_format"),
            Some(&Value::String("b64_json".to_string()))
        );
        assert_eq!(payload.get("style"), Some(&Value::String("vivid".to_string())));
    }

    #[test]
    fn test_build_openai_images_url() {
        assert_eq!(
            build_openai_images_url(None),
            "https://api.openai.com/v1/images/generations"
        );
        assert_eq!(
            build_openai_images_url(Some("https://api.openai.com/v1")),
            "https://api.openai.com/v1/images/generations"
        );
        assert_eq!(
            build_openai_images_url(Some("https://custom.gateway.com")),
            "https://custom.gateway.com/v1/images/generations"
        );
        assert_eq!(
            build_openai_images_url(Some("https://api.example.com/v1/images/generations")),
            "https://api.example.com/v1/images/generations"
        );
    }

    #[test]
    fn test_resolve_image_target_cloudflare() {
        let conn = ProviderConnection {
            id: "conn-cf".to_string(),
            provider: "cloudflare-ai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("Cloudflare".to_string()),
            email: None,
            priority: Some(10),
            is_active: true,
            data: crate::db::ConnectionData {
                api_key: Some("cf-test-token-123".to_string()),
                base_url: None,
                headers: None,
                default_model: None,
                extra: {
                    let mut m = HashMap::new();
                    m.insert(
                        "providerSpecificData".to_string(),
                        json!({ "accountId": "acc-xyz-789" }),
                    );
                    m
                },
            },
            raw_data: "{}".to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        let connections = vec![conn.clone()];
        let nodes = vec![];

        let target = resolve_image_target(
            Some("@cf/black-forest-labs/flux-1-schnell"),
            &connections,
            &nodes,
        );

        match target {
            ResolvedImageProvider::Cloudflare { url, api_key, model } => {
                assert_eq!(api_key.as_deref(), Some("cf-test-token-123"));
                assert_eq!(model, "@cf/black-forest-labs/flux-1-schnell");
                assert!(url.contains("acc-xyz-789"));
                assert!(url.contains("@cf/black-forest-labs/flux-1-schnell"));
            }
            _ => panic!("Expected Cloudflare target"),
        }
    }

    #[test]
    fn test_resolve_image_target_openai_prefix() {
        let conn = ProviderConnection {
            id: "conn-stability".to_string(),
            provider: "stability".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("Stability AI".to_string()),
            email: None,
            priority: Some(10),
            is_active: true,
            data: crate::db::ConnectionData {
                api_key: Some("sk-stability-token".to_string()),
                base_url: Some("https://api.stability.ai".to_string()),
                headers: None,
                default_model: None,
                extra: HashMap::new(),
            },
            raw_data: "{}".to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        let connections = vec![conn];
        let nodes = vec![];

        let target = resolve_image_target(Some("stability/sd3-medium"), &connections, &nodes);

        match target {
            ResolvedImageProvider::OpenAICompatible {
                url,
                api_key,
                model,
                headers: _,
            } => {
                assert_eq!(api_key.as_deref(), Some("sk-stability-token"));
                assert_eq!(model, "sd3-medium");
                assert_eq!(url, "https://api.stability.ai/v1/images/generations");
            }
            _ => panic!("Expected OpenAICompatible target"),
        }
    }

    #[test]
    fn test_resolve_image_target_default_openai() {
        let conn = ProviderConnection {
            id: "conn-openai".to_string(),
            provider: "openai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("OpenAI".to_string()),
            email: None,
            priority: Some(10),
            is_active: true,
            data: crate::db::ConnectionData {
                api_key: Some("sk-openai-token-123".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                headers: None,
                default_model: Some("dall-e-3".to_string()),
                extra: HashMap::new(),
            },
            raw_data: "{}".to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        let connections = vec![conn];
        let nodes = vec![];

        // No model specified -> defaults to dall-e-3 and OpenAI connection
        let target = resolve_image_target(None, &connections, &nodes);

        match target {
            ResolvedImageProvider::OpenAICompatible {
                url,
                api_key,
                model,
                headers: _,
            } => {
                assert_eq!(api_key.as_deref(), Some("sk-openai-token-123"));
                assert_eq!(model, "dall-e-3");
                assert_eq!(url, "https://api.openai.com/v1/images/generations");
            }
            _ => panic!("Expected OpenAICompatible target"),
        }
    }

    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::ApiKey;
    use crate::proxy::ProxyService;
    use crate::routes::{create_router, ConsoleLogBuffer};

    fn create_test_app_state() -> (AppState, Arc<crate::db::Database>) {
        let config = Arc::new(Config::default());
        let db = crate::db::Database::open_in_memory().unwrap();
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState {
            config: Arc::clone(&config),
            db: db.clone(),
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: ConsoleLogBuffer::new(),
            rate_limiter: Arc::new(crate::ratelimit::RateLimiter::from_config(&config)),
        };
        (state, db)
    }

    #[tokio::test]
    async fn test_router_unauthorized_image_generation() {
        let (state, _) = create_test_app_state();
        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"prompt": "A sunset over ocean"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_router_missing_prompt_image_generation() {
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-img-key".to_string(),
            key: "sk-img-test".to_string(),
            is_active: true,
            allowed_models: "*".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-img-test")
            .body(Body::from(r#"{"prompt": ""}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let err_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(err_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Missing required field: prompt"));
    }

    #[tokio::test]
    async fn test_router_invalid_json_image_generation() {
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-img-key".to_string(),
            key: "sk-img-test".to_string(),
            is_active: true,
            allowed_models: "*".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-img-test")
            .body(Body::from(r#"not a valid json"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let err_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(err_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid JSON request body"));
    }

    #[tokio::test]
    async fn test_router_disallowed_model_image_generation() {
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-img-key".to_string(),
            key: "sk-img-test".to_string(),
            is_active: true,
            allowed_models: "gpt-4o,claude-3-5-sonnet".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-img-test")
            .body(Body::from(
                r#"{"model": "dall-e-3", "prompt": "a beautiful forest"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let err_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(err_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Model not allowed"));
    }

    #[tokio::test]
    async fn test_router_options_image_generation() {
        let (state, _) = create_test_app_state();
        let app = create_router(state);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/v1/images/generations")
            .body(Body::empty())
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let req2 = Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/images/generations")
            .body(Body::empty())
            .unwrap();

        let resp2 = app.clone().oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        let req3 = Request::builder()
            .method("OPTIONS")
            .uri("/images/generations")
            .body(Body::empty())
            .unwrap();

        let resp3 = app.oneshot(req3).await.unwrap();
        assert_eq!(resp3.status(), StatusCode::OK);
    }
}
