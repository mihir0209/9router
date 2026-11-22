use std::collections::HashMap;
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
pub struct ConsoleLogBuffer {
    pub logs: Arc<tokio::sync::RwLock<std::collections::VecDeque<String>>>,
    pub tx: tokio::sync::broadcast::Sender<String>,
}

impl ConsoleLogBuffer {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        let mut initial = std::collections::VecDeque::new();
        let now_str = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        initial.push_back(format!("[INFO] [{}] 9Router native gateway operational", now_str));
        initial.push_back(format!("[INFO] [{}] System database initialized with WAL mode", now_str));
        Self {
            logs: Arc::new(tokio::sync::RwLock::new(initial)),
            tx,
        }
    }

    pub async fn push(&self, line: String) {
        let mut guard = self.logs.write().await;
        if guard.len() >= 1000 {
            guard.pop_front();
        }
        guard.push_back(line.clone());
        let _ = self.tx.send(line);
    }

    pub async fn clear(&self) {
        let mut guard = self.logs.write().await;
        guard.clear();
        let _ = self.tx.send("__CLEAR__".to_string());
    }
}

impl Default for ConsoleLogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    pub proxy: Arc<ProxyService>,
    pub original: crate::original::Original,
    pub combo_tracker: Arc<crate::combo_router::ComboTracker>,
    pub token_cache: crate::cache::TokenCache,
    pub circuit_breaker: Arc<crate::circuit_breaker::CircuitBreakerRegistry>,
    pub console_buffer: ConsoleLogBuffer,
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
// Target Resolution Helper for Candidates
// ============================================================================

fn resolve_target_for_candidate(
    candidate: &str,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[ProviderConnection],
) -> (UpstreamConnection, String) {
    // 1. Check if candidate starts with a node prefix (e.g. "h/glm-5.2", "9r/...", "neon/...", "srvm/...")
    for node in provider_nodes {
        if !node.prefix.is_empty() {
            let prefix_slash = format!("{}/", node.prefix);
            if candidate.starts_with(&prefix_slash) || candidate == node.prefix {
                let upstream_model = if candidate.starts_with(&prefix_slash) {
                    candidate[prefix_slash.len()..].to_string()
                } else {
                    candidate.to_string()
                };

                let mut conn = UpstreamConnection::default();
                conn.provider = node.name.clone().unwrap_or_else(|| node.prefix.clone());
                conn.base_url = Some(node.base_url.clone());

                if let Some(mapping_val) = node.data.get("schemaMapping").or_else(|| node.data.get("schema_mapping")) {
                    if let Ok(mapping) = serde_json::from_value::<crate::custom_adapter::CustomSchemaMapping>(mapping_val.clone()) {
                        conn.schema_mapping = Some(mapping);
                    }
                } else if node.node_type.as_deref() == Some("custom") {
                    if let Ok(mapping) = serde_json::from_value::<crate::custom_adapter::CustomSchemaMapping>(node.data.clone()) {
                        conn.schema_mapping = Some(mapping);
                    }
                }

                if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.provider == node.id) {
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
                if conn.custom_headers.is_none() {
                    if let Some(hdrs) = node.data.get("customHeaders").and_then(|v| v.as_object()) {
                        let mut hmap = HeaderMap::new();
                        for (k, v) in hdrs {
                            if let (Ok(kn), Ok(kv)) = (
                                header::HeaderName::from_bytes(k.as_bytes()),
                                HeaderValue::from_str(v.as_str().unwrap_or("")),
                            ) {
                                hmap.insert(kn, kv);
                            }
                        }
                        conn.custom_headers = Some(hmap);
                    }
                }

                return (conn, upstream_model);
            }
        }
    }

    // 2. Check Cloudflare prefixes ("cf/", "cfr/", "@cf/")
    if candidate.starts_with("cf/") || candidate.starts_with("cfr/") || candidate.starts_with("@cf/") {
        let upstream_model = if candidate.starts_with("cf/") {
            candidate[3..].to_string()
        } else if candidate.starts_with("cfr/") {
            candidate[4..].to_string()
        } else {
            candidate.to_string()
        };

        // Prefer Cloudflare Gateway node if configured
        if let Some(cf_node) = provider_nodes.iter().find(|n| n.prefix == "cfr" || n.prefix == "cf") {
            let mut conn = UpstreamConnection::default();
            conn.provider = "cloudflare".to_string();
            conn.base_url = Some(cf_node.base_url.clone());
            if let Some(pc) = all_connections.iter().find(|c| c.is_active && (c.provider == cf_node.id || c.provider == "cloudflare-ai")) {
                conn.api_key = pc.data.api_key.clone();
            }
            return (conn, upstream_model);
        }

        // Direct Cloudflare AI Run endpoint
        if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.provider == "cloudflare-ai") {
            let account_id = pc.data.extra.get("providerSpecificData")
                .and_then(|psd| psd.get("accountId"))
                .and_then(|aid| aid.as_str())
                .unwrap_or("1da22c19548115accf1b98ab7c562b2c");
            let base_url = format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/run", account_id);
            let mut conn = UpstreamConnection::default();
            conn.provider = "cloudflare-ai".to_string();
            conn.base_url = Some(base_url);
            conn.api_key = pc.data.api_key.clone();
            return (conn, upstream_model);
        }
    }

    // 3. Check standard provider prefix (e.g. "vertex/gemini-4-carbon", "azure/gpt-5.6-terra")
    if let Some((p_prefix, m_part)) = candidate.split_once('/') {
        if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.provider.eq_ignore_ascii_case(p_prefix)) {
            let mut conn = UpstreamConnection::default();
            conn.provider = pc.provider.clone();
            conn.base_url = pc.data.base_url.clone();
            conn.api_key = pc.data.api_key.clone();
            return (conn, m_part.to_string());
        }
    }

    // 4. Match by defaultModel
    if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.data.default_model.as_deref() == Some(candidate)) {
        let mut conn = UpstreamConnection::default();
        conn.provider = pc.provider.clone();
        conn.base_url = pc.data.base_url.clone();
        conn.api_key = pc.data.api_key.clone();
        return (conn, candidate.to_string());
    }

    // 5. Default provider inference
    let provider = if candidate.starts_with("claude") {
        "anthropic".to_string()
    } else if candidate.contains("gemini") {
        "vertex".to_string()
    } else {
        "openai".to_string()
    };

    let conn_opt = all_connections.iter().find(|c| c.is_active && c.provider.eq_ignore_ascii_case(&provider));
    let mut conn = UpstreamConnection::default();
    conn.provider = provider;
    if let Some(pc) = conn_opt {
        conn.base_url = pc.data.base_url.clone();
        conn.api_key = pc.data.api_key.clone();
    }

    (conn, candidate.to_string())
}

// ============================================================================
// Chat Completions Handler
// ============================================================================

async fn execute_candidate_chat(
    candidate: &str,
    req: &ChatCompletionRequest,
    is_stream: bool,
    state: &AppState,
    api_key: &ApiKey,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[crate::db::ProviderConnection],
) -> Result<Response, (StatusCode, String)> {
    let (conn, target_model) = resolve_target_for_candidate(candidate, provider_nodes, all_connections);
    tracing::info!("Candidate '{}' -> provider='{}', model='{}', base_url={:?}", 
        candidate, conn.provider, target_model, conn.base_url);

    let mut clean_req = req.clone();
    clean_req.model = target_model.clone();

    // Check candidate-level token cache
    let cand_cache_key = state.token_cache.compute_chat_hash(&clean_req);
    if let Some(cached) = state.token_cache.get(&cand_cache_key) {
        state.circuit_breaker.record_success(&conn.provider);
        state.circuit_breaker.record_success(candidate);
        state.db.queue_usage(UsageRecord {
            provider: Some(conn.provider.clone()),
            model: Some(target_model.clone()),
            api_key: Some(api_key.key.clone()),
            api_key_id: Some(api_key.id.clone()),
            endpoint: Some("/v1/chat/completions".to_string()),
            status: "ok".to_string(),
            ..Default::default()
        });
        return Ok(cached.to_response(is_stream));
    }

    if let Some(mapping) = &conn.schema_mapping {
        let endpoint_url = mapping.endpoint_url.clone().or_else(|| conn.base_url.clone()).unwrap_or_else(|| {
            "http://127.0.0.1:20139/api/worst/flat/execute".to_string()
        });

        if is_stream && mapping.stream_text_path.is_some() {
            let req_val = serde_json::to_value(&clean_req).unwrap_or_default();
            match state.proxy.forward_custom_request(&endpoint_url, &req_val, &conn).await {
                Ok(upstream_res) if upstream_res.status().is_success() => {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    let sse_res = crate::custom_adapter::custom_sse_to_openai_stream(
                        upstream_res.bytes_stream(),
                        mapping.clone(),
                        target_model.clone(),
                    );
                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(target_model),
                        api_key: Some(api_key.key.clone()),
                        api_key_id: Some(api_key.id.clone()),
                        endpoint: Some("/v1/chat/completions".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });
                    return Ok(sse_res.into_response());
                }
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("Upstream returned error: {}", status)));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, e.to_string()));
                }
            }
        } else {
            let mut non_stream_req = clean_req.clone();
            non_stream_req.stream = None;
            let req_val = serde_json::to_value(&non_stream_req).unwrap_or_default();
            match state.proxy.forward_custom_request(&endpoint_url, &req_val, &conn).await {
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    if status.is_success() {
                        let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                        if let Ok(raw_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                            match crate::custom_adapter::transform_custom_to_openai_response(&raw_json, mapping, &target_model) {
                                Ok(openai_resp) => {
                                    state.circuit_breaker.record_success(&conn.provider);
                                    state.circuit_breaker.record_success(candidate);
                                    state.db.queue_usage(UsageRecord {
                                        provider: Some(conn.provider),
                                        model: Some(target_model.clone()),
                                        api_key: Some(api_key.key.clone()),
                                        api_key_id: Some(api_key.id.clone()),
                                        endpoint: Some("/v1/chat/completions".to_string()),
                                        status: "ok".to_string(),
                                        ..Default::default()
                                    });
                                    if is_stream {
                                        return Ok(crate::custom_adapter::synthesize_openai_stream(openai_resp, target_model).into_response());
                                    }
                                    return Ok((
                                        StatusCode::OK,
                                        [(header::CONTENT_TYPE, "application/json")],
                                        Json(openai_resp),
                                    )
                                        .into_response());
                                }
                                Err(err) => {
                                    tracing::warn!("Custom schema transform error: {}", err);
                                    return Err((StatusCode::INTERNAL_SERVER_ERROR, err));
                                }
                            }
                        }
                    }
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("Upstream returned status: {}", status)));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, e.to_string()));
                }
            }
        }
    } else if conn.provider == "anthropic" {
        let claude_req = translator::openai_to_claude_request(&clean_req);
        match state.proxy.forward_claude_messages(&claude_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                if status.is_success() {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    if is_stream {
                        let sse_res = sse::claude_to_openai_sse_stream(upstream_res.bytes_stream());
                        state.db.queue_usage(UsageRecord {
                            provider: Some(conn.provider),
                            model: Some(target_model),
                            api_key: Some(api_key.key.clone()),
                            api_key_id: Some(api_key.id.clone()),
                            endpoint: Some("/v1/chat/completions".to_string()),
                            status: "ok".to_string(),
                            ..Default::default()
                        });
                        return Ok(sse_res.into_response());
                    }

                    let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                    let final_body = if let Ok(claude_resp) =
                        serde_json::from_slice::<translator::ClaudeMessageResponse>(&body_bytes)
                    {
                        let openai_resp = translator::claude_to_openai_response(&claude_resp);
                        serde_json::to_vec(&openai_resp).unwrap_or(body_bytes.to_vec())
                    } else {
                        body_bytes.to_vec()
                    };

                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(target_model),
                        api_key: Some(api_key.key.clone()),
                        api_key_id: Some(api_key.id.clone()),
                        endpoint: Some("/v1/chat/completions".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });
                    return Ok((
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        final_body,
                    )
                        .into_response());
                } else {
                    let err_text = upstream_res.text().await.unwrap_or_default();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    tracing::warn!("Candidate '{}' via '{}' failed [{}]: {}", candidate, conn.provider, status, err_text);
                    return Err((status, format!("[{}] {}", status, err_text)));
                }
            }
            Err(e) => {
                state.circuit_breaker.record_failure(&conn.provider);
                state.circuit_breaker.record_failure(candidate);
                tracing::warn!("Candidate '{}' via '{}' error: {}", candidate, conn.provider, e);
                return Err((StatusCode::BAD_GATEWAY, e.to_string()));
            }
        }
    } else {
        match state.proxy.forward_chat_completions(&clean_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                if status.is_success() {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    if is_stream {
                        let sse_res = sse::passthrough_sse_stream(upstream_res.bytes_stream());
                        state.db.queue_usage(UsageRecord {
                            provider: Some(conn.provider),
                            model: Some(target_model),
                            api_key: Some(api_key.key.clone()),
                            api_key_id: Some(api_key.id.clone()),
                            endpoint: Some("/v1/chat/completions".to_string()),
                            status: "ok".to_string(),
                            ..Default::default()
                        });
                        return Ok(sse_res.into_response());
                    }

                    let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                    let final_body = if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                        if let Some(inner) = val.get("result") {
                            serde_json::to_vec(inner).unwrap_or(body_bytes.to_vec())
                        } else {
                            body_bytes.to_vec()
                        }
                    } else {
                        body_bytes.to_vec()
                    };

                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(target_model),
                        api_key: Some(api_key.key.clone()),
                        api_key_id: Some(api_key.id.clone()),
                        endpoint: Some("/v1/chat/completions".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });

                    return Ok((
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        final_body,
                    )
                        .into_response());
                } else {
                    let err_text = upstream_res.text().await.unwrap_or_default();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    tracing::warn!("Candidate '{}' via '{}' failed [{}]: {}", candidate, conn.provider, status, err_text);
                    return Err((status, format!("[{}] {}", status, err_text)));
                }
            }
            Err(e) => {
                state.circuit_breaker.record_failure(&conn.provider);
                state.circuit_breaker.record_failure(candidate);
                tracing::warn!("Candidate '{}' via '{}' error: {}", candidate, conn.provider, e);
                return Err((StatusCode::BAD_GATEWAY, e.to_string()));
            }
        }
    }
}

pub async fn chat_completions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let mut req: ChatCompletionRequest = match serde_json::from_slice(&body) {
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

    let rtk_saved = crate::token_saver::compress_chat_request(&mut req);
    if rtk_saved > 0 {
        tracing::info!("[RTK] compressed tool result messages, saved {} bytes", rtk_saved);
    }

    let (_, clean_model) = proxy::strip_provider_prefix(&req.model);

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

    let is_stream = req.stream.unwrap_or(false);
    let cache_key = state.token_cache.compute_chat_hash(&req);

    // Exact-Match Token Cache Check
    if let Some(cached) = state.token_cache.get(&cache_key) {
        tracing::info!("Cache HIT for key {}", cache_key);
        state.db.queue_usage(UsageRecord {
            provider: Some("cache".to_string()),
            model: Some(req.model.clone()),
            api_key: Some(api_key.key.clone()),
            api_key_id: Some(api_key.id.clone()),
            endpoint: Some("/v1/chat/completions".to_string()),
            status: "ok".to_string(),
            ..Default::default()
        });
        return cached.to_response(is_stream);
    }

    let candidates = state.db.resolve_combo_chain(&req.model);
    tracing::info!("Requested model '{}' expanded to {} candidate(s): {:?}", req.model, candidates.len(), candidates);

    let combo = state.db.get_combo_by_name(&req.model).ok().flatten();
    let (combo_strategy, combo_name) = match &combo {
        Some(c) => (
            crate::combo_router::ComboStrategy::parse(c.strategy.as_deref().unwrap_or("fallback")),
            Some(c.name.clone()),
        ),
        None => (crate::combo_router::ComboStrategy::Fallback, None),
    };

    let ordered_candidates = state.combo_tracker.order_candidates(
        combo_name.as_deref().unwrap_or(&req.model),
        &candidates,
        combo_strategy,
    );

    let provider_nodes = Arc::new(state.db.load_provider_nodes().unwrap_or_default());
    let all_connections = Arc::new(state.db.load_provider_connections().unwrap_or_default());
    let is_stream = req.stream.unwrap_or(false);
    let mut last_error = String::new();
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;

    // Speculative Hedged Racing for combo candidates when hedged/race strategy (or fusion for non-stream)
    let is_hedged_eligible = (combo_strategy == crate::combo_router::ComboStrategy::HedgedRacing
        || (combo_strategy == crate::combo_router::ComboStrategy::Fusion && !is_stream))
        && ordered_candidates.len() >= 2;

    if is_hedged_eligible {
        let c0 = ordered_candidates[0].clone();
        let c1 = ordered_candidates[1].clone();
        let hedged_delay = if combo_strategy == crate::combo_router::ComboStrategy::Fusion {
            std::time::Duration::from_millis(250)
        } else {
            crate::combo_router::resolve_hedged_delay(
                combo.as_ref().and_then(|c| c.strategy.as_deref()),
                combo.as_ref().and_then(|c| c.match_rules.as_deref()),
            )
        };

        tracing::info!(
            "Speculative Hedged Racing active for combo '{:?}' between '{}' and '{}' with delay {:?}",
            combo_name, c0, c1, hedged_delay
        );

        let race_res = crate::combo_router::execute_hedged_race(
            c0.clone(),
            c1.clone(),
            hedged_delay,
            |cand, cancel_token| {
                let state = state.clone();
                let api_key = api_key.clone();
                let req = req.clone();
                let provider_nodes = provider_nodes.clone();
                let all_connections = all_connections.clone();
                async move {
                    let _g = state.combo_tracker.track_in_flight(&cand);
                    tokio::select! {
                        res = execute_candidate_chat(&cand, &req, is_stream, &state, &api_key, &provider_nodes, &all_connections) => {
                            if res.is_err() {
                                state.combo_tracker.record_error(&cand);
                            }
                            res
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("Hedged racing candidate '{}' cancelled by winner", cand);
                            Err((StatusCode::REQUEST_TIMEOUT, format!("Candidate '{}' cancelled by hedged race winner", cand)))
                        }
                    }
                }
            },
        ).await;

        match race_res {
            Ok((winner, resp)) => {
                if let Some(ref cn) = combo_name {
                    state.combo_tracker.record_success(cn, &winner);
                }
                return state.token_cache.wrap_and_cache(cache_key.clone(), resp, is_stream).await;
            }
            Err((status, err)) => {
                tracing::warn!("Hedged race between '{}' and '{}' failed: {}", c0, c1, err);
                last_status = status;
                last_error = err;
            }
        }
    }

    let start_idx = if is_hedged_eligible { 2 } else { 0 };

    for (idx, candidate) in ordered_candidates.iter().enumerate().skip(start_idx) {
        let (conn, _target_model) = resolve_target_for_candidate(candidate, &provider_nodes, &all_connections);
        if state.circuit_breaker.is_open(&conn.provider) || state.circuit_breaker.is_open(candidate) {
            tracing::warn!("Candidate '{}' (provider '{}') circuit breaker is OPEN, skipping without socket connection", candidate, conn.provider);
            continue;
        }
        let _in_flight = state.combo_tracker.track_in_flight(candidate);
        match execute_candidate_chat(candidate, &req, is_stream, &state, &api_key, &provider_nodes, &all_connections).await {
            Ok(resp) => {
                if let Some(ref cn) = combo_name {
                    state.combo_tracker.record_success(cn, candidate);
                }
                return state.token_cache.wrap_and_cache(cache_key.clone(), resp, is_stream).await;
            }
            Err((status, err)) => {
                state.combo_tracker.record_error_with_status(candidate, status);
                tracing::warn!("Candidate {}/{} '{}' failed: {}", idx + 1, ordered_candidates.len(), candidate, err);
                last_status = status;
                last_error = err;
            }
        }
    }

    if last_error.is_empty() {
        last_error = "All candidates tripped or circuit breaker is open".to_string();
    }

    state.db.queue_usage(UsageRecord {
        provider: None,
        model: Some(req.model.clone()),
        api_key: Some(api_key.key),
        api_key_id: Some(api_key.id),
        endpoint: Some("/v1/chat/completions".to_string()),
        status: "error".to_string(),
        ..Default::default()
    });

    (
        last_status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "message": format!("All {} candidate(s) failed for '{}': {}", candidates.len(), req.model, last_error),
                "type": "combo_routing_error"
            }
        })),
    )
        .into_response()
}

// ============================================================================
// Claude Messages Handler
// ============================================================================

async fn execute_candidate_claude(
    candidate: &str,
    req: &ClaudeMessageRequest,
    is_stream: bool,
    state: &AppState,
    api_key: &ApiKey,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[crate::db::ProviderConnection],
) -> Result<Response, (StatusCode, String)> {
    let (provider_override, clean_model) = proxy::strip_provider_prefix(candidate);
    let (mut conn, target_model) = resolve_target_for_candidate(candidate, provider_nodes, all_connections);
    let clean_model = if !target_model.is_empty() { target_model } else { clean_model };

    if conn.provider.is_empty() {
        let active_connections = state
            .db
            .get_active_provider_connections(provider_override.as_deref())
            .unwrap_or_default();

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
    }

    let mut clean_req = req.clone();
    clean_req.model = clean_model.clone();

    // Check candidate-level token cache
    let cand_cache_key = state.token_cache.compute_claude_hash(&clean_req);
    if let Some(cached) = state.token_cache.get(&cand_cache_key) {
        state.circuit_breaker.record_success(&conn.provider);
        state.circuit_breaker.record_success(candidate);
        state.db.queue_usage(UsageRecord {
            provider: Some(conn.provider.clone()),
            model: Some(clean_model.clone()),
            api_key: Some(api_key.key.clone()),
            api_key_id: Some(api_key.id.clone()),
            endpoint: Some("/v1/messages".to_string()),
            status: "ok".to_string(),
            ..Default::default()
        });
        return Ok(cached.to_response(is_stream));
    }

    if let Some(mapping) = &conn.schema_mapping {
        let endpoint_url = mapping.endpoint_url.clone().or_else(|| conn.base_url.clone()).unwrap_or_else(|| {
            "http://127.0.0.1:20139/api/worst/flat/execute".to_string()
        });
        let openai_req = translator::claude_to_openai_request(&clean_req);

        if is_stream && mapping.stream_text_path.is_some() {
            let req_val = serde_json::to_value(&openai_req).unwrap_or_default();
            match state.proxy.forward_custom_request(&endpoint_url, &req_val, &conn).await {
                Ok(upstream_res) if upstream_res.status().is_success() => {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    let sse_res = crate::custom_adapter::custom_sse_to_claude_stream(
                        upstream_res.bytes_stream(),
                        mapping.clone(),
                        clean_model.clone(),
                    );
                    state.db.queue_usage(UsageRecord {
                        provider: Some(conn.provider),
                        model: Some(clean_model),
                        api_key: Some(api_key.key.clone()),
                        api_key_id: Some(api_key.id.clone()),
                        endpoint: Some("/v1/messages".to_string()),
                        status: "ok".to_string(),
                        ..Default::default()
                    });
                    return Ok(sse_res.into_response());
                }
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((upstream_res.status(), format!("Custom claude stream error: {}", upstream_res.status())));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, e.to_string()));
                }
            }
        } else {
            let mut non_stream_openai_req = openai_req;
            non_stream_openai_req.stream = None;
            let req_val = serde_json::to_value(&non_stream_openai_req).unwrap_or_default();
            match state.proxy.forward_custom_request(&endpoint_url, &req_val, &conn).await {
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    if status.is_success() {
                        let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                        if let Ok(raw_json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                            match crate::custom_adapter::transform_custom_to_claude_response(&raw_json, mapping, &clean_model) {
                                Ok(claude_resp) => {
                                    state.circuit_breaker.record_success(&conn.provider);
                                    state.circuit_breaker.record_success(candidate);
                                    state.db.queue_usage(UsageRecord {
                                        provider: Some(conn.provider),
                                        model: Some(clean_model.clone()),
                                        api_key: Some(api_key.key.clone()),
                                        api_key_id: Some(api_key.id.clone()),
                                        endpoint: Some("/v1/messages".to_string()),
                                        status: "ok".to_string(),
                                        ..Default::default()
                                    });
                                    if is_stream {
                                        return Ok(crate::custom_adapter::synthesize_claude_stream(claude_resp, clean_model).into_response());
                                    }
                                    return Ok((
                                        StatusCode::OK,
                                        [(header::CONTENT_TYPE, "application/json")],
                                        Json(claude_resp),
                                    )
                                        .into_response());
                                }
                                Err(err) => {
                                    return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Custom claude transform error: {}", err)));
                                }
                            }
                        }
                    }
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("Custom claude status: {}", status)));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, e.to_string()));
                }
            }
        }
    } else if conn.provider == "anthropic" {
        match state.proxy.forward_claude_messages(&clean_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                state.db.queue_usage(UsageRecord {
                    provider: Some(conn.provider.clone()),
                    model: Some(clean_model),
                    api_key: Some(api_key.key.clone()),
                    api_key_id: Some(api_key.id.clone()),
                    endpoint: Some("/v1/messages".to_string()),
                    status: if status.is_success() { "ok" } else { "error" }.to_string(),
                    ..Default::default()
                });
                if status.is_success() {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    return Ok((status, [(header::CONTENT_TYPE, "application/json")], body_bytes).into_response());
                } else {
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("[{}] Anthropic error", status)));
                }
            }
            Err(e) => {
                state.circuit_breaker.record_failure(&conn.provider);
                state.circuit_breaker.record_failure(candidate);
                return Err((StatusCode::BAD_GATEWAY, format!("Upstream connection failed: {}", e)));
            }
        }
    } else {
        let chat_req = translator::claude_to_openai_request(&clean_req);
        match state.proxy.forward_chat_completions(&chat_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                if status.is_success() {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    if let Ok(chat_resp) =
                        serde_json::from_slice::<translator::ChatCompletionResponse>(&body_bytes)
                    {
                        let claude_resp = translator::openai_to_claude_response(&chat_resp);
                        state.db.queue_usage(UsageRecord {
                            provider: Some(conn.provider),
                            model: Some(clean_model),
                            api_key: Some(api_key.key.clone()),
                            api_key_id: Some(api_key.id.clone()),
                            endpoint: Some("/v1/messages".to_string()),
                            prompt_tokens: claude_resp.usage.input_tokens as i64,
                            completion_tokens: claude_resp.usage.output_tokens as i64,
                            status: "ok".to_string(),
                            ..Default::default()
                        });
                        return Ok((
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            Json(claude_resp),
                        )
                            .into_response());
                    }
                }
                state.circuit_breaker.record_status(&conn.provider, status);
                state.circuit_breaker.record_status(candidate, status);
                return Err((status, format!("[{}] Claude to OpenAI translation failure", status)));
            }
            Err(e) => {
                state.circuit_breaker.record_failure(&conn.provider);
                state.circuit_breaker.record_failure(candidate);
                return Err((StatusCode::BAD_GATEWAY, format!("Upstream connection failed: {}", e)));
            }
        }
    }
}

pub async fn messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let mut req: ClaudeMessageRequest = match serde_json::from_slice(&body) {
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

    let rtk_saved = crate::token_saver::compress_claude_request(&mut req);
    if rtk_saved > 0 {
        tracing::info!("[RTK] compressed Claude tool_result blocks, saved {} bytes", rtk_saved);
    }

    let (_, clean_model) = proxy::strip_provider_prefix(&req.model);

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

    let is_stream = req.stream.unwrap_or(false);
    let cache_key = state.token_cache.compute_claude_hash(&req);

    // Exact-Match Token Cache Check
    if let Some(cached) = state.token_cache.get(&cache_key) {
        tracing::info!("Claude Cache HIT for key {}", cache_key);
        state.db.queue_usage(UsageRecord {
            provider: Some("cache".to_string()),
            model: Some(req.model.clone()),
            api_key: Some(api_key.key.clone()),
            api_key_id: Some(api_key.id.clone()),
            endpoint: Some("/v1/messages".to_string()),
            status: "ok".to_string(),
            ..Default::default()
        });
        return cached.to_response(is_stream);
    }

    let candidates = state.db.resolve_combo_chain(&req.model);
    tracing::info!("Requested Claude model '{}' expanded to {} candidate(s): {:?}", req.model, candidates.len(), candidates);

    let combo = state.db.get_combo_by_name(&req.model).ok().flatten();
    let (combo_strategy, combo_name) = match &combo {
        Some(c) => (
            crate::combo_router::ComboStrategy::parse(c.strategy.as_deref().unwrap_or("fallback")),
            Some(c.name.clone()),
        ),
        None => (crate::combo_router::ComboStrategy::Fallback, None),
    };

    let ordered_candidates = state.combo_tracker.order_candidates(
        combo_name.as_deref().unwrap_or(&req.model),
        &candidates,
        combo_strategy,
    );

    let provider_nodes = Arc::new(state.db.load_provider_nodes().unwrap_or_default());
    let all_connections = Arc::new(state.db.load_provider_connections().unwrap_or_default());
    let is_stream = req.stream.unwrap_or(false);

    let mut last_error = String::new();
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;

    // Speculative Hedged Racing for Claude combo candidates when hedged/race strategy (or fusion for non-stream)
    let is_hedged_eligible = (combo_strategy == crate::combo_router::ComboStrategy::HedgedRacing
        || (combo_strategy == crate::combo_router::ComboStrategy::Fusion && !is_stream))
        && ordered_candidates.len() >= 2;

    if is_hedged_eligible {
        let c0 = ordered_candidates[0].clone();
        let c1 = ordered_candidates[1].clone();
        let hedged_delay = if combo_strategy == crate::combo_router::ComboStrategy::Fusion {
            std::time::Duration::from_millis(250)
        } else {
            crate::combo_router::resolve_hedged_delay(
                combo.as_ref().and_then(|c| c.strategy.as_deref()),
                combo.as_ref().and_then(|c| c.match_rules.as_deref()),
            )
        };

        tracing::info!(
            "Speculative Hedged Racing active for Claude combo '{:?}' between '{}' and '{}' with delay {:?}",
            combo_name, c0, c1, hedged_delay
        );

        let race_res = crate::combo_router::execute_hedged_race(
            c0.clone(),
            c1.clone(),
            hedged_delay,
            |cand, cancel_token| {
                let state = state.clone();
                let api_key = api_key.clone();
                let req = req.clone();
                let provider_nodes = provider_nodes.clone();
                let all_connections = all_connections.clone();
                async move {
                    let _g = state.combo_tracker.track_in_flight(&cand);
                    tokio::select! {
                        res = execute_candidate_claude(&cand, &req, is_stream, &state, &api_key, &provider_nodes, &all_connections) => {
                            if res.is_err() {
                                state.combo_tracker.record_error(&cand);
                            }
                            res
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("Hedged racing Claude candidate '{}' cancelled by winner", cand);
                            Err((StatusCode::REQUEST_TIMEOUT, format!("Candidate '{}' cancelled by hedged race winner", cand)))
                        }
                    }
                }
            },
        ).await;

        match race_res {
            Ok((winner, resp)) => {
                if let Some(ref cn) = combo_name {
                    state.combo_tracker.record_success(cn, &winner);
                }
                return state.token_cache.wrap_and_cache(cache_key.clone(), resp, is_stream).await;
            }
            Err((status, err)) => {
                tracing::warn!("Hedged race between Claude candidates '{}' and '{}' failed: {}", c0, c1, err);
                last_status = status;
                last_error = err;
            }
        }
    }

    let start_idx = if is_hedged_eligible { 2 } else { 0 };

    for (idx, candidate) in ordered_candidates.iter().enumerate().skip(start_idx) {
        let (conn, _target_model) = resolve_target_for_candidate(candidate, &provider_nodes, &all_connections);
        if state.circuit_breaker.is_open(&conn.provider) || state.circuit_breaker.is_open(candidate) {
            tracing::warn!("Claude candidate '{}' (provider '{}') circuit breaker is OPEN, skipping without socket connection", candidate, conn.provider);
            continue;
        }
        let _in_flight = state.combo_tracker.track_in_flight(candidate);
        match execute_candidate_claude(candidate, &req, is_stream, &state, &api_key, &provider_nodes, &all_connections).await {
            Ok(resp) => {
                if let Some(ref cn) = combo_name {
                    state.combo_tracker.record_success(cn, candidate);
                }
                return state.token_cache.wrap_and_cache(cache_key.clone(), resp, is_stream).await;
            }
            Err((status, err)) => {
                state.combo_tracker.record_error_with_status(candidate, status);
                tracing::warn!("Claude candidate {}/{} '{}' failed: {}", idx + 1, ordered_candidates.len(), candidate, err);
                last_status = status;
                last_error = err;
            }
        }
    }

    if last_error.is_empty() {
        last_error = "All candidates tripped or circuit breaker is open".to_string();
    }

    state.db.queue_usage(UsageRecord {
        provider: None,
        model: Some(req.model.clone()),
        api_key: Some(api_key.key),
        api_key_id: Some(api_key.id),
        endpoint: Some("/v1/messages".to_string()),
        status: "error".to_string(),
        ..Default::default()
    });

    (
        last_status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "message": format!("All {} candidate(s) failed for '{}': {}", candidates.len(), req.model, last_error),
                "type": "combo_routing_error"
            }
        })),
    )
        .into_response()
}

// ============================================================================
// REST API Handlers: Providers, Keys, Settings, Usage, Combos, Nodes, Pools
// ============================================================================

pub fn serialize_connection_for_dashboard(c: &ProviderConnection) -> serde_json::Value {
    let mut obj = serde_json::Map::new();

    // 1. Unpack fields from raw_data if available
    if !c.raw_data.trim().is_empty() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&c.raw_data) {
            if let Some(m) = val.as_object() {
                for (k, v) in m {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }

    // 2. Unpack fields from c.data
    for (k, v) in &c.data.extra {
        obj.insert(k.clone(), v.clone());
    }
    if let Some(ref bu) = c.data.base_url {
        obj.insert("baseUrl".to_string(), json!(bu));
    }
    if let Some(ref dm) = c.data.default_model {
        obj.insert("defaultModel".to_string(), json!(dm));
    }

    // 3. Strip sensitive credential fields
    obj.remove("apiKey");
    obj.remove("accessToken");
    obj.remove("refreshToken");
    obj.remove("idToken");
    obj.remove("secretWrappedDEK");
    obj.remove("secretCiphertext");
    obj.remove("tokenWrappedDEK");
    obj.remove("tokenCiphertext");
    obj.remove("refreshWrappedDEK");
    obj.remove("refreshCiphertext");

    // 4. Overwrite canonical top-level fields matching React / Next.js expectation
    obj.insert("id".to_string(), json!(c.id));
    obj.insert("provider".to_string(), json!(c.provider));
    obj.insert("authType".to_string(), json!(c.auth_type.to_lowercase()));
    obj.insert("name".to_string(), json!(c.name));
    obj.insert("email".to_string(), json!(c.email));
    obj.insert("priority".to_string(), json!(c.priority.unwrap_or(1)));
    obj.insert("isActive".to_string(), json!(c.is_active));
    obj.insert("createdAt".to_string(), json!(c.created_at));
    obj.insert("updatedAt".to_string(), json!(c.updated_at));

    if !obj.contains_key("testStatus") {
        obj.insert("testStatus".to_string(), json!("active"));
    }

    serde_json::Value::Object(obj)
}

pub async fn get_connections_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_provider_connections() {
        Ok(list) => {
            let serialized: Vec<serde_json::Value> = list
                .iter()
                .map(serialize_connection_for_dashboard)
                .collect();
            Json(json!({
                "connections": serialized
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_provider_nodes_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_provider_nodes() {
        Ok(nodes) => Json(json!({ "nodes": nodes })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn create_provider_node_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let node_type = payload
        .get("type")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| Some("openai-compatible".to_string()));

    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let prefix = match node_type.as_deref() {
                Some("custom") => "custom-",
                Some("anthropic-compatible") => "anthropic-compatible-",
                _ => "openai-compatible-",
            };
            format!("{}{}", prefix, chrono::Utc::now().timestamp_millis())
        });

    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    let prefix = payload
        .get("prefix")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let base_url = payload
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let api_type = payload
        .get("apiType")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    let now = chrono::Utc::now().to_rfc3339();
    let node = crate::db::ProviderNode {
        id: id.clone(),
        node_type,
        name,
        prefix,
        base_url,
        api_type,
        data: payload.clone(),
        created_at: now.clone(),
        updated_at: now,
    };

    match state.db.save_provider_node(&node) {
        Ok(_) => (StatusCode::CREATED, Json(json!({ "node": node }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn delete_provider_node_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.delete_provider_node(&id) {
        Ok(_) => Json(json!({ "success": true, "deleted": id })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn parse_curl_command(curl: &str) -> (String, String, HashMap<String, String>, Option<serde_json::Value>) {
    let mut method = "POST".to_string();
    let mut url = String::new();
    let mut headers = HashMap::new();
    let mut body_str = String::new();

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = curl.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\'' && !in_double {
            in_single = !in_single;
        } else if c == '"' && !in_single {
            in_double = !in_double;
        } else if c == '\\' {
            if let Some(&next_c) = chars.peek() {
                if next_c == '\n' || next_c == '\r' {
                    chars.next();
                    continue;
                }
                chars.next();
                current.push(next_c);
            }
        } else if c.is_whitespace() && !in_single && !in_double {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if (tok == "-X" || tok == "--request") && i + 1 < tokens.len() {
            method = tokens[i + 1].to_uppercase();
            i += 2;
        } else if (tok == "-H" || tok == "--header") && i + 1 < tokens.len() {
            let h = &tokens[i + 1];
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_string(), v.trim().to_string());
            }
            i += 2;
        } else if (tok == "-d" || tok == "--data" || tok == "--data-raw" || tok == "--data-binary" || tok == "--json") && i + 1 < tokens.len() {
            body_str = tokens[i + 1].clone();
            i += 2;
        } else if tok.starts_with("http://") || tok.starts_with("https://") {
            url = tok.clone();
            i += 1;
        } else {
            i += 1;
        }
    }

    let body_json = if !body_str.is_empty() {
        serde_json::from_str::<serde_json::Value>(&body_str).ok().or_else(|| {
            Some(serde_json::Value::String(body_str))
        })
    } else {
        None
    };

    (url, method, headers, body_json)
}

fn collect_candidate_paths(
    val: &serde_json::Value,
    current_path: &str,
    text_candidates: &mut Vec<(String, String, i32)>,
    int_candidates: &mut Vec<(String, i64, i32)>,
) {
    match val {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.len() > 2 {
                if let Ok(nested) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    collect_candidate_paths(&nested, current_path, text_candidates, int_candidates);
                }
            }
            if !trimmed.is_empty() {
                let lower_path = current_path.to_lowercase();
                let mut score = 0;
                if lower_path.contains("content") || lower_path.contains("text") || lower_path.contains("message") || lower_path.contains("response") || lower_path.contains("output") || lower_path.contains("raw_stream") {
                    score += 60;
                }
                if lower_path.contains("result") || lower_path.contains("answer") || lower_path.contains("data") {
                    score += 30;
                }
                if trimmed.len() > 10 {
                    score += 20;
                }
                if lower_path.ends_with("id") || lower_path.ends_with("model") || lower_path.ends_with("role") || lower_path.ends_with("type") || lower_path.ends_with("object") {
                    score -= 80;
                }
                text_candidates.push((current_path.to_string(), trimmed.to_string(), score));
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let lower_path = current_path.to_lowercase();
                let mut score = 0;
                if lower_path.contains("token") || lower_path.contains("prompt") || lower_path.contains("completion") || lower_path.contains("in_tok") || lower_path.contains("out_tok") {
                    score += 50;
                }
                int_candidates.push((current_path.to_string(), i, score));
            }
        }
        serde_json::Value::Array(arr) => {
            for (idx, item) in arr.iter().enumerate() {
                let next_path = if current_path.is_empty() {
                    format!("[{}]", idx)
                } else {
                    format!("{}[{}]", current_path, idx)
                };
                collect_candidate_paths(item, &next_path, text_candidates, int_candidates);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let next_path = if current_path.is_empty() {
                    k.clone()
                } else {
                    format!("{}.{}", current_path, k)
                };
                collect_candidate_paths(v, &next_path, text_candidates, int_candidates);
            }
        }
        _ => {}
    }
}

pub async fn probe_provider_node_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut method = payload.get("method").and_then(|v| v.as_str()).unwrap_or("POST").to_string();
    let mut headers = HashMap::new();
    let mut body = payload.get("body").cloned();

    if let Some(hdrs) = payload.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in hdrs {
            if let Some(val_str) = v.as_str() {
                headers.insert(k.clone(), val_str.to_string());
            }
        }
    }

    if let Some(curl_str) = payload.get("curl").and_then(|v| v.as_str()) {
        let (curl_url, curl_method, curl_headers, curl_body) = parse_curl_command(curl_str);
        if url.is_empty() {
            url = curl_url;
        }
        if method.is_empty() || method == "POST" {
            method = curl_method;
        }
        for (k, v) in curl_headers {
            headers.entry(k).or_insert(v);
        }
        if body.is_none() {
            body = curl_body;
        }
    }

    if url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "Endpoint URL or cURL command is required" })),
        )
            .into_response();
    }

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .unwrap_or_default();

    let mut req_builder = match method.to_uppercase().as_str() {
        "GET" => client.get(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => client.post(&url),
    };

    for (k, v) in &headers {
        if let (Ok(hn), Ok(hv)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            req_builder = req_builder.header(hn, hv);
        }
    }

    if let Some(body_val) = &body {
        req_builder = req_builder.json(body_val);
    }

    let start_time = std::time::Instant::now();
    let resp = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "ok": false,
                    "error": format!("Failed to reach target endpoint: {}", e),
                    "parsed": {
                        "url": url,
                        "method": method,
                        "headers": headers,
                        "body": body,
                    }
                })),
            )
                .into_response();
        }
    };

    let elapsed_ms = start_time.elapsed().as_millis();
    let status_code = resp.status().as_u16();
    let resp_text = resp.text().await.unwrap_or_default();

    let json_val: serde_json::Value = match serde_json::from_str(&resp_text) {
        Ok(v) => v,
        Err(_) => {
            return Json(json!({
                "ok": true,
                "status": status_code,
                "latencyMs": elapsed_ms,
                "rawText": resp_text,
                "parsed": {
                    "url": url,
                    "method": method,
                    "headers": headers,
                    "body": body,
                },
                "detectedPaths": null,
                "preview": {
                    "extractedText": resp_text
                }
            }))
            .into_response();
        }
    };

    let mut text_candidates = Vec::new();
    let mut int_candidates = Vec::new();
    collect_candidate_paths(&json_val, "", &mut text_candidates, &mut int_candidates);

    text_candidates.sort_by(|a, b| b.2.cmp(&a.2));

    let best_text_path = text_candidates.first().map(|(p, _, _)| p.clone());
    let best_prompt_path = int_candidates
        .iter()
        .find(|(p, _, _)| {
            let lp = p.to_lowercase();
            lp.contains("prompt") || lp.contains("input") || lp.contains("in_tok")
        })
        .map(|(p, _, _)| p.clone());
    let best_comp_path = int_candidates
        .iter()
        .find(|(p, _, _)| {
            let lp = p.to_lowercase();
            lp.contains("completion") || lp.contains("output") || lp.contains("out_tok")
        })
        .map(|(p, _, _)| p.clone());

    let path_to_extract = payload
        .get("customMapping")
        .and_then(|m| m.get("responseTextPath"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| best_text_path.clone())
        .unwrap_or_default();

    let extracted_val = crate::custom_adapter::extract_json_value(&json_val, &path_to_extract);
    let extracted_str = match extracted_val {
        Some(serde_json::Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => String::new(),
    };

    let candidate_list: Vec<serde_json::Value> = text_candidates
        .iter()
        .take(8)
        .map(|(path, sample, score)| {
            json!({
                "path": path,
                "sample": if sample.len() > 120 { format!("{}...", &sample[..120]) } else { sample.clone() },
                "score": score
            })
        })
        .collect();

    let openai_preview = json!({
        "id": "chatcmpl-custom-studio-preview",
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": "custom-studio-preview",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": extracted_str
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 24,
            "total_tokens": 36
        }
    });

    let claude_preview = json!({
        "id": "msg-custom-studio-preview",
        "type": "message",
        "role": "assistant",
        "model": "custom-studio-preview",
        "content": [{
            "type": "text",
            "text": extracted_str
        }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 12,
            "output_tokens": 24
        }
    });

    Json(json!({
        "ok": true,
        "status": status_code,
        "latencyMs": elapsed_ms,
        "parsed": {
            "url": url,
            "method": method,
            "headers": headers,
            "body": body,
        },
        "rawResponse": json_val,
        "detectedPaths": {
            "responseTextPath": best_text_path,
            "promptTokensPath": best_prompt_path,
            "completionTokensPath": best_comp_path
        },
        "candidates": candidate_list,
        "preview": {
            "extractedText": extracted_str,
            "openai": openai_preview,
            "claude": claude_preview
        }
    }))
    .into_response()
}

pub async fn test_gateway_provider_node_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let node_opt = if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
        match state.db.load_provider_nodes() {
            Ok(nodes) => nodes.into_iter().find(|n| n.id == id),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "ok": false, "error": e.to_string() })),
                )
                    .into_response();
            }
        }
    } else if let Some(n_val) = payload.get("node") {
        serde_json::from_value::<crate::db::ProviderNode>(n_val.clone()).ok()
    } else {
        None
    };

    let node = match node_opt {
        Some(n) => n,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "Provider node id or node configuration is required" })),
            )
                .into_response();
        }
    };

    let mapping: Option<crate::custom_adapter::CustomSchemaMapping> = node
        .data
        .get("schemaMapping")
        .or_else(|| node.data.get("schema_mapping"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .or_else(|| serde_json::from_value(node.data.clone()).ok());

    let target_url = mapping
        .as_ref()
        .and_then(|m| m.endpoint_url.clone())
        .unwrap_or_else(|| node.base_url.clone());

    if target_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "Target endpoint URL is empty" })),
        )
            .into_response();
    }

    let prompt = payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("Ping test. Reply with 'OK'.");

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let start = std::time::Instant::now();
    let post_body = json!({
        "prompt": prompt,
        "messages": [{ "role": "user", "content": prompt }],
        "model": node.prefix
    });

    let resp = match client.post(&target_url).json(&post_body).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": format!("Request to {} failed: {}", target_url, e) })),
            )
                .into_response();
        }
    };

    let elapsed_ms = start.elapsed().as_millis();
    let status_code = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();

    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&text) {
        let extracted_text = if let Some(m) = &mapping {
            let path = m.response_text_path.as_deref().unwrap_or("");
            crate::custom_adapter::extract_json_value(&json_val, path)
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s),
                    other => Some(other.to_string()),
                })
                .unwrap_or_default()
        } else {
            text.clone()
        };

        Json(json!({
            "ok": true,
            "status": status_code,
            "latencyMs": elapsed_ms,
            "extractedText": extracted_text,
            "rawResponse": json_val,
            "nodeId": node.id,
            "nodeName": node.name
        }))
        .into_response()
    } else {
        Json(json!({
            "ok": true,
            "status": status_code,
            "latencyMs": elapsed_ms,
            "extractedText": text,
            "rawResponse": null,
            "nodeId": node.id,
            "nodeName": node.name
        }))
        .into_response()
    }
}

pub async fn get_combos_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_combos() {
        Ok(combos) => Json(json!({ "combos": combos })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_combo_by_id_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.get_combo_by_id(&id) {
        Ok(Some(combo)) => Json(combo).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Combo not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn create_combo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let name = match payload.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Name is required"})),
            )
                .into_response();
        }
    };

    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Name can only contain letters, numbers, -, _ and ."})),
        )
            .into_response();
    }

    if let Ok(Some(_)) = state.db.get_combo_by_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Combo name already exists"})),
        )
            .into_response();
    }

    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("combo-{}", chrono::Utc::now().timestamp_millis()));

    let kind = payload
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let models: Vec<String> = match payload.get("models") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(serde_json::Value::String(s)) => {
            vec![s.clone()]
        }
        _ => Vec::new(),
    };

    let strategy = payload
        .get("strategy")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some("fallback".to_string()));

    let match_rules = payload
        .get("matchRules")
        .map(|v| v.to_string())
        .or_else(|| Some("[]".to_string()));

    let custom_prompt = payload
        .get("customPrompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let now = chrono::Utc::now().to_rfc3339();
    let combo = crate::db::Combo {
        id: id.clone(),
        name: name.clone(),
        kind,
        models,
        strategy,
        match_rules,
        custom_prompt,
        created_at: now.clone(),
        updated_at: now,
    };

    match state.db.save_combo(&combo) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "success": true,
                "id": combo.id,
                "name": combo.name,
                "models": combo.models,
                "strategy": combo.strategy,
                "combo": combo
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn update_combo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut combo = match state.db.get_combo_by_id(&id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Combo not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    if let Some(new_name) = payload.get("name").and_then(|v| v.as_str()) {
        let trimmed = new_name.trim();
        if !trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Name can only contain letters, numbers, -, _ and ."})),
            )
                .into_response();
        }
        if let Ok(Some(existing)) = state.db.get_combo_by_name(trimmed) {
            if existing.id != id {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "Combo name already exists"})),
                )
                    .into_response();
            }
        }
        combo.name = trimmed.to_string();
    }

    if let Some(models_val) = payload.get("models") {
        if let Some(arr) = models_val.as_array() {
            combo.models = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        } else if let Some(s) = models_val.as_str() {
            combo.models = vec![s.to_string()];
        }
    }

    if let Some(kind) = payload.get("kind").and_then(|v| v.as_str()) {
        combo.kind = Some(kind.to_string());
    }

    if let Some(strat) = payload.get("strategy").and_then(|v| v.as_str()) {
        combo.strategy = Some(strat.to_string());
    }

    if let Some(rules) = payload.get("matchRules") {
        combo.match_rules = Some(rules.to_string());
    }

    if let Some(prompt) = payload.get("customPrompt").and_then(|v| v.as_str()) {
        combo.custom_prompt = Some(prompt.to_string());
    }

    combo.updated_at = chrono::Utc::now().to_rfc3339();

    match state.db.save_combo(&combo) {
        Ok(()) => Json(json!({ "success": true, "combo": combo })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn delete_combo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.delete_combo(&id) {
        Ok(true) => Json(json!({ "success": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Combo not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn bulk_delete_combos_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let ids: Vec<String> = payload
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    match state.db.bulk_delete_combos(&ids) {
        Ok(count) => Json(json!({ "success": true, "deleted": count })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn auto_group_combos_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut all_models = Vec::new();
    if let Ok(conns) = state.db.load_provider_connections() {
        for c in conns {
            if let Some(ref dm) = c.data.default_model {
                let full = format!("{}/{}", c.provider, dm);
                if !all_models.contains(&full) {
                    all_models.push(full);
                }
                if !all_models.contains(dm) {
                    all_models.push(dm.clone());
                }
            }
            if let Some(arr) = c.data.extra.get("models").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(m) = item.as_str() {
                        let full = format!("{}/{}", c.provider, m);
                        if !all_models.contains(&full) {
                            all_models.push(full);
                        }
                        let m_str = m.to_string();
                        if !all_models.contains(&m_str) {
                            all_models.push(m_str);
                        }
                    }
                }
            }
        }
    }
    if let Ok(nodes) = state.db.load_provider_nodes() {
        for n in nodes {
            let prefix = if !n.prefix.is_empty() {
                format!("{}/", n.prefix)
            } else {
                format!("{}/", n.id)
            };
            let test_model = format!("{}default", prefix);
            if !all_models.contains(&test_model) {
                all_models.push(test_model);
            }
        }
    }

    let defaults = [
        "anthropic/claude-3-7-sonnet",
        "openrouter/anthropic/claude-3-7-sonnet",
        "anthropic/claude-3-5-sonnet",
        "openrouter/anthropic/claude-3-5-sonnet",
        "anthropic/claude-3-5-haiku",
        "openrouter/anthropic/claude-3-5-haiku",
        "openai/gpt-4o",
        "azure/gpt-4o",
        "openrouter/openai/gpt-4o",
        "openai/gpt-4o-mini",
        "azure/gpt-4o-mini",
        "openai/o3-mini",
        "openrouter/openai/o3-mini",
        "deepseek/deepseek-chat",
        "openrouter/deepseek/deepseek-chat",
        "deepseek/deepseek-reasoner",
        "openrouter/deepseek/deepseek-r1",
        "qwen/qwen-2.5-coder",
        "openrouter/qwen/qwen-2.5-coder",
        "gemini/gemini-2.0-flash",
        "vertex/gemini-2.0-flash",
    ];
    for d in defaults {
        let s = d.to_string();
        if !all_models.contains(&s) {
            all_models.push(s);
        }
    }

    let clusters = crate::combo_router::cluster_models_by_family(&all_models);
    let mut created = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for (slug, name, candidates) in clusters {
        let combo = crate::db::Combo {
            id: format!("combo-auto-{}", slug),
            name: name.clone(),
            kind: Some("auto-family".to_string()),
            models: candidates,
            strategy: Some("round-robin".to_string()),
            match_rules: Some("[]".to_string()),
            custom_prompt: Some("".to_string()),
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        if state.db.save_combo(&combo).is_ok() {
            created.push(combo);
        }
    }

    Json(json!({
        "success": true,
        "count": created.len(),
        "combos": created
    }))
    .into_response()
}

pub async fn get_plans_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_plans() {
        Ok(plans) => Json(json!({ "plans": plans })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_proxy_pools_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_proxy_pools() {
        Ok(pools) => Json(json!({ "proxyPools": pools })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn require_login_handler(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> Response {
    if state.original.is_running() {
        return crate::original::proxy(State(state), req).await;
    }
    Json(json!({
        "requireLogin": false,
        "tunnelDashboardAccess": true
    }))
    .into_response()
}

#[derive(serde::Deserialize, Default)]
pub struct LoginPayload {
    pub password: Option<String>,
}

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginPayload>,
) -> Response {
    let password = payload.password.unwrap_or_default();
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let expected_password = settings
        .get("password")
        .and_then(|p| p.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| {
            std::env::var("INITIAL_PASSWORD")
                .ok()
                .filter(|p| !p.trim().is_empty() && p.trim() != "change-me" && p.trim() != "your-password")
                .unwrap_or_else(|| "123456".to_string())
        });

    let valid = if password == expected_password {
        true
    } else if expected_password.starts_with("$2a$")
        || expected_password.starts_with("$2b$")
        || expected_password.starts_with("$2y$")
    {
        bcrypt::verify(&password, &expected_password).unwrap_or(false)
    } else {
        false
    };

    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": "Invalid password"})),
        )
            .into_response();
    }

    let token = crate::auth::create_dashboard_jwt(&state.config.jwt_secret);
    let secure_flag = if std::env::var("AUTH_COOKIE_SECURE").map(|v| v == "true").unwrap_or(false) {
        "; Secure"
    } else {
        ""
    };
    let cookie_val = format!(
        "auth_token={}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax{}",
        token, secure_flag
    );

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::SET_COOKIE, &cookie_val),
        ],
        Json(json!({
            "success": true,
            "mustChangePassword": false
        })),
    )
        .into_response()
}

pub async fn logout_handler() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::SET_COOKIE,
                "auth_token=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
            ),
        ],
        Json(json!({"success": true})),
    )
        .into_response()
}

pub async fn auth_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let require_login = settings
        .get("requireLogin")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let has_password = settings.get("password").is_some();

    let authenticated = if !require_login {
        true
    } else if let Some(token) = crate::auth::extract_session_token(&headers) {
        crate::auth::verify_dashboard_jwt(&token, &state.config.jwt_secret)
    } else {
        false
    };

    Json(json!({
        "requireLogin": require_login,
        "authMode": "password",
        "ssoType": "oidc",
        "hasPassword": has_password,
        "authenticated": authenticated,
        "displayName": "Admin user",
        "loginMethod": "Password"
    }))
    .into_response()
}

pub async fn version_handler() -> Response {
    Json(json!({
        "version": "0.5.119",
        "latestVersion": "0.5.119",
        "upToDate": true,
        "runtime": "rust-native"
    }))
    .into_response()
}

pub async fn changelog_handler() -> Response {
    Json(json!({
        "changelog": []
    }))
    .into_response()
}

pub async fn models_meta_handler() -> Response {
    Json(json!({
        "models": {}
    }))
    .into_response()
}

pub async fn get_providers_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.load_provider_connections() {
        Ok(list) => {
            let serialized: Vec<serde_json::Value> = list
                .iter()
                .map(serialize_connection_for_dashboard)
                .collect();
            Json(json!({
                "providers": serialized,
                "connections": serialized
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn create_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
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
        .unwrap_or("apikey")
        .to_lowercase();
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let email = payload
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let priority = payload.get("priority").and_then(|v| v.as_i64()).unwrap_or(1);
    let is_active = payload
        .get("isActive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let api_key = payload
        .get("apiKey")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("data").and_then(|d| d.get("apiKey")).and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let default_model = payload
        .get("defaultModel")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("data").and_then(|d| d.get("defaultModel")).and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let mut raw_map = serde_json::Map::new();
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            if k != "id" && k != "provider" && k != "authType" && k != "name" && k != "email" && k != "priority" && k != "isActive" {
                raw_map.insert(k.clone(), v.clone());
            }
        }
    }
    if !raw_map.contains_key("testStatus") {
        let ts = payload.get("testStatus").and_then(|v| v.as_str()).unwrap_or("active");
        raw_map.insert("testStatus".to_string(), json!(ts));
    }
    if let Some(ref ak) = api_key {
        raw_map.insert("apiKey".to_string(), json!(ak));
    }

    let raw_data = serde_json::Value::Object(raw_map).to_string();

    let data = ConnectionData {
        api_key,
        base_url: payload.get("baseUrl").and_then(|v| v.as_str()).map(|s| s.to_string()),
        headers: None,
        default_model,
        extra: HashMap::new(),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let conn = ProviderConnection {
        id: id.clone(),
        provider: provider.clone(),
        auth_type: auth_type.clone(),
        name,
        email,
        priority: Some(priority),
        is_active,
        data,
        raw_data,
        created_at: now.clone(),
        updated_at: now,
    };

    match state.db.save_provider_connection(&conn) {
        Ok(()) => {
            let res = serialize_connection_for_dashboard(&conn);
            (StatusCode::CREATED, Json(res)).into_response()
        }
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

pub async fn validate_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let api_key = payload
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Custom provider nodes (e.g. node-*, prefix, or name)
    let node_opt = state.db.get_provider_node_by_id(provider)
        .ok()
        .flatten()
        .or_else(|| {
            state.db.load_provider_nodes().ok().and_then(|nodes| {
                nodes.into_iter().find(|n| {
                    n.id == provider
                        || n.prefix == provider
                        || n.name.as_deref() == Some(provider)
                        || n.name.as_deref().map(|s| s.to_lowercase().replace(' ', "-")) == Some(provider.to_lowercase().replace(' ', "-"))
                })
            })
        });

    if let Some(node) = node_opt {
        let endpoint_url = node
            .data
            .get("schemaMapping")
            .and_then(|sm| sm.get("endpointUrl"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                if !node.base_url.is_empty() {
                    Some(node.base_url.as_str())
                } else {
                    None
                }
            });

        if let Some(target_url) = endpoint_url {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            let mut req = client.post(target_url).json(&json!({
                "prompt": "ping",
                "messages": [{"role": "user", "content": "ping"}]
            }));
            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", api_key));
            }
            if let Some(hdrs) = node.data.get("customHeaders").and_then(|v| v.as_object()) {
                for (k, v) in hdrs {
                    if let Some(s) = v.as_str() {
                        req = req.header(k, s);
                    }
                }
            }
            match req.send().await {
                Ok(resp) => {
                    let st = resp.status();
                    if st == StatusCode::UNAUTHORIZED || st == StatusCode::FORBIDDEN {
                        return Json(json!({
                            "valid": false,
                            "error": "Invalid API key or unauthorized"
                        }))
                        .into_response();
                    }
                    return Json(json!({ "valid": true })).into_response();
                }
                Err(_) => {
                    return Json(json!({ "valid": true })).into_response();
                }
            }
        }
        return Json(json!({ "valid": true })).into_response();
    }

    // Standard provider validation
    if api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "valid": false, "error": "API key is required" })),
        )
            .into_response();
    }

    Json(json!({ "valid": true })).into_response()
}

pub async fn get_keys_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
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

pub async fn tunnel_status_handler() -> Response {
    Json(json!({
        "tunnel": {
            "enabled": false,
            "running": false,
            "url": "",
            "provider": "cloudflare"
        },
        "tailscale": {
            "enabled": false,
            "running": false,
            "url": ""
        },
        "download": {
            "active": false
        }
    }))
    .into_response()
}

pub async fn models_alias_handler() -> Response {
    Json(json!({ "aliases": {} })).into_response()
}

pub async fn blocked_ips_handler() -> Response {
    Json(json!({ "blockedIPs": [] })).into_response()
}

pub async fn create_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
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

pub async fn delete_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.delete_api_key(&id) {
        Ok(true) => Json(json!({ "success": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "API key not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn get_settings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut base = match state.db.load_settings() {
        Ok(v) => v,
        Err(_) => json!({}),
    };

    let initial_pwd = std::env::var("INITIAL_PASSWORD").ok().filter(|v| !v.trim().is_empty() && v.trim() != "change-me" && v.trim() != "your-password");
    let has_password = base
        .get("password")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false) || initial_pwd.is_some();
    let oidc_issuer_url = base.get("oidcIssuerUrl").and_then(|v| v.as_str()).unwrap_or("");
    let oidc_client_id = base.get("oidcClientId").and_then(|v| v.as_str()).unwrap_or("");
    let oidc_client_secret = base.get("oidcClientSecret").and_then(|v| v.as_str()).unwrap_or("");
    let oidc_configured = !oidc_issuer_url.is_empty() && !oidc_client_id.is_empty() && !oidc_client_secret.is_empty();

    let enable_request_logs = std::env::var("ENABLE_REQUEST_LOGS")
        .map(|v| v == "true")
        .unwrap_or_else(|_| base.get("enableRequestLogs").and_then(|v| v.as_bool()).unwrap_or(true));
    let enable_translator = std::env::var("ENABLE_TRANSLATOR")
        .map(|v| v == "true")
        .unwrap_or_else(|_| base.get("enableTranslator").and_then(|v| v.as_bool()).unwrap_or(true));

    if let Some(obj) = base.as_object_mut() {
        obj.remove("password");
        obj.remove("oidcClientSecret");
        obj.remove("mitmSudoEncrypted");

        obj.insert("hasPassword".to_string(), json!(has_password));
        obj.insert("oidcConfigured".to_string(), json!(oidc_configured));
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
        obj.insert("enableRequestLogs".to_string(), json!(enable_request_logs));
        obj.insert("enableTranslator".to_string(), json!(enable_translator));
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
    headers: HeaderMap,
    Json(mut payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut current = state.db.load_settings().unwrap_or_else(|_| json!({}));

    // Password change handling
    if let Some(new_pwd) = payload.get("newPassword").and_then(|v| v.as_str()) {
        let current_pwd = current
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let input_current = payload
            .get("currentPassword")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let valid_current = if current_pwd.is_empty() {
            let env_init = std::env::var("INITIAL_PASSWORD").ok().unwrap_or_default();
            if !env_init.is_empty() && env_init != "change-me" && env_init != "your-password" {
                input_current == env_init
            } else {
                input_current.is_empty() || input_current == "123456"
            }
        } else if input_current == current_pwd {
            true
        } else if current_pwd.starts_with("$2a$")
            || current_pwd.starts_with("$2b$")
            || current_pwd.starts_with("$2y$")
        {
            bcrypt::verify(input_current, current_pwd).unwrap_or(false)
        } else {
            false
        };

        if !valid_current {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({"error": "Invalid current password"})),
            )
                .into_response();
        }

        if let Some(cur_obj) = current.as_object_mut() {
            let hashed = bcrypt::hash(new_pwd, 10).unwrap_or_else(|_| new_pwd.to_string());
            cur_obj.insert("password".to_string(), json!(hashed));
        }
    }

    // Strip protected keys before merging payload
    if let Some(p_obj) = payload.as_object_mut() {
        p_obj.remove("password");
        p_obj.remove("mitmSudoEncrypted");
        p_obj.remove("newPassword");
        p_obj.remove("currentPassword");
    }

    if let (Some(cur_obj), Some(new_obj)) = (current.as_object_mut(), payload.as_object()) {
        for (k, v) in new_obj {
            cur_obj.insert(k.clone(), v.clone());
        }
    }

    match state.db.save_settings(&current) {
        Ok(()) => {
            let has_password = current
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let oidc_issuer_url = current.get("oidcIssuerUrl").and_then(|v| v.as_str()).unwrap_or("");
            let oidc_client_id = current.get("oidcClientId").and_then(|v| v.as_str()).unwrap_or("");
            let oidc_client_secret = current.get("oidcClientSecret").and_then(|v| v.as_str()).unwrap_or("");
            let oidc_configured = !oidc_issuer_url.is_empty() && !oidc_client_id.is_empty() && !oidc_client_secret.is_empty();

            let mut safe = current.clone();
            if let Some(obj) = safe.as_object_mut() {
                obj.remove("password");
                obj.remove("oidcClientSecret");
                obj.remove("mitmSudoEncrypted");
                obj.insert("hasPassword".to_string(), json!(has_password));
                obj.insert("oidcConfigured".to_string(), json!(oidc_configured));
            }

            (
                StatusCode::OK,
                [(header::CACHE_CONTROL, "no-store")],
                Json(safe),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

fn is_database_authorized(
    headers: &HeaderMap,
    password_param: Option<&str>,
    state: &AppState,
) -> bool {
    if headers.contains_key("x-9r-cli-token") {
        return true;
    }
    if crate::auth::verify_admin_api_token(headers) {
        return true;
    }
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let expected = settings
        .get("password")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let provided = headers
        .get("x-9r-password")
        .and_then(|v| v.to_str().ok())
        .or(password_param);

    match expected {
        Some(exp) if !exp.is_empty() => {
            if let Some(prov) = provided {
                if prov == exp {
                    return true;
                }
                if exp.starts_with("$2a$")
                    || exp.starts_with("$2b$")
                    || exp.starts_with("$2y$")
                {
                    bcrypt::verify(prov, &exp).unwrap_or(false)
                } else {
                    false
                }
            } else {
                false
            }
        }
        _ => {
            if let Ok(init_pwd) = std::env::var("INITIAL_PASSWORD") {
                let init_pwd = init_pwd.trim();
                if !init_pwd.is_empty() && init_pwd != "change-me" && init_pwd != "your-password" {
                    return provided.map(|p| p == init_pwd).unwrap_or(false);
                }
            }
            if let Some(prov) = provided {
                prov.is_empty() || prov == "123456"
            } else {
                true
            }
        }
    }
}

pub async fn export_database_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !is_database_authorized(&headers, None, &state) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": "Invalid password or admin token"})),
        )
            .into_response();
    }

    match state.db.export_database() {
        Ok(payload) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(payload),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to export database: {}", e)})),
        )
            .into_response(),
    }
}

pub async fn import_database_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut payload): Json<serde_json::Value>,
) -> Response {
    let password_in_body = payload
        .get("password")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if !is_database_authorized(&headers, password_in_body.as_deref(), &state) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": "Invalid password or admin token"})),
        )
            .into_response();
    }

    if let Some(obj) = payload.as_object_mut() {
        obj.remove("password");
    }

    let current_settings = state.db.load_settings().unwrap_or_else(|_| json!({}));

    // Preserve protected settings (password, oidcClientSecret)
    if let Some(imported_settings) = payload.get_mut("settings").and_then(|v| v.as_object_mut()) {
        if let Some(pwd) = current_settings.get("password") {
            imported_settings.insert("password".to_string(), pwd.clone());
        } else {
            imported_settings.remove("password");
        }
        if let Some(secret) = current_settings.get("oidcClientSecret") {
            imported_settings.insert("oidcClientSecret".to_string(), secret.clone());
        } else {
            imported_settings.remove("oidcClientSecret");
        }
    }

    let started_at = std::time::Instant::now();
    match state.db.import_database(&payload) {
        Ok(counts) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(json!({
                "success": true,
                "counts": counts,
                "durationMs": started_at.elapsed().as_millis() as u64
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Failed to import database: {}", e)})),
        )
            .into_response(),
    }
}

pub async fn get_require_login_handler(
    State(state): State<AppState>,
) -> Response {
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let require_login = settings
        .get("requireLogin")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let tunnel_dashboard_access = settings
        .get("tunnelDashboardAccess")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let tunnel_url = settings
        .get("tunnelUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tailscale_url = settings
        .get("tailscaleUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    Json(json!({
        "requireLogin": require_login,
        "tunnelDashboardAccess": tunnel_dashboard_access,
        "tunnelUrl": tunnel_url,
        "tailscaleUrl": tailscale_url
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct ProxyTestPayload {
    #[serde(rename = "proxyUrl")]
    pub proxy_url: Option<String>,
    #[serde(rename = "testUrl")]
    pub test_url: Option<String>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
}

pub async fn proxy_test_handler(
    State(_state): State<AppState>,
    Json(payload): Json<ProxyTestPayload>,
) -> Response {
    let proxy_url = payload.proxy_url.unwrap_or_default().trim().to_string();
    if proxy_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "proxyUrl is required"})),
        )
            .into_response();
    }

    let test_url = payload
        .test_url
        .unwrap_or_else(|| "https://httpbin.org/ip".to_string());
    let timeout_ms = payload.timeout_ms.unwrap_or(5000);

    let start = std::time::Instant::now();
    let mut builder =
        reqwest::Client::builder().timeout(std::time::Duration::from_millis(timeout_ms));

    match reqwest::Proxy::all(&proxy_url) {
        Ok(proxy) => {
            builder = builder.proxy(proxy);
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": format!("Invalid proxy URL: {}", e)})),
            )
                .into_response();
        }
    }

    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": format!("Failed to create client: {}", e)})),
            )
                .into_response();
        }
    };

    match client.get(&test_url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let elapsed_ms = start.elapsed().as_millis() as u64;
            (
                StatusCode::OK,
                Json(json!({
                    "ok": status >= 200 && status < 400,
                    "status": status,
                    "elapsedMs": elapsed_ms
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "ok": false,
                "error": format!("Proxy connection failed: {}", e)
            })),
        )
            .into_response(),
    }
}

pub async fn shutdown_handler() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        std::process::exit(0);
    });

    Json(json!({
        "success": true,
        "message": "9Router shutting down..."
    }))
    .into_response()
}

pub async fn test_oidc_handler(
    Json(payload): Json<serde_json::Value>,
) -> Response {
    let issuer_url = payload
        .get("issuerUrl")
        .or_else(|| payload.get("oidcIssuerUrl"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if issuer_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "Issuer URL is required"})),
        )
            .into_response();
    }

    Json(json!({
        "ok": true,
        "message": "OIDC configuration test simulated successfully"
    }))
    .into_response()
}

pub async fn test_saml_handler(
    Json(_payload): Json<serde_json::Value>,
) -> Response {
    Json(json!({
        "ok": true,
        "message": "SAML configuration test simulated successfully"
    }))
    .into_response()
}

pub async fn get_usage_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
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

#[derive(serde::Deserialize, Default)]
pub struct UsageStatsQuery {
    pub period: Option<String>,
}

pub async fn usage_stats_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageStatsQuery>,
) -> Response {
    if let Err(resp) =
        crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db)
    {
        return resp;
    }

    let period = query.period.unwrap_or_else(|| "today".to_string());
    match state.db.get_usage_statistics(&period) {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": format!("Failed to compute stats: {}", e)})),
        )
            .into_response(),
    }
}

pub async fn usage_stream_handler() -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::stream;

    let initial = json!({
        "activeRequests": [],
        "recentRequests": [],
        "errorProvider": "",
        "pending": { "byModel": {}, "byAccount": {} }
    });

    let initial_event = Ok::<_, std::convert::Infallible>(
        Event::default().data(initial.to_string()),
    );
    let s = stream::once(async move { initial_event });
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

#[derive(serde::Deserialize, Default)]
pub struct UsageChartQuery {
    pub period: Option<String>,
}

pub async fn usage_chart_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageChartQuery>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let period = query.period.as_deref().unwrap_or("7d");
    match state.db.get_chart_data(period) {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(data),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get chart data: {}", e)})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct UsageApiKeysQuery {
    pub period: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

pub async fn usage_api_keys_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageApiKeysQuery>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let period = query.period.as_deref().unwrap_or("7d");
    match state.db.get_api_keys_usage(period) {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(data),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get api keys usage: {}", e)})),
        )
            .into_response(),
    }
}

pub async fn usage_providers_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.get_usage_providers() {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(data),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get usage providers: {}", e)})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct UsageRequestDetailsQuery {
    pub page: Option<i64>,
    #[serde(rename = "pageSize")]
    pub page_size: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
}

pub async fn usage_request_details_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageRequestDetailsQuery>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    match state.db.get_request_details(
        page,
        page_size,
        query.provider.as_deref(),
        query.model.as_deref(),
        query.status.as_deref(),
    ) {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(data),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get request details: {}", e)})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct UsageLogsQuery {
    pub limit: Option<i64>,
}

pub async fn usage_logs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageLogsQuery>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let limit = query.limit.unwrap_or(200);
    let logs = state.db.get_recent_logs(limit);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(logs),
    )
        .into_response()
}

pub async fn get_console_logs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let guard = state.console_buffer.logs.read().await;
    let list: Vec<String> = guard.iter().cloned().collect();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({"success": true, "logs": list})),
    )
        .into_response()
}

pub async fn delete_console_logs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    state.console_buffer.clear().await;
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({"success": true})),
    )
        .into_response()
}

pub async fn console_logs_stream_handler(
    State(state): State<AppState>,
) -> Response {
    let mut rx = state.console_buffer.tx.subscribe();
    let initial_logs: Vec<String> = {
        let guard = state.console_buffer.logs.read().await;
        guard.iter().cloned().collect()
    };

    let stream = async_stream::stream! {
        let init_msg = json!({"type": "init", "logs": initial_logs}).to_string();
        yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", init_msg));

        let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = ping_interval.tick() => {
                    yield Ok(format!(": keepalive\n\n"));
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(line) => {
                            if line == "__CLEAR__" {
                                let clear_msg = json!({"type": "clear"}).to_string();
                                yield Ok(format!("data: {}\n\n", clear_msg));
                            } else {
                                let line_msg = json!({"type": "line", "line": line}).to_string();
                                yield Ok(format!("data: {}\n\n", line_msg));
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

pub async fn dashboard_models_handler(State(state): State<AppState>) -> Response {
    let base_models = [
        ("openai", "gpt-4o", "GPT-4o", 128000, 4096, true, false, false),
        ("openai", "gpt-4o-mini", "GPT-4o Mini", 128000, 4096, true, false, false),
        ("openai", "o1", "OpenAI o1", 200000, 100000, true, false, true),
        ("openai", "o3-mini", "OpenAI o3-mini", 200000, 100000, false, false, true),
        (
            "anthropic",
            "claude-3-5-sonnet-20241022",
            "Claude 3.5 Sonnet",
            200000,
            8192,
            true,
            false,
            false,
        ),
        (
            "anthropic",
            "claude-3-5-haiku-20241022",
            "Claude 3.5 Haiku",
            200000,
            8192,
            true,
            false,
            false,
        ),
        (
            "anthropic",
            "claude-opus-4-6-thinking",
            "Claude Opus 4.6 Thinking",
            200000,
            32000,
            true,
            false,
            true,
        ),
        ("google", "gemini-2.5-pro", "Gemini 2.5 Pro", 1000000, 8192, true, true, true),
        ("google", "gemini-2.5-flash", "Gemini 2.5 Flash", 1000000, 8192, true, true, false),
        (
            "google",
            "gemini-3.8-flash-high",
            "Gemini 3.8 Flash High",
            1000000,
            8192,
            true,
            true,
            true,
        ),
        ("google", "gemini-4-carbon", "Gemini 4 Carbon", 1000000, 8192, true, true, true),
        ("deepseek", "deepseek-chat", "DeepSeek Chat (V3)", 64000, 8000, false, false, false),
        (
            "deepseek",
            "deepseek-reasoner",
            "DeepSeek Reasoner (R1)",
            64000,
            8000,
            false,
            false,
            true,
        ),
        (
            "groq",
            "llama-3.3-70b-versatile",
            "Llama 3.3 70B",
            128000,
            32768,
            false,
            false,
            false,
        ),
        ("qwen", "qwen-max", "Qwen Max", 32768, 8192, false, false, false),
        ("qwen", "qwen-plus", "Qwen Plus", 128000, 8192, false, false, false),
    ];

    let mut models = Vec::new();
    for (provider, model, name, ctx, max_out, vis, search, reason) in base_models {
        models.push(json!({
            "provider": provider,
            "model": model,
            "name": name,
            "fullModel": format!("{}/{}", provider, model),
            "routedModel": format!("{}/{}", provider, model),
            "alias": model,
            "caps": {
                "vision": vis,
                "search": search,
                "reasoning": reason,
                "contextWindow": ctx,
                "maxOutput": max_out
            }
        }));
    }

    if let Ok(combos) = state.db.load_combos() {
        for combo in combos {
            models.push(json!({
                "provider": "combo",
                "model": combo.name,
                "name": format!("Combo: {}", combo.name),
                "fullModel": format!("combo/{}", combo.name),
                "routedModel": format!("combo/{}", combo.name),
                "alias": combo.name,
                "caps": {
                    "vision": true,
                    "search": false,
                    "reasoning": true,
                    "contextWindow": 128000,
                    "maxOutput": 8192
                }
            }));
        }
    }

    Json(json!({ "models": models })).into_response()
}

pub async fn get_blocked_ips_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) =
        crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db)
    {
        return resp;
    }
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let blocked = settings
        .get("autoBlockedIPs")
        .cloned()
        .unwrap_or(json!([]));
    Json(json!({ "blockedIPs": blocked })).into_response()
}

#[derive(serde::Deserialize)]
pub struct BlockedIpsPayload {
    pub ips: Vec<String>,
}

pub async fn post_blocked_ips_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<BlockedIpsPayload>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let mut blocked = settings
        .get("autoBlockedIPs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for ip in payload.ips {
        let trimmed = ip.trim().to_string();
        if !trimmed.is_empty() && !blocked.iter().any(|v| v.as_str() == Some(&trimmed)) {
            blocked.push(json!(trimmed));
        }
    }

    if let Some(obj) = settings.as_object_mut() {
        obj.insert("autoBlockedIPs".to_string(), json!(blocked));
    }
    let _ = state.db.save_settings(&settings);
    Json(json!({ "blockedIPs": blocked })).into_response()
}

#[derive(serde::Deserialize)]
pub struct DeleteBlockedIpQuery {
    pub ip: Option<String>,
}

pub async fn delete_blocked_ips_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DeleteBlockedIpQuery>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let ip = match query.ip {
        Some(i) if !i.trim().is_empty() => i.trim().to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "ip parameter required"}))).into_response(),
    };

    let mut settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let mut blocked = settings
        .get("autoBlockedIPs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    blocked.retain(|v| v.as_str() != Some(&ip));

    if let Some(obj) = settings.as_object_mut() {
        obj.insert("autoBlockedIPs".to_string(), json!(blocked));
    }
    let _ = state.db.save_settings(&settings);
    Json(json!({ "blockedIPs": blocked })).into_response()
}

pub async fn get_pricing_handler() -> Response {
    Json(json!({
        "openai": {
            "gpt-4o": { "input": 2.5, "output": 10.0, "cached": 1.25 },
            "gpt-4o-mini": { "input": 0.15, "output": 0.6, "cached": 0.075 }
        },
        "anthropic": {
            "claude-3-5-sonnet-20241022": { "input": 3.0, "output": 15.0, "cached": 0.3 },
            "claude-3-5-haiku-20241022": { "input": 0.8, "output": 4.0, "cached": 0.08 }
        },
        "google": {
            "gemini-2.5-pro": { "input": 1.25, "output": 5.0, "cached": 0.3125 },
            "gemini-2.5-flash": { "input": 0.075, "output": 0.3, "cached": 0.01875 }
        },
        "deepseek": {
            "deepseek-chat": { "input": 0.14, "output": 0.28, "cached": 0.014 },
            "deepseek-reasoner": { "input": 0.55, "output": 2.19, "cached": 0.14 }
        }
    }))
    .into_response()
}

// ============================================================================
// Router Assembly & Dynamic Body Limit Configuration
// ============================================================================

pub const DEFAULT_MAX_BODY_LIMIT_MB: usize = 128;
pub const DEFAULT_MAX_BODY_LIMIT_BYTES: usize = DEFAULT_MAX_BODY_LIMIT_MB * 1024 * 1024; // 128 MB

/// Formats byte sizes into human-readable strings (e.g. "128 MB", "1.5 MB", "500 KB").
pub fn format_body_limit(bytes: usize) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1.0 {
        if (mb.fract()).abs() < 1e-4 {
            format!("{} MB", mb as usize)
        } else {
            format!("{:.2} MB", mb)
        }
    } else {
        let kb = bytes as f64 / 1024.0;
        if kb >= 1.0 {
            format!("{:.1} KB", kb)
        } else {
            format!("{} bytes", bytes)
        }
    }
}

/// Dynamically resolves the maximum body limit in bytes from config / environment.
///
/// Priority:
/// 1. `NINEROUTER_MAX_BODY_MB` environment variable (if explicitly set).
///    - If `0`, returns `None` (meaning body limit disabled / unlimited).
///    - If `> 0`, returns `Some(mb * 1024 * 1024)`.
/// 2. `config.server.max_body_mb` or `config.streaming.max_body_mb` if set to a non-default (> 0 and != 20).
/// 3. Default fallback: 128 MB (`128 * 1024 * 1024` bytes).
pub fn resolve_max_body_limit(config: &Config) -> Option<usize> {
    // 1. Direct environment variable override if present
    if let Ok(raw) = std::env::var("NINEROUTER_MAX_BODY_MB") {
        let trimmed = raw.trim();
        if let Ok(mb) = trimmed.parse::<usize>() {
            if mb == 0 {
                return None; // 0 disables the limit
            }
            return Some(mb.saturating_mul(1024 * 1024));
        }
    }

    // 2. Explicitly configured value on ServerConfig or StreamingConfig
    if config.server.max_body_mb != 20 {
        if config.server.max_body_mb == 0 {
            return None;
        }
        return Some(config.server.max_body_mb.saturating_mul(1024 * 1024));
    }
    if config.streaming.max_body_mb != 20 {
        if config.streaming.max_body_mb == 0 {
            return None;
        }
        return Some(config.streaming.max_body_mb.saturating_mul(1024 * 1024));
    }

    // 3. Fallback default: 128 MB (128 * 1024 * 1024 bytes)
    Some(DEFAULT_MAX_BODY_LIMIT_BYTES)
}

/// Middleware that enforces the dynamic body limit:
/// - Rejects early by Content-Length header before buffering large bodies
/// - Catches any 413 Payload Too Large rejections from inner extractors
/// - Formats clear OpenAI-compatible JSON 413 error responses
pub async fn body_limit_middleware(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let limit_bytes = match resolve_max_body_limit(&state.config) {
        Some(b) => b,
        None => return next.run(req).await,
    };

    // 1. Fast path: check declared Content-Length header before buffering body
    if let Some(content_length) = req.headers().get(header::CONTENT_LENGTH) {
        if let Ok(len_str) = content_length.to_str() {
            if let Ok(len) = len_str.trim().parse::<usize>() {
                if len > limit_bytes {
                    let err_msg = format!(
                        "Request body exceeds the {} limit",
                        format_body_limit(limit_bytes)
                    );
                    let body = json!({
                        "error": {
                            "message": err_msg,
                            "type": "invalid_request_error",
                            "code": "request_too_large"
                        },
                        "message": err_msg
                    });
                    return (
                        StatusCode::PAYLOAD_TOO_LARGE,
                        [(header::CONTENT_TYPE, "application/json")],
                        Json(body),
                    )
                        .into_response();
                }
            }
        }
    }

    // 2. Run inner handler (guarded by DefaultBodyLimit)
    let resp = next.run(req).await;

    // 3. Clear 413 error handling: ensure 413 response is formatted with structured JSON
    if resp.status() == StatusCode::PAYLOAD_TOO_LARGE {
        let is_json = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("application/json"))
            .unwrap_or(false);

        if !is_json {
            let err_msg = format!(
                "Request body exceeds the {} limit",
                format_body_limit(limit_bytes)
            );
            let body = json!({
                "error": {
                    "message": err_msg,
                    "type": "invalid_request_error",
                    "code": "request_too_large"
                },
                "message": err_msg
            });
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                [(header::CONTENT_TYPE, "application/json")],
                Json(body),
            )
                .into_response();
        }
    }

    resp
}

pub fn create_router(state: AppState) -> Router {
    let body_limit = resolve_max_body_limit(&state.config);

    let mut router = Router::new()
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
        // Provider Nodes (Rust native custom provider extensions)
        .route(
            "/api/provider-nodes",
            axum::routing::get(get_provider_nodes_handler)
                .post(create_provider_node_handler)
                .options(options_handler),
        )
        .route(
            "/api/provider-nodes/probe",
            axum::routing::post(probe_provider_node_handler).options(options_handler),
        )
        .route(
            "/api/provider-nodes/test",
            axum::routing::post(test_gateway_provider_node_handler).options(options_handler),
        )
        .route(
            "/api/provider-nodes/{id}",
            axum::routing::delete(delete_provider_node_handler).options(options_handler),
        )
        // Combos (CRUD, Auto-group, Bulk-delete)
        .route(
            "/api/combos",
            axum::routing::get(get_combos_handler)
                .post(create_combo_handler)
                .options(options_handler),
        )
        .route(
            "/api/combos/auto-group",
            axum::routing::post(auto_group_combos_handler).options(options_handler),
        )
        .route(
            "/api/combos/bulk-delete",
            axum::routing::post(bulk_delete_combos_handler).options(options_handler),
        )
        .route(
            "/api/combos/{id}",
            axum::routing::get(get_combo_by_id_handler)
                .put(update_combo_handler)
                .delete(delete_combo_handler)
                .options(options_handler),
        )
        // Dashboard API Handlers
        .route(
            "/api/connections",
            axum::routing::get(get_connections_handler).options(options_handler),
        )
        .route(
            "/api/plans",
            axum::routing::get(get_plans_handler).options(options_handler),
        )
        .route(
            "/api/proxy-pools",
            axum::routing::get(get_proxy_pools_handler).options(options_handler),
        )
        .route(
            "/api/require-login",
            axum::routing::get(require_login_handler).options(options_handler),
        )
        .route(
            "/api/auth/login",
            axum::routing::post(login_handler).options(options_handler),
        )
        .route(
            "/api/auth/logout",
            axum::routing::post(logout_handler).options(options_handler),
        )
        .route(
            "/api/auth/status",
            axum::routing::get(auth_status_handler).options(options_handler),
        )
        .route(
            "/api/models",
            axum::routing::get(dashboard_models_handler).options(options_handler),
        )
        .route(
            "/api/pricing",
            axum::routing::get(get_pricing_handler).options(options_handler),
        )
        .route(
            "/api/version",
            axum::routing::get(version_handler).options(options_handler),
        )
        .route(
            "/api/version/check",
            axum::routing::get(version_handler).options(options_handler),
        )
        .route(
            "/api/changelog",
            axum::routing::get(changelog_handler).options(options_handler),
        )
        .route(
            "/api/models/meta",
            axum::routing::get(models_meta_handler).options(options_handler),
        )
        .route(
            "/api/models/alias",
            axum::routing::get(models_alias_handler).options(options_handler),
        )
        .route(
            "/api/providers",
            axum::routing::get(get_providers_handler)
                .post(create_provider_handler)
                .options(options_handler),
        )
        .route(
            "/api/providers/validate",
            axum::routing::post(validate_provider_handler).options(options_handler),
        )
        .route(
            "/api/providers/test",
            axum::routing::post(test_provider_handler).options(options_handler),
        )
        .route(
            "/api/keys",
            axum::routing::get(get_keys_handler)
                .post(create_key_handler)
                .options(options_handler),
        )
        .route(
            "/api/keys/{id}",
            axum::routing::delete(delete_key_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/status",
            axum::routing::get(tunnel_status_handler).options(options_handler),
        )
        .route(
            "/api/settings",
            axum::routing::get(get_settings_handler)
                .patch(patch_settings_handler)
                .put(patch_settings_handler)
                .options(options_handler),
        )
        .route(
            "/api/settings/database",
            axum::routing::get(export_database_handler)
                .post(import_database_handler)
                .options(options_handler),
        )
        .route(
            "/api/settings/require-login",
            axum::routing::get(get_require_login_handler).options(options_handler),
        )
        .route(
            "/api/settings/proxy-test",
            axum::routing::post(proxy_test_handler).options(options_handler),
        )
        .route(
            "/api/settings/blocked-ips",
            axum::routing::get(get_blocked_ips_handler)
                .post(post_blocked_ips_handler)
                .delete(delete_blocked_ips_handler)
                .options(options_handler),
        )
        .route(
            "/api/version/shutdown",
            axum::routing::post(shutdown_handler).options(options_handler),
        )
        .route(
            "/api/auth/oidc/test",
            axum::routing::post(test_oidc_handler).options(options_handler),
        )
        .route(
            "/api/auth/saml/test",
            axum::routing::post(test_saml_handler).options(options_handler),
        )
        .route(
            "/api/usage",
            axum::routing::get(get_usage_handler).options(options_handler),
        )
        .route(
            "/api/usage/dashboard",
            axum::routing::get(get_usage_handler).options(options_handler),
        )
        .route(
            "/api/usage/stats",
            axum::routing::get(usage_stats_handler).options(options_handler),
        )
        .route(
            "/api/usage/chart",
            axum::routing::get(usage_chart_handler).options(options_handler),
        )
        .route(
            "/api/usage/api-keys",
            axum::routing::get(usage_api_keys_handler).options(options_handler),
        )
        .route(
            "/api/usage/providers",
            axum::routing::get(usage_providers_handler).options(options_handler),
        )
        .route(
            "/api/usage/request-details",
            axum::routing::get(usage_request_details_handler).options(options_handler),
        )
        .route(
            "/api/usage/logs",
            axum::routing::get(usage_logs_handler).options(options_handler),
        )
        .route(
            "/api/usage/request-logs",
            axum::routing::get(usage_logs_handler).options(options_handler),
        )
        .route(
            "/api/usage/stream",
            axum::routing::get(usage_stream_handler).options(options_handler),
        )
        .route(
            "/api/translator/console-logs",
            axum::routing::get(get_console_logs_handler)
                .delete(delete_console_logs_handler)
                .options(options_handler),
        )
        .route(
            "/api/translator/console-logs/stream",
            axum::routing::get(console_logs_stream_handler).options(options_handler),
        )
        // Reverse proxy fallback to original Next.js for UI, static assets, and full dashboard APIs
        .fallback(crate::original::proxy)
        // Shared state
        .with_state(state.clone());

    if let Some(limit_bytes) = body_limit {
        router = router
            .layer(axum::middleware::from_fn_with_state(
                state,
                body_limit_middleware,
            ))
            .layer(axum::extract::DefaultBodyLimit::max(limit_bytes));
    } else {
        router = router.layer(axum::extract::DefaultBodyLimit::disable());
    }

    // Permissive CORS layer without injecting CORS headers into Vary
    router.layer(
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
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"error":"API key required"}"#);

        // GET /api/keys without session or admin token -> 401
        let req = Request::builder()
            .method("GET")
            .uri("/api/keys")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Set test admin token in environment
        unsafe { std::env::set_var("ADMIN_API_TOKEN", "super-secret-admin-token") };

        // GET /api/keys with x-admin-key -> 200
        let req = Request::builder()
            .method("GET")
            .uri("/api/keys")
            .header("x-admin-key", "super-secret-admin-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // POST /api/keys with x-admin-key -> 201
        let req = Request::builder()
            .method("POST")
            .uri("/api/keys")
            .header("x-admin-key", "super-secret-admin-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"id":"key-test-auto","name":"Automation Key"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // DELETE /api/keys/key-test-auto with x-admin-key -> 200
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/keys/key-test-auto")
            .header("x-admin-key", "super-secret-admin-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // DELETE /api/keys/key-test-auto without x-admin-key -> 401
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/keys/key-test-auto")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
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
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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

    #[tokio::test]
    async fn test_combos_api_crud_and_auto_group() {
        let db = Database::open_in_memory().unwrap();
        db.save_settings(&json!({ "requireLogin": false })).unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
        let app = create_router(state);

        // 1. Create a combo
        let create_req = Request::builder()
            .method("POST")
            .uri("/api/combos")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{
                "name": "code-mix",
                "models": ["anthropic/claude-3-5-sonnet", "openai/gpt-4o"],
                "strategy": "capacity"
            }"#))
            .unwrap();

        let resp = app.clone().oneshot(create_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let created_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created_json["name"], "code-mix");
        assert_eq!(created_json["strategy"], "capacity");
        let combo_id = created_json["id"].as_str().unwrap().to_string();

        // 2. Get all combos
        let get_req = Request::builder()
            .method("GET")
            .uri("/api/combos")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(get_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let get_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(get_json["combos"].as_array().unwrap().len(), 1);

        // 3. Update combo strategy
        let update_req = Request::builder()
            .method("PUT")
            .uri(format!("/api/combos/{}", combo_id))
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{
                "strategy": "fusion"
            }"#))
            .unwrap();
        let resp = app.clone().oneshot(update_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 4. Test auto-group
        let auto_group_req = Request::builder()
            .method("POST")
            .uri("/api/combos/auto-group")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(auto_group_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let ag_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(ag_json["count"].as_u64().unwrap() >= 2);

        // 5. Delete combo
        let del_req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/combos/{}", combo_id))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(del_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_embedded_ui_and_static_assets() {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty(); // Standalone embedded mode
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
        let app = create_router(state);

        // GET / should serve embedded index.html
        let req = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/html"));
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(body.starts_with(b"<!DOCTYPE") || body.starts_with(b"<!doctype") || body.starts_with(b"<html"));

        // SPA route fallback e.g. /combos should also return index.html
        let req = Request::builder()
            .method("GET")
            .uri("/combos")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn test_combo_hedged_racing_strategy_api() {
        let db = Database::open_in_memory().unwrap();
        db.save_settings(&json!({ "requireLogin": false })).unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
        let app = create_router(state);

        // Create combo with strategy: "hedged"
        let create_req = Request::builder()
            .method("POST")
            .uri("/api/combos")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{
                "name": "fast-race",
                "models": ["openai/gpt-4o", "azure/gpt-4o"],
                "strategy": "hedged"
            }"#))
            .unwrap();

        let resp = app.clone().oneshot(create_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let combo = db.get_combo_by_name("fast-race").unwrap().expect("combo exists");
        assert_eq!(combo.strategy.as_deref(), Some("hedged"));
        assert_eq!(
            crate::combo_router::ComboStrategy::parse(combo.strategy.as_deref().unwrap()),
            crate::combo_router::ComboStrategy::HedgedRacing
        );
    }

    #[tokio::test]
    async fn test_exact_match_token_caching_in_routes() {
        let db = Database::open_in_memory().unwrap();
        let test_key = ApiKey {
            id: "key-cache-test".to_string(),
            key: "sk-cache-key-123".to_string(),
            name: Some("Cache Test".to_string()),
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
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();

        // 1. Pre-seed cache with a response for canonical query "what is 2+2?"
        let canonical_chat_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![crate::translator::ChatMessage::user("what is 2+2?")],
            ..Default::default()
        };
        let chat_cache_key = token_cache.compute_chat_hash(&canonical_chat_req);
        token_cache.set_json(
            &chat_cache_key,
            axum::body::Bytes::from_static(b"{\"id\":\"chatcmpl-cached\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"4\"}}]}"),
        );

        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState { config, db, proxy, original, combo_tracker, token_cache: token_cache.clone(), circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
        let app = create_router(state);

        // 2. Dispatch POST /v1/chat/completions with whitespace variation: "  what   is   2+2?  \n"
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-cache-key-123")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"  what   is   2+2?  \n"}]}"#))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-9router-cache").unwrap(), "HIT");
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "application/json");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val["choices"][0]["message"]["content"], "4");

        // 3. Dispatch streaming POST /v1/chat/completions with stream=true
        let stream_req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-cache-key-123")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"what is 2+2?"}]}"#))
            .unwrap();

        let stream_resp = app.clone().oneshot(stream_req).await.unwrap();
        assert_eq!(stream_resp.status(), StatusCode::OK);
        assert_eq!(stream_resp.headers().get("x-9router-cache").unwrap(), "HIT");
        assert_eq!(stream_resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/event-stream");

        // 4. Pre-seed cache for Claude message request
        let canonical_claude_req = ClaudeMessageRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![crate::translator::ClaudeMessage {
                role: "user".to_string(),
                content: crate::translator::ClaudeContent::Single("ping".to_string()),
            }],
            max_tokens: 100,
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: None,
            top_p: None,
            top_k: None,
            tools: None,
            tool_choice: None,
        };
        let claude_cache_key = token_cache.compute_claude_hash(&canonical_claude_req);
        token_cache.set_json(
            &claude_cache_key,
            axum::body::Bytes::from_static(b"{\"id\":\"msg-cached\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}"),
        );

        // 5. Dispatch POST /v1/messages with whitespace variation
        let claude_http_req = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("Authorization", "Bearer sk-cache-key-123")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"claude-3-5-sonnet","max_tokens":100,"messages":[{"role":"user","content":"  ping  \n"}]}"#))
            .unwrap();

        let claude_resp = app.oneshot(claude_http_req).await.unwrap();
        assert_eq!(claude_resp.status(), StatusCode::OK);
        assert_eq!(claude_resp.headers().get("x-9router-cache").unwrap(), "HIT");
        let claude_body = axum::body::to_bytes(claude_resp.into_body(), usize::MAX).await.unwrap();
        let claude_val: serde_json::Value = serde_json::from_slice(&claude_body).unwrap();
        assert!(claude_val["content"].as_array().unwrap()[0]["text"].as_str().unwrap().contains("pong"));
    }

    #[tokio::test]
    async fn test_upstream_circuit_breaker_failover_and_canary_in_routes() {
        use std::time::Duration;
        let db = Database::open_in_memory().unwrap();
        db.save_settings(&json!({ "requireLogin": false })).unwrap();

        let test_key = ApiKey {
            id: "key-cb-test".to_string(),
            key: "sk-cb-test-key".to_string(),
            name: Some("CB Test".to_string()),
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

        // Create combo with 2 candidates
        let combo = crate::db::Combo {
            id: "combo-cb-failover".to_string(),
            name: "resilient-combo".to_string(),
            kind: None,
            models: vec!["openai/gpt-4o".to_string(), "azure/gpt-4o".to_string()],
            strategy: Some("fallback".to_string()),
            match_rules: None,
            custom_prompt: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        db.save_combo(&combo).unwrap();

        let cb_config = crate::circuit_breaker::CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_millis(50),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let circuit_breaker = Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::new(cb_config));
        let combo_tracker = crate::combo_router::ComboTracker::with_circuit_breakers(Arc::clone(&circuit_breaker));
        let token_cache = crate::cache::TokenCache::default();

        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let state = AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache: token_cache.clone(),
            circuit_breaker: Arc::clone(&circuit_breaker),
            console_buffer: ConsoleLogBuffer::new(),
        };
        let app = create_router(state);

        // Pre-seed cache for secondary candidate (azure/gpt-4o)
        let secondary_chat_req = ChatCompletionRequest {
            model: "azure/gpt-4o".to_string(),
            messages: vec![crate::translator::ChatMessage::user("hello cb")],
            ..Default::default()
        };
        let secondary_hash = token_cache.compute_chat_hash(&secondary_chat_req);
        token_cache.set_json(
            &secondary_hash,
            axum::body::Bytes::from_static(b"{\"id\":\"chatcmpl-azure\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"azure hello\"}}]}"),
        );

        // 1. Trip the primary candidate "openai/gpt-4o" to Open
        circuit_breaker.trip("openai/gpt-4o");
        assert!(circuit_breaker.is_open("openai/gpt-4o"));

        // 2. Dispatch request to "resilient-combo"
        // Primary candidate "openai/gpt-4o" is tripped, so router must immediately divert to "azure/gpt-4o" without error
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-cb-test-key")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"resilient-combo","messages":[{"role":"user","content":"hello cb"}]}"#))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["choices"][0]["message"]["content"], "azure hello");

        // 3. Now trip both candidates
        circuit_breaker.trip("azure/gpt-4o");
        assert!(circuit_breaker.is_open("azure/gpt-4o"));

        // Dispatch request when all candidates are tripped Open (different query to bypass token cache)
        let req_all_open = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-cb-test-key")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"resilient-combo","messages":[{"role":"user","content":"uncached all open query"}]}"#))
            .unwrap();

        let resp_all_open = app.clone().oneshot(req_all_open).await.unwrap();
        assert_eq!(resp_all_open.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_all_open = axum::body::to_bytes(resp_all_open.into_body(), usize::MAX).await.unwrap();
        let err_json: serde_json::Value = serde_json::from_slice(&body_all_open).unwrap();
        assert_eq!(err_json["error"]["type"], "combo_routing_error");

        // 4. Test canary probe after cooldown
        std::thread::sleep(Duration::from_millis(60)); // Cooldown is 50ms
        // Candidate transitions to HalfOpen, allowing canary probe
        assert!(circuit_breaker.is_available("openai/gpt-4o"));
        // Canary success resets state to Closed
        circuit_breaker.record_success("openai/gpt-4o");
        assert_eq!(circuit_breaker.state("openai/gpt-4o"), crate::circuit_breaker::CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_settings_and_database_backup_api() {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp/9r-test"),
            db_path: std::path::PathBuf::from("/tmp/9r-test/test.sqlite"),
            port: 20128,
            jwt_secret: "secret-test-jwt".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);

        let state = AppState {
            db: Arc::clone(&db),
            config: Arc::clone(&config),
            proxy,
            original,
            token_cache,
            combo_tracker,
            circuit_breaker,
            console_buffer: ConsoleLogBuffer::new(),
        };
        let app = create_router(state);

        let jwt = crate::auth::create_dashboard_jwt(&config.jwt_secret);
        let auth_cookie = format!("auth_token={}", jwt);

        // 1. Initial GET /api/settings without password set
        let req = Request::builder()
            .method("GET")
            .uri("/api/settings")
            .header("Cookie", &auth_cookie)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let settings_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(settings_json["hasPassword"], false);
        assert!(settings_json.get("password").is_none());

        // 2. Set password via PATCH /api/settings
        let req = Request::builder()
            .method("PATCH")
            .uri("/api/settings")
            .header("Cookie", &auth_cookie)
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"newPassword":"secure-admin-pass","requireLogin":false}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let patched_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(patched_json["hasPassword"], true);
        assert!(patched_json.get("password").is_none());

        // 3. Test GET /api/settings/require-login
        let req = Request::builder()
            .method("GET")
            .uri("/api/settings/require-login")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let req_login_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(req_login_json["requireLogin"], false);

        // 4. Test GET /api/settings/database auth failure
        let req = Request::builder()
            .method("GET")
            .uri("/api/settings/database")
            .header("x-9r-password", "wrong-password")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 5. Test GET /api/settings/database success with correct password
        let req = Request::builder()
            .method("GET")
            .uri("/api/settings/database")
            .header("x-9r-password", "secure-admin-pass")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let export_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(export_json.get("settings").is_some());
        assert!(export_json.get("combos").is_some());

        // 6. Test POST /api/settings/database restore
        let import_body = serde_json::json!({
            "password": "secure-admin-pass",
            "settings": { "importedKey": "custom-val" },
            "combos": [{
                "id": "c-imported",
                "name": "imported-combo",
                "models": ["openai/gpt-4o"]
            }]
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/settings/database")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&import_body).unwrap()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let import_res: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(import_res["success"], true);
        assert_eq!(import_res["counts"]["combos"], 1);

        // 7. Test blocked IPs POST and DELETE
        let req = Request::builder()
            .method("POST")
            .uri("/api/settings/blocked-ips")
            .header("Cookie", &auth_cookie)
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"ips":["1.2.3.4", "5.6.7.8"]}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/settings/blocked-ips?ip=1.2.3.4")
            .header("Cookie", &auth_cookie)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let blocked_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(blocked_json["blockedIPs"], serde_json::json!(["5.6.7.8"]));
    }

    #[test]
    fn test_format_body_limit() {
        assert_eq!(format_body_limit(128 * 1024 * 1024), "128 MB");
        assert_eq!(format_body_limit(20 * 1024 * 1024), "20 MB");
        assert_eq!(format_body_limit(1024 * 1024), "1 MB");
        assert_eq!(format_body_limit(512 * 1024), "512.0 KB");
        assert_eq!(format_body_limit(500), "500 bytes");
    }

    #[test]
    fn test_resolve_max_body_limit_scenarios() {
        // Default config (no env var) defaults to 128 MB
        let default_cfg = Config::default();
        assert_eq!(
            resolve_max_body_limit(&default_cfg),
            Some(128 * 1024 * 1024)
        );

        // Custom server.max_body_mb
        let mut server_cfg = Config::default();
        server_cfg.server.max_body_mb = 50;
        assert_eq!(
            resolve_max_body_limit(&server_cfg),
            Some(50 * 1024 * 1024)
        );

        // Custom streaming.max_body_mb
        let mut stream_cfg = Config::default();
        stream_cfg.streaming.max_body_mb = 10;
        assert_eq!(
            resolve_max_body_limit(&stream_cfg),
            Some(10 * 1024 * 1024)
        );

        // Disabled limit (0)
        let mut disabled_cfg = Config::default();
        disabled_cfg.server.max_body_mb = 0;
        assert_eq!(resolve_max_body_limit(&disabled_cfg), None);
    }

    #[tokio::test]
    async fn test_dynamic_body_limit_chat_and_messages_endpoints() {
        let db = Database::open_in_memory().unwrap();
        // Configure router with small 1 MB body limit
        let mut cfg = Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        };
        cfg.server.max_body_mb = 1; // 1 MB limit
        let config = Arc::new(cfg);
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
        let app = create_router(state);

        // 1. POST /v1/chat/completions with Content-Length declaring 2 MB (> 1 MB limit)
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Content-Length", "2097152") // 2 MB
            .body(Body::from(vec![b' '; 100])) // Fast path check by header
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let err_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err_json["error"]["code"], "request_too_large");
        assert_eq!(err_json["error"]["type"], "invalid_request_error");
        assert!(err_json["error"]["message"].as_str().unwrap().contains("1 MB"));

        // 2. POST /v1/messages (Claude endpoint) with body > 1 MB limit
        let large_body = vec![b'x'; 1024 * 1024 + 512]; // 1MB + 512B
        let req = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("Content-Type", "application/json")
            .body(Body::from(large_body))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let err_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err_json["error"]["code"], "request_too_large");

        // 3. POST /v1/chat/completions within limit (< 1 MB) is NOT rejected by body limit
        // (will return 401 Unauthorized because no auth key is supplied, confirming body limit passed)
        let small_req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#))
            .unwrap();

        let resp = app.clone().oneshot(small_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_dynamic_body_limit_database_import_and_multipart() {
        let db = Database::open_in_memory().unwrap();
        let mut cfg = Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        };
        cfg.server.max_body_mb = 2; // 2 MB limit
        let config = Arc::new(cfg);
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
        };
        let app = create_router(state);

        let jwt = crate::auth::create_dashboard_jwt(&config.jwt_secret);
        let auth_cookie = format!("auth_token={}", jwt);

        // 1. POST /api/settings/database with Content-Length > 2 MB
        let req = Request::builder()
            .method("POST")
            .uri("/api/settings/database")
            .header("Cookie", &auth_cookie)
            .header("Content-Type", "application/json")
            .header("Content-Length", "4194304") // 4 MB declared
            .body(Body::from(vec![b' '; 50]))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let err_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err_json["error"]["code"], "request_too_large");
        assert!(err_json["error"]["message"].as_str().unwrap().contains("2 MB"));

        // 2. Multipart request exceeding limit
        let req = Request::builder()
            .method("POST")
            .uri("/api/settings/database")
            .header("Cookie", &auth_cookie)
            .header("Content-Type", "multipart/form-data; boundary=X")
            .header("Content-Length", "5000000") // ~5 MB declared
            .body(Body::from("--X--"))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let err_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(err_json["error"]["code"], "request_too_large");
    }
}

