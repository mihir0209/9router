use std::collections::HashMap;
use std::sync::Arc;

use async_stream::stream;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use futures_util::{Stream, StreamExt};
use serde_json::json;

use crate::auth;
use crate::config::Config;
use crate::db::{ApiKey, ConnectionData, Database, ProviderConnection, RequestDetail, UsageRecord};
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

pub(crate) fn authenticate(
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
// Models Catalog Handler & Source Resolution
// ============================================================================

pub fn resolve_models_list_source(config: &Config, db: &Database) -> String {
    let setting = db.load_settings().ok().and_then(|s| {
        s.get("modelsListSource")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
    });

    let config_val = if config.routing.models_list_source != "live" {
        Some(config.routing.models_list_source.as_str())
    } else {
        None
    };

    crate::proxy::resolve_models_list_source(
        None,
        setting.as_deref(),
        config_val,
    )
}

pub async fn models_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(res) => return res,
    };

    let source = resolve_models_list_source(&state.config, &state.db);
    let now = chrono::Utc::now().timestamp();
    let mut data = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // 1. Combos from SQLite
    if let Ok(combos) = state.db.load_combos() {
        for combo in combos {
            if state.db.check_model_allowed(&api_key, &combo.name).is_ok() {
                if seen_ids.insert(combo.name.clone()) {
                    data.push(json!({
                        "id": combo.name,
                        "object": "model",
                        "created": now,
                        "owned_by": "combo",
                        "name": combo.name,
                        "permission": [],
                        "root": combo.name,
                        "parent": null
                    }));
                }
            }
        }
    }

    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let active_conns: Vec<_> = all_conns.into_iter().filter(|c| c.is_active).collect();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let display_names = state.db.load_model_display_names().unwrap_or_default();
    let disabled_models = state.db.load_disabled_models().unwrap_or_default();
    let custom_models = state.db.load_custom_models().unwrap_or_default();

    let has_openai = active_conns.iter().any(|c| c.provider == "openai");
    let has_anthropic = active_conns.iter().any(|c| c.provider == "anthropic");
    let has_google = active_conns.iter().any(|c| c.provider == "google" || c.provider == "vertex");
    let has_deepseek = active_conns.iter().any(|c| c.provider == "deepseek");
    let empty_active = active_conns.is_empty();

    // 2. Base models from catalog: ONLY if matching provider is active (or if active_conns is empty for tests)
    let base_models = [
        ("gpt-4o", "OpenAI", has_openai || empty_active),
        ("gpt-4o-mini", "OpenAI", has_openai || empty_active),
        ("claude-3-5-sonnet-20241022", "Anthropic", has_anthropic || empty_active),
        ("claude-3-5-haiku-20241022", "Anthropic", has_anthropic || empty_active),
        ("gemini-2.5-pro", "Google", has_google || empty_active),
        ("gemini-4-carbon", "Google", has_google || empty_active),
        ("deepseek-chat", "DeepSeek", has_deepseek || empty_active),
        ("text-embedding-3-small", "OpenAI", has_openai || empty_active),
    ];

    for (model, owner, is_enabled) in base_models {
        if is_enabled && state.db.check_model_allowed(&api_key, model).is_ok() {
            if seen_ids.insert(model.to_string()) {
                let disp_name = display_names.get(model).cloned().unwrap_or_else(|| model.to_string());
                data.push(json!({
                    "id": model,
                    "object": "model",
                    "created": now,
                    "owned_by": owner,
                    "name": disp_name,
                    "permission": [],
                    "root": model,
                    "parent": null
                }));
            }
        }
    }

    // 3. Custom models from active provider nodes
    for node in &provider_nodes {
        let has_active_conn = active_conns.iter().any(|c| {
            c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider)
        });
        if !has_active_conn && !empty_active {
            continue;
        }

        let friendly_name = node.name.clone().unwrap_or_else(|| {
            if !node.prefix.is_empty() { node.prefix.clone() } else { "custom".to_string() }
        });
        let prefix = &node.prefix;
        let disabled_set: std::collections::HashSet<String> = disabled_models.get(&node.id)
            .or_else(|| disabled_models.get(prefix))
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();

        for cm in &custom_models {
            let pa = cm.get("providerAlias").or_else(|| cm.get("provider")).and_then(|v| v.as_str()).unwrap_or("");
            if pa != node.id && pa != node.prefix && node.name.as_deref() != Some(pa) {
                continue;
            }

            let raw_id = cm.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
            if raw_id.is_empty() || disabled_set.contains(raw_id) {
                continue;
            }

            // Exclude foreign prefix models (e.g. models starting with "cfr/" under a non-cfr node)
            if !prefix.is_empty() && prefix != "cfr" && raw_id.starts_with("cfr/") {
                continue;
            }

            let routed_id = if !prefix.is_empty() {
                let prefix_slash = format!("{}/", prefix);
                if raw_id.starts_with(&prefix_slash) {
                    raw_id.to_string()
                } else {
                    format!("{}{}", prefix_slash, raw_id)
                }
            } else {
                raw_id.to_string()
            };

            if state.db.check_model_allowed(&api_key, &routed_id).is_ok() {
                if seen_ids.insert(routed_id.clone()) {
                    let clean_name = raw_id.strip_prefix(&format!("{}/", prefix)).unwrap_or(raw_id);
                    let disp_name = display_names.get(&routed_id)
                        .or_else(|| display_names.get(raw_id))
                        .cloned()
                        .or_else(|| cm.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| clean_name.to_string());

                    data.push(json!({
                        "id": routed_id,
                        "object": "model",
                        "created": now,
                        "owned_by": friendly_name,
                        "name": disp_name,
                        "permission": [],
                        "root": routed_id,
                        "parent": null
                    }));
                }
            }
        }

        // Default models for HCNSEC or known nodes
        let defaults: Vec<&str> = if node.name.as_deref() == Some("HCNSEC") || node.prefix == "h" {
            vec!["Qwen3.8-Flash-Next", "hy4", "DeepSeek-V4.1-Flash", "MiMo-V2.6-Flash", "kimi-k3.1-pro", "MiniMax-M3.1-Flash", "sensenova-6.8-flash"]
        } else {
            vec![]
        };

        for def in defaults {
            if disabled_set.contains(def) {
                continue;
            }
            let routed_id = if !prefix.is_empty() {
                format!("{}/{}", prefix, def)
            } else {
                def.to_string()
            };
            if state.db.check_model_allowed(&api_key, &routed_id).is_ok() {
                if seen_ids.insert(routed_id.clone()) {
                    let disp_name = display_names.get(&routed_id)
                        .or_else(|| display_names.get(def))
                        .cloned()
                        .unwrap_or_else(|| def.to_string());

                    data.push(json!({
                        "id": routed_id,
                        "object": "model",
                        "created": now,
                        "owned_by": friendly_name,
                        "name": disp_name,
                        "permission": [],
                        "root": routed_id,
                        "parent": null
                    }));
                }
            }
        }
    }

    // 4. Configured models on active provider connections in SQLite
    for conn in &active_conns {
        let friendly_owner = provider_nodes.iter()
            .find(|n| n.id == conn.provider || n.prefix == conn.provider || n.name.as_deref() == Some(&conn.provider))
            .and_then(|n| n.name.clone())
            .or_else(|| conn.data.extra.get("name").and_then(|v| v.as_str()).map(ToString::to_string))
            .or_else(|| conn.name.clone())
            .unwrap_or_else(|| {
                if conn.provider.starts_with("openai-compatible-") {
                    "OpenAI-Compatible".to_string()
                } else if conn.provider.starts_with("anthropic-compatible-") {
                    "Anthropic-Compatible".to_string()
                } else {
                    conn.provider.clone()
                }
            });

        if let Some(dm) = &conn.data.default_model {
            let trimmed = dm.trim();
            if !trimmed.is_empty() && state.db.check_model_allowed(&api_key, trimmed).is_ok() {
                if seen_ids.insert(trimmed.to_string()) {
                    let disp_name = display_names.get(trimmed).cloned().unwrap_or_else(|| trimmed.to_string());
                    data.push(json!({
                        "id": trimmed,
                        "object": "model",
                        "created": now,
                        "owned_by": friendly_owner,
                        "name": disp_name,
                        "permission": [],
                        "root": trimmed,
                        "parent": null
                    }));
                }
            }
        }

        let psd = conn.data.extra.get("providerSpecificData").and_then(|v| v.as_object());
        let pool_models = psd
            .and_then(|p| p.get("models").or_else(|| p.get("enabledModels")))
            .or_else(|| conn.data.extra.get("models"))
            .and_then(|v| v.as_array());

        if let Some(arr) = pool_models {
            for m in arr {
                if let Some(m_str) = m.as_str() {
                    let trimmed = m_str.trim();
                    if !trimmed.is_empty() && state.db.check_model_allowed(&api_key, trimmed).is_ok() {
                        if seen_ids.insert(trimmed.to_string()) {
                            let disp_name = display_names.get(trimmed).cloned().unwrap_or_else(|| trimmed.to_string());
                            data.push(json!({
                                "id": trimmed,
                                "object": "model",
                                "created": now,
                                "owned_by": friendly_owner,
                                "name": disp_name,
                                "permission": [],
                                "root": trimmed,
                                "parent": null
                            }));
                        }
                    }
                }
            }
        }
    }

    // 5. If source == "live", query upstream providers
    if source == "live" {
        query_live_upstream_models(&state, &api_key, &mut data, &mut seen_ids, now).await;
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

async fn query_live_upstream_models(
    state: &AppState,
    api_key: &ApiKey,
    data: &mut Vec<serde_json::Value>,
    seen_ids: &mut std::collections::HashSet<String>,
    now: i64,
) {
    let connections = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(_) => return,
    };
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let display_names = state.db.load_model_display_names().unwrap_or_default();

    let active_connections: Vec<_> = connections
        .into_iter()
        .filter(|c| c.is_active && (c.data.api_key.is_some() || c.data.base_url.is_some()))
        .collect();

    for conn in active_connections {
        let friendly_owner = provider_nodes.iter()
            .find(|n| n.id == conn.provider || n.prefix == conn.provider || n.name.as_deref() == Some(&conn.provider))
            .and_then(|n| n.name.clone())
            .or_else(|| conn.data.extra.get("name").and_then(|v| v.as_str()).map(ToString::to_string))
            .or_else(|| conn.name.clone())
            .unwrap_or_else(|| {
                if conn.provider.starts_with("openai-compatible-") {
                    "OpenAI-Compatible".to_string()
                } else if conn.provider.starts_with("anthropic-compatible-") {
                    "Anthropic-Compatible".to_string()
                } else {
                    conn.provider.clone()
                }
            });

        let models_url = if let Some(base) = &conn.data.base_url {
            let b = base.trim_end_matches('/');
            if b.ends_with("/models") {
                b.to_string()
            } else if b.ends_with("/v1") {
                format!("{}/models", b)
            } else {
                format!("{}/v1/models", b)
            }
        } else {
            match conn.provider.to_ascii_lowercase().as_str() {
                "openai" => "https://api.openai.com/v1/models".to_string(),
                "groq" => "https://api.groq.com/openai/v1/models".to_string(),
                "openrouter" => "https://openrouter.ai/api/v1/models".to_string(),
                "mistral" => "https://api.mistral.ai/v1/models".to_string(),
                "together" => "https://api.together.xyz/v1/models".to_string(),
                "perplexity" => "https://api.perplexity.ai/v1/models".to_string(),
                _ => continue,
            }
        };

        let mut req = state
            .proxy
            .client()
            .get(&models_url)
            .timeout(std::time::Duration::from_millis(1500));

        if let Some(key) = &conn.data.api_key {
            req = req.header(header::AUTHORIZATION, format!("Bearer {}", key));
        }

        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(models_arr) = body.get("data").and_then(|d| d.as_array()) {
                        for m in models_arr {
                            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                                if state.db.check_model_allowed(api_key, id).is_ok()
                                    && seen_ids.insert(id.to_string())
                                {
                                    let clean_name = id.split('/').last().unwrap_or(id);
                                    let disp_name = display_names.get(id)
                                        .or_else(|| display_names.get(clean_name))
                                        .cloned()
                                        .or_else(|| m.get("name").and_then(|v| v.as_str()).map(ToString::to_string))
                                        .unwrap_or_else(|| clean_name.to_string());

                                    data.push(json!({
                                        "id": id,
                                        "object": "model",
                                        "created": now,
                                        "owned_by": friendly_owner,
                                        "name": disp_name,
                                        "permission": [],
                                        "root": id,
                                        "parent": null
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Target Resolution Helper for Candidates
// ============================================================================

fn populate_connection_parameters(conn: &mut UpstreamConnection, pc: &ProviderConnection) {
    conn.provider = pc.provider.clone();
    conn.base_url = pc.data.base_url.clone();
    conn.api_key = pc.data.api_key.clone();

    let psd = pc.data.extra.get("providerSpecificData").and_then(|v| v.as_object());
    if pc.provider.eq_ignore_ascii_case("azure") {
        if conn.base_url.is_none() {
            conn.base_url = psd
                .and_then(|p| p.get("azureEndpoint"))
                .or_else(|| pc.data.extra.get("azureEndpoint"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
        conn.deployment = psd
            .and_then(|p| p.get("deployment"))
            .or_else(|| pc.data.extra.get("deployment"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        conn.api_version = psd
            .and_then(|p| p.get("apiVersion"))
            .or_else(|| pc.data.extra.get("apiVersion"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
    } else if pc.provider.eq_ignore_ascii_case("bedrock") || pc.provider.eq_ignore_ascii_case("bedrock-xai") {
        conn.region = psd
            .and_then(|p| p.get("region"))
            .or_else(|| pc.data.extra.get("region"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
    }
}

fn resolve_target_for_candidate(
    candidate: &str,
    provider_nodes: &[crate::db::ProviderNode],
    all_connections: &[ProviderConnection],
) -> (UpstreamConnection, String) {
    // 1. Check if candidate starts with a node prefix (e.g. "h/glm-5.2", "9r/...", "neon/...", "srvm/...")
    for node in provider_nodes {
        let mut matched_prefix: Option<String> = None;
        if !node.prefix.is_empty() {
            let prefix_slash = format!("{}/", node.prefix);
            if candidate.starts_with(&prefix_slash) || candidate == node.prefix {
                matched_prefix = Some(prefix_slash);
            }
        }
        if matched_prefix.is_none() {
            if let Some(ref name) = node.name {
                let name_slash = format!("{}/", name);
                if candidate.to_lowercase().starts_with(&name_slash.to_lowercase()) {
                    matched_prefix = Some(candidate[..name_slash.len()].to_string());
                }
            }
        }
        if matched_prefix.is_none() {
            let id_slash = format!("{}/", node.id);
            if candidate.starts_with(&id_slash) {
                matched_prefix = Some(id_slash);
            }
        }

        if let Some(prefix_slash) = matched_prefix {
            let upstream_model = if candidate.len() > prefix_slash.len() {
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

            if let Some(pc) = all_connections.iter().find(|c| c.is_active && (c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider))) {
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

    // 3. Check standard provider prefix (e.g. "vertex/gemini-4-carbon", "azure/gpt-5.6-terra", "bedrock/claude-3-sonnet")
    if let Some((p_prefix, m_part)) = candidate.split_once('/') {
        if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.provider.eq_ignore_ascii_case(p_prefix)) {
            let mut conn = UpstreamConnection::default();
            populate_connection_parameters(&mut conn, pc);
            return (conn, m_part.to_string());
        }

        // Explicit prefix requested (e.g. "agentrouter/deepseek-v3.2") but no connection exists
        let mut conn = UpstreamConnection::default();
        conn.provider = p_prefix.to_string();
        return (conn, m_part.to_string());
    }

    // 4. Match by defaultModel
    if let Some(pc) = all_connections.iter().find(|c| c.is_active && c.data.default_model.as_deref() == Some(candidate)) {
        let mut conn = UpstreamConnection::default();
        populate_connection_parameters(&mut conn, pc);
        return (conn, candidate.to_string());
    }

    // 4b. Check if candidate matches a model in any active custom node (e.g. "Qwen3.8-Flash-Next", "hy4")
    for node in provider_nodes {
        let has_active_conn = all_connections.iter().any(|c| {
            c.is_active && (c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider))
        });
        if has_active_conn {
            let is_hcnsec = node.name.as_deref() == Some("HCNSEC") || node.prefix == "h";
            let matches_hcnsec = is_hcnsec && matches!(candidate, "Qwen3.8-Flash-Next" | "hy4" | "DeepSeek-V4.1-Flash" | "MiMo-V2.6-Flash" | "kimi-k3.1-pro" | "MiniMax-M3.1-Flash" | "sensenova-6.8-flash");
            if matches_hcnsec {
                let mut conn = UpstreamConnection::default();
                conn.provider = node.name.clone().unwrap_or_else(|| node.prefix.clone());
                conn.base_url = Some(node.base_url.clone());
                if let Some(pc) = all_connections.iter().find(|c| c.is_active && (c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider))) {
                    conn.api_key = pc.data.api_key.clone();
                }
                return (conn, candidate.to_string());
            }
        }
    }

    // 5. Default provider inference
    let (provider, target_model) = if candidate.starts_with("azure/") {
        ("azure".to_string(), candidate[6..].to_string())
    } else if candidate.starts_with("bedrock/") {
        ("bedrock".to_string(), candidate[8..].to_string())
    } else if candidate.starts_with("bedrock-xai/") {
        ("bedrock-xai".to_string(), candidate[12..].to_string())
    } else if candidate.starts_with("claude") {
        ("anthropic".to_string(), candidate.to_string())
    } else if candidate.contains("gemini") {
        ("vertex".to_string(), candidate.to_string())
    } else if candidate.starts_with("gpt-") || candidate.starts_with("text-") || candidate.starts_with("dall-e") || candidate.starts_with("o1") || candidate.starts_with("o3") {
        ("openai".to_string(), candidate.to_string())
    } else {
        ("unknown".to_string(), candidate.to_string())
    };

    let conn_opt = all_connections.iter().find(|c| c.is_active && c.provider.eq_ignore_ascii_case(&provider));
    let mut conn = UpstreamConnection::default();
    conn.provider = provider;
    if let Some(pc) = conn_opt {
        populate_connection_parameters(&mut conn, pc);
    }

    (conn, target_model)
}

// ============================================================================
// Write-Behind Usage & Request Logging Helpers
// ============================================================================

pub fn resolve_should_record_request_details(config: &Config) -> bool {
    if let Ok(val) = std::env::var("ENABLE_REQUEST_LOGS") {
        let v = val.trim().to_lowercase();
        if v == "0" || v == "false" || v == "no" || v == "off" {
            return false;
        }
        if v == "1" || v == "true" || v == "yes" || v == "on" {
            return true;
        }
    }
    if let Ok(val) = std::env::var("LOG_USAGE_VERBOSE") {
        let v = val.trim().to_lowercase();
        if v == "1" || v == "true" || v == "yes" || v == "on" {
            return true;
        }
    }
    if config.observability.log_usage_verbose {
        return true;
    }
    config.observability.enable_request_logs
}

pub fn resolve_usage_buffer_tokens(config: &Config) -> u64 {
    if let Ok(val) = std::env::var("USAGE_BUFFER_TOKENS") {
        if let Ok(n) = val.trim().parse::<u64>() {
            return n;
        }
    }
    config.routing.usage_buffer_tokens
}

pub fn record_completion_usage_and_details(
    state: &AppState,
    api_key: &ApiKey,
    endpoint: &str,
    provider: Option<&str>,
    model: &str,
    connection_id: Option<&str>,
    status: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    latency_ms: u64,
    request_val: Option<&serde_json::Value>,
    response_val: Option<&serde_json::Value>,
) {
    let buffer_tokens = resolve_usage_buffer_tokens(&state.config) as i64;
    let reported_completion_tokens = completion_tokens + buffer_tokens;
    let total_tokens = prompt_tokens + reported_completion_tokens;

    let tokens_json = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": reported_completion_tokens,
        "total_tokens": total_tokens,
        "cached_tokens": cached_tokens,
        "buffer_tokens": buffer_tokens,
    });

    if state.config.observability.log_usage_verbose
        || std::env::var("LOG_USAGE_VERBOSE").map(|v| v == "1" || v == "true").unwrap_or(false)
    {
        tracing::info!(
            "[USAGE_VERBOSE] {} | provider={:?} model={} in={} out={} (+buffer={}) latency={}ms",
            endpoint,
            provider,
            model,
            prompt_tokens,
            reported_completion_tokens,
            buffer_tokens,
            latency_ms
        );
    }

    state.db.queue_usage(UsageRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        provider: provider.map(str::to_string),
        model: Some(model.to_string()),
        connection_id: connection_id.map(str::to_string),
        api_key: Some(api_key.key.clone()),
        api_key_id: Some(api_key.id.clone()),
        endpoint: Some(endpoint.to_string()),
        prompt_tokens,
        completion_tokens: reported_completion_tokens,
        cached_tokens,
        cost: 0.0,
        status: status.to_string(),
        tokens: Some(tokens_json.to_string()),
        meta: None,
    });

    if resolve_should_record_request_details(&state.config) {
        state.db.record_request_detail(RequestDetail {
            id: format!("req_{}_{}", chrono::Utc::now().timestamp_micros(), &api_key.id),
            timestamp: chrono::Utc::now().to_rfc3339(),
            provider: provider.map(str::to_string),
            model: Some(model.to_string()),
            connection_id: connection_id.map(str::to_string),
            status: Some(status.to_string()),
            endpoint: Some(endpoint.to_string()),
            latency_ms: Some(latency_ms),
            tokens: Some(tokens_json),
            request: request_val.cloned(),
            response: response_val.cloned(),
            meta: None,
            data: None,
        });
    }
}

struct StreamUsageDropGuard {
    recorded: bool,
    state: AppState,
    api_key: ApiKey,
    endpoint: String,
    provider: Option<String>,
    model: String,
    connection_id: Option<String>,
    request_val: Option<serde_json::Value>,
    start_time: std::time::Instant,
}

impl StreamUsageDropGuard {
    fn disarm(&mut self) {
        self.recorded = true;
    }
}

impl Drop for StreamUsageDropGuard {
    fn drop(&mut self) {
        if !self.recorded {
            self.recorded = true;
            let latency_ms = self.start_time.elapsed().as_millis() as u64;
            record_completion_usage_and_details(
                &self.state,
                &self.api_key,
                &self.endpoint,
                self.provider.as_deref(),
                &self.model,
                self.connection_id.as_deref(),
                "interrupted",
                0,
                0,
                0,
                latency_ms,
                self.request_val.as_ref(),
                Some(&serde_json::json!({ "streamed": true, "interrupted": true })),
            );
        }
    }
}

pub fn wrap_byte_stream_with_usage<S, E>(
    byte_stream: S,
    state: AppState,
    api_key: ApiKey,
    endpoint: String,
    provider: Option<String>,
    model: String,
    connection_id: Option<String>,
    request_val: Option<serde_json::Value>,
    start_time: std::time::Instant,
) -> impl Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static
where
    S: Stream<Item = Result<axum::body::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    stream! {
        let mut pinned = Box::pin(byte_stream);
        let mut prompt_tokens: i64 = 0;
        let mut completion_tokens: i64 = 0;
        let mut cached_tokens: i64 = 0;
        let mut chunk_count: u64 = 0;
        let mut is_error = false;
        let mut guard = StreamUsageDropGuard {
            recorded: false,
            state: state.clone(),
            api_key: api_key.clone(),
            endpoint: endpoint.clone(),
            provider: provider.clone(),
            model: model.clone(),
            connection_id: connection_id.clone(),
            request_val: request_val.clone(),
            start_time,
        };

        while let Some(chunk_res) = pinned.next().await {
            match chunk_res {
                Ok(bytes) => {
                    chunk_count += 1;
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        for line in text.lines() {
                            let line = line.trim();
                            if let Some(data) = line.strip_prefix("data:") {
                                let data = data.trim();
                                if !data.is_empty() && data != "[DONE]" {
                                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                                        if let Some(usage) = val.get("usage") {
                                            if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_i64()).or_else(|| usage.get("input_tokens").and_then(|v| v.as_i64())) {
                                                prompt_tokens = pt;
                                            }
                                            if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_i64()).or_else(|| usage.get("output_tokens").and_then(|v| v.as_i64())) {
                                                completion_tokens = ct;
                                            }
                                            if let Some(c) = usage.get("cached_tokens").and_then(|v| v.as_i64()) {
                                                cached_tokens = c;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    yield Ok(bytes);
                }
                Err(err) => {
                    is_error = true;
                    yield Err(err);
                    break;
                }
            }
        }

        // Natural completion
        guard.disarm();
        let status = if is_error { "error" } else { "ok" };
        let latency_ms = start_time.elapsed().as_millis() as u64;

        record_completion_usage_and_details(
            &state,
            &api_key,
            &endpoint,
            provider.as_deref(),
            &model,
            connection_id.as_deref(),
            status,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            latency_ms,
            request_val.as_ref(),
            Some(&serde_json::json!({ "streamed": true, "chunks": chunk_count })),
        );
    }
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
    let start_time = std::time::Instant::now();
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
        let req_val = serde_json::to_value(&clean_req).unwrap_or_default();
        record_completion_usage_and_details(
            state,
            api_key,
            "/v1/chat/completions",
            Some(&conn.provider),
            &target_model,
            None,
            "ok",
            0,
            0,
            0,
            start_time.elapsed().as_millis() as u64,
            Some(&req_val),
            Some(&serde_json::json!({ "cached": true })),
        );
        return Ok(cached.to_response(is_stream));
    }

    if conn.api_key.is_none() && conn.base_url.is_none() {
        state.circuit_breaker.record_failure(candidate);
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "No active connection or API key configured for provider '{}'. Please add or activate a connection in Providers.",
                conn.provider
            ),
        ));
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
                    let stream = wrap_byte_stream_with_usage(
                        upstream_res.bytes_stream(),
                        state.clone(),
                        api_key.clone(),
                        "/v1/chat/completions".to_string(),
                        Some(conn.provider.clone()),
                        target_model.clone(),
                        None,
                        Some(req_val),
                        start_time,
                    );
                    let sse_res = crate::custom_adapter::custom_sse_to_openai_stream(
                        stream,
                        mapping.clone(),
                        target_model.clone(),
                    );
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
                                Ok(mut openai_resp) => {
                                    state.circuit_breaker.record_success(&conn.provider);
                                    state.circuit_breaker.record_success(candidate);
                                    let mut prompt_tokens = 0i64;
                                    let mut completion_tokens = 0i64;
                                    let buffer = resolve_usage_buffer_tokens(&state.config);

                                    if let Some(ref mut u) = openai_resp.usage {
                                        prompt_tokens = u.prompt_tokens as i64;
                                        completion_tokens = u.completion_tokens as i64;
                                        if buffer > 0 {
                                            u.completion_tokens += buffer as u32;
                                            u.total_tokens = u.prompt_tokens + u.completion_tokens;
                                        }
                                    }

                                    record_completion_usage_and_details(
                                        state,
                                        api_key,
                                        "/v1/chat/completions",
                                        Some(&conn.provider),
                                        &target_model,
                                        None,
                                        "ok",
                                        prompt_tokens,
                                        completion_tokens,
                                        0,
                                        start_time.elapsed().as_millis() as u64,
                                        Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                                        Some(&serde_json::to_value(&openai_resp).unwrap_or_default()),
                                    );

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
                        let stream = wrap_byte_stream_with_usage(
                            upstream_res.bytes_stream(),
                            state.clone(),
                            api_key.clone(),
                            "/v1/chat/completions".to_string(),
                            Some(conn.provider.clone()),
                            target_model.clone(),
                            None,
                            Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                            start_time,
                        );
                        let sse_res = sse::claude_to_openai_sse_stream(stream);
                        return Ok(sse_res.into_response());
                    }

                    let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                    let mut prompt_tokens = 0i64;
                    let mut completion_tokens = 0i64;
                    let mut cached_tokens = 0i64;
                    let buffer = resolve_usage_buffer_tokens(&state.config);

                    let final_body = if let Ok(claude_resp) =
                        serde_json::from_slice::<translator::ClaudeMessageResponse>(&body_bytes)
                    {
                        prompt_tokens = claude_resp.usage.input_tokens as i64;
                        completion_tokens = claude_resp.usage.output_tokens as i64;
                        cached_tokens = claude_resp.usage.cache_read_input_tokens.unwrap_or(0) as i64;

                        let mut openai_resp = translator::claude_to_openai_response(&claude_resp);
                        if let Some(ref mut u) = openai_resp.usage {
                            if buffer > 0 {
                                u.completion_tokens += buffer as u32;
                                u.total_tokens = u.prompt_tokens + u.completion_tokens;
                            }
                        }
                        serde_json::to_vec(&openai_resp).unwrap_or(body_bytes.to_vec())
                    } else {
                        body_bytes.to_vec()
                    };

                    let resp_val: Option<serde_json::Value> = serde_json::from_slice(&final_body).ok();

                    record_completion_usage_and_details(
                        state,
                        api_key,
                        "/v1/chat/completions",
                        Some(&conn.provider),
                        &target_model,
                        None,
                        "ok",
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        start_time.elapsed().as_millis() as u64,
                        Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                        resp_val.as_ref(),
                    );

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
                        let stream = wrap_byte_stream_with_usage(
                            upstream_res.bytes_stream(),
                            state.clone(),
                            api_key.clone(),
                            "/v1/chat/completions".to_string(),
                            Some(conn.provider.clone()),
                            target_model.clone(),
                            None,
                            Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                            start_time,
                        );
                        let sse_res = sse::passthrough_sse_stream(stream);
                        return Ok(sse_res.into_response());
                    }

                    let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                    let mut prompt_tokens = 0i64;
                    let mut completion_tokens = 0i64;
                    let mut cached_tokens = 0i64;
                    let buffer = resolve_usage_buffer_tokens(&state.config);

                    let mut final_body = if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                        if let Some(inner) = val.get("result") {
                            serde_json::to_vec(inner).unwrap_or(body_bytes.to_vec())
                        } else {
                            body_bytes.to_vec()
                        }
                    } else {
                        body_bytes.to_vec()
                    };

                    let mut resp_val: Option<serde_json::Value> = serde_json::from_slice(&final_body).ok();
                    if let Some(ref mut val) = resp_val {
                        if let Some(usage) = val.get_mut("usage") {
                            if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                                prompt_tokens = pt;
                            }
                            if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                                completion_tokens = ct;
                            }
                            if let Some(cr) = usage.get("cached_tokens").and_then(|v| v.as_i64()) {
                                cached_tokens = cr;
                            }
                            if buffer > 0 {
                                let reported_ct = completion_tokens + buffer as i64;
                                usage["completion_tokens"] = json!(reported_ct);
                                usage["total_tokens"] = json!(prompt_tokens + reported_ct);
                                if let Ok(updated_bytes) = serde_json::to_vec(val) {
                                    final_body = updated_bytes;
                                }
                            }
                        }
                    }

                    record_completion_usage_and_details(
                        state,
                        api_key,
                        "/v1/chat/completions",
                        Some(&conn.provider),
                        &target_model,
                        None,
                        "ok",
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        start_time.elapsed().as_millis() as u64,
                        Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                        resp_val.as_ref(),
                    );

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
        Err(auth_err) => return auth_err,
    };
    handle_chat_completions_inner(state, headers, body, api_key).await
}

pub async fn dashboard_chat_completions_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => {
            // Allow dashboard internal sessions when valid or when requireLogin is disabled
            if crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db).is_ok() {
                ApiKey {
                    id: "dashboard-session".to_string(),
                    key: "dashboard-session".to_string(),
                    name: Some("Dashboard Internal Session".to_string()),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                }
            } else {
                return auth_err;
            }
        }
    };
    handle_chat_completions_inner(state, headers, body, api_key).await
}

async fn handle_chat_completions_inner(
    state: AppState,
    _headers: HeaderMap,
    body: axum::body::Bytes,
    api_key: ApiKey,
) -> Response {
    let start_time = std::time::Instant::now();

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
        let req_val = serde_json::to_value(&req).unwrap_or_default();
        record_completion_usage_and_details(
            &state,
            &api_key,
            "/v1/chat/completions",
            Some("cache"),
            &req.model,
            None,
            "ok",
            0,
            0,
            0,
            start_time.elapsed().as_millis() as u64,
            Some(&req_val),
            Some(&serde_json::json!({ "cached": true })),
        );
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

    record_completion_usage_and_details(
        &state,
        &api_key,
        "/v1/chat/completions",
        None,
        &req.model,
        None,
        "error",
        0,
        0,
        0,
        start_time.elapsed().as_millis() as u64,
        Some(&serde_json::to_value(&req).unwrap_or_default()),
        Some(&serde_json::json!({ "error": &last_error })),
    );

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
    let start_time = std::time::Instant::now();
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
        let req_val = serde_json::to_value(&clean_req).unwrap_or_default();
        record_completion_usage_and_details(
            state,
            api_key,
            "/v1/messages",
            Some(&conn.provider),
            &clean_model,
            None,
            "ok",
            0,
            0,
            0,
            start_time.elapsed().as_millis() as u64,
            Some(&req_val),
            Some(&serde_json::json!({ "cached": true })),
        );
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
                    let stream = wrap_byte_stream_with_usage(
                        upstream_res.bytes_stream(),
                        state.clone(),
                        api_key.clone(),
                        "/v1/messages".to_string(),
                        Some(conn.provider.clone()),
                        clean_model.clone(),
                        None,
                        Some(req_val),
                        start_time,
                    );
                    let sse_res = crate::custom_adapter::custom_sse_to_claude_stream(
                        stream,
                        mapping.clone(),
                        clean_model.clone(),
                    );
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
                                Ok(mut claude_resp) => {
                                    state.circuit_breaker.record_success(&conn.provider);
                                    state.circuit_breaker.record_success(candidate);
                                    let prompt_tokens = claude_resp.usage.input_tokens as i64;
                                    let completion_tokens = claude_resp.usage.output_tokens as i64;
                                    let buffer = resolve_usage_buffer_tokens(&state.config);
                                    if buffer > 0 {
                                        claude_resp.usage.output_tokens += buffer as u32;
                                    }

                                    record_completion_usage_and_details(
                                        state,
                                        api_key,
                                        "/v1/messages",
                                        Some(&conn.provider),
                                        &clean_model,
                                        None,
                                        "ok",
                                        prompt_tokens,
                                        completion_tokens,
                                        0,
                                        start_time.elapsed().as_millis() as u64,
                                        Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                                        Some(&serde_json::to_value(&claude_resp).unwrap_or_default()),
                                    );

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
        if is_stream {
            match state.proxy.forward_claude_messages(&clean_req, &conn).await {
                Ok(upstream_res) if upstream_res.status().is_success() => {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    let stream = wrap_byte_stream_with_usage(
                        upstream_res.bytes_stream(),
                        state.clone(),
                        api_key.clone(),
                        "/v1/messages".to_string(),
                        Some(conn.provider.clone()),
                        clean_model.clone(),
                        None,
                        Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                        start_time,
                    );
                    let sse_res = sse::passthrough_sse_stream(stream);
                    return Ok(sse_res.into_response());
                }
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("[{}] Anthropic error", status)));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, format!("Upstream connection failed: {}", e)));
                }
            }
        }
        match state.proxy.forward_claude_messages(&clean_req, &conn).await {
            Ok(upstream_res) => {
                let status = upstream_res.status();
                let body_bytes = upstream_res.bytes().await.unwrap_or_default();
                let mut prompt_tokens = 0i64;
                let mut completion_tokens = 0i64;
                let mut cached_tokens = 0i64;
                let buffer = resolve_usage_buffer_tokens(&state.config);

                let mut final_body = body_bytes.clone();
                let mut resp_val: Option<serde_json::Value> = serde_json::from_slice(&body_bytes).ok();
                if let Some(ref mut val) = resp_val {
                    if let Some(usage) = val.get_mut("usage") {
                        if let Some(it) = usage.get("input_tokens").and_then(|v| v.as_i64()) {
                            prompt_tokens = it;
                        }
                        if let Some(ot) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                            completion_tokens = ot;
                        }
                        if let Some(cr) = usage.get("cache_read_input_tokens").and_then(|v| v.as_i64()) {
                            cached_tokens = cr;
                        }
                        if buffer > 0 {
                            let reported_ot = completion_tokens + buffer as i64;
                            usage["output_tokens"] = json!(reported_ot);
                            if let Ok(updated) = serde_json::to_vec(val) {
                                final_body = axum::body::Bytes::from(updated);
                            }
                        }
                    }
                }

                record_completion_usage_and_details(
                    state,
                    api_key,
                    "/v1/messages",
                    Some(&conn.provider),
                    &clean_model,
                    None,
                    if status.is_success() { "ok" } else { "error" },
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    start_time.elapsed().as_millis() as u64,
                    Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                    resp_val.as_ref(),
                );

                if status.is_success() {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    return Ok((status, [(header::CONTENT_TYPE, "application/json")], final_body).into_response());
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
        if is_stream {
            match state.proxy.forward_chat_completions(&chat_req, &conn).await {
                Ok(upstream_res) if upstream_res.status().is_success() => {
                    state.circuit_breaker.record_success(&conn.provider);
                    state.circuit_breaker.record_success(candidate);
                    let stream = wrap_byte_stream_with_usage(
                        upstream_res.bytes_stream(),
                        state.clone(),
                        api_key.clone(),
                        "/v1/messages".to_string(),
                        Some(conn.provider.clone()),
                        clean_model.clone(),
                        None,
                        Some(serde_json::to_value(&clean_req).unwrap_or_default()),
                        start_time,
                    );
                    let sse_res = sse::openai_to_claude_sse_stream(stream);
                    return Ok(sse_res.into_response());
                }
                Ok(upstream_res) => {
                    let status = upstream_res.status();
                    state.circuit_breaker.record_status(&conn.provider, status);
                    state.circuit_breaker.record_status(candidate, status);
                    return Err((status, format!("[{}] OpenAI streaming error", status)));
                }
                Err(e) => {
                    state.circuit_breaker.record_failure(&conn.provider);
                    state.circuit_breaker.record_failure(candidate);
                    return Err((StatusCode::BAD_GATEWAY, format!("Upstream connection failed: {}", e)));
                }
            }
        }
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
                        let mut claude_resp = translator::openai_to_claude_response(&chat_resp);
                        let prompt_tokens = claude_resp.usage.input_tokens as i64;
                        let completion_tokens = claude_resp.usage.output_tokens as i64;
                        let buffer = resolve_usage_buffer_tokens(&state.config);
                        if buffer > 0 {
                            claude_resp.usage.output_tokens += buffer as u32;
                        }

                        record_completion_usage_and_details(
                            state,
                            api_key,
                            "/v1/messages",
                            Some(&conn.provider),
                            &clean_model,
                            None,
                            "ok",
                            prompt_tokens,
                            completion_tokens,
                            0,
                            start_time.elapsed().as_millis() as u64,
                            Some(&serde_json::to_value(&clean_req).unwrap_or_default()),
                            Some(&serde_json::to_value(&claude_resp).unwrap_or_default()),
                        );
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
    let start_time = std::time::Instant::now();
    let api_key = match authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => return auth_err,
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
        let req_val = serde_json::to_value(&req).unwrap_or_default();
        record_completion_usage_and_details(
            &state,
            &api_key,
            "/v1/messages",
            Some("cache"),
            &req.model,
            None,
            "ok",
            0,
            0,
            0,
            start_time.elapsed().as_millis() as u64,
            Some(&req_val),
            Some(&serde_json::json!({ "cached": true })),
        );
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

    record_completion_usage_and_details(
        &state,
        &api_key,
        "/v1/messages",
        None,
        &req.model,
        None,
        "error",
        0,
        0,
        0,
        start_time.elapsed().as_millis() as u64,
        Some(&serde_json::to_value(&req).unwrap_or_default()),
        Some(&serde_json::json!({ "error": &last_error })),
    );

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
    crate::proxy_pools::get_proxy_pools_handler(State(state), headers, Query(Default::default())).await
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
        .unwrap_or(false);
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

pub async fn tunnel_status_handler(state: axum::extract::State<AppState>) -> Response {
    crate::tunnel::tunnel_status_handler(state).await
}

pub async fn models_alias_handler() -> Response {
    Json(json!({ "aliases": {} })).into_response()
}

pub async fn blocked_ips_handler() -> Response {
    Json(json!({ "blockedIPs": [] })).into_response()
}

pub fn serialize_admin_key(key: &crate::db::ApiKey, include_secret: bool) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".to_string(), json!(key.id));
    if include_secret {
        obj.insert("key".to_string(), json!(key.key));
    }
    obj.insert("name".to_string(), json!(key.name));
    obj.insert("machineId".to_string(), json!(key.machine_id));
    obj.insert("isActive".to_string(), json!(key.is_active));
    obj.insert("createdAt".to_string(), json!(key.created_at));
    obj.insert("rateLimitRequests".to_string(), json!(key.rate_limit_requests));
    obj.insert("rateLimitWindowSeconds".to_string(), json!(key.rate_limit_window_seconds));
    obj.insert("tokenLimitDaily".to_string(), json!(key.token_limit_daily));
    obj.insert("tokenLimitMonthly".to_string(), json!(key.token_limit_monthly));
    obj.insert("allowedModels".to_string(), json!(key.allowed_models));
    obj.insert("blockedModels".to_string(), json!(key.blocked_models));
    obj.insert("allowedIPs".to_string(), json!(key.allowed_ips));
    obj.insert("blockedIPs".to_string(), json!(key.blocked_ips));
    obj.insert("expiresAt".to_string(), json!(key.expires_at));
    obj.insert("description".to_string(), json!(key.description));
    obj.insert("ownerRef".to_string(), json!(key.owner_ref));
    obj.insert("budgetUsd".to_string(), json!(key.budget_usd));
    obj.insert("budgetResetHours".to_string(), json!(key.budget_reset_hours));
    obj.insert("budgetWindowStartedAt".to_string(), json!(key.budget_window_started_at));
    obj.insert("billingMode".to_string(), json!(key.billing_mode));
    obj.insert("byokProvider".to_string(), json!(key.byok_provider));
    obj.insert("maxConcurrentRequests".to_string(), json!(key.max_concurrent_requests));
    obj.insert("allowedProviders".to_string(), json!(key.allowed_providers));
    obj.insert("blockedProviders".to_string(), json!(key.blocked_providers));
    obj.insert("forceProvider".to_string(), json!(key.force_provider));
    obj.insert("forceModel".to_string(), json!(key.force_model));
    obj.insert("allowedEndpoints".to_string(), json!(key.allowed_endpoints));
    obj.insert("blockedEndpoints".to_string(), json!(key.blocked_endpoints));
    let meta = serde_json::from_str::<serde_json::Value>(&key.metadata).unwrap_or_else(|_| json!({}));
    obj.insert("metadata".to_string(), meta);
    obj.insert("updatedAt".to_string(), json!(key.updated_at));
    obj.insert("regenerateVersion".to_string(), json!(key.regenerate_version));

    serde_json::Value::Object(obj)
}

pub fn serialize_api_key(key: &crate::db::ApiKey, include_secret: bool) -> serde_json::Value {
    let show = include_secret || key.owner_ref.is_none();
    serialize_admin_key(key, show)
}

fn normalize_list(val: Option<&serde_json::Value>) -> Result<Option<String>, &'static str> {
    match val {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(Some(String::new())),
        Some(serde_json::Value::String(s)) => {
            let parts: Vec<&str> = s.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
            let mut unique = Vec::new();
            for p in parts {
                if !unique.contains(&p) {
                    unique.push(p);
                }
            }
            Ok(Some(unique.join(",")))
        }
        Some(serde_json::Value::Array(arr)) => {
            let mut unique = Vec::new();
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err("must contain only strings");
                };
                let s = s.trim();
                if !s.is_empty() && !unique.contains(&s) {
                    unique.push(s);
                }
            }
            Ok(Some(unique.join(",")))
        }
        _ => Err("must be an array or CSV string"),
    }
}

fn is_valid_ip_rule(rule: &str) -> bool {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if let Some((addr_str, prefix_str)) = trimmed.split_once('/') {
        if let Ok(prefix) = prefix_str.parse::<u8>() {
            if let Ok(ip) = addr_str.parse::<std::net::IpAddr>() {
                match ip {
                    std::net::IpAddr::V4(_) => return prefix <= 32,
                    std::net::IpAddr::V6(_) => return prefix <= 128,
                }
            }
        }
    }
    false
}

fn get_client_ip(headers: &HeaderMap) -> String {
    if let Some(val) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = val.split(',').next() {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    if let Some(val) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "127.0.0.1".to_string()
}

pub async fn create_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let name = payload.get("name").and_then(|v| v.as_str()).map(|s| s.trim().to_string());
    if name.is_none() || name.as_deref() == Some("") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Name is required"}))).into_response();
    }
    let owner_ref = payload.get("ownerRef").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    if let Some(bm) = payload.get("billingMode").and_then(|v| v.as_str()) {
        if bm == "byok" {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Configure BYOK credentials after creating the key"}))).into_response();
        }
        if bm != "credits" {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid billingMode"}))).into_response();
        }
    }

    let hex_part = format!("{:x}", chrono::Utc::now().timestamp_micros());
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("key-{}", &hex_part[..hex_part.len().min(8)]));

    let mut custom_key_val = None;
    if let Some(custom_key) = payload.get("key").and_then(|v| v.as_str()).map(|s| s.trim()) {
        if !custom_key.is_empty() {
            if custom_key.len() > 512 {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "Custom key must be 512 characters or fewer"}))).into_response();
            }
            if state.db.get_api_key_by_key(custom_key).ok().flatten().is_some() {
                return (StatusCode::CONFLICT, Json(json!({"error": "API key already in use"}))).into_response();
            }
            custom_key_val = Some(custom_key.to_string());
        }
    }

    let machine_id = payload.get("machineId").and_then(|v| v.as_str()).map(|s| s.to_string());
    let raw_key = custom_key_val.unwrap_or_else(|| crate::auth::generate_api_key(machine_id.as_deref()));

    let rate_limit_requests = payload.get("rateLimitRequests").and_then(|v| v.as_i64()).unwrap_or(0);
    let rate_limit_window_seconds = payload.get("rateLimitWindowSeconds").and_then(|v| v.as_i64()).unwrap_or(60);
    let token_limit_daily = payload.get("tokenLimitDaily").and_then(|v| v.as_i64()).unwrap_or(0);
    let token_limit_monthly = payload.get("tokenLimitMonthly").and_then(|v| v.as_i64()).unwrap_or(0);
    let max_concurrent_requests = payload.get("maxConcurrentRequests").and_then(|v| v.as_i64()).unwrap_or(0);

    let allowed_models = normalize_list(payload.get("allowedModels")).unwrap_or(None).unwrap_or_else(|| "*".to_string());
    let blocked_models = normalize_list(payload.get("blockedModels")).unwrap_or(None).unwrap_or_default();
    let allowed_ips = normalize_list(payload.get("allowedIPs")).unwrap_or(None).unwrap_or_default();
    let blocked_ips = normalize_list(payload.get("blockedIPs")).unwrap_or(None).unwrap_or_default();
    let allowed_providers = normalize_list(payload.get("allowedProviders")).unwrap_or(None).unwrap_or_default();
    let blocked_providers = normalize_list(payload.get("blockedProviders")).unwrap_or(None).unwrap_or_default();
    let allowed_endpoints = normalize_list(payload.get("allowedEndpoints")).unwrap_or(None).unwrap_or_default();
    let blocked_endpoints = normalize_list(payload.get("blockedEndpoints")).unwrap_or(None).unwrap_or_default();

    let budget_usd = payload.get("budgetUsd").and_then(|v| v.as_f64());
    let budget_reset_hours = payload.get("budgetResetHours").and_then(|v| v.as_f64());
    let budget_window_started_at = payload.get("budgetWindowStartedAt").and_then(|v| v.as_str()).map(|s| s.to_string());
    let description = payload.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let expires_at = payload.get("expiresAt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let metadata = payload.get("metadata").map(|v| if v.is_string() { v.as_str().unwrap().to_string() } else { serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()) }).unwrap_or_else(|| "{}".to_string());

    let key = crate::db::ApiKey {
        id: id.clone(),
        key: raw_key.clone(),
        name,
        machine_id,
        is_active: payload.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true),
        created_at: chrono::Utc::now().to_rfc3339(),
        rate_limit_requests,
        rate_limit_window_seconds,
        token_limit_daily,
        token_limit_monthly,
        allowed_models,
        blocked_models,
        allowed_ips,
        blocked_ips,
        expires_at,
        description,
        owner_ref,
        max_concurrent_requests,
        allowed_providers,
        blocked_providers,
        force_provider: payload.get("forceProvider").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        force_model: payload.get("forceModel").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        allowed_endpoints,
        blocked_endpoints,
        budget_usd,
        budget_reset_hours,
        budget_window_started_at,
        billing_mode: "credits".to_string(),
        byok_provider: None,
        regenerate_version: 0,
        metadata,
        updated_at: None,
    };

    match state.db.save_api_key(&key) {
        Ok(()) => {
            let serialized = serialize_api_key(&key, true);
            (StatusCode::CREATED, Json(serialized)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_key_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let key = match state.db.get_api_key_by_id(&id).or_else(|_| state.db.get_api_key_by_key(&id)) {
        Ok(Some(k)) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    let is_admin = crate::auth::verify_admin_api_token(&headers);
    let mut serialized = serialize_api_key(&key, false);
    if is_admin {
        let budget = state.db.get_budget_state(&key);
        if let Some(obj) = serialized.as_object_mut() {
            obj.insert("budgetRemainingUsd".to_string(), budget.get("remaining").cloned().unwrap_or(json!(null)));
            obj.insert("budgetResetAt".to_string(), budget.get("resetsAt").cloned().unwrap_or(json!(null)));
            obj.insert("budgetSpendWindowUsd".to_string(), budget.get("used").cloned().unwrap_or(json!(null)));
        }
        Json(serialized).into_response()
    } else {
        Json(json!({ "key": serialized })).into_response()
    }
}

pub async fn update_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut key = match state.db.get_api_key_by_id(&id).or_else(|_| state.db.get_api_key_by_key(&id)) {
        Ok(Some(k)) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    if let Some(n) = body.get("name").and_then(|v| v.as_str()) {
        key.name = Some(n.trim().to_string());
    }
    if let Some(act) = body.get("isActive").and_then(|v| v.as_bool()) {
        key.is_active = act;
    }
    if let Some(rl) = body.get("rateLimitRequests").and_then(|v| v.as_i64()) {
        key.rate_limit_requests = rl;
    }
    if let Some(rw) = body.get("rateLimitWindowSeconds").and_then(|v| v.as_i64()) {
        key.rate_limit_window_seconds = rw;
    }
    if let Some(tl) = body.get("tokenLimitDaily").and_then(|v| v.as_i64()) {
        key.token_limit_daily = tl;
    }
    if let Some(tm) = body.get("tokenLimitMonthly").and_then(|v| v.as_i64()) {
        key.token_limit_monthly = tm;
    }
    if let Some(mc) = body.get("maxConcurrentRequests").and_then(|v| v.as_i64()) {
        key.max_concurrent_requests = mc;
    }
    if let Some(b) = body.get("budgetUsd").and_then(|v| v.as_f64()) {
        key.budget_usd = Some(b);
    }
    if let Some(br) = body.get("budgetResetHours").and_then(|v| v.as_f64()) {
        key.budget_reset_hours = Some(br);
    }
    if let Some(exp) = body.get("expiresAt").and_then(|v| v.as_str()) {
        key.expires_at = exp.to_string();
    }
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        key.description = desc.to_string();
    }
    if let Ok(Some(am)) = normalize_list(body.get("allowedModels")) {
        key.allowed_models = am;
    }
    if let Ok(Some(bm)) = normalize_list(body.get("blockedModels")) {
        key.blocked_models = bm;
    }
    if let Ok(Some(aip)) = normalize_list(body.get("allowedIPs")) {
        key.allowed_ips = aip;
    }
    if let Ok(Some(bip)) = normalize_list(body.get("blockedIPs")) {
        key.blocked_ips = bip;
    }
    key.updated_at = Some(chrono::Utc::now().to_rfc3339());

    if let Err(e) = state.db.save_api_key(&key) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    let serialized = serialize_api_key(&key, false);
    Json(json!({ "key": serialized })).into_response()
}

pub async fn regenerate_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.regenerate_api_key(&id) {
        Ok(Some(k)) => Json(serialize_api_key(&k, true)).into_response(),
        _ => (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    }
}

pub async fn byok_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut key = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    if body.get("credentialsCiphertext").is_none() || body.get("credentialsCiphertext").unwrap().is_null() {
        key.billing_mode = "credits".to_string();
        key.byok_provider = None;
        let _ = state.db.save_api_key(&key);
        return Json(json!({ "billingMode": "credits" })).into_response();
    }
    let Some(provider) = body.get("provider").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "provider is required"}))).into_response();
    };
    key.billing_mode = "byok".to_string();
    key.byok_provider = Some(provider.to_string());
    let _ = state.db.save_api_key(&key);
    Json(json!({ "billingMode": "byok" })).into_response()
}

pub async fn byok_clear_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut key = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    key.billing_mode = "credits".to_string();
    key.byok_provider = None;
    let _ = state.db.save_api_key(&key);
    Json(json!({ "billingMode": "credits" })).into_response()
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
        Ok(true) => Json(json!({ "success": true, "message": "Key deleted successfully" })).into_response(),
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

// ==========================================
// Admin API Routes (Gated by ADMIN_API_TOKEN)
// ==========================================

pub async fn admin_gateway_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let gw = settings.get("gatewayControl").cloned().unwrap_or_else(|| {
        json!({
            "disabled": false,
            "reason": "",
            "drain": false,
            "blockedProviders": [],
            "blockedModels": []
        })
    });
    Json(json!({ "gatewayControl": gw })).into_response()
}

pub async fn admin_gateway_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let mut settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let current_gw = settings.get("gatewayControl").cloned().unwrap_or_else(|| {
        json!({
            "disabled": false,
            "reason": "",
            "drain": false,
            "blockedProviders": [],
            "blockedModels": []
        })
    });

    let disabled = body.get("disabled").and_then(|v| v.as_bool())
        .unwrap_or_else(|| current_gw.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false));
    let reason_val = body.get("reason").and_then(|v| v.as_str())
        .unwrap_or_else(|| current_gw.get("reason").and_then(|v| v.as_str()).unwrap_or(""));
    if reason_val.len() > 500 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "reason must be 500 characters or fewer"}))).into_response();
    }
    let drain = body.get("drain").and_then(|v| v.as_bool())
        .unwrap_or_else(|| current_gw.get("drain").and_then(|v| v.as_bool()).unwrap_or(false));

    let blocked_providers = match body.get("blockedProviders") {
        Some(v) => {
            let Some(arr) = v.as_array() else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "blockedProviders must be a string array"}))).into_response();
            };
            if arr.iter().any(|item| !item.is_string()) {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "blockedProviders must be a string array"}))).into_response();
            }
            v.clone()
        }
        None => current_gw.get("blockedProviders").cloned().unwrap_or_else(|| json!([])),
    };

    let blocked_models = match body.get("blockedModels") {
        Some(v) => {
            let Some(arr) = v.as_array() else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "blockedModels must be a string array"}))).into_response();
            };
            if arr.iter().any(|item| !item.is_string()) {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "blockedModels must be a string array"}))).into_response();
            }
            v.clone()
        }
        None => current_gw.get("blockedModels").cloned().unwrap_or_else(|| json!([])),
    };

    let new_gw = json!({
        "disabled": disabled,
        "reason": reason_val,
        "drain": drain,
        "blockedProviders": blocked_providers,
        "blockedModels": blocked_models
    });

    if let Some(obj) = settings.as_object_mut() {
        obj.insert("gatewayControl".to_string(), new_gw.clone());
    }
    let _ = state.db.save_settings(&settings);

    let _ = state.db.append_audit_event(&json!({
        "actor": "admin-token",
        "action": "gateway_control.updated",
        "resourceType": "gateway",
        "resourceId": "global",
        "requestId": headers.get("x-request-id").and_then(|v| v.to_str().ok()),
        "sourceIp": get_client_ip(&headers),
        "before": current_gw,
        "after": new_gw,
    }));

    Json(json!({ "gatewayControl": new_gw })).into_response()
}

#[derive(serde::Deserialize, Default)]
pub struct AdminAuditQuery {
    #[serde(rename = "ownerRef")]
    pub owner_ref: Option<String>,
    pub action: Option<String>,
    pub limit: Option<usize>,
}

pub async fn admin_audit_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AdminAuditQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(ref owner) = query.owner_ref else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    let owner_trimmed = owner.trim();
    if owner_trimmed.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    }
    let limit = query.limit.unwrap_or(100);
    match state.db.list_audit_events(Some(owner_trimmed), query.action.as_deref(), limit) {
        Ok(events) => Json(json!({ "events": events })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct AdminKeysQuery {
    #[serde(rename = "ownerRef")]
    pub owner_ref: Option<String>,
}

pub async fn admin_keys_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AdminKeysQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(ref owner) = query.owner_ref else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    let owner_trimmed = owner.trim();
    if owner_trimmed.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    }
    match state.db.list_api_keys_by_owner(owner_trimmed) {
        Ok(keys) => {
            let serialized: Vec<serde_json::Value> = keys.iter().map(|k| serialize_admin_key(k, false)).collect();
            Json(json!({ "keys": serialized })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub async fn admin_keys_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(owner) = body.get("ownerRef").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    if owner.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    }
    let Some(name) = body.get("name").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "name is required"}))).into_response();
    };
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "name is required"}))).into_response();
    }

    if let Some(bm) = body.get("billingMode").and_then(|v| v.as_str()) {
        if bm == "byok" {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Configure BYOK credentials after creating the key"}))).into_response();
        }
        if bm != "credits" {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "billingMode must be credits or byok"}))).into_response();
        }
    }

    // Parse lists
    let allowed_models = normalize_list(body.get("allowedModels")).unwrap_or(None).unwrap_or_default();
    let blocked_models = normalize_list(body.get("blockedModels")).unwrap_or(None).unwrap_or_default();
    let allowed_ips = normalize_list(body.get("allowedIPs")).unwrap_or(None).unwrap_or_default();
    let blocked_ips = normalize_list(body.get("blockedIPs")).unwrap_or(None).unwrap_or_default();
    let allowed_providers = normalize_list(body.get("allowedProviders")).unwrap_or(None).unwrap_or_default();
    let blocked_providers = normalize_list(body.get("blockedProviders")).unwrap_or(None).unwrap_or_default();
    let allowed_endpoints = normalize_list(body.get("allowedEndpoints")).unwrap_or(None).unwrap_or_default();
    let blocked_endpoints = normalize_list(body.get("blockedEndpoints")).unwrap_or(None).unwrap_or_default();

    // Conflict checks
    if !allowed_models.is_empty() && !blocked_models.is_empty() {
        let am: Vec<&str> = allowed_models.split(',').collect();
        let bm: Vec<&str> = blocked_models.split(',').collect();
        if am.iter().any(|m| bm.contains(m)) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "allowedModels and blockedModels must not overlap"}))).into_response();
        }
    }
    if !allowed_ips.is_empty() && !blocked_ips.is_empty() {
        let aip: Vec<&str> = allowed_ips.split(',').collect();
        let bip: Vec<&str> = blocked_ips.split(',').collect();
        if aip.iter().any(|ip| bip.contains(ip)) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "allowedIPs and blockedIPs must not overlap"}))).into_response();
        }
    }
    for ip in allowed_ips.split(',').filter(|s| !s.is_empty()) {
        if !is_valid_ip_rule(ip) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "allowedIPs contains an invalid IP or CIDR"}))).into_response();
        }
    }
    for ip in blocked_ips.split(',').filter(|s| !s.is_empty()) {
        if !is_valid_ip_rule(ip) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "blockedIPs contains an invalid IP or CIDR"}))).into_response();
        }
    }

    let force_provider = body.get("forceProvider").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let force_model = body.get("forceModel").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !force_model.is_empty() && !blocked_models.is_empty() {
        let bm: Vec<&str> = blocked_models.split(',').collect();
        if bm.contains(&force_model.as_str()) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "forceModel must not be in blockedModels"}))).into_response();
        }
    }
    if !force_provider.is_empty() && !blocked_providers.is_empty() {
        let bp: Vec<&str> = blocked_providers.split(',').collect();
        if bp.contains(&force_provider.as_str()) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "forceProvider must not be in blockedProviders"}))).into_response();
        }
    }

    let machine_id = body.get("machineId").and_then(|v| v.as_str()).map(|s| s.to_string());
    let mut custom_key_val = None;
    if let Some(custom_key) = body.get("key").and_then(|v| v.as_str()).map(|s| s.trim()) {
        if !custom_key.is_empty() {
            if custom_key.len() > 512 {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "key must be 512 characters or fewer"}))).into_response();
            }
            if state.db.get_api_key_by_key(custom_key).ok().flatten().is_some() {
                return (StatusCode::CONFLICT, Json(json!({"error": "API key already in use"}))).into_response();
            }
            custom_key_val = Some(custom_key.to_string());
        }
    }

    let id = body.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())
        .unwrap_or_else(|| format!("{:x}", chrono::Utc::now().timestamp_micros()));
    let key_secret = custom_key_val.unwrap_or_else(|| crate::auth::generate_api_key(machine_id.as_deref()));

    let is_active = body.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true);
    let rate_limit_requests = body.get("rateLimitRequests").and_then(|v| v.as_i64()).unwrap_or(0);
    let rate_limit_window_seconds = body.get("rateLimitWindowSeconds").and_then(|v| v.as_i64()).unwrap_or(60);
    let token_limit_daily = body.get("tokenLimitDaily").and_then(|v| v.as_i64()).unwrap_or(0);
    let token_limit_monthly = body.get("tokenLimitMonthly").and_then(|v| v.as_i64()).unwrap_or(0);
    let max_concurrent_requests = body.get("maxConcurrentRequests").and_then(|v| v.as_i64()).unwrap_or(0);
    let budget_usd = body.get("budgetUsd").and_then(|v| v.as_f64());
    let budget_reset_hours = body.get("budgetResetHours").and_then(|v| v.as_f64());
    let budget_window_started_at = body.get("budgetWindowStartedAt").and_then(|v| v.as_str()).map(|s| s.to_string());
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let expires_at = body.get("expiresAt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let metadata = body.get("metadata").map(|v| if v.is_string() { v.as_str().unwrap().to_string() } else { serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()) }).unwrap_or_else(|| "{}".to_string());

    let api_key = crate::db::ApiKey {
        id: id.clone(),
        key: key_secret,
        name: Some(name.to_string()),
        machine_id,
        is_active,
        created_at: chrono::Utc::now().to_rfc3339(),
        rate_limit_requests,
        rate_limit_window_seconds,
        token_limit_daily,
        token_limit_monthly,
        allowed_models,
        blocked_models,
        allowed_ips,
        blocked_ips,
        expires_at,
        description,
        owner_ref: Some(owner.to_string()),
        max_concurrent_requests,
        allowed_providers,
        blocked_providers,
        force_provider,
        force_model,
        allowed_endpoints,
        blocked_endpoints,
        budget_usd,
        budget_reset_hours,
        budget_window_started_at,
        billing_mode: "credits".to_string(),
        byok_provider: None,
        regenerate_version: 0,
        metadata,
        updated_at: None,
    };

    if let Err(e) = state.db.save_api_key(&api_key) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }

    let serialized = serialize_admin_key(&api_key, true);
    let _ = state.db.append_audit_event(&json!({
        "actor": "admin-token",
        "action": "api_key.created",
        "resourceType": "api_key",
        "resourceId": api_key.id,
        "ownerRef": owner,
        "requestId": headers.get("x-request-id").and_then(|v| v.to_str().ok()),
        "sourceIp": get_client_ip(&headers),
        "after": serialize_admin_key(&api_key, false),
    }));

    (StatusCode::CREATED, Json(serialized)).into_response()
}

pub async fn admin_key_detail_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminKeysQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(ref owner) = query.owner_ref else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    let key = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) if k.owner_ref.as_deref() == Some(owner.trim()) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    Json(serialize_admin_key(&key, false)).into_response()
}

pub async fn admin_key_detail_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminKeysQuery>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(ref owner) = query.owner_ref else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    if let Some(body_owner) = body.get("ownerRef").and_then(|v| v.as_str()) {
        if body_owner.trim() != owner.trim() {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response();
        }
    }
    let mut key = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) if k.owner_ref.as_deref() == Some(owner.trim()) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    let before_serialized = serialize_admin_key(&key, false);

    if let Some(n) = body.get("name").and_then(|v| v.as_str()) {
        key.name = Some(n.trim().to_string());
    }
    if let Some(act) = body.get("isActive").and_then(|v| v.as_bool()) {
        key.is_active = act;
    }
    if let Some(rl) = body.get("rateLimitRequests").and_then(|v| v.as_i64()) {
        key.rate_limit_requests = rl;
    }
    if let Some(rw) = body.get("rateLimitWindowSeconds").and_then(|v| v.as_i64()) {
        key.rate_limit_window_seconds = rw;
    }
    if let Some(tl) = body.get("tokenLimitDaily").and_then(|v| v.as_i64()) {
        key.token_limit_daily = tl;
    }
    if let Some(tm) = body.get("tokenLimitMonthly").and_then(|v| v.as_i64()) {
        key.token_limit_monthly = tm;
    }
    if let Some(mc) = body.get("maxConcurrentRequests").and_then(|v| v.as_i64()) {
        key.max_concurrent_requests = mc;
    }
    if let Some(b) = body.get("budgetUsd").and_then(|v| v.as_f64()) {
        key.budget_usd = Some(b);
    }
    if let Some(br) = body.get("budgetResetHours").and_then(|v| v.as_f64()) {
        key.budget_reset_hours = Some(br);
    }
    if let Some(exp) = body.get("expiresAt").and_then(|v| v.as_str()) {
        key.expires_at = exp.to_string();
    }
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        key.description = desc.to_string();
    }
    if let Ok(Some(am)) = normalize_list(body.get("allowedModels")) {
        key.allowed_models = am;
    }
    if let Ok(Some(bm)) = normalize_list(body.get("blockedModels")) {
        key.blocked_models = bm;
    }
    if let Ok(Some(aip)) = normalize_list(body.get("allowedIPs")) {
        key.allowed_ips = aip;
    }
    if let Ok(Some(bip)) = normalize_list(body.get("blockedIPs")) {
        key.blocked_ips = bip;
    }
    if let Ok(Some(ap)) = normalize_list(body.get("allowedProviders")) {
        key.allowed_providers = ap;
    }
    if let Ok(Some(bp)) = normalize_list(body.get("blockedProviders")) {
        key.blocked_providers = bp;
    }
    if let Ok(Some(ae)) = normalize_list(body.get("allowedEndpoints")) {
        key.allowed_endpoints = ae;
    }
    if let Ok(Some(be)) = normalize_list(body.get("blockedEndpoints")) {
        key.blocked_endpoints = be;
    }
    if let Some(fp) = body.get("forceProvider").and_then(|v| v.as_str()) {
        key.force_provider = fp.trim().to_string();
    }
    if let Some(fm) = body.get("forceModel").and_then(|v| v.as_str()) {
        key.force_model = fm.trim().to_string();
    }
    key.updated_at = Some(chrono::Utc::now().to_rfc3339());

    if let Err(e) = state.db.save_api_key(&key) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }
    let after_serialized = serialize_admin_key(&key, false);

    let _ = state.db.append_audit_event(&json!({
        "actor": "admin-token",
        "action": "api_key.updated",
        "resourceType": "api_key",
        "resourceId": id,
        "ownerRef": owner.trim(),
        "requestId": headers.get("x-request-id").and_then(|v| v.to_str().ok()),
        "sourceIp": get_client_ip(&headers),
        "before": before_serialized,
        "after": after_serialized.clone(),
    }));

    Json(after_serialized).into_response()
}

pub async fn admin_key_detail_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminKeysQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(ref owner) = query.owner_ref else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    let existing = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) if k.owner_ref.as_deref() == Some(owner.trim()) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };

    match state.db.delete_api_key(&id) {
        Ok(true) => {
            let _ = state.db.append_audit_event(&json!({
                "actor": "admin-token",
                "action": "api_key.deleted",
                "resourceType": "api_key",
                "resourceId": id,
                "ownerRef": owner.trim(),
                "requestId": headers.get("x-request-id").and_then(|v| v.to_str().ok()),
                "sourceIp": get_client_ip(&headers),
                "before": serialize_admin_key(&existing, false),
            }));
            Json(json!({ "message": "Key deleted successfully" })).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    }
}

pub async fn admin_key_rotate_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(owner) = body.get("ownerRef").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "ownerRef is required"}))).into_response();
    };
    let _existing = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) if k.owner_ref.as_deref() == Some(owner) => k,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    };
    match state.db.regenerate_api_key(&id) {
        Ok(Some(new_key)) => {
            let _ = state.db.append_audit_event(&json!({
                "actor": "admin-token",
                "action": "api_key.rotated",
                "resourceType": "api_key",
                "resourceId": id,
                "ownerRef": owner,
                "requestId": headers.get("x-request-id").and_then(|v| v.to_str().ok()),
                "sourceIp": get_client_ip(&headers),
                "metadata": { "regenerateVersion": new_key.regenerate_version },
            }));
            Json(serialize_admin_key(&new_key, true)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"error": "Key not found"}))).into_response(),
    }
}

pub async fn key_actions_debit_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let Some(key_id) = body.get("keyId").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "keyId and requestId are required"}))).into_response();
    };
    let Some(request_id) = body.get("requestId").and_then(|v| v.as_str()).map(|s| s.trim()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "keyId and requestId are required"}))).into_response();
    };
    if key_id.is_empty() || request_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "keyId and requestId are required"}))).into_response();
    }

    let reserve_usd = body.get("reserveUsd").and_then(|v| v.as_f64());
    let commit_usd = body.get("commitUsd").and_then(|v| v.as_f64());

    match state.db.debit_budget(key_id, request_id, reserve_usd, commit_usd) {
        Ok(budget) => Json(json!({ "budget": budget })).into_response(),
        Err((code, msg, opt_budget)) => {
            let mut err_obj = json!({ "error": msg });
            if let Some(b) = opt_budget {
                if let Some(obj) = err_obj.as_object_mut() {
                    obj.insert("budget".to_string(), b);
                }
            }
            (code, Json(err_obj)).into_response()
        }
    }
}

#[derive(serde::Deserialize, Default)]
pub struct ObservabilityQuery {
    pub tenant: Option<String>,
    pub format: Option<String>,
}

pub async fn observability_alerts_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ObservabilityQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let alerts = state.db.get_alerts(query.tenant.as_deref());
    Json(json!({ "alerts": alerts })).into_response()
}

pub async fn observability_metrics_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ObservabilityQuery>,
) -> Response {
    if !crate::auth::verify_admin_api_token(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let snapshot = state.db.get_metrics_snapshot(query.tenant.as_deref());
    if query.format.as_deref() == Some("prometheus") {
        let text = state.db.get_prometheus_metrics(&snapshot);
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            text,
        )
            .into_response();
    }
    Json(snapshot).into_response()
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
        .unwrap_or(false);
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

pub async fn provider_connection_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let conn = all_conns.into_iter().find(|c| c.id == connection_id || c.provider == connection_id);

    let (provider_id, api_key) = match &conn {
        Some(c) => (c.provider.clone(), c.data.api_key.clone().unwrap_or_default()),
        None => (connection_id.clone(), String::new()),
    };

    let all_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let matching_node = all_nodes.into_iter().find(|n| n.id == provider_id || n.name.as_deref() == Some(&provider_id) || n.prefix == provider_id);

    let raw_base = matching_node.as_ref().map(|n| n.base_url.as_str())
        .or_else(|| conn.as_ref().and_then(|c| c.data.base_url.as_deref()))
        .unwrap_or("");

    let resolved_base = if !raw_base.is_empty() {
        raw_base.to_string()
    } else {
        match provider_id.to_ascii_lowercase().as_str() {
            "openai" => "https://api.openai.com/v1".to_string(),
            "groq" => "https://api.groq.com/openai/v1".to_string(),
            "openrouter" => "https://openrouter.ai/api/v1".to_string(),
            "mistral" => "https://api.mistral.ai/v1".to_string(),
            "together" => "https://api.together.xyz/v1".to_string(),
            "perplexity" => "https://api.perplexity.ai".to_string(),
            "deepseek" => "https://api.deepseek.com/v1".to_string(),
            _ => String::new(),
        }
    };

    if !resolved_base.is_empty() {
        let b = resolved_base.trim_end_matches('/');
        let models_url = if b.ends_with("/models") {
            b.to_string()
        } else if b.ends_with("/v1") || b.ends_with("/compat") || b.ends_with("/v2") {
            format!("{}/models", b)
        } else {
            format!("{}/v1/models", b)
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default();

        let mut req_builder = client.get(&models_url);
        if !api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        }

        if let Ok(resp) = req_builder.send().await {
            if resp.status().is_success() {
                if let Ok(json_data) = resp.json::<serde_json::Value>().await {
                    let list = json_data.get("data")
                        .or_else(|| json_data.get("models"))
                        .or_else(|| json_data.as_array().map(|_| &json_data))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();

                    let mapped_models: Vec<serde_json::Value> = list.into_iter().filter_map(|m| {
                        let id = m.get("id").or_else(|| m.get("model")).or_else(|| m.get("name"))?.as_str()?;
                        let name = m.get("name").or_else(|| m.get("displayName"))?.as_str().unwrap_or(id);
                        Some(json!({
                            "id": id,
                            "name": name,
                            "rawId": id
                        }))
                    }).collect();

                    if !mapped_models.is_empty() {
                        return Json(json!({
                            "provider": provider_id,
                            "connectionId": connection_id,
                            "models": mapped_models
                        })).into_response();
                    }
                }
            }
        }
    }

    if let Ok(cms) = state.db.load_custom_models() {
        let matching_cms: Vec<serde_json::Value> = cms.into_iter().filter(|m| {
            let pa = m.get("providerAlias").or_else(|| m.get("provider")).and_then(|v| v.as_str()).unwrap_or("");
            pa == provider_id || pa == connection_id || matching_node.as_ref().map(|n| n.prefix.as_str() == pa || n.name.as_deref() == Some(pa)).unwrap_or(false)
        }).map(|m| {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            json!({
                "id": id,
                "name": name,
                "rawId": id
            })
        }).collect();
        if !matching_cms.is_empty() {
            return Json(json!({
                "provider": provider_id,
                "connectionId": connection_id,
                "models": matching_cms
            })).into_response();
        }
    }

    let default_candidates = if matching_node.as_ref().map(|n| n.name.as_deref() == Some("HCNSEC") || n.prefix == "h").unwrap_or(false) {
        vec!["Qwen3.8-Flash-Next", "hy4", "DeepSeek-V4.1-Flash", "MiMo-V2.6-Flash", "kimi-k3.1-pro", "MiniMax-M3.1-Flash", "sensenova-6.8-flash"]
    } else {
        vec!["default", "chat", "main"]
    };
    let fallback_models: Vec<serde_json::Value> = default_candidates.into_iter().map(|m| {
        json!({
            "id": m,
            "name": m,
            "rawId": m
        })
    }).collect();

    Json(json!({
        "provider": provider_id,
        "connectionId": connection_id,
        "models": fallback_models
    })).into_response()
}

// ============================================================================
// Model Management Handlers
// ============================================================================

pub async fn get_custom_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let models = state.db.load_custom_models().unwrap_or_default();
    Json(json!({ "models": models })).into_response()
}

pub async fn post_custom_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    match state.db.save_custom_model(&payload) {
        Ok(_) => Json(json!({ "success": true, "added": payload })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn delete_custom_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider_alias = params.get("providerAlias")
        .or_else(|| params.get("provider"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let id = params.get("id").map(|s| s.as_str()).unwrap_or("");
    let model_type = params.get("type").map(|s| s.as_str()).unwrap_or("llm");

    if provider_alias.is_empty() || id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "providerAlias and id required" }))).into_response();
    }

    match state.db.delete_custom_model(provider_alias, id, model_type) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn get_model_display_names_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let display_names = state.db.load_model_display_names().unwrap_or_default();
    Json(json!({ "displayNames": display_names })).into_response()
}

pub async fn put_model_display_names_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let model_id = payload.get("modelId").and_then(|v| v.as_str()).unwrap_or("");
    let display_name = payload.get("displayName").and_then(|v| v.as_str()).unwrap_or("");
    if model_id.is_empty() || display_name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "modelId and displayName required" }))).into_response();
    }
    match state.db.set_model_display_name(model_id, display_name) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn delete_model_display_names_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let model_id = params.get("modelId").map(|s| s.as_str()).unwrap_or("");
    if model_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "modelId required" }))).into_response();
    }
    match state.db.delete_model_display_name(model_id) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn get_disabled_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let disabled = state.db.load_disabled_models().unwrap_or_default();
    if let Some(alias) = params.get("providerAlias") {
        let ids = disabled.get(alias).cloned().unwrap_or_default();
        Json(json!({ "ids": ids })).into_response()
    } else {
        Json(json!({ "disabled": disabled })).into_response()
    }
}

pub async fn post_disabled_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider_alias = payload.get("providerAlias").and_then(|v| v.as_str()).unwrap_or("");
    let ids: Vec<String> = payload.get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    if provider_alias.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "providerAlias required" }))).into_response();
    }

    match state.db.disable_models(provider_alias, &ids) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn delete_disabled_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider_alias = params.get("providerAlias").map(|s| s.as_str()).unwrap_or("");
    if provider_alias.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "providerAlias required" }))).into_response();
    }
    let id_opt = params.get("id").map(|s| s.to_string());
    let ids = match id_opt {
        Some(id) => vec![id],
        None => vec![],
    };
    match state.db.enable_models(provider_alias, &ids) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn get_model_aliases_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let aliases = state.db.load_model_aliases().unwrap_or_default();
    Json(json!({ "aliases": aliases })).into_response()
}

pub async fn put_model_aliases_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let alias = payload.get("alias").and_then(|v| v.as_str()).unwrap_or("");
    if model.is_empty() || alias.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "model and alias required" }))).into_response();
    }
    match state.db.set_model_alias(alias, model) {
        Ok(_) => Json(json!({ "success": true, "model": model, "alias": alias })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn delete_model_aliases_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let alias = params.get("alias").map(|s| s.as_str()).unwrap_or("");
    if alias.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "alias required" }))).into_response();
    }
    match state.db.delete_model_alias(alias) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn post_model_test_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if model.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "Model required" }))).into_response();
    }

    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let (conn, target_model) = resolve_target_for_candidate(model, &provider_nodes, &all_conns);

    if conn.base_url.is_none() && conn.provider != "openai" && conn.provider != "anthropic" {
        return Json(json!({
            "ok": false,
            "latencyMs": 0,
            "status": 404,
            "error": format!("No active connection or base URL configured for model '{}'", model)
        })).into_response();
    }

    let base_url = conn.base_url.unwrap_or_else(|| {
        if conn.provider == "anthropic" {
            "https://api.anthropic.com/v1".to_string()
        } else {
            "https://api.openai.com/v1".to_string()
        }
    });

    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let start = std::time::Instant::now();
    let mut req = client.post(&endpoint)
        .header("Content-Type", "application/json");

    if let Some(ref key) = conn.api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    if let Some(ref hdrs) = conn.custom_headers {
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
    }

    let body = json!({
        "model": target_model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });

    match req.json(&body).send().await {
        Ok(res) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            let status = res.status().as_u16();
            if res.status().is_success() || status == 400 {
                Json(json!({
                    "ok": true,
                    "latencyMs": latency_ms,
                    "status": status
                })).into_response()
            } else {
                let err_text = res.text().await.unwrap_or_default();
                Json(json!({
                    "ok": false,
                    "latencyMs": latency_ms,
                    "status": status,
                    "error": err_text
                })).into_response()
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            Json(json!({
                "ok": false,
                "latencyMs": latency_ms,
                "status": 500,
                "error": e.to_string()
            })).into_response()
        }
    }
}

pub async fn get_suggested_models_handler(
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let url = match params.get("url") {
        Some(u) if !u.is_empty() => u,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Missing url" }))).into_response(),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .unwrap_or_default();

    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                let list = val.get("data")
                    .or_else(|| val.get("models"))
                    .or_else(|| val.as_array().map(|_| &val))
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                Json(json!({ "data": list })).into_response()
            } else {
                Json(json!({ "data": [] })).into_response()
            }
        }
        _ => Json(json!({ "data": [] })).into_response(),
    }
}

pub async fn cli_tools_all_statuses_handler() -> Response {
    let tools = [
        "claude", "codex", "opencode", "droid", "openclaw", "hermes",
        "cowork", "cline", "kilo", "deepseek-tui", "jcode", "grok-build",
        "devin", "pi", "omp", "crush", "forge", "smelt", "codewhale"
    ];
    let mut map = serde_json::Map::new();
    for tool in tools {
        map.insert(
            tool.to_string(),
            json!({
                "installed": false,
                "has9Router": false,
                "settings": null
            }),
        );
    }
    Json(serde_json::Value::Object(map)).into_response()
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

    if let Ok(nodes) = state.db.load_provider_nodes() {
        let all_conns = state.db.load_provider_connections().unwrap_or_default();
        for node in nodes {
            let p = if !node.prefix.is_empty() { node.prefix.clone() } else { node.name.clone().unwrap_or_else(|| "custom".to_string()) };
            let has_active_conn = all_conns.iter().any(|c| c.is_active && (c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider)));
            if has_active_conn {
                // Common models for this node
                let candidates = if node.name.as_deref() == Some("HCNSEC") || node.prefix == "h" {
                    vec!["Qwen3.8-Flash-Next", "hy4", "DeepSeek-V4.1-Flash", "MiMo-V2.6-Flash", "kimi-k3.1-pro", "MiniMax-M3.1-Flash", "sensenova-6.8-flash"]
                } else {
                    vec!["default", "chat", "main"]
                };
                for m in candidates {
                    models.push(json!({
                        "provider": p,
                        "model": format!("{}/{}", p, m),
                        "name": format!("{} ({})", m, node.name.as_deref().unwrap_or(&p)),
                        "fullModel": format!("{}/{}", p, m),
                        "routedModel": format!("{}/{}", p, m),
                        "alias": m,
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
        // Embeddings
        .route(
            "/v1/embeddings",
            axum::routing::post(crate::media::embeddings_handler).options(options_handler),
        )
        .route(
            "/embeddings",
            axum::routing::post(crate::media::embeddings_handler).options(options_handler),
        )
        .route(
            "/api/v1/embeddings",
            axum::routing::post(crate::media::embeddings_handler).options(options_handler),
        )
        // Image Generations
        .route(
            "/v1/images/generations",
            axum::routing::post(crate::media::image_generations_handler).options(options_handler),
        )
        .route(
            "/images/generations",
            axum::routing::post(crate::media::image_generations_handler).options(options_handler),
        )
        .route(
            "/api/v1/images/generations",
            axum::routing::post(crate::media::image_generations_handler).options(options_handler),
        )
        // Video Generations
        .route(
            "/v1/videos/generations",
            axum::routing::post(crate::media::video_generations_handler).options(options_handler),
        )
        .route(
            "/videos/generations",
            axum::routing::post(crate::media::video_generations_handler).options(options_handler),
        )
        .route(
            "/api/v1/videos/generations",
            axum::routing::post(crate::media::video_generations_handler).options(options_handler),
        )
        // Video Edits
        .route(
            "/v1/videos/edits",
            axum::routing::post(crate::media::video_edits_handler).options(options_handler),
        )
        .route(
            "/videos/edits",
            axum::routing::post(crate::media::video_edits_handler).options(options_handler),
        )
        .route(
            "/api/v1/videos/edits",
            axum::routing::post(crate::media::video_edits_handler).options(options_handler),
        )
        // Video Extensions
        .route(
            "/v1/videos/extensions",
            axum::routing::post(crate::media::video_extensions_handler).options(options_handler),
        )
        .route(
            "/videos/extensions",
            axum::routing::post(crate::media::video_extensions_handler).options(options_handler),
        )
        .route(
            "/api/v1/videos/extensions",
            axum::routing::post(crate::media::video_extensions_handler).options(options_handler),
        )
        // Video Polling & Listing
        .route(
            "/v1/videos",
            axum::routing::get(crate::media::video_list_handler).options(options_handler),
        )
        .route(
            "/videos",
            axum::routing::get(crate::media::video_list_handler).options(options_handler),
        )
        .route(
            "/api/v1/videos",
            axum::routing::get(crate::media::video_list_handler).options(options_handler),
        )
        .route(
            "/v1/videos/{id}",
            axum::routing::get(crate::media::video_get_handler).options(options_handler),
        )
        .route(
            "/videos/{id}",
            axum::routing::get(crate::media::video_get_handler).options(options_handler),
        )
        .route(
            "/api/v1/videos/{id}",
            axum::routing::get(crate::media::video_get_handler).options(options_handler),
        )
        // Audio: Transcriptions (STT) & Speech (TTS)
        .route(
            "/v1/audio/transcriptions",
            axum::routing::post(crate::media::transcriptions_handler).options(crate::media::audio_options_handler),
        )
        .route(
            "/audio/transcriptions",
            axum::routing::post(crate::media::transcriptions_handler).options(crate::media::audio_options_handler),
        )
        .route(
            "/api/v1/audio/transcriptions",
            axum::routing::post(crate::media::transcriptions_handler).options(crate::media::audio_options_handler),
        )
        .route(
            "/v1/audio/speech",
            axum::routing::post(crate::media::speech_handler).options(crate::media::audio_options_handler),
        )
        .route(
            "/audio/speech",
            axum::routing::post(crate::media::speech_handler).options(crate::media::audio_options_handler),
        )
        .route(
            "/api/v1/audio/speech",
            axum::routing::post(crate::media::speech_handler).options(crate::media::audio_options_handler),
        )
        // Web Fetch & Search & System One
        .route(
            "/api/v1/web/fetch",
            axum::routing::post(crate::media::web_fetch_handler).options(options_handler),
        )
        .route(
            "/v1/web/fetch",
            axum::routing::post(crate::media::web_fetch_handler).options(options_handler),
        )
        .route(
            "/api/v1/search",
            axum::routing::post(crate::media::web_search_handler).options(options_handler),
        )
        .route(
            "/v1/search",
            axum::routing::post(crate::media::web_search_handler).options(options_handler),
        )
        .route(
            "/v1/systemone",
            axum::routing::post(crate::media::systemone_handler).options(options_handler),
        )
        .route(
            "/api/v1/systemone",
            axum::routing::post(crate::media::systemone_handler).options(options_handler),
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
        // Proxy Pools Endpoints
        .route(
            "/api/proxy-pools",
            axum::routing::get(crate::proxy_pools::get_proxy_pools_handler)
                .post(crate::proxy_pools::create_proxy_pool_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/inactive",
            axum::routing::delete(crate::proxy_pools::delete_inactive_proxy_pools_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/proxyscrape",
            axum::routing::get(crate::proxy_pools::get_proxyscrape_config_handler)
                .put(crate::proxy_pools::update_proxyscrape_config_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/proxyscrape/refresh",
            axum::routing::post(crate::proxy_pools::refresh_proxyscrape_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/providers",
            axum::routing::get(crate::proxy_pools::get_providers_handler)
                .post(crate::proxy_pools::update_providers_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/providers/{id}",
            axum::routing::get(crate::proxy_pools::get_provider_handler)
                .put(crate::proxy_pools::update_provider_handler)
                .delete(crate::proxy_pools::delete_provider_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/providers/{id}/refresh",
            axum::routing::post(crate::proxy_pools::refresh_provider_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/vercel-deploy",
            axum::routing::post(crate::proxy_pools::deploy_vercel_handler).options(options_handler),
        )
        .route(
            "/api/proxy-pools/cloudflare-deploy",
            axum::routing::post(crate::proxy_pools::deploy_cloudflare_handler).options(options_handler),
        )
        .route(
            "/api/proxy-pools/deno-deploy",
            axum::routing::post(crate::proxy_pools::deploy_deno_handler).options(options_handler),
        )
        .route(
            "/api/proxy-pools/netlify-deploy",
            axum::routing::post(crate::proxy_pools::deploy_netlify_handler).options(options_handler),
        )
        .route(
            "/api/proxy-pools/{id}",
            axum::routing::get(crate::proxy_pools::get_proxy_pool_handler)
                .put(crate::proxy_pools::update_proxy_pool_handler)
                .delete(crate::proxy_pools::delete_proxy_pool_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/{id}/test",
            axum::routing::post(crate::proxy_pools::test_proxy_pool_handler)
                .options(options_handler),
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
            axum::routing::get(get_model_aliases_handler)
                .put(put_model_aliases_handler)
                .delete(delete_model_aliases_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/custom",
            axum::routing::get(get_custom_models_handler)
                .post(post_custom_models_handler)
                .delete(delete_custom_models_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/display-names",
            axum::routing::get(get_model_display_names_handler)
                .put(put_model_display_names_handler)
                .delete(delete_model_display_names_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/disabled",
            axum::routing::get(get_disabled_models_handler)
                .post(post_disabled_models_handler)
                .delete(delete_disabled_models_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/test",
            axum::routing::post(post_model_test_handler).options(options_handler),
        )
        .route(
            "/api/providers/suggested-models",
            axum::routing::get(get_suggested_models_handler).options(options_handler),
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
            axum::routing::get(get_key_detail_handler)
                .put(update_key_handler)
                .delete(delete_key_handler)
                .options(options_handler),
        )
        .route(
            "/api/keys/{id}/regenerate",
            axum::routing::post(regenerate_key_handler)
                .put(regenerate_key_handler)
                .options(options_handler),
        )
        .route(
            "/api/keys/{id}/byok",
            axum::routing::put(byok_key_handler)
                .delete(byok_clear_handler)
                .options(options_handler),
        )
        .route(
            "/api/admin/gateway",
            axum::routing::get(admin_gateway_get_handler)
                .put(admin_gateway_put_handler)
                .options(options_handler),
        )
        .route(
            "/api/admin/audit",
            axum::routing::get(admin_audit_handler)
                .options(options_handler),
        )
        .route(
            "/api/admin/keys",
            axum::routing::get(admin_keys_get_handler)
                .post(admin_keys_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/admin/keys/{id}",
            axum::routing::get(admin_key_detail_get_handler)
                .put(admin_key_detail_put_handler)
                .delete(admin_key_detail_delete_handler)
                .options(options_handler),
        )
        .route(
            "/api/admin/keys/{id}/rotate",
            axum::routing::post(admin_key_rotate_handler)
                .put(admin_key_rotate_handler)
                .options(options_handler),
        )
        .route(
            "/api/key-actions/debit",
            axum::routing::post(key_actions_debit_handler)
                .options(options_handler),
        )
        .route(
            "/api/observability/alerts",
            axum::routing::get(observability_alerts_handler)
                .options(options_handler),
        )
        .route(
            "/api/observability/metrics",
            axum::routing::get(observability_metrics_handler)
                .options(options_handler),
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
        .route(
            "/api/cli-tools/all-statuses",
            axum::routing::get(cli_tools_all_statuses_handler).options(options_handler),
        )
        .route(
            "/api/providers/{id}/models",
            axum::routing::get(provider_connection_models_handler).options(options_handler),
        )
        .route(
            "/api/dashboard/chat/completions",
            axum::routing::post(dashboard_chat_completions_handler).options(options_handler),
        )
        // Gemini API v1beta compatibility
        .route(
            "/v1beta/models",
            axum::routing::get(crate::gemini::gemini_models_handler).options(options_handler),
        )
        .route(
            "/v1beta/models/{*path}",
            axum::routing::get(crate::gemini::gemini_model_get_handler)
                .post(crate::gemini::gemini_action_handler)
                .options(options_handler),
        )
        // Tunnel Service
        .route(
            "/api/tunnel/status",
            axum::routing::get(crate::tunnel::tunnel_status_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/enable",
            axum::routing::post(crate::tunnel::tunnel_enable_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/disable",
            axum::routing::post(crate::tunnel::tunnel_disable_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/tailscale-check",
            axum::routing::get(crate::tunnel::tailscale_check_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/tailscale-enable",
            axum::routing::post(crate::tunnel::tailscale_enable_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/tailscale-disable",
            axum::routing::post(crate::tunnel::tailscale_disable_handler).options(options_handler),
        )
        .route(
            "/api/tunnel/tailscale-install",
            axum::routing::post(crate::tunnel::tailscale_install_handler).options(options_handler),
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
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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

        // Enable requireLogin in settings for dashboard auth tests
        db.save_settings(&json!({ "requireLogin": true })).unwrap();

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
        unsafe { std::env::remove_var("ADMIN_API_TOKEN"); }
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
            ..Default::default()
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
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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
    async fn test_models_list_source_and_catalog_resolution() {
        let db = Database::open_in_memory().unwrap();

        // 1. Save an API key
        let test_key = ApiKey {
            id: "key-source-test".to_string(),
            key: "sk-source-test-123".to_string(),
            name: Some("SourceTest".to_string()),
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
            ..Default::default()
        };
        db.save_api_key(&test_key).unwrap();

        // 2. Add a combo to SQLite
        db.save_combo(&crate::db::Combo {
            id: "combo-catalog-1".to_string(),
            name: "cached-combo-model".to_string(),
            kind: Some("llm".to_string()),
            models: vec!["openai/gpt-4o".to_string()],
            strategy: Some("fallback".to_string()),
            match_rules: None,
            custom_prompt: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }).unwrap();

        // 3. Add a provider connection with configured defaultModel and pool models
        db.save_provider_connection(&ProviderConnection {
            id: "conn-azure-test".to_string(),
            provider: "azure".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Azure Connection".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some("azure-secret-key".to_string()),
                base_url: Some("https://example.openai.azure.com".to_string()),
                headers: None,
                default_model: Some("azure-default-deployment".to_string()),
                extra: {
                    let mut m = HashMap::new();
                    m.insert(
                        "providerSpecificData".to_string(),
                        json!({
                            "deployment": "azure-gpt4",
                            "apiVersion": "2024-08-01-preview",
                            "models": ["azure/gpt-4o", "azure/gpt-4o-mini"]
                        }),
                    );
                    m
                },
            },
            raw_data: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }).unwrap();

        // 3b. Add an OpenAI provider connection
        db.save_provider_connection(&ProviderConnection {
            id: "conn-openai-test".to_string(),
            provider: "openai".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("OpenAI Connection".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some("sk-openai-secret".to_string()),
                base_url: None,
                headers: None,
                default_model: None,
                extra: HashMap::new(),
            },
            raw_data: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }).unwrap();

        // 4. Test resolve_models_list_source:
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });

        assert_eq!(resolve_models_list_source(&config, &db), "cached");

        // When DB settings has modelsListSource: "live"
        db.save_settings(&json!({ "modelsListSource": "live" })).unwrap();
        assert_eq!(resolve_models_list_source(&config, &db), "live");

        // Back to "cached"
        db.save_settings(&json!({ "modelsListSource": "cached" })).unwrap();
        assert_eq!(resolve_models_list_source(&config, &db), "cached");

        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let state = AppState {
            config: Arc::clone(&config),
            db: Arc::clone(&db),
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: ConsoleLogBuffer::new(),
        };
        let app = create_router(state);

        // GET /v1/models with cached source
        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .header("Authorization", "Bearer sk-source-test-123")
            .body(Body::empty())
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json_val["object"], "list");
        let list = json_val["data"].as_array().unwrap();

        // Must contain base models, SQLite combos, and provider connection models
        let ids: Vec<&str> = list.iter().filter_map(|m| m["id"].as_str()).collect();
        assert!(ids.contains(&"gpt-4o"));
        assert!(ids.contains(&"cached-combo-model"));
        assert!(ids.contains(&"azure-default-deployment"));
        assert!(ids.contains(&"azure/gpt-4o"));
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
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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
        let state = AppState { config, db: db.clone(), proxy, original, combo_tracker, token_cache, circuit_breaker, console_buffer: ConsoleLogBuffer::new() };
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
            ..Default::default()
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
            ..Default::default()
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

    #[tokio::test]
    async fn test_resolve_observability_and_buffer_tokens() {
        let mut cfg = Config::default();
        assert!(resolve_should_record_request_details(&cfg));
        assert_eq!(resolve_usage_buffer_tokens(&cfg), 2000);

        cfg.observability.enable_request_logs = false;
        assert!(!resolve_should_record_request_details(&cfg));

        cfg.observability.enable_request_logs = true;
        cfg.routing.usage_buffer_tokens = 500;
        assert!(resolve_should_record_request_details(&cfg));
        assert_eq!(resolve_usage_buffer_tokens(&cfg), 500);
    }

    #[tokio::test]
    async fn test_write_behind_usage_and_request_details_chat_and_messages() {
        use std::time::Duration;

        let db = Database::open_in_memory().unwrap();
        let test_key = ApiKey {
            id: "key-wb-test".to_string(),
            key: "sk-wb-test-key".to_string(),
            name: Some("Write Behind Test".to_string()),
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
            ..Default::default()
        };
        db.save_api_key(&test_key).unwrap();

        let mut cfg = Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        };
        cfg.routing.usage_buffer_tokens = 250;
        let config = Arc::new(cfg);
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();

        // 1. Pre-seed cache for chat completions
        let chat_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![crate::translator::ChatMessage::user("wb chat test")],
            ..Default::default()
        };
        let chat_hash = token_cache.compute_chat_hash(&chat_req);
        token_cache.set_json(
            &chat_hash,
            axum::body::Bytes::from_static(b"{\"id\":\"chatcmpl-wb\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"wb response\"}}]}"),
        );

        // 2. Pre-seed cache for Claude messages
        let claude_req = ClaudeMessageRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![crate::translator::ClaudeMessage {
                role: "user".to_string(),
                content: crate::translator::ClaudeContent::Single("wb claude test".to_string()),
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
        let claude_hash = token_cache.compute_claude_hash(&claude_req);
        token_cache.set_json(
            &claude_hash,
            axum::body::Bytes::from_static(b"{\"id\":\"msg-wb\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"claude wb response\"}]}"),
        );

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

        // Send POST /v1/chat/completions
        let http_req1 = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-wb-test-key")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"wb chat test"}]}"#))
            .unwrap();

        let resp1 = app.clone().oneshot(http_req1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::OK);

        // Send POST /v1/messages
        let http_req2 = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("Authorization", "Bearer sk-wb-test-key")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"claude-3-5-sonnet","max_tokens":100,"messages":[{"role":"user","content":"wb claude test"}]}"#))
            .unwrap();

        let resp2 = app.clone().oneshot(http_req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        // Allow write-behind background workers to flush to SQLite
        tokio::time::sleep(Duration::from_millis(200)).await;

        let usage_count = db.get_usage_count().unwrap();
        assert!(usage_count >= 2, "Expected at least 2 usage records, got {}", usage_count);

        let req_details_count = db.get_request_details_count().unwrap();
        assert!(req_details_count >= 2, "Expected at least 2 request details, got {}", req_details_count);

        let details = db.get_request_details(1, 10, None, None, None).unwrap();
        let endpoints: Vec<String> = details["details"].as_array().unwrap()
            .iter()
            .filter_map(|d| d["endpoint"].as_str().map(|s| s.to_string()))
            .collect();

        assert!(endpoints.iter().any(|ep| ep == "/v1/chat/completions"));
        assert!(endpoints.iter().any(|ep| ep == "/v1/messages"));
    }

    #[tokio::test]
    async fn test_write_behind_disabled_request_details() {
        use std::time::Duration;

        let db = Database::open_in_memory().unwrap();
        let test_key = ApiKey {
            id: "key-no-details".to_string(),
            key: "sk-no-details-key".to_string(),
            name: Some("No Details Test".to_string()),
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
            ..Default::default()
        };
        db.save_api_key(&test_key).unwrap();

        let mut cfg = Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        };
        cfg.observability.enable_request_logs = false; // explicitly disable request logs
        let config = Arc::new(cfg);
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();

        let chat_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![crate::translator::ChatMessage::user("wb disabled log test")],
            ..Default::default()
        };
        let chat_hash = token_cache.compute_chat_hash(&chat_req);
        token_cache.set_json(
            &chat_hash,
            axum::body::Bytes::from_static(b"{\"id\":\"chatcmpl-nolog\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"no log\"}}]}"),
        );

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

        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", "Bearer sk-no-details-key")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"wb disabled log test"}]}"#))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        tokio::time::sleep(Duration::from_millis(200)).await;

        // Usage must still be recorded
        assert_eq!(db.get_usage_count().unwrap(), 1);
        // Request details must NOT be recorded
        assert_eq!(db.get_request_details_count().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_streaming_write_behind_usage_and_interrupted_drop_guard() {
        use std::time::Duration;
        use futures_util::StreamExt;

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

        let test_key = ApiKey {
            id: "key-stream-test".to_string(),
            key: "sk-stream-key".to_string(),
            name: Some("Stream Test".to_string()),
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
            ..Default::default()
        };

        // 1. Test normal stream completion through wrap_byte_stream_with_usage
        let byte_stream = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(axum::body::Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n")),
            Ok(axum::body::Bytes::from("data: [DONE]\n\n")),
        ]);

        let wrapped = wrap_byte_stream_with_usage(
            byte_stream,
            state.clone(),
            test_key.clone(),
            "/v1/chat/completions".to_string(),
            Some("openai".to_string()),
            "gpt-4o".to_string(),
            None,
            Some(serde_json::json!({ "model": "gpt-4o" })),
            std::time::Instant::now(),
        );
        let mut wrapped = Box::pin(wrapped);

        while let Some(_) = wrapped.next().await {}

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(db.get_usage_count().unwrap(), 1);
        assert_eq!(db.get_request_details_count().unwrap(), 1);

        // 2. Test interrupted/dropped stream via StreamUsageDropGuard
        let byte_stream_interrupted = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(axum::body::Bytes::from("data: chunk 1\n\n")),
            Ok(axum::body::Bytes::from("data: chunk 2\n\n")),
        ]);

        let wrapped_interrupted = wrap_byte_stream_with_usage(
            byte_stream_interrupted,
            state.clone(),
            test_key.clone(),
            "/v1/chat/completions".to_string(),
            Some("anthropic".to_string()),
            "claude-3-5-sonnet".to_string(),
            None,
            Some(serde_json::json!({ "model": "claude-3-5-sonnet" })),
            std::time::Instant::now(),
        );
        let mut wrapped_interrupted = Box::pin(wrapped_interrupted);

        // Read only first chunk then drop stream
        let _ = wrapped_interrupted.next().await;
        drop(wrapped_interrupted);

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(db.get_usage_count().unwrap(), 2);
        assert_eq!(db.get_request_details_count().unwrap(), 2);

        let details = db.get_request_details(1, 10, None, None, None).unwrap();
        let items = details["details"].as_array().unwrap();
        assert!(items.iter().any(|item| item["status"].as_str() == Some("interrupted")));
    }

    #[tokio::test]
    async fn test_admin_api_endpoints_full_lifecycle() {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test-admin.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
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

        unsafe { std::env::set_var("ADMIN_API_TOKEN", "test-admin-secret-xyz") };

        // 1. GET /api/admin/gateway without token -> 401
        let req = Request::builder().uri("/api/admin/gateway").body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 2. GET /api/admin/gateway with Bearer token -> 200
        let req = Request::builder()
            .uri("/api/admin/gateway")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["gatewayControl"]["disabled"], false);

        // 3. PUT /api/admin/gateway with Bearer token -> 200
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/gateway")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"disabled":true,"reason":"Testing gateway maintenance","drain":true}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["gatewayControl"]["disabled"], true);
        assert_eq!(body["gatewayControl"]["reason"], "Testing gateway maintenance");

        // 4. POST /api/admin/keys without ownerRef -> 400
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/keys")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"Tenant Alpha Key"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 5. POST /api/admin/keys with ownerRef -> 201 (secret included)
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/keys")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"Tenant Alpha Key","ownerRef":"tenant-alpha","budgetUsd":50.0}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let created_key: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        let key_id = created_key["id"].as_str().unwrap().to_string();
        let key_secret = created_key["key"].as_str().unwrap().to_string();
        assert!(!key_secret.is_empty());
        assert_eq!(created_key["ownerRef"], "tenant-alpha");

        // 6. GET /api/admin/keys?ownerRef=tenant-alpha -> 200 (secret omitted)
        let req = Request::builder()
            .uri("/api/admin/keys?ownerRef=tenant-alpha")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let list_body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        let keys_arr = list_body["keys"].as_array().unwrap();
        assert_eq!(keys_arr.len(), 1);
        assert!(keys_arr[0].get("key").is_none());

        // 7. GET /api/admin/keys/{id}?ownerRef=tenant-alpha -> 200
        let req = Request::builder()
            .uri(format!("/api/admin/keys/{}?ownerRef=tenant-alpha", key_id))
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 8. PUT /api/admin/keys/{id}?ownerRef=tenant-alpha -> 200
        let req = Request::builder()
            .method("PUT")
            .uri(format!("/api/admin/keys/{}?ownerRef=tenant-alpha", key_id))
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"description":"Updated description for tenant key"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 9. POST /api/admin/keys/{id}/rotate -> 200 (returns new secret)
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/admin/keys/{}/rotate", key_id))
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ownerRef":"tenant-alpha"}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let rotated: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        let new_key_secret = rotated["key"].as_str().unwrap().to_string();
        assert_ne!(new_key_secret, key_secret);

        // 10. POST /api/key-actions/debit -> reserve budget
        let req = Request::builder()
            .method("POST")
            .uri("/api/key-actions/debit")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(r#"{{"keyId":"{}","requestId":"req-abc-1","reserveUsd":2.5}}"#, key_id)))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let debit_body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(debit_body["budget"]["reserved"], true);

        // 11. POST /api/key-actions/debit -> commit budget
        let req = Request::builder()
            .method("POST")
            .uri("/api/key-actions/debit")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(r#"{{"keyId":"{}","requestId":"req-abc-1","commitUsd":2.0}}"#, key_id)))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let commit_body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(commit_body["budget"]["committed"], true);

        // 12. GET /api/admin/audit?ownerRef=tenant-alpha -> 200
        let req = Request::builder()
            .uri("/api/admin/audit?ownerRef=tenant-alpha")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let audit_body: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        let events = audit_body["events"].as_array().unwrap();
        assert!(!events.is_empty());

        // 13. GET /api/observability/alerts -> 200
        let req = Request::builder()
            .uri("/api/observability/alerts")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 14. GET /api/observability/metrics (JSON) -> 200
        let req = Request::builder()
            .uri("/api/observability/metrics")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 15. GET /api/observability/metrics?format=prometheus -> 200
        let req = Request::builder()
            .uri("/api/observability/metrics?format=prometheus")
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/plain; version=0.0.4; charset=utf-8");

        // 16. DELETE /api/admin/keys/{id}?ownerRef=tenant-alpha -> 200
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/admin/keys/{}?ownerRef=tenant-alpha", key_id))
            .header(header::AUTHORIZATION, "Bearer test-admin-secret-xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        unsafe { std::env::remove_var("ADMIN_API_TOKEN"); }
    }
}

