use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{ProviderConnection, ProviderNode};
use crate::routes::AppState;

// ============================================================================
// Types & Request/Response Structs
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
    #[serde(default)]
    pub image: Option<Value>,
    #[serde(default)]
    pub image_url: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoEditRequest {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub video: Option<Value>,
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub mask: Option<Value>,
    #[serde(default)]
    pub image: Option<Value>,
    #[serde(default)]
    pub aspect_ratio: Option<String>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoExtensionRequest {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub video: Option<Value>,
    #[serde(default)]
    pub video_id: Option<String>,
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VideoPollQuery {
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoGenerationResponse {
    pub created: i64,
    pub data: Vec<VideoData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    pub created: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: String, // "pending", "processing", "completed", "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>, // "generations", "edits", "extensions"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub videos: Option<Vec<VideoOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<VideoData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoProviderType {
    OpenAiCompatible,
    Xai,
    OpenRouter,
    LumaAi,
    Runway,
    CogVideo,
    Vertex,
}

#[derive(Debug, Clone)]
pub struct ResolvedVideoProvider {
    pub provider_type: VideoProviderType,
    pub endpoint_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub custom_headers: Option<HeaderMap>,
    pub provider_id: String,
    pub connection_id: Option<String>,
}

// ============================================================================
// Video Task Store
// ============================================================================

pub type VideoTaskStore = Arc<RwLock<HashMap<String, VideoTask>>>;

static GLOBAL_VIDEO_TASKS: OnceLock<VideoTaskStore> = OnceLock::new();

pub fn get_video_task_store() -> &'static VideoTaskStore {
    GLOBAL_VIDEO_TASKS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

pub fn create_video_task(
    action: &str,
    model: Option<&str>,
    prompt: Option<&str>,
    endpoint_url: Option<String>,
    api_key: Option<String>,
    provider: Option<String>,
    connection_id: Option<String>,
) -> VideoTask {
    let now = chrono::Utc::now().timestamp();
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let id = format!("video-{}-{}", action, nanos);
    let task = VideoTask {
        id: id.clone(),
        request_id: Some(id.clone()),
        object: Some("video.generation".to_string()),
        created: now,
        updated_at: Some(now),
        model: model.map(|m| m.to_string()),
        status: "pending".to_string(),
        action: Some(action.to_string()),
        prompt: prompt.map(|p| p.to_string()),
        progress: Some(0.0),
        url: None,
        b64_json: None,
        video: None,
        videos: None,
        data: None,
        error: None,
        upstream_id: None,
        endpoint_url,
        api_key,
        provider,
        connection_id,
    };
    insert_task(task.clone());
    task
}

pub fn insert_task(task: VideoTask) {
    let mut store = get_video_task_store().write().unwrap();
    store.insert(task.id.clone(), task);
}

pub fn get_task(id: &str) -> Option<VideoTask> {
    let store = get_video_task_store().read().unwrap();
    store.get(id).cloned()
}

pub fn update_task_status(id: &str, status: &str, progress: Option<f32>) -> Option<VideoTask> {
    let mut store = get_video_task_store().write().unwrap();
    if let Some(task) = store.get_mut(id) {
        task.status = status.to_string();
        task.updated_at = Some(chrono::Utc::now().timestamp());
        if let Some(p) = progress {
            task.progress = Some(p);
        }
        return Some(task.clone());
    }
    None
}

pub fn complete_task(
    id: &str,
    url: Option<String>,
    b64_json: Option<String>,
) -> Option<VideoTask> {
    let mut store = get_video_task_store().write().unwrap();
    if let Some(task) = store.get_mut(id) {
        task.status = "completed".to_string();
        task.updated_at = Some(chrono::Utc::now().timestamp());
        task.progress = Some(1.0);
        task.url = url.clone();
        task.b64_json = b64_json.clone();
        let output = VideoOutput {
            url: url.clone(),
            b64_json: b64_json.clone(),
            mime_type: Some("video/mp4".to_string()),
            width: None,
            height: None,
            duration: None,
        };
        task.video = Some(output.clone());
        task.videos = Some(vec![output]);
        task.data = Some(vec![VideoData {
            id: Some(id.to_string()),
            url,
            b64_json,
            status: Some("completed".to_string()),
        }]);
        return Some(task.clone());
    }
    None
}

pub fn fail_task(id: &str, error_msg: &str) -> Option<VideoTask> {
    let mut store = get_video_task_store().write().unwrap();
    if let Some(task) = store.get_mut(id) {
        task.status = "failed".to_string();
        task.updated_at = Some(chrono::Utc::now().timestamp());
        task.error = Some(json!({ "message": error_msg }));
        return Some(task.clone());
    }
    None
}

pub fn clear_all_tasks() {
    let mut store = get_video_task_store().write().unwrap();
    store.clear();
}

// ============================================================================
// URL & Provider Resolution Helpers
// ============================================================================

pub fn build_video_url_for_action(base_url: Option<&str>, action: &str) -> String {
    let base = base_url.unwrap_or("https://api.openai.com");
    let trimmed = base.trim_end_matches('/');

    let target_suffix = format!("/videos/{}", action);
    if trimmed.ends_with(&target_suffix) {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/videos") {
        return format!("{}/{}", trimmed, action);
    }
    if trimmed.ends_with("/v1") {
        return format!("{}/videos/{}", trimmed, action);
    }
    format!("{}/v1/videos/{}", trimmed, action)
}

pub fn build_video_url(base_url: Option<&str>) -> String {
    build_video_url_for_action(base_url, "generations")
}

pub fn build_video_poll_url(base_url: Option<&str>, id: &str) -> String {
    let base = base_url.unwrap_or("https://api.openai.com");
    let trimmed = base.trim_end_matches('/');

    if trimmed.ends_with("/videos") {
        return format!("{}/{}", trimmed, id);
    }
    for act in &["generations", "edits", "extensions"] {
        let act_suffix = format!("/videos/{}", act);
        if trimmed.ends_with(&act_suffix) {
            let prefix = &trimmed[..trimmed.len() - act.len() - 1];
            return format!("{}/{}", prefix, id);
        }
    }
    if trimmed.ends_with("/v1") {
        return format!("{}/videos/{}", trimmed, id);
    }
    format!("{}/v1/videos/{}", trimmed, id)
}

pub fn resolve_video_target(
    candidate_model: Option<&str>,
    connections: &[ProviderConnection],
    nodes: &[ProviderNode],
) -> ResolvedVideoProvider {
    resolve_video_target_for_action(candidate_model, "generations", connections, nodes)
}

pub fn resolve_video_target_for_action(
    candidate_model: Option<&str>,
    action: &str,
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

            let endpoint = build_video_url_for_action(Some(&node.base_url), action);
            let mut api_key = None;
            let mut custom_headers = None;
            let mut connection_id = None;

            if let Some(pc) = connections.iter().find(|c| {
                c.is_active
                    && (c.provider == node.id
                        || c.provider == node.prefix
                        || node.name.as_ref() == Some(&c.provider))
            }) {
                api_key = pc.data.api_key.clone();
                connection_id = Some(pc.id.clone());
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
                provider_id: node.id.clone(),
                connection_id,
            };
        }
    }

    // 2. Check if model has a provider prefix, e.g. "xai/grok-imagine" or "luma/dream-machine"
    if let Some((prov, m)) = model_str.split_once('/') {
        if let Some(pc) = connections.iter().find(|c| c.is_active && c.provider == prov) {
            let endpoint = build_video_url_for_action(pc.data.base_url.as_deref(), action);
            return ResolvedVideoProvider {
                provider_type: match prov {
                    "xai" => VideoProviderType::Xai,
                    "openrouter" => VideoProviderType::OpenRouter,
                    "luma" => VideoProviderType::LumaAi,
                    "runway" => VideoProviderType::Runway,
                    "vertex" => VideoProviderType::Vertex,
                    "cogvideo" => VideoProviderType::CogVideo,
                    _ => VideoProviderType::OpenAiCompatible,
                },
                endpoint_url: endpoint,
                api_key: pc.data.api_key.clone(),
                model: m.to_string(),
                custom_headers: None,
                provider_id: prov.to_string(),
                connection_id: Some(pc.id.clone()),
            };
        }
    }

    // 3. Inferred provider from model name
    let inferred_provider = if model_str.contains("grok") || model_str.contains("imagine") {
        Some("xai")
    } else if model_str.contains("luma") || model_str.contains("ray") || model_str.contains("dream-machine") {
        Some("luma")
    } else if model_str.contains("runway") || model_str.contains("gen3") || model_str.contains("gen-3") {
        Some("runway")
    } else if model_str.contains("veo") {
        Some("vertex")
    } else if model_str.contains("kling") {
        Some("kling")
    } else if model_str.contains("cogvideo") {
        Some("cogvideo")
    } else if model_str.contains("pika") {
        Some("pika")
    } else if model_str.contains("sora") {
        Some("openai")
    } else {
        None
    };

    if let Some(inf_prov) = inferred_provider {
        if let Some(pc) = connections.iter().find(|c| c.is_active && c.provider == inf_prov) {
            let endpoint = build_video_url_for_action(pc.data.base_url.as_deref(), action);
            return ResolvedVideoProvider {
                provider_type: match inf_prov {
                    "xai" => VideoProviderType::Xai,
                    "openrouter" => VideoProviderType::OpenRouter,
                    "luma" => VideoProviderType::LumaAi,
                    "runway" => VideoProviderType::Runway,
                    "vertex" => VideoProviderType::Vertex,
                    "cogvideo" => VideoProviderType::CogVideo,
                    _ => VideoProviderType::OpenAiCompatible,
                },
                endpoint_url: endpoint,
                api_key: pc.data.api_key.clone(),
                model: model_str.to_string(),
                custom_headers: None,
                provider_id: inf_prov.to_string(),
                connection_id: Some(pc.id.clone()),
            };
        }
    }

    // 4. Any active video provider
    let video_providers = [
        "xai", "luma", "runway", "kling", "cogvideo", "pika", "openai", "openrouter", "vertex",
    ];
    if let Some(pc) = connections.iter().find(|c| c.is_active && video_providers.contains(&c.provider.as_str())) {
        let endpoint = build_video_url_for_action(pc.data.base_url.as_deref(), action);
        let prov_id = pc.provider.clone();
        return ResolvedVideoProvider {
            provider_type: match prov_id.as_str() {
                "xai" => VideoProviderType::Xai,
                "openrouter" => VideoProviderType::OpenRouter,
                "luma" => VideoProviderType::LumaAi,
                "runway" => VideoProviderType::Runway,
                "vertex" => VideoProviderType::Vertex,
                "cogvideo" => VideoProviderType::CogVideo,
                _ => VideoProviderType::OpenAiCompatible,
            },
            endpoint_url: endpoint,
            api_key: pc.data.api_key.clone(),
            model: if model_str.is_empty() {
                "sora-1.0".to_string()
            } else {
                model_str.to_string()
            },
            custom_headers: None,
            provider_id: prov_id,
            connection_id: Some(pc.id.clone()),
        };
    }

    // 5. Fallback
    ResolvedVideoProvider {
        provider_type: VideoProviderType::OpenAiCompatible,
        endpoint_url: build_video_url_for_action(None, action),
        api_key: None,
        model: if model_str.is_empty() {
            "sora-1.0".to_string()
        } else {
            model_str.to_string()
        },
        custom_headers: None,
        provider_id: "openai".to_string(),
        connection_id: None,
    }
}

// ============================================================================
// Core Execution Helper
// ============================================================================

async fn execute_video_creation(
    state: &AppState,
    headers: &HeaderMap,
    action: &str,
    model: Option<&str>,
    prompt: Option<&str>,
    payload: Value,
) -> Response {
    let api_key = match crate::routes::authenticate(headers, &state.db) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    let requested_model = model.unwrap_or("sora-1.0");
    let (_, clean_model) = crate::proxy::strip_provider_prefix(requested_model);
    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": format!("Model not allowed for this API key: {}", e),
                    "type": "permission_error"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let connections = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();

    let target = resolve_video_target_for_action(model, action, &connections, &provider_nodes);

    if target.api_key.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "No configured video provider connection found for model. Connect a video provider (such as Luma, Runway, Kling, xAI, or OpenAI Sora) under Providers to generate videos.",
                    "type": "provider_not_found"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let task = create_video_task(
        action,
        Some(&target.model),
        prompt,
        Some(target.endpoint_url.clone()),
        target.api_key.clone(),
        Some(target.provider_id.clone()),
        target.connection_id.clone(),
    );

    let mut req_builder = state
        .proxy
        .client()
        .post(&target.endpoint_url)
        .header(header::CONTENT_TYPE, "application/json");

    if let Some(ref key) = target.api_key {
        req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", key));
    }
    if let Some(ref cid) = target.connection_id {
        req_builder = req_builder.header("x-connection-id", cid);
    }
    if let Some(hdrs) = target.custom_headers {
        for (k, v) in hdrs {
            if let Some(k) = k {
                req_builder = req_builder.header(k, v);
            }
        }
    }

    match req_builder.json(&payload).send().await {
        Ok(res) => {
            let status = res.status();
            let body_bytes = res.bytes().await.unwrap_or_default();
            if status.is_success() {
                if let Ok(mut resp_val) = serde_json::from_slice::<Value>(&body_bytes) {
                    let upstream_id = resp_val
                        .get("id")
                        .or_else(|| resp_val.get("request_id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let upstream_status = resp_val
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending");

                    let video_url = resp_val
                        .get("video")
                        .and_then(|v| v.get("url"))
                        .and_then(|u| u.as_str())
                        .or_else(|| resp_val.get("url").and_then(|u| u.as_str()))
                        .or_else(|| {
                            resp_val
                                .get("data")
                                .and_then(|d| d.get(0))
                                .and_then(|item| item.get("url"))
                                .and_then(|u| u.as_str())
                        })
                        .map(|s| s.to_string());

                    let b64_json = resp_val
                        .get("video")
                        .and_then(|v| v.get("b64_json"))
                        .and_then(|b| b.as_str())
                        .or_else(|| resp_val.get("b64_json").and_then(|b| b.as_str()))
                        .or_else(|| {
                            resp_val
                                .get("data")
                                .and_then(|d| d.get(0))
                                .and_then(|item| item.get("b64_json"))
                                .and_then(|b| b.as_str())
                        })
                        .map(|s| s.to_string());

                    if video_url.is_some() || b64_json.is_some() || upstream_status == "completed" {
                        complete_task(&task.id, video_url, b64_json);
                    } else if upstream_status == "failed" {
                        fail_task(&task.id, "Upstream marked video task failed");
                    } else {
                        update_task_status(&task.id, upstream_status, None);
                    }

                    if let Some(uid) = upstream_id {
                        let mut store = get_video_task_store().write().unwrap();
                        if let Some(t) = store.get_mut(&task.id) {
                            t.upstream_id = Some(uid.clone());
                        }
                        if let Some(t) = store.get(&task.id).cloned() {
                            store.insert(uid, t);
                        }
                    }

                    let mut headers_out = HeaderMap::new();
                    headers_out.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    headers_out.insert(
                        header::ACCESS_CONTROL_ALLOW_ORIGIN,
                        HeaderValue::from_static("*"),
                    );
                    if let Ok(tid_hdr) = HeaderValue::from_str(&task.id) {
                        headers_out.insert(
                            HeaderName::from_static("x-9router-task-id"),
                            tid_hdr,
                        );
                    }
                    if let Some(ref cid) = target.connection_id {
                        if let Ok(v) = HeaderValue::from_str(cid) {
                            headers_out.insert(
                                HeaderName::from_static("x-9router-connection-id"),
                                v,
                            );
                        }
                    }

                    if resp_val.get("id").is_none() && resp_val.is_object() {
                        resp_val["id"] = json!(task.id);
                        resp_val["request_id"] = json!(task.id);
                        return (status, headers_out, resp_val.to_string()).into_response();
                    }

                    return (status, headers_out, body_bytes).into_response();
                }
            } else {
                fail_task(&task.id, &format!("Upstream returned status {}", status));
            }

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
        Err(e) => {
            fail_task(&task.id, &format!("Upstream video creation failed: {e}"));
            (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                json!({
                    "error": {
                        "message": format!("Upstream video {action} failed: {e}"),
                        "type": "upstream_error"
                    }
                })
                .to_string(),
            )
                .into_response()
        }
    }
}

// ============================================================================
// Route Handlers
// ============================================================================

pub async fn video_generations_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VideoGenerationRequest>,
) -> Response {
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

    let mut payload = json!({
        "prompt": req.prompt,
        "model": req.model.as_deref().unwrap_or("sora-1.0")
    });

    if let Some(ref ar) = req.aspect_ratio {
        payload["aspect_ratio"] = json!(ar);
    }
    if let Some(d) = req.duration {
        payload["duration"] = json!(d);
    }
    if let Some(l) = req.loop_video {
        payload["loop"] = json!(l);
    }
    if let Some(ref rf) = req.response_format {
        payload["response_format"] = json!(rf);
    }
    if let Some(ref img) = req.image {
        payload["image"] = img.clone();
    }
    if let Some(ref img_url) = req.image_url {
        payload["image_url"] = img_url.clone();
    }
    for (k, v) in req.extra {
        payload[k] = v;
    }

    execute_video_creation(
        &state,
        &headers,
        "generations",
        req.model.as_deref(),
        Some(&req.prompt),
        payload,
    )
    .await
}

pub async fn video_edits_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VideoEditRequest>,
) -> Response {
    let has_prompt = req.prompt.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_video = req.video.is_some() || req.video_url.is_some();
    let has_image = req.image.is_some();

    if !has_prompt && !has_video && !has_image {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "At least one of prompt, video, video_url, or image is required for video editing",
                    "type": "invalid_request_error"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let mut payload = json!({
        "model": req.model.as_deref().unwrap_or("sora-1.0")
    });

    if let Some(ref p) = req.prompt {
        payload["prompt"] = json!(p);
    }
    if let Some(ref v) = req.video {
        payload["video"] = v.clone();
    }
    if let Some(ref vu) = req.video_url {
        payload["video_url"] = json!(vu);
    }
    if let Some(ref m) = req.mask {
        payload["mask"] = m.clone();
    }
    if let Some(ref img) = req.image {
        payload["image"] = img.clone();
    }
    if let Some(ref ar) = req.aspect_ratio {
        payload["aspect_ratio"] = json!(ar);
    }
    if let Some(d) = req.duration {
        payload["duration"] = json!(d);
    }
    for (k, v) in req.extra {
        payload[k] = v;
    }

    execute_video_creation(
        &state,
        &headers,
        "edits",
        req.model.as_deref(),
        req.prompt.as_deref(),
        payload,
    )
    .await
}

pub async fn video_extensions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VideoExtensionRequest>,
) -> Response {
    let has_video = req.video.is_some() || req.video_id.is_some() || req.video_url.is_some();
    let has_prompt = req.prompt.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);

    if !has_video && !has_prompt {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            json!({
                "error": {
                    "message": "At least one of video, video_id, video_url, or prompt is required for video extension",
                    "type": "invalid_request_error"
                }
            })
            .to_string(),
        )
            .into_response();
    }

    let mut payload = json!({
        "model": req.model.as_deref().unwrap_or("sora-1.0")
    });

    if let Some(ref p) = req.prompt {
        payload["prompt"] = json!(p);
    }
    if let Some(ref v) = req.video {
        payload["video"] = v.clone();
    }
    if let Some(ref vid) = req.video_id {
        payload["video_id"] = json!(vid);
    }
    if let Some(ref vu) = req.video_url {
        payload["video_url"] = json!(vu);
    }
    if let Some(d) = req.duration {
        payload["duration"] = json!(d);
    }
    for (k, v) in req.extra {
        payload[k] = v;
    }

    execute_video_creation(
        &state,
        &headers,
        "extensions",
        req.model.as_deref(),
        req.prompt.as_deref(),
        payload,
    )
    .await
}

pub async fn video_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<VideoPollQuery>,
) -> Response {
    let api_key = match crate::routes::authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    if let Some(mut task) = get_task(&id) {
        if let Some(ref m) = task.model {
            let (_, clean_model) = crate::proxy::strip_provider_prefix(m);
            if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
                return (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({
                        "error": {
                            "message": format!("Model not allowed for this API key: {}", e),
                            "type": "permission_error"
                        }
                    })
                    .to_string(),
                )
                    .into_response();
            }
        }

        // If completed or failed, return directly
        if task.status == "completed" || task.status == "failed" {
            return (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                ],
                Json(task),
            )
                .into_response();
        }

        // If pending or processing, and has upstream info, attempt to poll upstream
        if let Some(ref endpoint_url) = task.endpoint_url {
            let poll_id = task.upstream_id.as_deref().unwrap_or(&id);
            let poll_url = build_video_poll_url(Some(endpoint_url), poll_id);
            let mut poll_builder = state.proxy.client().get(&poll_url);

            if let Some(ref key) = task.api_key {
                poll_builder = poll_builder.header(header::AUTHORIZATION, format!("Bearer {}", key));
            }
            if let Some(ref cid) = task.connection_id {
                poll_builder = poll_builder.header("x-connection-id", cid);
            }

            if let Ok(res) = poll_builder.send().await {
                if res.status().is_success() {
                    if let Ok(body) = res.json::<Value>().await {
                        let up_status = body
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("processing");

                        if up_status == "completed"
                            || body.get("video").is_some()
                            || body.get("url").is_some()
                        {
                            let u = body
                                .get("video")
                                .and_then(|v| v.get("url"))
                                .and_then(|u| u.as_str())
                                .or_else(|| body.get("url").and_then(|u| u.as_str()))
                                .or_else(|| {
                                    body.get("data")
                                        .and_then(|d| d.get(0))
                                        .and_then(|i| i.get("url"))
                                        .and_then(|u| u.as_str())
                                })
                                .map(|s| s.to_string());

                            let b = body
                                .get("video")
                                .and_then(|v| v.get("b64_json"))
                                .and_then(|u| u.as_str())
                                .or_else(|| body.get("b64_json").and_then(|u| u.as_str()))
                                .or_else(|| {
                                    body.get("data")
                                        .and_then(|d| d.get(0))
                                        .and_then(|i| i.get("b64_json"))
                                        .and_then(|u| u.as_str())
                                })
                                .map(|s| s.to_string());

                            if let Some(updated) = complete_task(&id, u, b) {
                                return (
                                    StatusCode::OK,
                                    [
                                        (header::CONTENT_TYPE, "application/json"),
                                        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                                    ],
                                    Json(updated),
                                )
                                    .into_response();
                            }
                        } else if up_status == "failed" {
                            let err_msg = body
                                .get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("Upstream task failed");
                            if let Some(updated) = fail_task(&id, err_msg) {
                                return (
                                    StatusCode::OK,
                                    [
                                        (header::CONTENT_TYPE, "application/json"),
                                        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                                    ],
                                    Json(updated),
                                )
                                    .into_response();
                            }
                        } else {
                            let progress = body.get("progress").and_then(|p| p.as_f64()).map(|p| p as f32);
                            if let Some(updated) = update_task_status(&id, up_status, progress) {
                                task = updated;
                            }
                        }
                    }
                }
            }
        }

        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(task),
        )
            .into_response();
    }

    // Task not in local store: try checking upstream via connection-id or query provider
    let connections = state.db.load_provider_connections().unwrap_or_default();
    let pref_conn_id = headers.get("x-connection-id").and_then(|v| v.to_str().ok());

    let target_conn = if let Some(cid) = pref_conn_id {
        connections.iter().find(|c| c.is_active && c.id == cid)
    } else if let Some(ref prov) = query.provider {
        connections.iter().find(|c| c.is_active && c.provider == *prov)
    } else {
        connections.iter().find(|c| {
            c.is_active
                && (c.provider == "xai"
                    || c.provider == "luma"
                    || c.provider == "runway"
                    || c.provider == "openai"
                    || c.provider == "kling"
                    || c.provider == "openrouter")
        })
    };

    if let Some(conn) = target_conn {
        let poll_url = build_video_poll_url(conn.data.base_url.as_deref(), &id);
        let mut req = state.proxy.client().get(&poll_url);
        if let Some(ref key) = conn.data.api_key {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", key));
        }
        if let Ok(res) = req.send().await {
            let status = res.status();
            let body_bytes = res.bytes().await.unwrap_or_default();
            if status.is_success() {
                if let Ok(resp_json) = serde_json::from_slice::<Value>(&body_bytes) {
                    let task_status = resp_json
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("completed");
                    let video_url = resp_json
                        .get("video")
                        .and_then(|v| v.get("url"))
                        .and_then(|u| u.as_str())
                        .or_else(|| resp_json.get("url").and_then(|u| u.as_str()))
                        .map(|s| s.to_string());
                    let b64_json = resp_json
                        .get("video")
                        .and_then(|v| v.get("b64_json"))
                        .and_then(|u| u.as_str())
                        .or_else(|| resp_json.get("b64_json").and_then(|u| u.as_str()))
                        .map(|s| s.to_string());

                    let now = chrono::Utc::now().timestamp();
                    let fetched_task = VideoTask {
                        id: id.clone(),
                        request_id: Some(id.clone()),
                        object: Some("video.generation".to_string()),
                        created: now,
                        updated_at: Some(now),
                        model: resp_json.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()),
                        status: task_status.to_string(),
                        action: Some("generations".to_string()),
                        prompt: None,
                        progress: if task_status == "completed" { Some(1.0) } else { None },
                        url: video_url.clone(),
                        b64_json: b64_json.clone(),
                        video: video_url.as_ref().map(|u| VideoOutput {
                            url: Some(u.clone()),
                            b64_json: b64_json.clone(),
                            mime_type: Some("video/mp4".to_string()),
                            width: None,
                            height: None,
                            duration: None,
                        }),
                        videos: video_url.as_ref().map(|u| vec![VideoOutput {
                            url: Some(u.clone()),
                            b64_json: b64_json.clone(),
                            mime_type: Some("video/mp4".to_string()),
                            width: None,
                            height: None,
                            duration: None,
                        }]),
                        data: video_url.as_ref().map(|u| vec![VideoData {
                            id: Some(id.clone()),
                            url: Some(u.clone()),
                            b64_json: b64_json.clone(),
                            status: Some(task_status.to_string()),
                        }]),
                        error: resp_json.get("error").cloned(),
                        upstream_id: Some(id.clone()),
                        endpoint_url: Some(poll_url),
                        api_key: conn.data.api_key.clone(),
                        provider: Some(conn.provider.clone()),
                        connection_id: Some(conn.id.clone()),
                    };
                    insert_task(fetched_task.clone());
                    return (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, "application/json"),
                            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                        ],
                        Json(fetched_task),
                    )
                        .into_response();
                }
            }
        }
    }

    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "error": {
                "message": format!("Video task '{}' not found", id),
                "type": "invalid_request_error",
                "param": "id",
                "code": "video_task_not_found"
            }
        })
        .to_string(),
    )
        .into_response()
}

pub async fn video_list_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = crate::routes::authenticate(&headers, &state.db) {
        return e.into_response();
    }
    let store = get_video_task_store().read().unwrap();
    let tasks: Vec<VideoTask> = store.values().cloned().collect();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({
            "object": "list",
            "data": tasks
        })),
    )
        .into_response()
}

// ============================================================================
// Unit & Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
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
            config,
            db: db.clone(),
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: ConsoleLogBuffer::new(),
        };
        (state, db)
    }

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
        assert_eq!(
            build_video_url_for_action(Some("https://api.x.ai/v1"), "edits"),
            "https://api.x.ai/v1/videos/edits"
        );
        assert_eq!(
            build_video_url_for_action(Some("https://api.x.ai/v1"), "extensions"),
            "https://api.x.ai/v1/videos/extensions"
        );
        assert_eq!(
            build_video_poll_url(Some("https://api.x.ai/v1"), "task-123"),
            "https://api.x.ai/v1/videos/task-123"
        );
        assert_eq!(
            build_video_poll_url(Some("https://api.x.ai/v1/videos/generations"), "task-456"),
            "https://api.x.ai/v1/videos/task-456"
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

    #[test]
    fn test_resolve_video_target_multi_provider() {
        let connections = vec![
            ProviderConnection {
                id: "conn-xai".to_string(),
                provider: "xai".to_string(),
                auth_type: "api_key".to_string(),
                name: Some("xAI".to_string()),
                email: None,
                priority: Some(1),
                is_active: true,
                data: crate::db::ConnectionData {
                    api_key: Some("xai-key-123".to_string()),
                    base_url: Some("https://api.x.ai/v1".to_string()),
                    ..Default::default()
                },
                raw_data: "{}".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            ProviderConnection {
                id: "conn-luma".to_string(),
                provider: "luma".to_string(),
                auth_type: "api_key".to_string(),
                name: Some("Luma".to_string()),
                email: None,
                priority: Some(2),
                is_active: true,
                data: crate::db::ConnectionData {
                    api_key: Some("luma-key-456".to_string()),
                    base_url: Some("https://api.lumalabs.ai/v1".to_string()),
                    ..Default::default()
                },
                raw_data: "{}".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
        ];

        // 1. Explicit provider prefix
        let target1 = resolve_video_target(Some("xai/grok-imagine"), &connections, &[]);
        assert_eq!(target1.provider_type, VideoProviderType::Xai);
        assert_eq!(target1.model, "grok-imagine");
        assert_eq!(target1.api_key.as_deref(), Some("xai-key-123"));

        // 2. Inferred provider from model name
        let target2 = resolve_video_target(Some("dream-machine-v1"), &connections, &[]);
        assert_eq!(target2.provider_type, VideoProviderType::LumaAi);
        assert_eq!(target2.api_key.as_deref(), Some("luma-key-456"));

        // 3. Edits action endpoint
        let target_edit = resolve_video_target_for_action(Some("xai/grok-imagine"), "edits", &connections, &[]);
        assert_eq!(target_edit.endpoint_url, "https://api.x.ai/v1/videos/edits");
    }

    #[test]
    fn test_task_store_lifecycle() {
        clear_all_tasks();

        let task = create_video_task(
            "generations",
            Some("sora-1.0"),
            Some("a cat running in field"),
            Some("https://api.openai.com/v1/videos/generations".to_string()),
            Some("test-key".to_string()),
            Some("openai".to_string()),
            Some("conn-1".to_string()),
        );

        let tid = task.id.clone();
        assert_eq!(task.status, "pending");
        assert_eq!(task.progress, Some(0.0));

        // Get task
        let fetched = get_task(&tid).expect("task should exist in store");
        assert_eq!(fetched.id, tid);
        assert_eq!(fetched.status, "pending");

        // Update status to processing
        let updated = update_task_status(&tid, "processing", Some(0.5)).expect("update succeeded");
        assert_eq!(updated.status, "processing");
        assert_eq!(updated.progress, Some(0.5));

        // Complete task with URL
        let completed = complete_task(&tid, Some("https://storage.example.com/video.mp4".to_string()), None)
            .expect("complete succeeded");
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.progress, Some(1.0));
        assert_eq!(completed.url.as_deref(), Some("https://storage.example.com/video.mp4"));
        assert!(completed.video.is_some());
        assert_eq!(completed.video.as_ref().unwrap().url.as_deref(), Some("https://storage.example.com/video.mp4"));
        assert_eq!(completed.data.as_ref().unwrap()[0].url.as_deref(), Some("https://storage.example.com/video.mp4"));

        // Fail another task
        let task2 = create_video_task("edits", Some("sora-1.0"), None, None, None, None, None);
        let failed = fail_task(&task2.id, "GPU rendering out of memory").expect("fail succeeded");
        assert_eq!(failed.status, "failed");
        assert!(failed.error.is_some());
        assert!(failed.error.unwrap()["message"].as_str().unwrap().contains("out of memory"));
    }

    #[tokio::test]
    async fn test_video_polling_states_and_not_found() {
        clear_all_tasks();
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-vid-key".to_string(),
            key: "sk-vid-test".to_string(),
            is_active: true,
            allowed_models: "*".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        // 1. Unauthorized poll
        let req_unauth = Request::builder()
            .method("GET")
            .uri("/v1/videos/non-existent-task")
            .body(Body::empty())
            .unwrap();
        let resp_unauth = app.clone().oneshot(req_unauth).await.unwrap();
        assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

        // 2. Not found poll
        let req_404 = Request::builder()
            .method("GET")
            .uri("/v1/videos/non-existent-task")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp_404 = app.clone().oneshot(req_404).await.unwrap();
        assert_eq!(resp_404.status(), StatusCode::NOT_FOUND);

        // 3. Pending task poll
        let pending_task = create_video_task("generations", Some("sora-1.0"), Some("city skyline"), None, None, None, None);
        let req_pending = Request::builder()
            .method("GET")
            .uri(format!("/v1/videos/{}", pending_task.id))
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp_pending = app.clone().oneshot(req_pending).await.unwrap();
        assert_eq!(resp_pending.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp_pending.into_body(), 1024 * 1024).await.unwrap();
        let json_pending: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_pending["status"], "pending");
        assert_eq!(json_pending["id"], pending_task.id);

        // 4. Processing task poll via /api/v1/videos/{id}
        update_task_status(&pending_task.id, "processing", Some(0.42));
        let req_proc = Request::builder()
            .method("GET")
            .uri(format!("/api/v1/videos/{}", pending_task.id))
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp_proc = app.clone().oneshot(req_proc).await.unwrap();
        assert_eq!(resp_proc.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp_proc.into_body(), 1024 * 1024).await.unwrap();
        let json_proc: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_proc["status"], "processing");
        assert_eq!(json_proc["progress"], 0.42);

        // 5. Completed task poll returns video URL and b64_json
        complete_task(&pending_task.id, Some("https://cdn.test/video.mp4".to_string()), Some("b64fake".to_string()));
        let req_comp = Request::builder()
            .method("GET")
            .uri(format!("/v1/videos/{}", pending_task.id))
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp_comp = app.clone().oneshot(req_comp).await.unwrap();
        assert_eq!(resp_comp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp_comp.into_body(), 1024 * 1024).await.unwrap();
        let json_comp: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_comp["status"], "completed");
        assert_eq!(json_comp["url"], "https://cdn.test/video.mp4");
        assert_eq!(json_comp["b64_json"], "b64fake");
        assert_eq!(json_comp["video"]["url"], "https://cdn.test/video.mp4");
        assert_eq!(json_comp["data"][0]["url"], "https://cdn.test/video.mp4");

        // 6. List tasks via /v1/videos
        let req_list = Request::builder()
            .method("GET")
            .uri("/v1/videos")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp_list = app.oneshot(req_list).await.unwrap();
        assert_eq!(resp_list.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp_list.into_body(), 1024 * 1024).await.unwrap();
        let json_list: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_list["object"], "list");
        assert!(json_list["data"].as_array().unwrap().len() >= 1);
    }

    #[tokio::test]
    async fn test_video_edits_and_extensions_validation() {
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-vid-key".to_string(),
            key: "sk-vid-test".to_string(),
            is_active: true,
            allowed_models: "*".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        // 1. Empty Video Edits Request -> 400 Bad Request
        let req_edit_empty = Request::builder()
            .method("POST")
            .uri("/v1/videos/edits")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp_edit_empty = app.clone().oneshot(req_edit_empty).await.unwrap();
        assert_eq!(resp_edit_empty.status(), StatusCode::BAD_REQUEST);

        // 2. Video Edits with no provider -> 400 Provider Not Found
        let req_edit_valid = Request::builder()
            .method("POST")
            .uri("/api/v1/videos/edits")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{"prompt": "add neon glow", "video_url": "https://cdn.test/in.mp4"}"#))
            .unwrap();
        let resp_edit_valid = app.clone().oneshot(req_edit_valid).await.unwrap();
        assert_eq!(resp_edit_valid.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp_edit_valid.into_body(), 1024 * 1024).await.unwrap();
        let json_err: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_err["error"]["type"], "provider_not_found");

        // 3. Empty Video Extension Request -> 400 Bad Request
        let req_ext_empty = Request::builder()
            .method("POST")
            .uri("/v1/videos/extensions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{}"#))
            .unwrap();
        let resp_ext_empty = app.clone().oneshot(req_ext_empty).await.unwrap();
        assert_eq!(resp_ext_empty.status(), StatusCode::BAD_REQUEST);

        // 4. Video Extension with no provider -> 400 Provider Not Found
        let req_ext_valid = Request::builder()
            .method("POST")
            .uri("/api/v1/videos/extensions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{"video_id": "video-123", "duration": 5}"#))
            .unwrap();
        let resp_ext_valid = app.clone().oneshot(req_ext_valid).await.unwrap();
        assert_eq!(resp_ext_valid.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp_ext_valid.into_body(), 1024 * 1024).await.unwrap();
        let json_err: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_err["error"]["type"], "provider_not_found");

        // 5. Empty Video Generation Request -> 400 Bad Request
        let req_gen_empty = Request::builder()
            .method("POST")
            .uri("/v1/videos/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{"prompt": "   "}"#))
            .unwrap();
        let resp_gen_empty = app.oneshot(req_gen_empty).await.unwrap();
        assert_eq!(resp_gen_empty.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_video_disallowed_model() {
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-vid-key".to_string(),
            key: "sk-vid-test".to_string(),
            is_active: true,
            allowed_models: "gpt-4o,claude-3-5-sonnet".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/videos/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::from(r#"{"model": "sora-1.0", "prompt": "a drone shot of mountains"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json_err: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json_err["error"]["message"].as_str().unwrap().contains("not allowed for this API key"));
    }

    #[tokio::test]
    async fn test_video_routes_options_preflight() {
        let (state, _) = create_test_app_state();
        let app = create_router(state);

        let endpoints = [
            "/v1/videos/generations",
            "/videos/generations",
            "/api/v1/videos/generations",
            "/v1/videos/edits",
            "/videos/edits",
            "/api/v1/videos/edits",
            "/v1/videos/extensions",
            "/videos/extensions",
            "/api/v1/videos/extensions",
            "/v1/videos/test-id",
            "/videos/test-id",
            "/api/v1/videos/test-id",
            "/v1/videos",
            "/videos",
            "/api/v1/videos",
        ];

        for ep in endpoints {
            let req = Request::builder()
                .method("OPTIONS")
                .uri(ep)
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "OPTIONS failed for endpoint: {}", ep);
        }
    }

    #[tokio::test]
    async fn test_video_failed_task_polling() {
        clear_all_tasks();
        let (state, db) = create_test_app_state();
        let key = ApiKey {
            id: "test-vid-key".to_string(),
            key: "sk-vid-test".to_string(),
            is_active: true,
            allowed_models: "*".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).unwrap();
        let app = create_router(state);

        let task = create_video_task("generations", Some("sora-1.0"), Some("failed prompt"), None, None, None, None);
        fail_task(&task.id, "Content policy violation: prompt violates guidelines");

        let req = Request::builder()
            .method("GET")
            .uri(format!("/v1/videos/{}", task.id))
            .header(header::AUTHORIZATION, "Bearer sk-vid-test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json_resp: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_resp["status"], "failed");
        assert!(json_resp["error"]["message"].as_str().unwrap().contains("Content policy violation"));
    }
}
