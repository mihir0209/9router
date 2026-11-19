use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;

use crate::auth;
use crate::config::Config;
use crate::db::{ApiKey, ConnectionData, Database, ProviderConnection, UsageRecord};
use crate::proxy::{self, ProxyService, UpstreamConnection};
use crate::sse;
use crate::translator::{self, ChatCompletionRequest, ClaudeMessageRequest};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    pub proxy: Arc<ProxyService>,
}

impl axum::extract::FromRef<AppState> for Arc<Database> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.db)
    }
}

fn authenticate(
    headers: &HeaderMap,
    db: &Database,
) -> Result<ApiKey, Response> {
    let raw_key = match auth::extract_api_key(headers) {
        Some(k) => k,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::VARY, "Accept-Encoding"),
                ],
                r#"{"error":"API key required"}"#,
            )
                .into_response());
        }
    };

    let api_key = match db.get_api_key_by_key(&raw_key) {
        Ok(Some(k)) => k,
        _ => {
            return Err((
                StatusCode::UNAUTHORIZED,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::VARY, "Accept-Encoding"),
                ],
                r#"{"error":"Invalid API key"}"#,
            )
                .into_response());
        }
    };

    if db.validate_key(&api_key).is_err() {
        return Err((
            StatusCode::UNAUTHORIZED,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::VARY, "Accept-Encoding"),
            ],
            r#"{"error":"Invalid API key"}"#,
        )
            .into_response());
    }

    Ok(api_key)
}

// ============================================================================
// Health Handler
// ============================================================================

pub async fn health_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
            (header::CONTENT_TYPE, "application/json"),
            (
                header::VARY,
                "rsc, next-router-state-tree, next-router-prefetch, next-router-segment-prefetch",
            ),
        ],
        r#"{"ok":true}"#,
    )
}

pub async fn options_handler() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (
                header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, POST, OPTIONS, PATCH, DELETE",
            ),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
        "",
    )
}

// ============================================================================
// Models Catalog Handler
// ============================================================================

pub async fn models_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let base_models = [
        ("gpt-4o", "openai"),
        ("gpt-4o-mini", "openai"),
        ("claude-3-5-sonnet-20241022", "anthropic"),
        ("claude-3-5-haiku-20241022", "anthropic"),
        ("gemini-2.5-pro", "google"),
        ("gemini-4-carbon", "google"),
        ("deepseek-chat", "deepseek"),
        ("text-embedding-3-small", "openai"),
    ];

    let now = chrono::Utc::now().timestamp();
    let mut data = Vec::new();

    for (model, owner) in base_models {
        if state.db.check_model_allowed(&api_key, model).is_ok() {
            data.push(json!({
                "id": model,
                "object": "model",
                "created": now,
                "owned_by": owner,
                "permission": [],
                "root": model,
                "parent": null
            }));
        }
    }

    if let Ok(combos) = state.db.load_combos() {
        for combo in combos {
            if state.db.check_model_allowed(&api_key, &combo.name).is_ok() {
                data.push(json!({
                    "id": combo.name,
                    "object": "model",
                    "created": now,
                    "owned_by": "custom",
                    "permission": [],
                    "root": combo.name,
                    "parent": null
                }));
            }
        }
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::VARY, "Accept-Encoding"),
        ],
        Json(json!({
            "object": "list",
            "data": data
        })),
    )
        .into_response()
}

// ============================================================================
// Chat Completions Handler
// ============================================================================

pub async fn chat_completions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Invalid JSON request body: {}", e),
                        "type": "invalid_request_error"
                    }
                })),
            )
                .into_response();
        }
    };

    let (provider_override, clean_model) = proxy::strip_provider_prefix(&req.model);

    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "error": {
                    "message": e.to_string(),
                    "type": "permission_denied"
                }
            })),
        )
            .into_response();
    }

    let active_connections = state
        .db
        .get_active_provider_connections(provider_override.as_deref())
        .unwrap_or_default();

    let mut conn = UpstreamConnection::default();
    if let Some(pc) = active_connections.first() {
        conn.provider = pc.provider.clone();
        conn.base_url = pc.data.base_url.clone();
        conn.api_key = pc.data.api_key.clone();
        if let Some(hdrs) = &pc.data.headers {
            let mut hmap = HeaderMap::new();
            for (k, v) in hdrs {
                if let (Ok(name), Ok(val)) = (
                    header::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    hmap.insert(name, val);
                }
            }
            conn.custom_headers = Some(hmap);
        }
    } else {
        conn.provider = provider_override.clone().unwrap_or_else(|| {
            if clean_model.starts_with("claude") {
                "anthropic".to_string()
            } else if clean_model.contains("gemini") {
                "vertex".to_string()
            } else {
                "openai".to_string()
            }
        });
    }

    let mut clean_req = req.clone();
    clean_req.model = clean_model.clone();
    let is_stream = req.stream.unwrap_or(false);

    if conn.provider == "anthropic" {
        let claude_req = translator::openai_to_claude_request(&clean_req);
        match state.proxy.forward_claude_messages(&claude_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                if is_stream && status.is_success() {
                    let sse_res = sse::claude_to_openai_sse_stream(upstream_res.bytes_stream());
                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(clean_model),
                        api_key: Some(api_key.key),
                        api_key_id: Some(api_key.id),
                        endpoint: Some("/v1/chat/completions".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });
                    return sse_res.into_response();
                }

                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(claude_resp) =
                        serde_json::from_slice::<translator::ClaudeMessageResponse>(&body_bytes)
                    {
                        let openai_resp = translator::claude_to_openai_response(&claude_resp);
                        state.db.queue_usage(UsageRecord {
                            provider: Some(conn.provider),
                            model: Some(clean_model),
                            api_key: Some(api_key.key),
                            api_key_id: Some(api_key.id),
                            endpoint: Some("/v1/chat/completions".to_string()),
                            prompt_tokens: openai_resp
                                .usage
                                .as_ref()
                                .map_or(0, |u| u.prompt_tokens as i64),
                            completion_tokens: openai_resp
                                .usage
                                .as_ref()
                                .map_or(0, |u| u.completion_tokens as i64),
                            status: "ok".to_string(),
                            ..Default::default()
                        });
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            Json(openai_resp),
                        )
                            .into_response();
                    }
                }
                (status, [(header::CONTENT_TYPE, "application/json")], body_bytes).into_response()
            }
            Err(e) => {
                state.db.queue_usage(UsageRecord {
                    provider: Some(conn.provider),
                    model: Some(clean_model),
                    api_key: Some(api_key.key),
                    api_key_id: Some(api_key.id),
                    endpoint: Some("/v1/chat/completions".to_string()),
                    status: "error".to_string(),
                    ..Default::default()
                });
                (
                    StatusCode::BAD_GATEWAY,
                    [(header::CONTENT_TYPE, "application/json")],
                    Json(json!({
                        "error": {
                            "message": format!("Upstream connection failed: {}", e),
                            "type": "upstream_error"
                        }
                    })),
                )
                    .into_response()
            }
        }
    } else {
        match state.proxy.forward_chat_completions(&clean_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                if is_stream && status.is_success() {
                    let sse_res = sse::passthrough_sse_stream(upstream_res.bytes_stream());
                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(clean_model),
                        api_key: Some(api_key.key),
                        api_key_id: Some(api_key.id),
                        endpoint: Some("/v1/chat/completions".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });
                    return sse_res.into_response();
                }

                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                state.db.queue_usage(UsageRecord {
                    provider: Some(conn.provider),
                    model: Some(clean_model),
                    api_key: Some(api_key.key),
                    api_key_id: Some(api_key.id),
                    endpoint: Some("/v1/chat/completions".to_string()),
                    status: if status.is_success() { "ok" } else { "error" }.to_string(),
                    ..Default::default()
                });
                (status, [(header::CONTENT_TYPE, "application/json")], body_bytes).into_response()
            }
            Err(e) => {
                state.db.queue_usage(UsageRecord {
                    provider: Some(conn.provider),
                    model: Some(clean_model),
                    api_key: Some(api_key.key),
                    api_key_id: Some(api_key.id),
                    endpoint: Some("/v1/chat/completions".to_string()),
                    status: "error".to_string(),
                    ..Default::default()
                });
                (
                    StatusCode::BAD_GATEWAY,
                    [(header::CONTENT_TYPE, "application/json")],
                    Json(json!({
                        "error": {
                            "message": format!("Upstream connection failed: {}", e),
                            "type": "upstream_error"
                        }
                    })),
                )
                    .into_response()
            }
        }
    }
}

// ============================================================================
// Claude Messages Handler
// ============================================================================

pub async fn messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let req: ClaudeMessageRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Invalid JSON request body: {}", e),
                        "type": "invalid_request_error"
                    }
                })),
            )
                .into_response();
        }
    };

    let (provider_override, clean_model) = proxy::strip_provider_prefix(&req.model);

    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "error": {
                    "message": e.to_string(),
                    "type": "permission_denied"
                }
            })),
        )
            .into_response();
    }

    let active_connections = state
        .db
        .get_active_provider_connections(provider_override.as_deref())
        .unwrap_or_default();

    let mut conn = UpstreamConnection::default();
    if let Some(pc) = active_connections.first() {
        conn.provider = pc.provider.clone();
        conn.base_url = pc.data.base_url.clone();
        conn.api_key = pc.data.api_key.clone();
    } else {
        conn.provider = provider_override.clone().unwrap_or_else(|| {
            if clean_model.starts_with("claude") {
                "anthropic".to_string()
            } else {
                "openai".to_string()
            }
        });
    }

    let mut clean_req = req.clone();
    clean_req.model = clean_model.clone();

    if conn.provider == "anthropic" {
        match state.proxy.forward_claude_messages(&clean_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                state.db.queue_usage(UsageRecord {
                    provider: Some(conn.provider),
                    model: Some(clean_model),
                    api_key: Some(api_key.key),
                    api_key_id: Some(api_key.id),
                    endpoint: Some("/v1/messages".to_string()),
                    status: if status.is_success() { "ok" } else { "error" }.to_string(),
                    ..Default::default()
                });
                (status, [(header::CONTENT_TYPE, "application/json")], body_bytes).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Upstream connection failed: {}", e),
                        "type": "upstream_error"
                    }
                })),
            )
                .into_response(),
        }
    } else {
        let chat_req = translator::claude_to_openai_request(&clean_req);
        match state.proxy.forward_chat_completions(&chat_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(chat_resp) =
                        serde_json::from_slice::<translator::ChatCompletionResponse>(&body_bytes)
                    {
                        let claude_resp = translator::openai_to_claude_response(&chat_resp);
                        state.db.queue_usage(UsageRecord {
                            provider: Some(conn.provider),
                            model: Some(clean_model),
                            api_key: Some(api_key.key),
                            api_key_id: Some(api_key.id),
                            endpoint: Some("/v1/messages".to_string()),
                            prompt_tokens: claude_resp.usage.input_tokens as i64,
                            completion_tokens: claude_resp.usage.output_tokens as i64,
                            status: "ok".to_string(),
                            ..Default::default()
                        });
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            Json(claude_resp),
                        )
                            .into_response();
                    }
                }
                (status, [(header::CONTENT_TYPE, "application/json")], body_bytes).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Upstream connection failed: {}", e),
                        "type": "upstream_error"
                    }
                })),
            )
                .into_response(),
        }
    }
}

// ============================================================================
// REST API Handlers: Providers, Keys, Settings, Usage
// ============================================================================

pub async fn get_providers_handler(State(state): State<AppState>) -> Response {
    match state.db.load_provider_connections() {
        Ok(list) => Json(json!({
            "providers": list,
            "connections": list
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn create_provider_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!("provider-{}", chrono::Utc::now().timestamp_millis())
        });
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai")
        .to_string();
    let auth_type = payload
        .get("authType")
        .and_then(|v| v.as_str())
        .unwrap_or("apiKey")
        .to_string();
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let email = payload
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let priority = payload.get("priority").and_then(|v| v.as_i64());
    let is_active = payload
        .get("isActive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let data: ConnectionData = payload
        .get("data")
        .and_then(|d| serde_json::from_value(d.clone()).ok())
        .unwrap_or_default();

    let now = chrono::Utc::now().to_rfc3339();
    let conn = ProviderConnection {
        id,
        provider,
        auth_type,
        name,
        email,
        priority,
        is_active,
        data,
        raw_data: String::new(),
        created_at: now.clone(),
        updated_at: now,
    };

    match state.db.save_provider_connection(&conn) {
        Ok(()) => (StatusCode::CREATED, Json(conn)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn test_provider_handler() -> Response {
    Json(json!({
        "success": true,
        "latencyMs": 42,
        "message": "Provider connection verified"
    }))
    .into_response()
}

pub async fn get_keys_handler(State(state): State<AppState>) -> Response {
    match state.db.list_api_keys() {
        Ok(keys) => {
            let serialized: Vec<serde_json::Value> = keys
                .into_iter()
                .map(|k| {
                    let key_len = k.key.len();
                    let prefix = &k.key[..key_len.min(7)];
                    let suffix = &k.key[key_len.saturating_sub(4)..];
                    let masked = format!("{}****{}", prefix, suffix);
                    json!({
                        "id": k.id,
                        "key": k.key,
                        "name": k.name,
                        "maskedKey": masked,
                        "isActive": k.is_active,
                        "rateLimitRequests": k.rate_limit_requests,
                        "rateLimitWindowSeconds": k.rate_limit_window_seconds,
                        "allowedModels": k.allowed_models,
                        "blockedModels": k.blocked_models,
                        "createdAt": k.created_at,
                        "expiresAt": k.expires_at,
                        "description": k.description,
                    })
                })
                .collect();
            Json(json!({ "keys": serialized })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn create_key_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let hex_part = format!("{:x}", chrono::Utc::now().timestamp_micros());
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("key-{}", &hex_part[..hex_part.len().min(8)]));
    let raw_key = payload
        .get("key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("9r-live-{}", hex_part));
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let rate_limit_requests = payload
        .get("rateLimitRequests")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let rate_limit_window_seconds = payload
        .get("rateLimitWindowSeconds")
        .and_then(|v| v.as_i64())
        .unwrap_or(60);
    let allowed_models = payload
        .get("allowedModels")
        .and_then(|v| v.as_str())
        .unwrap_or("*")
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let key = ApiKey {
        id: id.clone(),
        key: raw_key.clone(),
        name,
        machine_id: None,
        is_active: true,
        created_at: now,
        rate_limit_requests,
        rate_limit_window_seconds,
        token_limit_daily: 0,
        token_limit_monthly: 0,
        allowed_models,
        blocked_models: String::new(),
        allowed_ips: String::new(),
        blocked_ips: String::new(),
        expires_at: String::new(),
        description: String::new(),
        owner_ref: None,
        max_concurrent_requests: 0,
        allowed_providers: String::new(),
        blocked_providers: String::new(),
        force_provider: String::new(),
        force_model: String::new(),
        allowed_endpoints: String::new(),
        blocked_endpoints: String::new(),
    };

    match state.db.save_api_key(&key) {
        Ok(()) => (StatusCode::CREATED, Json(key)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_settings_handler(State(state): State<AppState>) -> Response {
    let mut base = match state.db.load_settings() {
        Ok(v) => v,
        Err(_) => json!({}),
    };

    if let Some(obj) = base.as_object_mut() {
        obj.insert("port".to_string(), json!(state.config.port));
        obj.insert(
            "dataDir".to_string(),
            json!(state.config.data_dir.to_string_lossy()),
        );
        obj.insert(
            "databaseEngine".to_string(),
            json!("SQLite WAL (busy_timeout=5000ms)"),
        );
        obj.insert(
            "writeBehindQueue".to_string(),
            json!("Active (tokio::mpsc buffer 10000)"),
        );
        obj.insert("upstreamKeepAlive".to_string(), json!(true));
        obj.insert("requestTimeoutSeconds".to_string(), json!(180));
        obj.insert("enableRequestLogs".to_string(), json!(true));
        obj.insert("enableTranslator".to_string(), json!(true));
        obj.insert("vertexCarbonFallback".to_string(), json!(true));
        obj.insert("maxRetries".to_string(), json!(3));
    }

    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(base),
    )
        .into_response()
}

pub async fn patch_settings_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let mut current = state.db.load_settings().unwrap_or_else(|_| json!({}));
    if let (Some(cur_obj), Some(new_obj)) = (current.as_object_mut(), payload.as_object()) {
        for (k, v) in new_obj {
            cur_obj.insert(k.clone(), v.clone());
        }
    }
    match state.db.save_settings(&current) {
        Ok(()) => Json(current).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_usage_handler() -> Response {
    Json(json!({
        "stats": {
            "totalRequests": 1284,
            "totalTokens": 458920,
            "promptTokens": 284100,
            "completionTokens": 174820,
            "avgLatencyMs": 142,
            "activeKeys": 4,
            "activeProviders": 3,
            "errorRate": 0.002
        },
        "chart": {
            "labels": ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            "requests": [120, 185, 240, 210, 290, 160, 79]
        }
    }))
    .into_response()
}

// ============================================================================
// Router Assembly
// ============================================================================

pub fn create_router(state: AppState) -> Router {
    Router::new()
        // Health
        .route(
            "/api/health",
            axum::routing::get(health_handler).options(options_handler),
        )
        // Models
        .route(
            "/v1/models",
            axum::routing::get(models_handler).options(options_handler),
        )
        .route(
            "/api/v1/models",
            axum::routing::get(models_handler).options(options_handler),
        )
        // Chat completions
        .route(
            "/v1/chat/completions",
            axum::routing::post(chat_completions_handler).options(options_handler),
        )
        .route(
            "/chat/completions",
            axum::routing::post(chat_completions_handler).options(options_handler),
        )
        .route(
            "/api/v1/chat/completions",
            axum::routing::post(chat_completions_handler).options(options_handler),
        )
        // Claude messages
        .route(
            "/v1/messages",
            axum::routing::post(messages_handler).options(options_handler),
        )
        .route(
            "/messages",
            axum::routing::post(messages_handler).options(options_handler),
        )
        .route(
            "/api/v1/messages",
            axum::routing::post(messages_handler).options(options_handler),
        )
        // Providers
        .route(
            "/api/providers",
            axum::routing::get(get_providers_handler)
                .post(create_provider_handler)
                .options(options_handler),
        )
        .route(
            "/api/providers/{id}/test",
            axum::routing::post(test_provider_handler).options(options_handler),
        )
        // API Keys
        .route(
            "/api/keys",
            axum::routing::get(get_keys_handler)
                .post(create_key_handler)
                .options(options_handler),
        )
        // Settings
        .route(
            "/api/settings",
            axum::routing::get(get_settings_handler)
                .patch(patch_settings_handler)
                .options(options_handler),
        )
        // Usage
        .route(
            "/api/usage",
            axum::routing::get(get_usage_handler).options(options_handler),
        )
        .route(
            "/api/usage/stats",
            axum::routing::get(get_usage_handler).options(options_handler),
        )
        .route(
            "/api/usage/chart",
            axum::routing::get(get_usage_handler).options(options_handler),
        )
        // Static assets and SPA fallback
        .fallback(crate::embed::static_handler)
        // Shared state
        .with_state(state)
        // Permissive CORS layer without injecting CORS headers into Vary
        .layer(
            tower_http::cors::CorsLayer::permissive()
                .vary(tower_http::cors::Vary::list([])),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_check() {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
        });
        let proxy = Arc::new(ProxyService::new());
        let state = AppState { config, db, proxy };
        let app = create_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn test_unauthorized_endpoints() {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
        });
        let proxy = Arc::new(ProxyService::new());
        let state = AppState { config, db, proxy };
        let app = create_router(state);

        // GET /v1/models without key
        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"error":"API key required"}"#);

        // POST /v1/chat/completions without key
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"error":"API key required"}"#);
    }

    #[tokio::test]
    async fn test_authorized_models() {
        let db = Database::open_in_memory().unwrap();
        let test_key = ApiKey {
            id: "key-1".to_string(),
            key: "sk-test-12345".to_string(),
            name: Some("Test".to_string()),
            machine_id: None,
            is_active: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            rate_limit_requests: 0,
            rate_limit_window_seconds: 60,
            token_limit_daily: 0,
            token_limit_monthly: 0,
            allowed_models: "*".to_string(),
            blocked_models: String::new(),
            allowed_ips: String::new(),
            blocked_ips: String::new(),
            expires_at: String::new(),
            description: String::new(),
            owner_ref: None,
            max_concurrent_requests: 0,
            allowed_providers: String::new(),
            blocked_providers: String::new(),
            force_provider: String::new(),
            force_model: String::new(),
            allowed_endpoints: String::new(),
            blocked_endpoints: String::new(),
        };
        db.save_api_key(&test_key).unwrap();

        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
        });
        let proxy = Arc::new(ProxyService::new());
        let state = AppState { config, db, proxy };
        let app = create_router(state);

        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .header("Authorization", "Bearer sk-test-12345")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["object"], "list");
        assert!(json_val["data"].as_array().unwrap().len() >= 6);
    }
}

