use std::collections::HashMap;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{ProviderConnection, ProviderNode};
use crate::routes::AppState;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenerationRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub aspect_ratio: Option<String>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub loop_video: Option<bool>,
    #[serde(default)]
    pub response_format: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenerationResponse {
    pub created: i64,
    pub data: Vec<VideoData>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoProviderType {
    OpenAiCompatible,
    LumaAi,
    Runway,
    CogVideo,
}

#[derive(Debug, Clone)]
pub struct ResolvedVideoProvider {
    pub provider_type: VideoProviderType,
    pub endpoint_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub custom_headers: Option<HeaderMap>,
}

// ============================================================================
// Helpers
// ============================================================================

pub fn build_video_url(base_url: Option<&str>) -> String {
    let base = base_url.unwrap_or("https://api.openai.com");
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/videos/generations") {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/v1") {
        return format!("{}/videos/generations", trimmed);
    }
    format!("{}/v1/videos/generations", trimmed)
}

pub fn resolve_video_target(
    candidate_model: Option<&str>,
    connections: &[ProviderConnection],
    nodes: &[ProviderNode],
) -> ResolvedVideoProvider {
    let model_str = candidate_model.unwrap_or("").trim();

    // 1. Check custom provider nodes with prefix
    for node in nodes {
        let prefix_slash = format!("{}/", node.prefix);
        if !node.prefix.is_empty()
            && (model_str.starts_with(&prefix_slash) || model_str == node.prefix)
        {
            let upstream_model = if model_str.len() > prefix_slash.len() {
                model_str[prefix_slash.len()..].to_string()
            } else {
                model_str.to_string()
            };

            let endpoint = build_video_url(Some(&node.base_url));
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

            return ResolvedVideoProvider {
                provider_type: VideoProviderType::OpenAiCompatible,
                endpoint_url: endpoint,
                api_key,
                model: upstream_model,
                custom_headers,
            };
        }
    }

    // 2. Active video connection (luma, runway, cogvideo, pika, openai)
    if let Some(pc) = connections.iter().find(|c| {
        c.is_active
            && (c.provider == "luma"
                || c.provider == "runway"
                || c.provider == "cogvideo"
                || c.provider == "pika"
                || c.provider == "kling"
                || c.provider == "openai")
    }) {
        let endpoint = build_video_url(pc.data.base_url.as_deref());
        return ResolvedVideoProvider {
            provider_type: VideoProviderType::OpenAiCompatible,
            endpoint_url: endpoint,
            api_key: pc.data.api_key.clone(),
            model: model_str.to_string(),
            custom_headers: None,
        };
    }

    // 3. Fallback
    ResolvedVideoProvider {
        provider_type: VideoProviderType::OpenAiCompatible,
        endpoint_url: "https://api.openai.com/v1/videos/generations".to_string(),
        api_key: None,
        model: if model_str.is_empty() {
            "sora-1.0".to_string()
        } else {
            model_str.to_string()
        },
        custom_headers: None,
    }
}

// ============================================================================
// Handler
// ============================================================================

pub async fn video_generations_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VideoGenerationRequest>,
) -> Response {
    if let Err(e) = crate::routes::authenticate(&headers, &state.db) {
        return e.into_response();
    }

    if req.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "prompt is required and cannot be empty",
                    "type": "invalid_request_error"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let connections = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let target = resolve_video_target(req.model.as_deref(), &connections, &provider_nodes);

    if target.api_key.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "No configured video provider connection found for model. Connect a video provider (such as Luma, Runway, Kling, or OpenAI Sora) under Providers to generate videos.",
                    "type": "provider_not_found"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    // Forward to upstream
    let mut req_builder = state
        .proxy
        .client()
        .post(&target.endpoint_url)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(ref key) = target.api_key {
        req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", key));
    }

    if let Some(hdrs) = target.custom_headers {
        for (k, v) in hdrs {
            if let Some(k) = k {
                req_builder = req_builder.header(k, v);
            }
        }
    }

    let mut payload = json!({
        "prompt": req.prompt,
        "model": target.model
    });

    if let Some(ref ar) = req.aspect_ratio {
        payload["aspect_ratio"] = json!(ar);
    }
    if let Some(d) = req.duration {
        payload["duration"] = json!(d);
    }
    for (k, v) in req.extra {
        payload[k] = v;
    }

    match req_builder.json(&payload).send().await {
        Ok(res) => {
            let status = res.status();
            let body_bytes = res.bytes().await.unwrap_or_default();
            (
                status,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                ],
                body_bytes,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": format!("Upstream video generation failed: {e}"),
                    "type": "upstream_error"
                }
            })
            .to_string(),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_video_url() {
        assert_eq!(
            build_video_url(Some("https://api.openai.com")),
            "https://api.openai.com/v1/videos/generations"
        );
        assert_eq!(
            build_video_url(Some("https://api.luma.ai/v1")),
            "https://api.luma.ai/v1/videos/generations"
        );
        assert_eq!(
            build_video_url(Some("https://custom.endpoint.com/videos/generations")),
            "https://custom.endpoint.com/videos/generations"
        );
    }

    #[test]
    fn test_resolve_video_target_fallback() {
        let target = resolve_video_target(Some("sora-2"), &[], &[]);
        assert_eq!(target.model, "sora-2");
        assert_eq!(target.api_key, None);
        assert_eq!(
            target.endpoint_url,
            "https://api.openai.com/v1/videos/generations"
        );
    }
}
