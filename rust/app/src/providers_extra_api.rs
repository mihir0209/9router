//! Wave 2 Part A: Native Rust implementations for remaining `models/*`, `pricing`,
//! `provider-nodes/*`, `providers/*`, `combos/presets`, and `proxy-pools/health` endpoints.

use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use dashmap::DashMap;
use reqwest::Client;
use ring::{digest, hmac};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use url::Url;

use crate::auth::{resolve_configured_admin_token, verify_internal_auth};
use crate::db::{Combo, Database, ProviderConnection, ProviderNode, ProxyPool};
use crate::media::web::assert_public_url_resolved;
use crate::routes::{options_handler, serialize_connection_for_dashboard, AppState};
use crate::token_refresh::{
    extract_access_token, extract_token_expiry, refresh_oauth_token, sync_model_catalog,
};

// ============================================================================
// Constants & Provider Registry Parity Tables
// ============================================================================

const MODEL_LOCK_PREFIX: &str = "modelLock_";
const MAX_CACHED_MODELS_PER_PROVIDER: usize = 20_000;
const INTERNAL_MODELS_FETCH_HEADER: &str = "x-9r-internal-models-fetch";
const KILO_MODELS_URL: &str = "https://api.kilo.ai/api/gateway/models";
const KILO_CACHE_TTL: Duration = Duration::from_secs(3600);
const CATALOG_SYNC_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
const CATALOG_SYNC_URL: &str = "https://models.dev/api.json";

const DEFAULT_HEALTH_CHECK_INTERVAL_MS: u64 = 5 * 60 * 1000;
const DEFAULT_MAX_CONSECUTIVE_FAILURES: u64 = 3;
const DEFAULT_DELETE_INACTIVE_AFTER_MS: u64 = 24 * 60 * 60 * 1000;

const MAX_OVERRIDE_HEADERS: usize = 20;
const MAX_OVERRIDE_HEADER_VALUE_LEN: usize = 8192;

const BLOCKED_OVERRIDE_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "connection",
    "transfer-encoding",
    "authorization",
    "cookie",
];

const BLOCKED_CUSTOM_MEDIA_HEADERS: &[&str] = &[
    "authorization",
    "content-length",
    "cookie",
    "host",
    "proxy-authorization",
];

const VALID_PRICING_FIELDS: &[&str] = &[
    "input",
    "output",
    "cached",
    "reasoning",
    "cache_creation",
];

const USAGE_SUPPORTED_PROVIDERS: &[&str] = &[
    "antigravity",
    "claude",
    "clinepass",
    "codebuddy-cn",
    "codex",
    "commandcode",
    "cursor",
    "deepseek",
    "gemini-cli",
    "github",
    "glm-cn",
    "glm",
    "grok-cli",
    "groq",
    "kimi",
    "kiro",
    "minimax-cn",
    "minimax",
    "ollama",
    "qoder-cn",
    "opencode-go",
    "opencode-zen",
    "qoder",
    "vercel-ai-gateway",
    "xiaomi-mimo",
    "codebuddy-intl",
    "zed",
    "minimax-code",
    "minimax-code-global",
    "synthetic",
];

const USAGE_APIKEY_PROVIDERS: &[&str] = &[
    "clinepass",
    "codebuddy-cn",
    "commandcode",
    "deepseek",
    "glm-cn",
    "glm",
    "groq",
    "kimi",
    "kiro",
    "minimax-cn",
    "minimax",
    "ollama",
    "qoder-cn",
    "opencode-go",
    "opencode-zen",
    "qoder",
    "vercel-ai-gateway",
    "xiaomi-mimo",
    "codebuddy-intl",
    "synthetic",
];

const FREE_PROVIDER_IDS: &[&str] = &["gemini-cli", "kiro", "mimo-free", "opencode"];

const OAUTH_PROVIDER_IDS: &[&str] = &[
    "antigravity",
    "claude",
    "cline",
    "clinepass",
    "codebuddy-cn",
    "codex",
    "cursor",
    "github",
    "gitlab",
    "glm",
    "grok-cli",
    "iflow",
    "kilocode",
    "kimi",
    "qoder-cn",
    "qoder",
    "xai",
    "xiaomi-mimo",
    "codebuddy-intl",
    "zed",
    "muse",
    "minimax-code",
    "minimax-code-global",
];

const AWS_CREDENTIAL_PROVIDERS: &[&str] = &["bedrock", "bedrock-xai"];

const CLIENT_SAFE_FIELDS: &[&str] = &[
    "id",
    "provider",
    "authType",
    "name",
    "email",
    "displayName",
    "priority",
    "globalPriority",
    "isActive",
    "defaultModel",
    "testStatus",
    "lastError",
    "lastErrorAt",
    "errorCode",
    "expiresAt",
    "lastUsedAt",
    "consecutiveUseCount",
    "createdAt",
    "updatedAt",
];

const CLIENT_SAFE_PSD_FIELDS: &[&str] = &[
    "baseUrl",
    "azureEndpoint",
    "deployment",
    "apiVersion",
    "accountId",
    "region",
    "projectId",
    "resourceUrl",
    "proxyPoolId",
    "connectionProxyEnabled",
    "connectionProxyUrl",
    "connectionNoProxy",
    "githubLogin",
    "githubName",
    "githubEmail",
    "githubUserId",
    "username",
    "firstName",
    "lastName",
    "authMethod",
    "authKind",
    "profileArn",
];

const CU_PRESET_MODELS: &[&str] = &[
    "default",
    "claude-4.5-opus-high-thinking",
    "claude-4.5-opus-high",
    "claude-4.5-sonnet-thinking",
    "claude-4.5-sonnet",
    "claude-4.5-haiku",
    "claude-4.5-opus",
    "gpt-5.2-codex",
    "claude-4.6-opus-max",
    "claude-4.6-sonnet-medium-thinking",
    "kimi-k2.5",
    "gemini-3-flash-preview",
    "gpt-5.2",
    "gpt-5.3-codex",
];

const CC_PRESET_MODELS: &[&str] = &[
    "claude-fable-5-2",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5",
    "claude-sonnet-5-5",
    "claude-sonnet-5",
    "claude-haiku-5-5",
    "claude-haiku-4-5-20251001",
];

const CLAUDE_CLI_DEFAULT_ALIASES: &[(&str, &str)] = &[
    ("fable", "cc/claude-fable-5"),
    ("opus", "cc/claude-opus-5"),
    ("sonnet", "cc/claude-sonnet-5"),
    ("haiku", "cc/claude-haiku-4-5-20251001"),
    ("default", "cc/claude-sonnet-5"),
    ("opusplan", "cc/claude-opus-5"),
];

const WEB_LOGIN_PROVIDERS: &[&str] = &[
    "grok-web",
    "chatgpt-web",
    "gemini-web",
    "muse-web",
    "deepseek-web",
    "uc-web",
];

static ALIAS_TO_ID: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("ag", "antigravity"),
        ("aai", "assemblyai"),
        ("polly", "aws-polly"),
        ("bfl", "black-forest-labs"),
        ("bb", "blackbox"),
        ("brave", "brave-search"),
        ("bpm", "byteplus"),
        ("ch", "chutes"),
        ("cc", "claude"),
        ("cl", "cline"),
        ("cf", "cloudflare-ai"),
        ("cbcn", "codebuddy-cn"),
        ("cx", "codex"),
        ("cmc", "commandcode"),
        ("cu", "cursor"),
        ("dg", "deepgram"),
        ("ds", "deepseek"),
        ("el", "elevenlabs"),
        ("fal", "fal-ai"),
        ("fl", "featherless"),
        ("gc", "gemini-cli"),
        ("gh", "github"),
        ("gpse", "google-pse"),
        ("gcli", "grok-cli"),
        ("grok-build", "grok-cli"),
        ("gb", "grok-cli"),
        ("gw", "grok-web"),
        ("hf", "huggingface"),
        ("hyp", "hyperbolic"),
        ("if", "iflow"),
        ("jina", "jina-ai"),
        ("kc", "kilocode"),
        ("kimi-coding", "kimi"),
        ("kmc", "kimi"),
        ("kr", "kiro"),
        ("mmf", "mimo-free"),
        ("nb", "nanobanana"),
        ("qdcn", "qoder-cn"),
        ("ocg", "opencode-go"),
        ("ocz", "opencode-zen"),
        ("oc", "opencode"),
        ("or", "openrouter"),
        ("pw", "perplexity-web"),
        ("pplx", "perplexity"),
        ("pplx-agent", "perplexity-agent"),
        ("pplx-responses", "perplexity-agent"),
        ("qd", "qoder"),
        ("runway", "runwayml"),
        ("stability", "stability-ai"),
        ("vn", "venice"),
        ("vercel", "vercel-ai-gateway"),
        ("vxp", "vertex-partner"),
        ("vx", "vertex"),
        ("ark", "volcengine-ark"),
        ("mimo", "xiaomi-mimo"),
        ("mimo-desktop", "xiaomi-mimo"),
        ("xmd", "xiaomi-mimo"),
        ("xmtp", "xiaomi-tokenplan"),
        ("cbai", "codebuddy-intl"),
        ("zd", "zed"),
        ("af", "api-airforce"),
        ("airforce", "api-airforce"),
        ("qianfan", "baidu"),
        ("ernie", "baidu"),
        ("baidu-qianfan", "baidu"),
        ("bzl", "bazaarlink"),
        ("bazaar-link", "bazaarlink"),
        ("bm", "bluesminds"),
        ("blue-sminds", "bluesminds"),
        ("kgw", "kilo-gateway"),
        ("kilogateway", "kilo-gateway"),
        ("llm-7", "llm7"),
        ("samba", "sambanova"),
        ("sambanova-ai", "sambanova"),
        ("hunyuan", "tencent"),
        ("tencent-hunyuan", "tencent"),
        ("morphllm", "morph"),
        ("ps", "poolside"),
        ("tr", "tokenrouter"),
        ("fish", "fish-audio"),
        ("th", "tokenharbor"),
        ("thh", "tokenharbor"),
        ("dahl-inference", "dahl"),
        ("atria-asi", "atria"),
        ("agnes-ai", "agnes"),
        ("b-ai", "bai"),
        ("systemone", "v1m"),
        ("jev", "v1m"),
        ("muse-ai", "muse"),
        ("meta-model-api", "muse"),
        ("muse-code", "muse"),
        ("muse-subscription", "muse"),
        ("mm", "minimax-code"),
        ("mmg", "minimax-code-global"),
        ("br", "bedrock"),
        ("aws-bedrock", "bedrock"),
        ("brx", "bedrock-xai"),
        ("bedrock-grok", "bedrock-xai"),
        ("nous-portal", "nous"),
        ("nous-research", "nous"),
        ("ns", "nous"),
        ("ds-web", "deepseek-web"),
        ("gweb", "gemini-web"),
        ("mu", "muse-web"),
        ("cg", "chatgpt-web"),
        ("veo-free", "veoaifree-web"),
        ("uc", "uc-web"),
        ("rc", "runcrate"),
        ("drex", "nace"),
        ("syn", "synthetic"),
        ("synthetic-new", "synthetic"),
    ])
});

static ID_TO_ALIAS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("antigravity", "ag"),
        ("aws-polly", "polly"),
        ("brave-search", "brave"),
        ("claude", "cc"),
        ("cline", "cl"),
        ("codebuddy-cn", "cbcn"),
        ("codex", "cx"),
        ("cursor", "cu"),
        ("elevenlabs", "el"),
        ("gemini-cli", "gc"),
        ("github", "gh"),
        ("google-pse", "gpse"),
        ("grok-cli", "gcli"),
        ("iflow", "if"),
        ("jina-ai", "jina"),
        ("kilocode", "kc"),
        ("kiro", "kr"),
        ("mimo-free", "mmf"),
        ("qoder-cn", "qdcn"),
        ("opencode-zen", "ocz"),
        ("opencode", "oc"),
        ("qoder", "qd"),
        ("codebuddy-intl", "cbai"),
        ("zed", "zd"),
        ("api-airforce", "af"),
        ("baidu", "qianfan"),
        ("bazaarlink", "bzl"),
        ("bluesminds", "bm"),
        ("kilo-gateway", "kgw"),
        ("sambanova", "samba"),
        ("tencent", "hunyuan"),
        ("fish-audio", "fish"),
        ("minimax-code", "mm"),
        ("minimax-code-global", "mmg"),
        ("bedrock", "br"),
        ("bedrock-xai", "brx"),
        ("veoaifree-web", "veo-free"),
        ("uc-web", "uc"),
    ])
});

// ============================================================================
// Shared In-Memory State (Catalog Sync, Kilo Cache, Web Login, Proxy Health)
// ============================================================================

#[derive(Debug, Clone, Default)]
struct CatalogSyncState {
    running: bool,
    last_sync_ms: Option<i64>,
    last_error: Option<String>,
    last_result: Option<Value>,
    etag: Option<String>,
    file_version: Option<u32>,
}

static CATALOG_SYNC_STATE: LazyLock<Mutex<CatalogSyncState>> =
    LazyLock::new(|| Mutex::new(CatalogSyncState::default()));

#[derive(Debug, Clone)]
struct KiloModelsCache {
    models: Vec<Value>,
    fetched_at: Instant,
}

static KILO_MODELS_CACHE: LazyLock<Mutex<Option<KiloModelsCache>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone)]
struct WebLoginSession {
    profile: String,
    status: String,
    error: Option<String>,
    credential: Option<String>,
    saved_connection_id: Option<String>,
    account_email: Option<String>,
    cancelled: Arc<AtomicBool>,
}

static WEB_LOGIN_SESSIONS: LazyLock<DashMap<String, WebLoginSession>> = LazyLock::new(DashMap::new);

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyHealthStats {
    failures: u64,
    successes: u64,
    last_check: Option<i64>,
    last_success: Option<i64>,
    last_failure: Option<i64>,
    avg_latency: f64,
    total_checks: u64,
}

static PROXY_HEALTH_STATS: LazyLock<DashMap<String, ProxyHealthStats>> = LazyLock::new(DashMap::new);
static PROXY_HEALTH_RUNNING: AtomicBool = AtomicBool::new(false);

// ============================================================================
// Common Helpers
// ============================================================================

fn json_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn http_client(timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .unwrap_or_default()
}

fn resolve_provider_alias(alias_or_id: &str) -> String {
    ALIAS_TO_ID
        .get(alias_or_id)
        .copied()
        .unwrap_or(alias_or_id)
        .to_string()
}

fn is_openai_compatible(provider_id: &str) -> bool {
    provider_id.starts_with("openai-compatible-")
}

fn is_anthropic_compatible(provider_id: &str) -> bool {
    provider_id.starts_with("anthropic-compatible-")
}

fn is_compatible_provider(provider_id: &str) -> bool {
    is_openai_compatible(provider_id) || is_anthropic_compatible(provider_id)
}

fn uses_aws_credential_form(provider: &str) -> bool {
    AWS_CREDENTIAL_PROVIDERS.contains(&provider)
}

fn is_loopback_host_str(raw: &str) -> bool {
    let mut name = raw.trim().to_ascii_lowercase();
    if name.starts_with('[') {
        if let Some(end) = name.find(']') {
            name = name[1..end].to_string();
        } else {
            return false;
        }
    } else if let Some((h, port)) = name.rsplit_once(':') {
        if port.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') {
            name = h.to_string();
        }
    }
    if let Some(stripped) = name.strip_prefix("::ffff:") {
        name = stripped.to_string();
    }
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn is_local_request(headers: &HeaderMap) -> bool {
    if headers.contains_key("x-9r-via-proxy") {
        return false;
    }
    if let Some(real_ip) = headers.get("x-9r-real-ip").and_then(|v| v.to_str().ok()) {
        if !is_loopback_host_str(real_ip) {
            return false;
        }
    } else if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        if !is_loopback_host_str(host) {
            return false;
        }
    } else {
        return false;
    }

    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        match Url::parse(origin) {
            Ok(parsed) => {
                if !parsed
                    .host_str()
                    .map(is_loopback_host_str)
                    .unwrap_or(false)
                {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

fn parse_raw_json_obj(raw: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn get_psd_object(conn: &ProviderConnection) -> Map<String, Value> {
    if let Some(psd) = conn
        .data
        .extra
        .get("providerSpecificData")
        .and_then(|v| v.as_object())
    {
        return psd.clone();
    }
    let raw = parse_raw_json_obj(&conn.raw_data);
    raw.get("providerSpecificData")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

fn get_raw_field_str(conn: &ProviderConnection, key: &str) -> Option<String> {
    conn.data
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            parse_raw_json_obj(&conn.raw_data)
                .get(key)
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
        })
}

fn set_connection_json_field(conn: &mut ProviderConnection, key: &str, val: Option<Value>) {
    let mut raw_obj = parse_raw_json_obj(&conn.raw_data);
    match val {
        Some(v) => {
            raw_obj.insert(key.to_string(), v.clone());
            conn.data.extra.insert(key.to_string(), v);
        }
        None => {
            raw_obj.remove(key);
            conn.data.extra.remove(key);
        }
    }
    conn.raw_data = serde_json::to_string(&Value::Object(raw_obj)).unwrap_or_default();
}

fn serialize_provider_connection_response(conn: &ProviderConnection) -> Value {
    let mut obj = match serialize_connection_for_dashboard(conn) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    if uses_aws_credential_form(&conn.provider) {
        if let Some(Value::Object(psd)) = obj.get_mut("providerSpecificData") {
            psd.remove("sessionToken");
        }
    }
    Value::Object(obj)
}

fn find_connection_by_id(db: &Database, id: &str) -> anyhow::Result<Option<ProviderConnection>> {
    let all = db.load_provider_connections()?;
    Ok(all.into_iter().find(|c| c.id == id))
}

fn find_provider_node_flexible(db: &Database, id: &str) -> anyhow::Result<Option<ProviderNode>> {
    if let Some(exact) = db.get_provider_node_by_id(id)? {
        return Ok(Some(exact));
    }
    let all = db.load_provider_nodes()?;
    let id_lower = id.to_ascii_lowercase();
    let slug = |s: &str| -> String {
        let mut out = String::new();
        let mut prev_dash = false;
        for ch in s.to_ascii_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
        out.trim_matches('-').to_string()
    };
    Ok(all.into_iter().find(|n| {
        n.id == id
            || n.prefix == id
            || n.data.get("prefix").and_then(|v| v.as_str()) == Some(id)
            || n.name
                .as_deref()
                .map(|nm| nm.to_ascii_lowercase() == id_lower || slug(nm) == id_lower)
                .unwrap_or(false)
    }))
}

fn sanitize_custom_headers_map(raw: Option<&Value>) -> Map<String, Value> {
    let mut result = Map::new();
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return result;
    };
    for (raw_name, raw_val) in obj {
        let name = raw_name.trim();
        let val = match raw_val.as_str() {
            Some(s) => s.trim().to_string(),
            None => continue,
        };
        if name.is_empty()
            || val.is_empty()
            || BLOCKED_CUSTOM_MEDIA_HEADERS.contains(&name.to_ascii_lowercase().as_str())
            || name.contains('\r')
            || name.contains('\n')
            || val.contains('\r')
            || val.contains('\n')
        {
            continue;
        }
        result.insert(name.to_string(), Value::String(val));
    }
    result
}

fn sanitize_retry_config(raw: Option<&Value>) -> Option<Option<Value>> {
    let v = raw?;
    if v.is_null() || v == &Value::Bool(false) || v == &Value::String(String::new()) {
        return Some(None);
    }
    let obj = v.as_object()?;
    let http_retry = obj.get("httpRetry").or_else(|| obj.get("retry"));
    let error_rules = obj.get("errorRules").or_else(|| obj.get("rules"));
    let has_http = http_retry
        .and_then(|h| h.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    let has_rules = error_rules
        .and_then(|r| r.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_http && !has_rules {
        return Some(None);
    }
    let mut out = Map::new();
    out.insert(
        "httpRetry".to_string(),
        http_retry.cloned().unwrap_or_else(|| json!({})),
    );
    out.insert(
        "errorRules".to_string(),
        error_rules.cloned().unwrap_or_else(|| json!([])),
    );
    Some(Some(Value::Object(out)))
}

// ============================================================================
// 1. PUT /api/models (PARTIAL_METHODS handler)
// ============================================================================

/// `PUT /api/models` — Update model alias.
pub async fn models_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let alias = body
        .get("alias")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");

    if model.is_empty() || alias.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Model and alias required");
    }

    let aliases = match state.db.load_model_aliases() {
        Ok(a) => a,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    if aliases
        .iter()
        .any(|(key, val)| val == alias && key != model)
    {
        return json_err(StatusCode::BAD_REQUEST, "Alias already in use");
    }

    match state.db.set_model_alias(alias, model) {
        Ok(()) => Json(json!({ "success": true, "model": model, "alias": alias })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ============================================================================
// 2. /api/models/availability (GET, POST)
// ============================================================================

struct ModelLockInfo {
    model: String,
    until: String,
}

fn get_active_model_locks(conn: &ProviderConnection) -> Vec<ModelLockInfo> {
    let now_ms = Utc::now().timestamp_millis();
    let raw_obj = parse_raw_json_obj(&conn.raw_data);
    let mut locks = Vec::new();
    let mut seen = HashSet::new();

    for (k, v) in raw_obj.iter().chain(conn.data.extra.iter()) {
        if !k.starts_with(MODEL_LOCK_PREFIX) || seen.contains(k) {
            continue;
        }
        seen.insert(k.clone());
        let Some(until_str) = v.as_str() else {
            continue;
        };
        if until_str.trim().is_empty() {
            continue;
        }
        let active = chrono::DateTime::parse_from_rfc3339(until_str)
            .map(|dt| dt.timestamp_millis() > now_ms)
            .unwrap_or(false);
        if active {
            let model_part = &k[MODEL_LOCK_PREFIX.len()..];
            locks.push(ModelLockInfo {
                model: if model_part.is_empty() {
                    "__all".to_string()
                } else {
                    model_part.to_string()
                },
                until: until_str.to_string(),
            });
        }
    }
    locks
}

/// `GET /api/models/availability`
pub async fn models_availability_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let connections = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to fetch model availability: {e}"),
            )
        }
    };

    let mut models = Vec::new();
    for conn in &connections {
        let locks = get_active_model_locks(conn);
        let conn_name = conn
            .name
            .clone()
            .or_else(|| conn.email.clone())
            .unwrap_or_else(|| conn.id.clone());
        let last_error = get_raw_field_str(conn, "lastError")
            .map(Value::String)
            .unwrap_or(Value::Null);
        let test_status = get_raw_field_str(conn, "testStatus");

        for lock in &locks {
            models.push(json!({
                "provider": conn.provider,
                "model": lock.model,
                "status": "cooldown",
                "until": lock.until,
                "connectionId": conn.id,
                "connectionName": conn_name,
                "lastError": last_error,
            }));
        }

        if locks.is_empty() && test_status.as_deref() == Some("unavailable") {
            models.push(json!({
                "provider": conn.provider,
                "model": "__all",
                "status": "unavailable",
                "connectionId": conn.id,
                "connectionName": conn_name,
                "lastError": last_error,
            }));
        }
    }

    let count = models.len();
    Json(json!({
        "models": models,
        "unavailableCount": count,
    }))
    .into_response()
}

/// `POST /api/models/availability`
pub async fn models_availability_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");

    if action != "clearCooldown" || provider.is_empty() || model.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Invalid request");
    }

    let connections = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to clear cooldown: {e}"),
            )
        }
    };

    let lock_key = format!("{MODEL_LOCK_PREFIX}{model}");
    for mut conn in connections.into_iter().filter(|c| c.provider == provider) {
        let has_lock = conn
            .data
            .extra
            .get(&lock_key)
            .map(|v| !v.is_null())
            .unwrap_or(false)
            || parse_raw_json_obj(&conn.raw_data)
                .get(&lock_key)
                .map(|v| !v.is_null())
                .unwrap_or(false);
        if !has_lock {
            continue;
        }
        set_connection_json_field(&mut conn, &lock_key, Some(Value::Null));
        if get_raw_field_str(&conn, "testStatus").as_deref() == Some("unavailable") {
            set_connection_json_field(&mut conn, "testStatus", Some(json!("active")));
            set_connection_json_field(&mut conn, "lastError", Some(Value::Null));
            set_connection_json_field(&mut conn, "lastErrorAt", Some(Value::Null));
            set_connection_json_field(&mut conn, "backoffLevel", Some(json!(0)));
        }
        conn.updated_at = Utc::now().to_rfc3339();
        if let Err(e) = state.db.save_provider_connection(&conn) {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to clear cooldown: {e}"),
            );
        }
    }

    Json(json!({ "ok": true })).into_response()
}

// ============================================================================
// 3. /api/models/catalog-sync (GET, POST)
// ============================================================================

fn resolve_catalog_file_paths(data_dir: &FsPath) -> (PathBuf, PathBuf) {
    (
        data_dir.join("model-catalog.json"),
        data_dir.join("models_catalog.json"),
    )
}

fn read_catalog_summary(data_dir: &FsPath) -> Value {
    let (node_path, rust_path) = resolve_catalog_file_paths(data_dir);
    for path in [&node_path, &rust_path] {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if let Some(obj) = parsed.as_object() {
            if obj.contains_key("models") || obj.contains_key("providers") {
                let models = obj
                    .get("models")
                    .and_then(|v| v.as_object())
                    .map(|m| m.len())
                    .unwrap_or(0);
                let providers = obj
                    .get("providers")
                    .and_then(|v| v.as_object())
                    .map(|p| p.len())
                    .unwrap_or(0);
                let synced_at = obj.get("syncedAt").cloned().unwrap_or(Value::Null);
                return json!({
                    "syncedAt": synced_at,
                    "models": models,
                    "providers": providers,
                    "bytes": bytes.len(),
                });
            } else {
                // Raw models.dev/api.json shape stored by token_refresh::sync_model_catalog
                let providers = obj.len();
                let models: usize = obj
                    .values()
                    .filter_map(|p| p.get("models").and_then(|m| m.as_object()).map(|m| m.len()))
                    .sum();
                let synced_at = std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| json!(d.as_millis() as i64))
                    .unwrap_or(Value::Null);
                return json!({
                    "syncedAt": synced_at,
                    "models": models,
                    "providers": providers,
                    "bytes": bytes.len(),
                });
            }
        }
    }
    Value::Null
}

/// `GET /api/models/catalog-sync`
pub async fn models_catalog_sync_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let (node_catalog_file, _) = resolve_catalog_file_paths(&state.config.data_dir);
    let catalog = read_catalog_summary(&state.config.data_dir);
    let guard = CATALOG_SYNC_STATE.lock().unwrap_or_else(|e| e.into_inner());
    Json(json!({
        "running": guard.running,
        "lastSync": guard.last_sync_ms,
        "lastError": guard.last_error,
        "lastResult": guard.last_result,
        "etag": guard.etag,
        "fileVersion": guard.file_version,
        "file": node_catalog_file.display().to_string(),
        "url": CATALOG_SYNC_URL,
        "intervalMs": CATALOG_SYNC_INTERVAL_MS,
        "catalog": catalog,
    }))
    .into_response()
}

/// `POST /api/models/catalog-sync`
pub async fn models_catalog_sync_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let current_etag = {
        let mut guard = CATALOG_SYNC_STATE.lock().unwrap_or_else(|e| e.into_inner());
        if guard.running {
            let msg = guard
                .last_error
                .clone()
                .unwrap_or_else(|| "sync in progress".to_string());
            return json_err(StatusCode::SERVICE_UNAVAILABLE, msg);
        }
        guard.running = true;
        guard.etag.clone()
    };

    let client = http_client(60);
    let res = sync_model_catalog(
        &client,
        &state.config.data_dir,
        &state.config.token_refresh,
        current_etag.as_deref(),
    )
    .await;

    let mut guard = CATALOG_SYNC_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.running = false;
    if res.status == "error" {
        guard.last_error = res.error.clone();
        let err_msg = res
            .error
            .unwrap_or_else(|| "catalog sync failed".to_string());
        return json_err(StatusCode::SERVICE_UNAVAILABLE, err_msg);
    }

    guard.last_sync_ms = Some(Utc::now().timestamp_millis());
    guard.last_error = None;
    if res.etag.is_some() {
        guard.etag = res.etag.clone();
    }
    let result_json = json!({
        "status": res.status,
        "etag": res.etag,
        "models": res.models_count,
        "providers": res.providers_count,
        "filePath": res.file_path,
    });
    guard.last_result = Some(result_json.clone());

    Json(json!({
        "success": true,
        "result": result_json,
    }))
    .into_response()
}

// ============================================================================
// 4. /api/models/refresh (GET, POST)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ModelsRefreshQuery {
    pub provider: Option<String>,
}

fn has_live_model_source(provider: &str, conn: Option<&ProviderConnection>) -> bool {
    let live_providers = [
        "kiro",
        "qoder",
        "qoder-cn",
        "kimchi",
        "github",
        "clinepass",
        "cline",
        "grok-cli",
        "cursor",
        "zed",
        "commandcode",
    ];
    if live_providers.contains(&provider) {
        return true;
    }
    let has_key = conn
        .and_then(|c| c.data.api_key.as_deref())
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    has_key && is_compatible_provider(provider)
}

fn normalize_models_list_source(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("live") => "live",
        Some("static") => "static",
        _ => "cached",
    }
}

fn load_all_provider_model_caches(db: &Database) -> anyhow::Result<HashMap<String, Value>> {
    let raw_map = db.kv_scope_map("providerModelCache")?;
    let mut out = HashMap::new();
    for (provider, val_str) in raw_map {
        if let Ok(val) = serde_json::from_str::<Value>(&val_str) {
            if val.is_object() {
                out.insert(provider, val);
            }
        }
    }
    Ok(out)
}

fn parse_openai_style_models(data: &Value) -> Vec<Value> {
    if let Some(arr) = data.as_array() {
        return arr.clone();
    }
    data.get("data")
        .or_else(|| data.get("models"))
        .or_else(|| data.get("results"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

async fn fetch_compatible_model_entries(conn: &ProviderConnection) -> Vec<Value> {
    let api_key = conn.data.api_key.as_deref().unwrap_or("").trim();
    if api_key.is_empty() {
        return Vec::new();
    }
    let psd = get_psd_object(conn);
    let base_url = psd
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .or(conn.data.base_url.as_deref())
        .unwrap_or("")
        .trim()
        .trim_end_matches('/');
    if base_url.is_empty() {
        return Vec::new();
    }

    let mut url = format!("{base_url}/models");
    let client = http_client(5);
    let mut req = client
        .get(&url)
        .header("Content-Type", "application/json")
        .header(INTERNAL_MODELS_FETCH_HEADER, "1");

    if is_openai_compatible(&conn.provider) {
        req = req.header(header::AUTHORIZATION, format!("Bearer {api_key}"));
    } else if is_anthropic_compatible(&conn.provider) {
        if url.ends_with("/messages/models") {
            url = url[..url.len() - 9].to_string();
        } else if url.ends_with("/messages") {
            url = format!("{}/models", &url[..url.len() - 9]);
        }
        req = client
            .get(&url)
            .header("Content-Type", "application/json")
            .header(INTERNAL_MODELS_FETCH_HEADER, "1")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"));
    } else {
        return Vec::new();
    }

    let Ok(resp) = req.send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(data) = resp.json::<Value>().await else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in parse_openai_style_models(&data) {
        let id = item
            .get("id")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("model"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("id".to_string(), json!(id));
        if let Some(name) = item.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            entry.insert("name".to_string(), json!(name));
        }
        out.push(Value::Object(entry));
    }
    out
}

async fn resolve_live_provider_models(conn: &ProviderConnection, provider: &str) -> (Vec<Value>, &'static str, Option<String>) {
    let psd = get_psd_object(conn);
    let configured = psd
        .get("models")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .or_else(|| {
            psd.get("enabledModels")
                .and_then(|v| v.as_array())
                .filter(|a| !a.is_empty())
        });
    if let Some(arr) = configured {
        let mut seen = HashSet::new();
        let models: Vec<Value> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty() && seen.insert((*id).to_string()))
            .map(|id| json!({ "id": id }))
            .collect();
        return (models, "configured", None);
    }

    if is_compatible_provider(provider) {
        let fetched = fetch_compatible_model_entries(conn).await;
        return (fetched, "live", None);
    }

    // Live catalog probes for known live-source providers
    let client = http_client(8);
    match provider {
        "github" => {
            if let Some(tok) = extract_access_token(conn) {
                if let Ok(resp) = client
                    .get("https://api.githubcopilot.com/models")
                    .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                    .header("User-Agent", "GitHubCopilotChat/0.38.0")
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        if let Ok(val) = resp.json::<Value>().await {
                            let list = parse_openai_style_models(&val);
                            let models: Vec<Value> = list
                                .into_iter()
                                .filter_map(|m| {
                                    let id = m.get("id")?.as_str()?.trim();
                                    if id.is_empty() {
                                        None
                                    } else {
                                        Some(json!({ "id": id, "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(id) }))
                                    }
                                })
                                .collect();
                            if !models.is_empty() {
                                return (models, "live", None);
                            }
                        }
                    }
                }
            }
        }
        "commandcode" => {
            if let Ok(resp) = client
                .get("https://api.commandcode.ai/alpha/models")
                .header(header::ACCEPT, "application/json")
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(val) = resp.json::<Value>().await {
                        let models: Vec<Value> = parse_openai_style_models(&val)
                            .into_iter()
                            .filter_map(|m| {
                                let id = m.get("id")?.as_str()?.trim();
                                (!id.is_empty()).then(|| json!({ "id": id, "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(id) }))
                            })
                            .collect();
                        if !models.is_empty() {
                            return (models, "live", None);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // Fallback static entries from cursor/claude or empty
    let static_ids: &[&str] = match provider {
        "cursor" => CU_PRESET_MODELS,
        "claude" => CC_PRESET_MODELS,
        _ => &[],
    };
    let models: Vec<Value> = static_ids.iter().map(|id| json!({ "id": id })).collect();
    (models, "static", None)
}

/// `GET /api/models/refresh`
pub async fn models_refresh_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ModelsRefreshQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let caches = match load_all_provider_model_caches(&state.db) {
        Ok(c) => c,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read model cache state: {e}"),
            )
        }
    };
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let connections = state.db.load_provider_connections().unwrap_or_default();
    let active_providers: HashSet<String> = connections
        .iter()
        .filter(|c| c.is_active)
        .map(|c| c.provider.clone())
        .collect();

    let only = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let selected: Vec<(&String, &Value)> = match only {
        Some(p) => caches
            .get_key_value(p)
            .into_iter()
            .collect(),
        None => caches.iter().collect(),
    };

    let entries: Vec<Value> = selected
        .into_iter()
        .map(|(provider, entry)| {
            let models_arr = entry
                .get("models")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let fetched_at = entry.get("fetchedAt").cloned().unwrap_or(Value::Null);
            let conn_ref = connections.iter().find(|c| c.provider == *provider);
            let mut obj = Map::new();
            obj.insert("provider".to_string(), json!(provider));
            obj.insert("count".to_string(), json!(models_arr.len()));
            obj.insert("fetchedAt".to_string(), fetched_at);
            obj.insert(
                "providerActive".to_string(),
                json!(active_providers.contains(provider)),
            );
            obj.insert(
                "canRefresh".to_string(),
                json!(has_live_model_source(provider, conn_ref)),
            );
            if only.is_some() {
                let ids: Vec<Value> = models_arr
                    .iter()
                    .filter_map(|m| m.get("id").cloned())
                    .collect();
                obj.insert("ids".to_string(), Value::Array(ids));
            }
            Value::Object(obj)
        })
        .collect();

    let source = normalize_models_list_source(
        settings.get("modelsListSource").and_then(|v| v.as_str()),
    );

    Json(json!({
        "modelsListSource": source,
        "providers": entries,
    }))
    .into_response()
}

/// `POST /api/models/refresh`
pub async fn models_refresh_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let provider_filter: Option<String> = serde_json::from_slice::<Value>(&body_bytes)
        .ok()
        .and_then(|v| {
            v.get("provider")
                .and_then(|p| p.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        });

    let connections = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to refresh models: {e}"),
            )
        }
    };

    let mut by_provider: HashMap<String, ProviderConnection> = HashMap::new();
    for conn in connections.into_iter().filter(|c| c.is_active) {
        by_provider.entry(conn.provider.clone()).or_insert(conn);
    }

    if let Some(ref target) = provider_filter {
        if !by_provider.contains_key(target) {
            return json_err(
                StatusCode::NOT_FOUND,
                format!("No active connection for provider \"{}\"", target),
            );
        }
    }

    let targets: Vec<String> = match provider_filter {
        Some(ref p) => vec![p.clone()],
        None => by_provider.keys().cloned().collect(),
    };

    let mut results = Vec::new();
    for target in targets {
        let Some(conn) = by_provider.get(&target) else {
            continue;
        };
        if !has_live_model_source(&target, Some(conn)) {
            if provider_filter.is_some() {
                let _ = state.db.kv_delete("providerModelCache", &target);
            }
            results.push(json!({
                "provider": target,
                "refreshed": false,
                "reason": "no-live-source",
                "count": 0,
            }));
            continue;
        }

        let (models, source, err) = resolve_live_provider_models(conn, &target).await;
        if models.is_empty() {
            results.push(json!({
                "provider": target,
                "refreshed": false,
                "count": 0,
                "source": source,
                "reason": "empty",
                "truncated": false,
                "totalDiscovered": 0,
                "error": err,
            }));
            continue;
        }

        let total = models.len();
        let truncated = total > MAX_CACHED_MODELS_PER_PROVIDER;
        let capped: Vec<Value> = models
            .into_iter()
            .take(MAX_CACHED_MODELS_PER_PROVIDER)
            .collect();
        let count = capped.len();
        let payload = json!({
            "models": capped,
            "fetchedAt": Utc::now().to_rfc3339(),
        });
        match state.db.kv_upsert(
            "providerModelCache",
            &target,
            &serde_json::to_string(&payload).unwrap_or_default(),
        ) {
            Ok(()) => {
                results.push(json!({
                    "provider": target,
                    "refreshed": true,
                    "count": count,
                    "source": source,
                    "reason": Value::Null,
                    "truncated": truncated,
                    "totalDiscovered": total,
                    "error": err,
                }));
            }
            Err(e) => {
                results.push(json!({
                    "provider": target,
                    "refreshed": false,
                    "reason": "error",
                    "error": e.to_string(),
                }));
            }
        }
    }

    Json(json!({ "ok": true, "results": results })).into_response()
}

// ============================================================================
// 5. PATCH / DELETE /api/pricing (PARTIAL_METHODS handlers)
// ============================================================================

fn load_user_pricing_map(db: &Database) -> anyhow::Result<Map<String, Value>> {
    let raw = db.kv_scope_map("pricing")?;
    let mut map = Map::new();
    for (provider, val_str) in raw {
        if let Ok(val) = serde_json::from_str::<Value>(&val_str) {
            if val.is_object() {
                map.insert(provider, val);
            }
        }
    }
    Ok(map)
}

fn default_pricing_map() -> Map<String, Value> {
    let mut defaults = Map::new();
    defaults.insert(
        "openai".to_string(),
        json!({
            "gpt-4o": { "input": 2.5, "output": 10.0, "cached": 1.25 },
            "gpt-4o-mini": { "input": 0.15, "output": 0.6, "cached": 0.075 }
        }),
    );
    defaults.insert(
        "anthropic".to_string(),
        json!({
            "claude-3-5-sonnet-20241022": { "input": 3.0, "output": 15.0, "cached": 0.3 },
            "claude-3-5-haiku-20241022": { "input": 0.8, "output": 4.0, "cached": 0.08 }
        }),
    );
    defaults.insert(
        "google".to_string(),
        json!({
            "gemini-2.5-pro": { "input": 1.25, "output": 5.0, "cached": 0.3125 },
            "gemini-2.5-flash": { "input": 0.075, "output": 0.3, "cached": 0.01875 }
        }),
    );
    defaults.insert(
        "deepseek".to_string(),
        json!({
            "deepseek-chat": { "input": 0.14, "output": 0.28, "cached": 0.014 },
            "deepseek-reasoner": { "input": 0.55, "output": 2.19, "cached": 0.14 }
        }),
    );
    defaults.insert(
        "muse".to_string(),
        json!({
            "muse-spark-1.3-contributor": { "input": 0.10, "output": 0.20, "cached": 0.002, "reasoning": 0.20, "cache_creation": 0 },
            "muse-spark-1.2-contributor": { "input": 0.10, "output": 0.20, "cached": 0.002, "reasoning": 0.20, "cache_creation": 0 }
        }),
    );
    defaults.insert(
        "gh".to_string(),
        json!({
            "gpt-5.3-codex": { "input": 1.75, "output": 14.00, "cached": 0.175, "reasoning": 14.00, "cache_creation": 1.75 }
        }),
    );
    defaults
}

fn get_merged_pricing(db: &Database) -> anyhow::Result<Value> {
    let user_pricing = load_user_pricing_map(db)?;
    let mut merged = default_pricing_map();

    for (provider, user_models_val) in user_pricing {
        let Some(user_models) = user_models_val.as_object() else {
            continue;
        };
        let target_provider = merged
            .entry(provider)
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(target_models) = target_provider.as_object_mut() {
            for (model, rate_val) in user_models {
                if let (Some(existing_rate), Some(incoming_rate)) = (
                    target_models.get(model).and_then(|v| v.as_object()).cloned(),
                    rate_val.as_object(),
                ) {
                    let mut combined = existing_rate;
                    for (k, v) in incoming_rate {
                        combined.insert(k.clone(), v.clone());
                    }
                    target_models.insert(model.clone(), Value::Object(combined));
                } else {
                    target_models.insert(model.clone(), rate_val.clone());
                }
            }
        }
    }

    Ok(Value::Object(merged))
}

/// `PATCH /api/pricing` — Update pricing configuration.
pub async fn pricing_patch_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let Some(providers_obj) = body.as_object() else {
        return json_err(StatusCode::BAD_REQUEST, "Invalid pricing data format");
    };

    for (provider, models_val) in providers_obj {
        let Some(models_obj) = models_val.as_object() else {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("Invalid pricing for provider: {provider}"),
            );
        };
        for (model, pricing_val) in models_obj {
            let Some(pricing_obj) = pricing_val.as_object() else {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid pricing for model: {provider}/{model}"),
                );
            };
            for (key, value) in pricing_obj {
                if !VALID_PRICING_FIELDS.contains(&key.as_str()) {
                    return json_err(
                        StatusCode::BAD_REQUEST,
                        format!("Invalid pricing field: {key} for {provider}/{model}"),
                    );
                }
                let valid_num = value
                    .as_f64()
                    .map(|n| n.is_finite() && n >= 0.0)
                    .unwrap_or(false);
                if !valid_num {
                    return json_err(
                        StatusCode::BAD_REQUEST,
                        format!(
                            "Invalid pricing value for {key} in {provider}/{model}: must be non-negative number"
                        ),
                    );
                }
            }
        }
    }

    let current_user = load_user_pricing_map(&state.db).unwrap_or_default();
    for (provider, models_val) in providers_obj {
        let mut merged_provider = current_user
            .get(provider)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(models_obj) = models_val.as_object() {
            for (model, rate) in models_obj {
                merged_provider.insert(model.clone(), rate.clone());
            }
        }
        let encoded = serde_json::to_string(&Value::Object(merged_provider)).unwrap_or_default();
        if let Err(e) = state.db.kv_upsert("pricing", provider, &encoded) {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to update pricing: {e}"),
            );
        }
    }

    match load_user_pricing_map(&state.db) {
        Ok(updated) => Json(Value::Object(updated)).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to update pricing: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct PricingDeleteQuery {
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// `DELETE /api/pricing` — Reset pricing to defaults (optional `?provider=...&model=...`).
pub async fn pricing_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PricingDeleteQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let provider = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let op_res = match (provider, model) {
        (Some(p), Some(m)) => {
            let mut map = load_user_pricing_map(&state.db).unwrap_or_default();
            if let Some(Value::Object(mut models_obj)) = map.remove(p) {
                models_obj.remove(m);
                if models_obj.is_empty() {
                    state.db.kv_delete("pricing", p)
                } else {
                    let encoded =
                        serde_json::to_string(&Value::Object(models_obj)).unwrap_or_default();
                    state.db.kv_upsert("pricing", p, &encoded)
                }
            } else {
                Ok(())
            }
        }
        (Some(p), None) => state.db.kv_delete("pricing", p),
        (None, _) => state.db.kv_clear_scope("pricing"),
    };

    if let Err(e) = op_res {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to reset pricing: {e}"),
        );
    }

    match get_merged_pricing(&state.db) {
        Ok(pricing) => Json(pricing).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to reset pricing: {e}"),
        ),
    }
}

// ============================================================================
// 6. PUT /api/provider-nodes/{id} (PARTIAL_METHODS handler)
// ============================================================================

fn node_to_response_json(node: &ProviderNode) -> Value {
    let mut obj = match &node.data {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    obj.insert("id".to_string(), json!(node.id));
    obj.insert("type".to_string(), json!(node.node_type));
    obj.insert("name".to_string(), json!(node.name));
    obj.insert("prefix".to_string(), json!(node.prefix));
    obj.insert("baseUrl".to_string(), json!(node.base_url));
    if let Some(ref at) = node.api_type {
        obj.insert("apiType".to_string(), json!(at));
    }
    obj.insert("createdAt".to_string(), json!(node.created_at));
    obj.insert("updatedAt".to_string(), json!(node.updated_at));
    Value::Object(obj)
}

/// `PUT /api/provider-nodes/{id}` — Update provider node and cascade to its connections.
pub async fn provider_node_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let node = match find_provider_node_flexible(&state.db, &id) {
        Ok(Some(n)) => n,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "Provider node not found"),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Name is required");
    }

    let prefix = body
        .get("prefix")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if prefix.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Prefix is required");
    }

    let node_type = node.node_type.as_deref().unwrap_or("");
    let api_type = body.get("apiType").and_then(|v| v.as_str()).map(str::trim);
    if node_type == "openai-compatible" && !matches!(api_type, Some("chat" | "responses")) {
        return json_err(StatusCode::BAD_REQUEST, "Invalid OpenAI compatible API type");
    }

    let raw_base_url = body
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let raw_endpoint_url = body
        .get("endpointUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");

    if node_type == "custom-media" && raw_base_url.is_empty() && raw_endpoint_url.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "Base URL or full endpoint URL is required",
        );
    }
    if node_type != "custom-media" && raw_base_url.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Base URL is required");
    }

    let mut sanitized_base_url = raw_base_url.to_string();
    if node_type == "anthropic-compatible" {
        sanitized_base_url = sanitized_base_url.trim_end_matches('/').to_string();
        if sanitized_base_url.ends_with("/messages") {
            sanitized_base_url =
                sanitized_base_url[..sanitized_base_url.len() - "/messages".len()].to_string();
        }
    } else if node_type == "custom-embedding" {
        sanitized_base_url = sanitized_base_url.trim_end_matches('/').to_string();
        if sanitized_base_url.ends_with("/embeddings") {
            sanitized_base_url =
                sanitized_base_url[..sanitized_base_url.len() - "/embeddings".len()].to_string();
        }
    }

    let mut updated_data = match &node.data {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    updated_data.insert("name".to_string(), json!(name));
    updated_data.insert("prefix".to_string(), json!(prefix));
    updated_data.insert("baseUrl".to_string(), json!(sanitized_base_url));

    if node_type == "custom-media" {
        let auth_hdr = match body.get("authHeader").and_then(|v| v.as_str()) {
            Some(v @ ("bearer" | "x-api-key" | "api-key")) => v,
            _ => "bearer",
        };
        let models: Vec<Value> = body
            .get("models")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let s = match item {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => return None,
                        };
                        let t = s.trim();
                        (!t.is_empty()).then(|| json!(t))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let custom_headers = sanitize_custom_headers_map(body.get("customHeaders"));
        updated_data.insert("endpointUrl".to_string(), json!(raw_endpoint_url));
        updated_data.insert("authHeader".to_string(), json!(auth_hdr));
        updated_data.insert("models".to_string(), Value::Array(models));
        updated_data.insert("customHeaders".to_string(), Value::Object(custom_headers));
    }

    let next_api_type = if node_type == "openai-compatible" {
        let at = api_type.unwrap_or("chat").to_string();
        updated_data.insert("apiType".to_string(), json!(at));
        Some(at)
    } else {
        node.api_type.clone()
    };

    let retry_update = sanitize_retry_config(body.get("retry"));
    if let Some(ref retry_val) = retry_update {
        match retry_val {
            Some(r) => {
                updated_data.insert("retry".to_string(), r.clone());
            }
            None => {
                updated_data.insert("retry".to_string(), Value::Null);
            }
        }
    }

    let now = Utc::now().to_rfc3339();
    let updated_node = ProviderNode {
        id: node.id.clone(),
        node_type: node.node_type.clone(),
        name: Some(name.to_string()),
        prefix: prefix.to_string(),
        base_url: sanitized_base_url.clone(),
        api_type: next_api_type.clone(),
        data: Value::Object(updated_data.clone()),
        created_at: node.created_at.clone(),
        updated_at: now.clone(),
    };

    if let Err(e) = state.db.save_provider_node(&updated_node) {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to update provider node: {e}"),
        );
    }

    // Cascade updates to all providerConnections matching this node id
    if let Ok(conns) = state.db.load_provider_connections() {
        for mut conn in conns.into_iter().filter(|c| c.provider == id || c.provider == node.id) {
            let mut psd = get_psd_object(&conn);
            psd.insert("prefix".to_string(), json!(prefix));
            if node_type == "openai-compatible" {
                if let Some(ref at) = next_api_type {
                    psd.insert("apiType".to_string(), json!(at));
                }
            } else {
                psd.remove("apiType");
            }
            psd.insert("baseUrl".to_string(), json!(sanitized_base_url));
            for field in [
                "endpointUrl",
                "mediaKind",
                "authHeader",
                "models",
                "customHeaders",
            ] {
                if let Some(val) = updated_data.get(field) {
                    psd.insert(field.to_string(), val.clone());
                }
            }
            psd.insert("nodeName".to_string(), json!(name));
            if let Some(ref retry_val) = retry_update {
                match retry_val {
                    Some(r) => {
                        psd.insert("retry".to_string(), r.clone());
                    }
                    None => {
                        psd.remove("retry");
                    }
                }
            }
            set_connection_json_field(&mut conn, "providerSpecificData", Some(Value::Object(psd)));
            conn.updated_at = now.clone();
            let _ = state.db.save_provider_connection(&conn);
        }
    }

    Json(json!({ "node": node_to_response_json(&updated_node) })).into_response()
}

// ============================================================================
// 7. POST /api/provider-nodes/validate
// ============================================================================

fn models_error_message(status: u16) -> String {
    match status {
        401 | 403 => "API key unauthorized".to_string(),
        404 => "/models endpoint not found - try chat validation with model ID".to_string(),
        s if s >= 500 => "Server error - try again later".to_string(),
        s => format!("Unexpected response ({s})"),
    }
}

fn chat_error_message(status: u16) -> String {
    match status {
        401 | 403 => "API key unauthorized".to_string(),
        400 => "Invalid model or bad request".to_string(),
        404 => "Chat endpoint not found".to_string(),
        s if s >= 500 => "Server error - try again later".to_string(),
        s => format!("Chat request failed ({s})"),
    }
}

fn format_reqwest_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "Request timeout (>10s) - provider node not responding".to_string();
    }
    let msg = err.to_string();
    if msg.contains("Connection refused") {
        return "Connection refused - provider node offline or unreachable".to_string();
    }
    if msg.contains("dns error") || msg.contains("failed to lookup") {
        return "DNS lookup failed - invalid domain or network issue".to_string();
    }
    "Network connection failed - check URL and network connectivity".to_string()
}

/// `POST /api/provider-nodes/validate`
pub async fn provider_nodes_validate_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let base_url = body
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let api_key = body
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let node_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let model_id = body
        .get("modelId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if base_url.is_empty() || api_key.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Base URL and API key required");
    }

    if Url::parse(base_url).is_err() {
        return json_err(StatusCode::BAD_REQUEST, "Invalid URL format");
    }

    if !is_local_request(&headers) && assert_public_url_resolved(base_url).await.is_err() {
        return json_err(StatusCode::BAD_REQUEST, "URL not allowed");
    }

    let client = http_client(10);

    if node_type == "custom-embedding" {
        let Some(mid) = model_id else {
            return Json(json!({
                "valid": false,
                "error": "Model ID required for embedding validation"
            }))
            .into_response();
        };
        let normalized_base = base_url.trim_end_matches('/');
        let url = format!("{normalized_base}/embeddings");
        match client
            .post(&url)
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({ "model": mid, "input": "ping" }))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let dims = resp
                        .json::<Value>()
                        .await
                        .ok()
                        .and_then(|d| {
                            d.get("data")?
                                .get(0)?
                                .get("embedding")?
                                .as_array()
                                .map(|a| a.len())
                        });
                    return Json(json!({
                        "valid": true,
                        "method": "embeddings",
                        "dimensions": dims
                    }))
                    .into_response();
                }
                if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    return Json(json!({ "valid": false, "error": "API key unauthorized" }))
                        .into_response();
                }
                let err_body = resp.text().await.unwrap_or_default();
                let preview: String = err_body.chars().take(200).collect();
                let suffix = if preview.is_empty() {
                    String::new()
                } else {
                    format!(": {preview}")
                };
                return Json(json!({
                    "valid": false,
                    "error": format!("Embeddings request failed ({}){}", status.as_u16(), suffix),
                    "method": "embeddings"
                }))
                .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "valid": false, "error": format_reqwest_error(&e) })),
                )
                    .into_response();
            }
        }
    }

    if node_type == "anthropic-compatible" {
        let mut normalized_base = base_url.trim_end_matches('/').to_string();
        if normalized_base.ends_with("/messages") {
            normalized_base =
                normalized_base[..normalized_base.len() - "/messages".len()].to_string();
        }
        let models_url = format!("{normalized_base}/models");
        match client
            .get(&models_url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if resp.status().is_success() {
                    return Json(json!({ "valid": true })).into_response();
                }
                if status == 401 || status == 403 {
                    return Json(json!({ "valid": false, "error": "API key unauthorized" }))
                        .into_response();
                }
                if let Some(mid) = model_id {
                    let chat_url = format!("{normalized_base}/chat/completions");
                    match client
                        .post(&chat_url)
                        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-api-key", api_key)
                        .header("anthropic-version", "2023-06-01")
                        .json(&json!({
                            "model": mid,
                            "messages": [{ "role": "user", "content": "ping" }],
                            "max_tokens": 1
                        }))
                        .send()
                        .await
                    {
                        Ok(chat_res) => {
                            if chat_res.status().is_success() {
                                return Json(json!({ "valid": true, "method": "chat" }))
                                    .into_response();
                            }
                            return Json(json!({
                                "valid": false,
                                "error": chat_error_message(chat_res.status().as_u16()),
                                "method": "chat"
                            }))
                            .into_response();
                        }
                        Err(e) => {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(json!({ "valid": false, "error": format_reqwest_error(&e) })),
                            )
                                .into_response();
                        }
                    }
                }
                return Json(json!({
                    "valid": false,
                    "error": models_error_message(status)
                }))
                .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "valid": false, "error": format_reqwest_error(&e) })),
                )
                    .into_response();
            }
        }
    }

    // OpenAI compatible (default)
    let normalized_base = base_url.trim_end_matches('/');
    let is_direct_chat_url = normalized_base.ends_with("/chat/completions")
        || normalized_base.ends_with("/responses")
        || normalized_base.contains("/chat-completions");

    if !is_direct_chat_url {
        let models_url = format!("{normalized_base}/models");
        match client
            .get(&models_url)
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    return Json(json!({ "valid": true })).into_response();
                }
                let st = resp.status().as_u16();
                if st == 401 || st == 403 {
                    return Json(json!({ "valid": false, "error": "API key unauthorized" }))
                        .into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "valid": false, "error": format_reqwest_error(&e) })),
                )
                    .into_response();
            }
        }
    }

    if model_id.is_some() || is_direct_chat_url {
        let target_chat_url = if is_direct_chat_url {
            normalized_base.to_string()
        } else {
            format!("{normalized_base}/chat/completions")
        };
        match client
            .post(&target_chat_url)
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({
                "model": model_id.unwrap_or("gpt-4o-mini"),
                "messages": [{ "role": "user", "content": "ping" }],
                "max_tokens": 1
            }))
            .send()
            .await
        {
            Ok(chat_res) => {
                if chat_res.status().is_success() {
                    return Json(json!({ "valid": true, "method": "chat" })).into_response();
                }
                return Json(json!({
                    "valid": false,
                    "error": chat_error_message(chat_res.status().as_u16()),
                    "method": "chat"
                }))
                .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "valid": false, "error": format_reqwest_error(&e) })),
                )
                    .into_response();
            }
        }
    }

    Json(json!({ "valid": false, "error": models_error_message(404) })).into_response()
}

// ============================================================================
// 8. /api/providers/{id} (GET, PUT, DELETE)
// ============================================================================

/// `GET /api/providers/{id}`
pub async fn provider_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    match find_connection_by_id(&state.db, &id) {
        Ok(Some(conn)) => {
            Json(json!({ "connection": serialize_provider_connection_response(&conn) }))
                .into_response()
        }
        Ok(None) => json_err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `PUT /api/providers/{id}`
pub async fn provider_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let mut conn = match find_connection_by_id(&state.db, &id) {
        Ok(Some(c)) => c,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let body_obj = body.as_object().cloned().unwrap_or_default();

    let has_any_proxy_field = body_obj.contains_key("connectionProxyEnabled")
        || body_obj.contains_key("connectionProxyUrl")
        || body_obj.contains_key("connectionNoProxy");
    let proxy_enabled = body_obj
        .get("connectionProxyEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let proxy_url = body_obj
        .get("connectionProxyUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let no_proxy = body_obj
        .get("connectionNoProxy")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if has_any_proxy_field && proxy_enabled && proxy_url.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "Connection proxy URL is required when connection proxy is enabled",
        );
    }

    let (has_proxy_pool_field, proxy_pool_id): (bool, Option<String>) =
        match body_obj.get("proxyPoolId") {
            None => (false, None),
            Some(Value::Null) => (true, None),
            Some(Value::String(s)) if s.trim().is_empty() || s.trim() == "__none__" => {
                (true, None)
            }
            Some(Value::String(s)) => {
                let pool_id = s.trim().to_string();
                match state.db.get_proxy_pool_by_id(&pool_id) {
                    Ok(Some(_)) => (true, Some(pool_id)),
                    Ok(None) => return json_err(StatusCode::BAD_REQUEST, "Proxy pool not found"),
                    Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                }
            }
            Some(other) => {
                let pool_id = other.to_string().trim().to_string();
                if pool_id.is_empty() {
                    (true, None)
                } else {
                    match state.db.get_proxy_pool_by_id(&pool_id) {
                        Ok(Some(_)) => (true, Some(pool_id)),
                        Ok(None) => {
                            return json_err(StatusCode::BAD_REQUEST, "Proxy pool not found")
                        }
                        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                    }
                }
            }
        };

    if let Some(v) = body_obj.get("name") {
        conn.name = v.as_str().map(ToString::to_string);
    }
    if let Some(p) = body_obj.get("priority").and_then(|v| v.as_i64()) {
        conn.priority = Some(p);
    }
    if let Some(gp) = body_obj.get("globalPriority") {
        set_connection_json_field(&mut conn, "globalPriority", Some(gp.clone()));
    }
    if let Some(dm) = body_obj.get("defaultModel") {
        conn.data.default_model = dm.as_str().map(ToString::to_string);
        set_connection_json_field(&mut conn, "defaultModel", Some(dm.clone()));
    }
    if let Some(active) = body_obj.get("isActive").and_then(|v| v.as_bool()) {
        conn.is_active = active;
    }
    if let Some(key) = body_obj.get("apiKey").and_then(|v| v.as_str()) {
        if !key.is_empty() && conn.auth_type.eq_ignore_ascii_case("apikey") {
            conn.data.api_key = Some(key.to_string());
            set_connection_json_field(&mut conn, "apiKey", Some(json!(key)));
        }
    }
    for f in ["testStatus", "lastError", "lastErrorAt"] {
        if let Some(v) = body_obj.get(f) {
            set_connection_json_field(&mut conn, f, Some(v.clone()));
        }
    }

    let existing_psd = get_psd_object(&conn);
    let incoming_psd_opt = body_obj
        .get("providerSpecificData")
        .and_then(|v| v.as_object());
    if !existing_psd.is_empty()
        || incoming_psd_opt.is_some()
        || has_any_proxy_field
        || has_proxy_pool_field
    {
        let mut merged_psd = existing_psd;
        let is_aws = uses_aws_credential_form(&conn.provider);
        if let Some(incoming_psd) = incoming_psd_opt {
            for (k, v) in incoming_psd {
                if is_aws && k == "sessionToken" && v.as_str() == Some("") {
                    continue;
                }
                if is_aws && k == "sessionToken" && v.is_null() {
                    merged_psd.remove("sessionToken");
                    continue;
                }
                merged_psd.insert(k.clone(), v.clone());
            }
        }
        if has_any_proxy_field {
            merged_psd.insert("connectionProxyEnabled".to_string(), json!(proxy_enabled));
            merged_psd.insert("connectionProxyUrl".to_string(), json!(proxy_url));
            merged_psd.insert("connectionNoProxy".to_string(), json!(no_proxy));
        }
        if has_proxy_pool_field {
            match proxy_pool_id {
                None => {
                    merged_psd.remove("proxyPoolId");
                }
                Some(pid) => {
                    merged_psd.insert("proxyPoolId".to_string(), json!(pid));
                }
            }
        }
        set_connection_json_field(
            &mut conn,
            "providerSpecificData",
            Some(Value::Object(merged_psd)),
        );
    }

    conn.updated_at = Utc::now().to_rfc3339();
    if let Err(e) = state.db.save_provider_connection(&conn) {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to update connection: {e}"),
        );
    }

    Json(json!({ "connection": serialize_provider_connection_response(&conn) })).into_response()
}

/// `DELETE /api/providers/{id}`
pub async fn provider_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    match state.db.delete_provider_connection(&id) {
        Ok(true) => Json(json!({ "message": "Connection deleted successfully" })).into_response(),
        Ok(false) => json_err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to delete connection: {e}"),
        ),
    }
}

// ============================================================================
// 9. /api/providers/{id}/overrides (GET, PUT)
// ============================================================================

fn builtin_headers_for_provider(canonical: &str) -> Value {
    match canonical {
        "anthropic" => json!({
            "anthropic-version": "2023-06-01",
            "Anthropic-Beta": "claude-code-20250219,interleaved-thinking-2025-05-14"
        }),
        "antigravity" => json!({ "User-Agent": "antigravity/hub/2.24.0 darwin/arm64" }),
        "claude" => json!({
            "Anthropic-Version": "2023-06-01",
            "Anthropic-Beta": "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28",
            "Anthropic-Dangerous-Direct-Browser-Access": "true",
            "User-Agent": "claude-cli/2.1.322 (external, sdk-cli)",
            "X-App": "cli",
            "X-Stainless-Helper-Method": "stream",
            "X-Stainless-Retry-Count": "0",
            "X-Stainless-Runtime-Version": "v24.14.0",
            "X-Stainless-Package-Version": "0.80.0",
            "X-Stainless-Runtime": "node",
            "X-Stainless-Lang": "js",
            "X-Stainless-Arch": "arm64",
            "X-Stainless-Os": "MacOS",
            "X-Stainless-Timeout": "600"
        }),
        "cline" | "clinepass" => json!({
            "HTTP-Referer": "https://cline.bot",
            "X-Title": "Cline"
        }),
        "codebuddy-cn" => json!({
            "User-Agent": "CLI/2.108.1 CodeBuddy/2.108.1",
            "X-Product": "SaaS",
            "X-IDE-Type": "CLI",
            "X-IDE-Name": "CLI",
            "x-requested-with": "XMLHttpRequest",
            "x-codebuddy-request": "1"
        }),
        "codebuddy-intl" => json!({
            "User-Agent": "IDE/2.108.1 CodeBuddy/2.108.1",
            "X-Product": "SaaS",
            "X-IDE-Type": "IDE",
            "X-IDE-Name": "IDE",
            "x-requested-with": "XMLHttpRequest",
            "x-codebuddy-request": "1"
        }),
        "codex" => json!({
            "originator": "codex_cli_rs",
            "User-Agent": "codex_cli_rs/0.167.1",
            "version": "0.167.1"
        }),
        "commandcode" => json!({
            "User-Agent": "cli",
            "x-command-code-version": "1.88.5",
            "x-cli-environment": "production",
            "x-cli-surface": "cli",
            "x-taste-learning": "false"
        }),
        "cursor" => json!({
            "connect-accept-encoding": "gzip",
            "connect-protocol-version": "1",
            "Content-Type": "application/connect+proto",
            "User-Agent": "connect-es/1.6.1"
        }),
        "github" => json!({
            "copilot-integration-id": "vscode-chat",
            "editor-version": "vscode/1.110.0",
            "editor-plugin-version": "copilot-chat/0.38.0",
            "user-agent": "GitHubCopilotChat/0.38.0",
            "openai-intent": "conversation-panel",
            "x-github-api-version": "2025-04-01",
            "x-vscode-user-agent-library-version": "electron-fetch",
            "X-Initiator": "user",
            "Accept": "application/json",
            "Content-Type": "application/json"
        }),
        "glm" | "kimi" | "minimax" | "minimax-cn" => json!({
            "Anthropic-Version": "2023-06-01",
            "Anthropic-Beta": "claude-code-20250219,interleaved-thinking-2025-05-14"
        }),
        "grok-cli" => json!({
            "User-Agent": "grok-shell/1.0.44 (linux; x86_64)",
            "x-grok-client-identifier": "grok-shell",
            "x-grok-client-version": "1.0.44"
        }),
        "iflow" => json!({ "User-Agent": "iFlow-Cli" }),
        "kimchi" => json!({ "User-Agent": "kimchi/0.1.50" }),
        "kiro" => json!({
            "Content-Type": "application/json",
            "Accept": "application/vnd.amazon.eventstream",
            "User-Agent": "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0",
            "X-Amz-User-Agent": "aws-sdk-js/3.0.0 kiro-ide/1.0.0"
        }),
        "opencode" => json!({ "x-opencode-client": "desktop" }),
        "openrouter" | "api-airforce" => json!({
            "HTTP-Referer": "https://endpoint-proxy.local",
            "X-Title": "Endpoint Proxy"
        }),
        "zed" => json!({ "content-type": "application/json" }),
        "minimax-code" | "minimax-code-global" => json!({
            "User-Agent": "MiniMaxAgent",
            "X-Mavis-Agent-Id": "main"
        }),
        _ => json!({}),
    }
}

fn normalize_override_payload(body: &Value) -> Result<Option<Value>, String> {
    let Some(headers_val) = body.get("headers") else {
        return Ok(None);
    };
    if headers_val.is_null() {
        return Ok(None);
    }
    let Some(headers_obj) = headers_val.as_object() else {
        return Err("headers must be an object".to_string());
    };

    let entries: Vec<(&String, &Value)> = headers_obj
        .iter()
        .filter(|(_, v)| !v.is_null() && v.as_str() != Some(""))
        .collect();

    if entries.len() > MAX_OVERRIDE_HEADERS {
        return Err(format!("Too many headers (max {MAX_OVERRIDE_HEADERS})"));
    }

    let mut clean = Map::new();
    for (name, value) in entries {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(format!("Invalid header name: {name}"));
        }
        let Some(val_str) = value.as_str() else {
            return Err(format!("Invalid value for header {name}"));
        };
        if val_str.contains('\r') || val_str.contains('\n') {
            return Err(format!("Invalid value for header {name}"));
        }
        if val_str.len() > MAX_OVERRIDE_HEADER_VALUE_LEN {
            return Err(format!(
                "Header {name} value too long (max {MAX_OVERRIDE_HEADER_VALUE_LEN})"
            ));
        }
        if BLOCKED_OVERRIDE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!("Header {name} cannot be overridden"));
        }
        clean.insert(name.clone(), Value::String(val_str.to_string()));
    }

    if clean.is_empty() {
        Ok(None)
    } else {
        Ok(Some(json!({ "headers": Value::Object(clean) })))
    }
}

/// `GET /api/providers/{id}/overrides`
pub async fn provider_overrides_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let canonical = resolve_provider_alias(&id);
    let settings = match state.db.load_settings() {
        Ok(s) => s,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get overrides: {e}"),
            )
        }
    };
    let user_headers = settings
        .get("providerOverrides")
        .and_then(|po| po.get(&canonical))
        .and_then(|ov| ov.get("headers"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    Json(json!({
        "headers": user_headers,
        "builtinHeaders": builtin_headers_for_provider(&canonical),
    }))
    .into_response()
}

/// `PUT /api/providers/{id}/overrides`
pub async fn provider_overrides_put_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let canonical = resolve_provider_alias(&id);
    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| json!({}));
    let override_val = match normalize_override_payload(&body) {
        Ok(ov) => ov,
        Err(err) => return json_err(StatusCode::BAD_REQUEST, err),
    };

    let mut settings = match state.db.load_settings() {
        Ok(s) => s,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save overrides: {e}"),
            )
        }
    };

    let mut overrides_map = settings
        .get("providerOverrides")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let response_headers = match override_val {
        Some(ov) => {
            let h = ov.get("headers").cloned().unwrap_or_else(|| json!({}));
            overrides_map.insert(canonical, ov);
            h
        }
        None => {
            overrides_map.remove(&canonical);
            json!({})
        }
    };

    if let Some(obj) = settings.as_object_mut() {
        obj.insert("providerOverrides".to_string(), Value::Object(overrides_map));
    }

    if let Err(e) = state.db.save_settings(&settings) {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save overrides: {e}"),
        );
    }

    Json(json!({ "headers": response_headers })).into_response()
}

// ============================================================================
// 10. Web Identity Probes & Connection Test Engine
//     Used by /api/providers/{id}/test, /api/providers/test-batch, /api/providers/web-login
// ============================================================================

#[derive(Debug, Clone)]
struct WebIdentityResult {
    status: &'static str, // "valid" | "invalid" | "unknown"
    email: String,
    error: Option<String>,
}

fn parse_cookie_jar(raw: &str) -> HashMap<String, String> {
    let mut jar = HashMap::new();
    for part in raw.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            let key = k.trim();
            if !key.is_empty() {
                jar.insert(key.to_string(), v.trim().to_string());
            }
        }
    }
    jar
}

fn decode_jwt_payload(token: &str) -> Option<Value> {
    let stripped = token.strip_prefix("base64-").unwrap_or(token);
    let part = stripped.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(part))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn has_web_identity_probe(provider_id: &str) -> bool {
    WEB_LOGIN_PROVIDERS.contains(&provider_id)
}

async fn fetch_web_identity(provider_id: &str, credential: &str) -> WebIdentityResult {
    let client = http_client(15);
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

    match provider_id {
        "deepseek-web" => {
            let mut token = credential.trim().to_string();
            if let Ok(parsed) = serde_json::from_str::<Value>(&token) {
                if let Some(v) = parsed.get("value").and_then(|x| x.as_str()) {
                    token = v.to_string();
                }
            }
            if token.is_empty() {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing userToken".to_string()),
                };
            }
            let resp = client
                .get("https://chat.deepseek.com/api/v0/users/current")
                .header(header::USER_AGENT, ua)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ACCEPT, "*/*")
                .header(header::ORIGIN, "https://chat.deepseek.com")
                .header(header::REFERER, "https://chat.deepseek.com/")
                .header("X-Client-Bundle-Id", "com.deepseek.chat")
                .header("X-Client-Platform", "web")
                .header("X-Client-Version", "2.0.0")
                .send()
                .await;
            match resp {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("userToken is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("DeepSeek returned HTTP {st}")),
                        };
                    }
                    let Ok(j) = r.json::<Value>().await else {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some("DeepSeek returned invalid JSON".to_string()),
                        };
                    };
                    if j.get("code").and_then(|v| v.as_i64()) == Some(40003) {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("userToken is invalid or expired".to_string()),
                        };
                    }
                    let biz = j.get("data").and_then(|d| d.get("biz_data"));
                    if biz.and_then(|b| b.get("token")).and_then(|t| t.as_str()).is_none() {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("DeepSeek rejected the userToken".to_string()),
                        };
                    }
                    let email = biz
                        .and_then(|b| b.get("email"))
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    WebIdentityResult {
                        status: "valid",
                        email,
                        error: None,
                    }
                }
            }
        }
        "chatgpt-web" => {
            let cookie = if credential.contains('=') {
                credential.trim().to_string()
            } else if credential.trim().is_empty() {
                String::new()
            } else {
                format!("__Secure-next-auth.session-token={}", credential.trim())
            };
            if cookie.is_empty() {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing session cookie".to_string()),
                };
            }
            match client
                .get("https://chatgpt.com/api/auth/session")
                .header(header::USER_AGENT, ua)
                .header(header::COOKIE, cookie)
                .header(header::ACCEPT, "application/json")
                .send()
                .await
            {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("Session cookie is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("ChatGPT returned HTTP {st}")),
                        };
                    }
                    let Ok(j) = r.json::<Value>().await else {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some("ChatGPT returned invalid JSON".to_string()),
                        };
                    };
                    let Some(email) = j.get("user").and_then(|u| u.get("email")).and_then(|e| e.as_str()) else {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("ChatGPT session is not signed in".to_string()),
                        };
                    };
                    WebIdentityResult {
                        status: "valid",
                        email: email.to_string(),
                        error: None,
                    }
                }
            }
        }
        "grok-web" => {
            let jar = parse_cookie_jar(credential);
            if !jar.contains_key("sso") && !credential.trim().starts_with("sso=") && credential.trim().is_empty() {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing sso cookie".to_string()),
                };
            }
            let cookie = if credential.contains('=') {
                credential.to_string()
            } else {
                format!("sso={}", credential.trim())
            };
            match client
                .get("https://grok.com/api/auth/session")
                .header(header::USER_AGENT, ua)
                .header(header::COOKIE, cookie)
                .header(header::ACCEPT, "application/json")
                .header(header::ORIGIN, "https://grok.com")
                .header(header::REFERER, "https://grok.com/")
                .send()
                .await
            {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("sso cookie is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("Grok returned HTTP {st}")),
                        };
                    }
                    let Ok(j) = r.json::<Value>().await else {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some("Grok returned invalid JSON".to_string()),
                        };
                    };
                    if j.get("status").and_then(|v| v.as_str()) != Some("authenticated") {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("Grok session is not signed in".to_string()),
                        };
                    }
                    let email = j
                        .get("session")
                        .and_then(|s| s.get("email"))
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    WebIdentityResult {
                        status: "valid",
                        email,
                        error: None,
                    }
                }
            }
        }
        "gemini-web" => {
            let jar = parse_cookie_jar(credential);
            if !jar.contains_key("__Secure-1PSID") {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing __Secure-1PSID cookie".to_string()),
                };
            }
            match client
                .get("https://gemini.google.com/app")
                .header(header::USER_AGENT, ua)
                .header(header::COOKIE, credential)
                .header(header::ACCEPT, "text/html,application/xhtml+xml")
                .send()
                .await
            {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("__Secure-1PSID cookie is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("Gemini returned HTTP {st}")),
                        };
                    }
                    let text = r.text().await.unwrap_or_default();
                    if !text.contains("\"SNlM0e\"") {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("Gemini session is not signed in".to_string()),
                        };
                    }
                    WebIdentityResult {
                        status: "valid",
                        email: String::new(),
                        error: None,
                    }
                }
            }
        }
        "muse-web" => {
            let jar = parse_cookie_jar(credential);
            if !jar.contains_key("hatch_sess") {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing hatch_sess cookie".to_string()),
                };
            }
            match client
                .get("https://muse.ai/api/session")
                .header(header::USER_AGENT, ua)
                .header(header::COOKIE, credential)
                .header(header::ACCEPT, "application/json")
                .header(header::ORIGIN, "https://muse.ai")
                .header(header::REFERER, "https://muse.ai/thread/new")
                .send()
                .await
            {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("muse.ai session cookie is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("muse.ai returned HTTP {st}")),
                        };
                    }
                    WebIdentityResult {
                        status: "valid",
                        email: String::new(),
                        error: None,
                    }
                }
            }
        }
        "uc-web" => {
            let jar = parse_cookie_jar(credential);
            let Some(client_cookie) = jar.get("__client") else {
                return WebIdentityResult {
                    status: "invalid",
                    email: String::new(),
                    error: Some("Missing __client cookie".to_string()),
                };
            };
            let clerk_origin = "https://uncensored.com";
            let mut sid = decode_jwt_payload(client_cookie)
                .and_then(|v| v.get("sid")?.as_str().map(ToString::to_string))
                .unwrap_or_default();
            if sid.is_empty() {
                match client
                    .get("https://clerk.uncensored.com/v1/client?_clerk_js_version=5.127.1")
                    .header(header::USER_AGENT, ua)
                    .header(header::COOKIE, format!("__client={client_cookie}"))
                    .header(header::ACCEPT, "application/json")
                    .header(header::ORIGIN, clerk_origin)
                    .header(header::REFERER, "https://uncensored.com/")
                    .send()
                    .await
                {
                    Err(e) => {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(e.to_string()),
                        }
                    }
                    Ok(r) => {
                        let st = r.status().as_u16();
                        if st == 401 || st == 403 {
                            return WebIdentityResult {
                                status: "invalid",
                                email: String::new(),
                                error: Some("Clerk session is invalid or expired".to_string()),
                            };
                        }
                        if !r.status().is_success() {
                            return WebIdentityResult {
                                status: "unknown",
                                email: String::new(),
                                error: Some(format!("Clerk returned HTTP {st}")),
                            };
                        }
                        let Ok(j) = r.json::<Value>().await else {
                            return WebIdentityResult {
                                status: "unknown",
                                email: String::new(),
                                error: Some("Clerk returned invalid JSON".to_string()),
                            };
                        };
                        let payload = j
                            .get("response")
                            .or_else(|| j.get("client"))
                            .unwrap_or(&j);
                        let sessions = payload
                            .get("sessions")
                            .and_then(|s| s.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let active = sessions
                            .iter()
                            .find(|s| s.get("status").and_then(|v| v.as_str()) == Some("active"))
                            .or_else(|| sessions.first());
                        sid = active
                            .and_then(|s| s.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if sid.is_empty() {
                            return WebIdentityResult {
                                status: "invalid",
                                email: String::new(),
                                error: Some(
                                    "Not signed in to uncensored.com — use Login via Browser"
                                        .to_string(),
                                ),
                            };
                        }
                    }
                }
            }
            match client
                .post(format!(
                    "https://clerk.uncensored.com/v1/client/sessions/{sid}/tokens?_clerk_js_version=5.127.1"
                ))
                .header(header::USER_AGENT, ua)
                .header(header::COOKIE, format!("__client={client_cookie}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, clerk_origin)
                .send()
                .await
            {
                Err(e) => WebIdentityResult {
                    status: "unknown",
                    email: String::new(),
                    error: Some(e.to_string()),
                },
                Ok(r) => {
                    let st = r.status().as_u16();
                    if st == 401 || st == 403 {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("Clerk session is invalid or expired".to_string()),
                        };
                    }
                    if !r.status().is_success() {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some(format!("Clerk returned HTTP {st}")),
                        };
                    }
                    let Ok(j) = r.json::<Value>().await else {
                        return WebIdentityResult {
                            status: "unknown",
                            email: String::new(),
                            error: Some("Clerk returned invalid JSON".to_string()),
                        };
                    };
                    let Some(jwt) = j.get("jwt").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                        return WebIdentityResult {
                            status: "invalid",
                            email: String::new(),
                            error: Some("Clerk did not mint a session token".to_string()),
                        };
                    };
                    let uid = decode_jwt_payload(jwt)
                        .and_then(|p| p.get("uid")?.as_str().map(ToString::to_string))
                        .unwrap_or_default();
                    WebIdentityResult {
                        status: "valid",
                        email: if uid.contains('@') { uid } else { String::new() },
                        error: None,
                    }
                }
            }
        }
        _ => WebIdentityResult {
            status: "unknown",
            email: String::new(),
            error: Some("No identity probe for this provider".to_string()),
        },
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&k, data).as_ref().to_vec()
}

async fn probe_bedrock_credentials(
    conn: &ProviderConnection,
    client: &Client,
) -> (bool, Option<String>, Option<u16>) {
    let psd = get_psd_object(conn);
    let access_key_id = psd
        .get("accessKeyId")
        .and_then(|v| v.as_str())
        .or_else(|| conn.data.api_key.as_deref())
        .unwrap_or("")
        .trim();
    let secret_access_key = psd
        .get("secretAccessKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return (false, Some("Missing AWS accessKeyId or secretAccessKey".to_string()), None);
    }

    let region = psd
        .get("region")
        .and_then(|v| v.as_str())
        .filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .unwrap_or("us-east-1");
    let session_token = psd
        .get("sessionToken")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let host = format!("bedrock.{region}.amazonaws.com");
    let url = format!("https://{host}/foundation-models");
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let payload_hash = hex_encode(digest::digest(&digest::SHA256, b"").as_ref());

    let mut canonical_headers = format!("accept:application/json\nhost:{host}\nx-amz-date:{amz_date}\n");
    let mut signed_headers = "accept;host;x-amz-date".to_string();
    if let Some(st) = session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{st}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }

    let canonical_request = format!(
        "GET\n/foundation-models\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let credential_scope = format!("{date_stamp}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex_encode(digest::digest(&digest::SHA256, canonical_request.as_bytes()).as_ref())
    );

    let k_date = hmac_sha256(format!("AWS4{secret_access_key}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"bedrock");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let mut req = client
        .get(&url)
        .header(header::ACCEPT, "application/json")
        .header("x-amz-date", amz_date)
        .header(header::AUTHORIZATION, authorization);
    if let Some(st) = session_token {
        req = req.header("x-amz-security-token", st);
    }

    match req.send().await {
        Ok(resp) => {
            let st = resp.status().as_u16();
            if resp.status().is_success() {
                return (true, None, Some(st));
            }
            let err_type = resp
                .headers()
                .get("x-amzn-errortype")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            if err_type == "AccessDeniedException" {
                return (true, None, Some(st));
            }
            let msg = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(ToString::to_string))
                .unwrap_or_else(|| format!("AWS Bedrock returned {st}"));
            (false, Some(msg), Some(st))
        }
        Err(e) => (false, Some(e.to_string()), None),
    }
}

struct EffectiveProxy {
    connection_proxy_enabled: bool,
    connection_proxy_url: String,
    connection_no_proxy: String,
    vercel_relay_url: String,
}

fn resolve_effective_proxy(db: &Database, psd: &Map<String, Value>) -> EffectiveProxy {
    let pool_id = psd
        .get("proxyPoolId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "__none__");

    if let Some(pid) = pool_id {
        if let Ok(Some(pool)) = db.get_proxy_pool_by_id(pid) {
            if pool.is_active {
                if let Some(url) = pool.proxy_url().filter(|u| !u.trim().is_empty()) {
                    let ptype = pool.proxy_type();
                    if matches!(ptype, "vercel" | "cloudflare" | "deno" | "netlify") {
                        return EffectiveProxy {
                            connection_proxy_enabled: false,
                            connection_proxy_url: String::new(),
                            connection_no_proxy: String::new(),
                            vercel_relay_url: url.to_string(),
                        };
                    }
                    return EffectiveProxy {
                        connection_proxy_enabled: true,
                        connection_proxy_url: url.to_string(),
                        connection_no_proxy: pool.no_proxy().unwrap_or("").to_string(),
                        vercel_relay_url: String::new(),
                    };
                }
            }
        }
    }

    EffectiveProxy {
        connection_proxy_enabled: psd
            .get("connectionProxyEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        connection_proxy_url: psd
            .get("connectionProxyUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        connection_no_proxy: psd
            .get("connectionNoProxy")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        vercel_relay_url: String::new(),
    }
}

fn build_client_for_proxy(proxy: &EffectiveProxy, timeout_secs: u64) -> Result<Client, String> {
    let mut builder = Client::builder().timeout(Duration::from_secs(timeout_secs));
    if proxy.connection_proxy_enabled && !proxy.connection_proxy_url.is_empty() {
        let mut p = reqwest::Proxy::all(&proxy.connection_proxy_url)
            .map_err(|e| format!("Invalid proxy URL: {e}"))?;
        if !proxy.connection_no_proxy.is_empty() {
            if let Some(np) = reqwest::NoProxy::from_string(&proxy.connection_no_proxy) {
                p = p.no_proxy(Some(np));
            }
        }
        builder = builder.proxy(p);
    }
    builder
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

#[derive(Debug)]
struct ConnectionProbeOutcome {
    valid: bool,
    error: Option<String>,
    refreshed: bool,
    status_code: Option<u16>,
    latency_ms: u64,
    tested_at: String,
}

async fn test_api_key_connection(
    conn: &ProviderConnection,
    client: &Client,
) -> (bool, Option<String>, Option<u16>) {
    let api_key = conn.data.api_key.as_deref().unwrap_or("");
    let psd = get_psd_object(conn);

    if is_openai_compatible(&conn.provider) {
        let base_url = psd
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .or(conn.data.base_url.as_deref())
            .unwrap_or("")
            .trim()
            .trim_end_matches('/');
        if base_url.is_empty() {
            return (false, Some("Missing base URL".to_string()), None);
        }
        return match client
            .get(format!("{base_url}/models"))
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .send()
            .await
        {
            Ok(r) => {
                let st = r.status().as_u16();
                let ok = r.status().is_success();
                (ok, (!ok).then(|| "Invalid API key or base URL".to_string()), Some(st))
            }
            Err(e) => (false, Some(e.to_string()), None),
        };
    }

    if is_anthropic_compatible(&conn.provider) {
        let mut base_url = psd
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .or(conn.data.base_url.as_deref())
            .unwrap_or("")
            .trim()
            .trim_end_matches('/')
            .to_string();
        if base_url.is_empty() {
            return (false, Some("Missing base URL".to_string()), None);
        }
        if base_url.ends_with("/messages") {
            base_url = base_url[..base_url.len() - "/messages".len()].to_string();
        }
        let model = conn
            .data
            .default_model
            .as_deref()
            .unwrap_or("claude-3-haiku-20240307");
        return match client
            .post(format!("{base_url}/v1/messages"))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
            .json(&json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "test" }]
            }))
            .send()
            .await
        {
            Ok(r) => {
                let st = r.status().as_u16();
                let valid = st != 401 && st != 403;
                (valid, (!valid).then(|| "Invalid API key or base URL".to_string()), Some(st))
            }
            Err(e) => (false, Some(e.to_string()), None),
        };
    }

    if uses_aws_credential_form(&conn.provider) {
        return probe_bedrock_credentials(conn, client).await;
    }

    if has_web_identity_probe(&conn.provider) {
        let id_res = fetch_web_identity(&conn.provider, api_key).await;
        return match id_res.status {
            "valid" => (true, None, Some(200)),
            "invalid" => (false, id_res.error, Some(401)),
            _ => (
                true,
                Some(format!(
                    "Could not verify the session: {}",
                    id_res.error.unwrap_or_default()
                )),
                None,
            ),
        };
    }

    match conn.provider.as_str() {
        "commandcode" => {
            match client
                .get("https://api.commandcode.ai/alpha/whoami?limits=1")
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .header(header::ACCEPT, "application/json")
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    let err = if ok {
                        None
                    } else if st == 401 || st == 403 {
                        Some("Invalid API key".to_string())
                    } else {
                        Some(format!("Command Code API error ({st})"))
                    };
                    (ok, err, Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "cloudflare-ai" => {
            let Some(account_id) = psd.get("accountId").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return (false, Some("Missing Account ID".to_string()), None);
            };
            let url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1/chat/completions");
            match client
                .post(url)
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .json(&json!({
                    "model": "@cf/meta/llama-3.1-8b-instruct",
                    "messages": [{ "role": "user", "content": "test" }],
                    "max_tokens": 1
                }))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let valid = st != 401 && st != 403 && st != 404;
                    (valid, (!valid).then(|| "Invalid API token or Account ID".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "azure" => {
            let endpoint = psd
                .get("azureEndpoint")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim_end_matches('/');
            let deployment = psd
                .get("deployment")
                .and_then(|v| v.as_str())
                .unwrap_or("gpt-4");
            let api_version = psd
                .get("apiVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2024-10-01-preview");
            let url = format!("{endpoint}/openai/deployments/{deployment}/chat/completions?api-version={api_version}");
            let mut req = client.post(url).header("api-key", api_key);
            if let Some(org) = psd.get("organization").and_then(|v| v.as_str()) {
                req = req.header("OpenAI-Organization", org);
            }
            match req
                .json(&json!({
                    "messages": [{ "role": "user", "content": "test" }],
                    "max_completion_tokens": 1
                }))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let valid = st != 401 && st != 403;
                    (valid, (!valid).then(|| "Invalid API key or Azure configuration".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "openai" => bearer_get_probe(client, "https://api.openai.com/v1/models", api_key).await,
        "vercel-ai-gateway" => {
            bearer_get_probe(client, "https://ai-gateway.vercel.sh/v1/models", api_key).await
        }
        "anthropic" => {
            anthropic_msg_probe(
                client,
                "https://api.anthropic.com/v1/messages",
                api_key,
                "claude-3-haiku-20240307",
                false,
            )
            .await
        }
        "gemini" => {
            match client
                .get(format!(
                    "https://generativelanguage.googleapis.com/v1/models?key={api_key}"
                ))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "Invalid API key".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "openrouter" => bearer_get_probe(client, "https://openrouter.ai/api/v1/auth/key", api_key).await,
        "nous" => {
            match client
                .post("https://inference-api.nousresearch.com/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .json(&json!({
                    "model": "Hermes-3-Llama-3.1-70B",
                    "messages": [{ "role": "user", "content": "ping" }],
                    "max_tokens": 1,
                    "stream": false
                }))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let valid = st != 401 && st != 403;
                    (valid, (!valid).then(|| "Invalid API key".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "glm" => {
            anthropic_msg_probe(
                client,
                "https://api.z.ai/api/anthropic/v1/messages",
                api_key,
                "glm-4.7",
                true,
            )
            .await
        }
        "glm-cn" => {
            openai_post_probe(
                client,
                "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
                api_key,
                "glm-4.7",
            )
            .await
        }
        "minimax" => {
            anthropic_msg_probe(
                client,
                "https://api.minimax.io/anthropic/v1/messages",
                api_key,
                "minimax-m2",
                true,
            )
            .await
        }
        "minimax-cn" => {
            anthropic_msg_probe(
                client,
                "https://api.minimaxi.com/anthropic/v1/messages",
                api_key,
                "minimax-m2",
                true,
            )
            .await
        }
        "kimi" => {
            anthropic_msg_probe(
                client,
                "https://api.kimi.com/coding/v1/messages",
                api_key,
                "kimi-latest",
                true,
            )
            .await
        }
        "alicode" | "alicode-intl" | "alims-intl" => {
            let url = match conn.provider.as_str() {
                "alicode-intl" => "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
                "alims-intl" => "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
                _ => "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
            };
            openai_post_probe(client, url, api_key, "qwen3.5-plus").await
        }
        "volcengine-ark" => {
            openai_post_probe(
                client,
                "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
                api_key,
                "doubao-1.5-pro-32k",
            )
            .await
        }
        "byteplus" => {
            openai_post_probe(
                client,
                "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
                api_key,
                "seed-2-0-pro-260328",
            )
            .await
        }
        "deepseek" => bearer_get_probe(client, "https://api.deepseek.com/models", api_key).await,
        "groq" => bearer_get_probe(client, "https://api.groq.com/openai/v1/models", api_key).await,
        "mistral" => bearer_get_probe(client, "https://api.mistral.ai/v1/models", api_key).await,
        "xai" => bearer_get_probe(client, "https://api.x.ai/v1/models", api_key).await,
        "nvidia" => bearer_get_probe(client, "https://integrate.api.nvidia.com/v1/models", api_key).await,
        "perplexity" => bearer_get_probe(client, "https://api.perplexity.ai/v1/models", api_key).await,
        "together" => bearer_get_probe(client, "https://api.together.xyz/v1/models", api_key).await,
        "fireworks" => {
            bearer_get_probe(client, "https://api.fireworks.ai/inference/v1/models", api_key).await
        }
        "cerebras" => bearer_get_probe(client, "https://api.cerebras.ai/v1/models", api_key).await,
        "cohere" => bearer_get_probe(client, "https://api.cohere.ai/v1/models", api_key).await,
        "nebius" => bearer_get_probe(client, "https://api.studio.nebius.ai/v1/models", api_key).await,
        "siliconflow" => {
            bearer_get_probe(client, "https://api.siliconflow.com/v1/models", api_key).await
        }
        "hyperbolic" => {
            bearer_get_probe(client, "https://api.hyperbolic.xyz/v1/models", api_key).await
        }
        "tokenharbor" => {
            bearer_get_probe(client, "https://tokenharbor.ai/v1/models", api_key).await
        }
        "dahl" => {
            bearer_get_probe(client, "https://inference.dahl.global/v1/models", api_key).await
        }
        "atria" => {
            bearer_get_probe(client, "https://api.atria-asi.ai/v1/models", api_key).await
        }
        "agnes" => {
            bearer_get_probe(client, "https://apihub.agnes-ai.com/v1/models", api_key).await
        }
        "bai" => bearer_get_probe(client, "https://api.b.ai/v1/models", api_key).await,
        "muse" => bearer_get_probe(client, "https://api.meta.ai/v1/models", api_key).await,
        "runcrate" => {
            bearer_get_probe(client, "https://www.runcrate.ai/api/v1/billing/balance", api_key).await
        }
        "synthetic" => {
            bearer_get_probe(client, "https://api.synthetic.new/v2/quotas", api_key).await
        }
        "featherless" => {
            bearer_get_probe(client, "https://api.featherless.ai/v1/models", api_key).await
        }
        "venice" => {
            bearer_get_probe(client, "https://api.venice.ai/api/v1/models", api_key).await
        }
        "api-airforce" => bearer_get_probe(client, "https://api.airforce/v1/models", api_key).await,
        "baidu" => {
            bearer_get_probe(client, "https://qianfan.baidubce.com/v2/models", api_key).await
        }
        "bazaarlink" => {
            bearer_get_probe(client, "https://bazaarlink.ai/api/v1/models", api_key).await
        }
        "bluesminds" => {
            bearer_get_probe(client, "https://api.bluesminds.com/v1/models", api_key).await
        }
        "kilo-gateway" => {
            bearer_get_probe(client, KILO_MODELS_URL, api_key).await
        }
        "sambanova" => {
            bearer_get_probe(client, "https://api.sambanova.ai/v1/models", api_key).await
        }
        "tencent" => {
            bearer_get_probe(client, "https://api.hunyuan.cloud.tencent.com/v1/models", api_key).await
        }
        "morph" => bearer_get_probe(client, "https://api.morphllm.com/v1/models", api_key).await,
        "poolside" => {
            bearer_get_probe(client, "https://inference.poolside.ai/v1/models", api_key).await
        }
        "tokenrouter" => {
            bearer_get_probe(client, "https://api.tokenrouter.com/v1/models", api_key).await
        }
        "ollama" => bearer_get_probe(client, "https://ollama.com/api/tags", api_key).await,
        "ollama-local" => {
            let host = psd
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("http://localhost:11434")
                .trim_end_matches('/');
            match client.get(format!("{host}/api/tags")).send().await {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| format!("Ollama not reachable at {host}")), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "deepgram" => {
            match client
                .get("https://api.deepgram.com/v1/projects")
                .header(header::AUTHORIZATION, format!("Token {api_key}"))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "Invalid API key".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "assemblyai" => {
            bearer_get_probe(client, "https://api.assemblyai.com/v1/account", api_key).await
        }
        "nanobanana" => {
            bearer_get_probe(client, "https://api.nanobananaapi.ai/v1/models", api_key).await
        }
        "fal-ai" => {
            match client
                .get("https://api.fal.ai/v1/models?limit=1")
                .header(header::AUTHORIZATION, format!("Key {api_key}"))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let valid = st != 401 && st != 403;
                    (valid, (!valid).then(|| "Invalid API key".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "chutes" => bearer_get_probe(client, "https://llm.chutes.ai/v1/models", api_key).await,
        "perplexity-web" => {
            let token = api_key
                .strip_prefix("__Secure-next-auth.session-token=")
                .unwrap_or(api_key);
            match client
                .get("https://www.perplexity.ai/api/auth/session")
                .header(header::USER_AGENT, "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .header(header::COOKIE, format!("__Secure-next-auth.session-token={token}"))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    if !r.status().is_success() {
                        return (false, Some("Invalid session cookie".to_string()), Some(st));
                    }
                    let valid = r
                        .json::<Value>()
                        .await
                        .ok()
                        .and_then(|v| v.get("user").cloned())
                        .map(|u| !u.is_null())
                        .unwrap_or(false);
                    (
                        valid,
                        (!valid).then(|| "Session expired — re-paste cookie".to_string()),
                        Some(st),
                    )
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "opencode" => {
            match client
                .get("https://opencode.ai/zen/v1/models")
                .header(header::AUTHORIZATION, "Bearer public")
                .header(header::USER_AGENT, "opencode/1.18.31")
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "OpenCode free tier unavailable".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "opencode-go" => {
            openai_post_probe(
                client,
                "https://opencode.ai/inference/go/openai/v1/chat/completions",
                api_key,
                "gpt-4o-mini",
            )
            .await
        }
        "xiaomi-mimo" => {
            bearer_get_probe(client, "https://api.xiaomimimo.com/v1/models", api_key).await
        }
        "xiaomi-tokenplan" => {
            bearer_get_probe(client, "https://token-plan-sgp.xiaomimimo.com/v1/models", api_key).await
        }
        "blackbox" => {
            bearer_get_probe(client, "https://api.blackbox.ai/v1/models", api_key).await
        }
        "qoder" | "qoder-cn" => {
            let url = if conn.provider == "qoder-cn" {
                "https://openapi.qoder.com.cn/api/v1/jobToken/exchange"
            } else {
                "https://openapi.qoder.sh/api/v1/jobToken/exchange"
            };
            let pat = if api_key.starts_with("pt-") {
                api_key.to_string()
            } else {
                format!("pt-{api_key}")
            };
            match client
                .post(url)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header("Cosy-Version", "1.0.1")
                .header("Cosy-ClientType", "5")
                .json(&json!({ "personal_token": pat }))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "Invalid Personal Access Token".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "llm7" => {
            let base = psd
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("https://api.llm7.io/v1")
                .trim_end_matches('/');
            match client
                .get(format!("{base}/models"))
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "Invalid API key or base URL".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "sensenova" | "sensenova-intl" => {
            let url = if conn.provider == "sensenova-intl" {
                "https://token.sensenova.ai/v1/models"
            } else {
                "https://token.sensenova.cn/v1/models"
            };
            match client
                .get(url)
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .header(header::ACCEPT, "application/json")
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    let err = if ok {
                        None
                    } else if st == 401 || st == 403 {
                        Some("Invalid API key".to_string())
                    } else {
                        Some(format!("SenseNova API error ({st})"))
                    };
                    (ok, err, Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        "kimchi" => {
            match client
                .get("https://api.cast.ai/v1/llm/openai/supported-providers")
                .header(header::ACCEPT, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
                .header(header::USER_AGENT, "kimchi/0.1.40")
                .send()
                .await
            {
                Ok(r) => {
                    let st = r.status().as_u16();
                    let ok = r.status().is_success();
                    (ok, (!ok).then(|| "Invalid API key".to_string()), Some(st))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        _ => (false, Some("Provider test not supported".to_string()), None),
    }
}

async fn bearer_get_probe(client: &Client, url: &str, api_key: &str) -> (bool, Option<String>, Option<u16>) {
    match client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) => {
            let st = r.status().as_u16();
            let ok = r.status().is_success();
            (ok, (!ok).then(|| "Invalid API key".to_string()), Some(st))
        }
        Err(e) => (false, Some(e.to_string()), None),
    }
}

async fn anthropic_msg_probe(
    client: &Client,
    url: &str,
    api_key: &str,
    model: &str,
    check_403: bool,
) -> (bool, Option<String>, Option<u16>) {
    match client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "test" }]
        }))
        .send()
        .await
    {
        Ok(r) => {
            let st = r.status().as_u16();
            let valid = st != 401 && (!check_403 || st != 403);
            (valid, (!valid).then(|| "Invalid API key".to_string()), Some(st))
        }
        Err(e) => (false, Some(e.to_string()), None),
    }
}

async fn openai_post_probe(
    client: &Client,
    url: &str,
    api_key: &str,
    model: &str,
) -> (bool, Option<String>, Option<u16>) {
    match client
        .post(url)
        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "test" }]
        }))
        .send()
        .await
    {
        Ok(r) => {
            let st = r.status().as_u16();
            let valid = st != 401 && st != 403;
            (valid, (!valid).then(|| "Invalid API key".to_string()), Some(st))
        }
        Err(e) => (false, Some(e.to_string()), None),
    }
}

async fn test_oauth_connection(
    db: &Database,
    conn: &mut ProviderConnection,
    client: &Client,
) -> (bool, Option<String>, Option<String>, bool, Option<u16>) {
    let provider_owned = conn.provider.clone();
    let provider = provider_owned.as_str();
    let token_exists = matches!(provider, "cursor" | "codebuddy-cn" | "codebuddy-intl");
    let check_expiry = matches!(provider, "claude" | "kiro" | "kimi" | "kimi-coding");
    let refreshable = matches!(
        provider,
        "claude" | "codex" | "gemini-cli" | "antigravity" | "kiro" | "kimi" | "kimi-coding" | "cline" | "grok-cli" | "nous"
    );

    let supported = token_exists
        || check_expiry
        || matches!(
            provider,
            "codex"
                | "gemini-cli"
                | "antigravity"
                | "github"
                | "iflow"
                | "qoder"
                | "qoder-cn"
                | "kilocode"
                | "cline"
                | "gitlab"
                | "kimchi"
                | "grok-cli"
                | "muse"
                | "nous"
        );
    if !supported {
        return (false, Some("Provider test not supported".to_string()), None, false, None);
    }

    let Some(mut access_token) = extract_access_token(conn) else {
        return (false, Some("No access token".to_string()), None, false, None);
    };

    if token_exists {
        return (true, None, None, false, Some(200));
    }

    let expired = extract_token_expiry(conn)
        .map(|exp| exp <= Utc::now())
        .unwrap_or(false);
    let mut refreshed = false;

    if refreshable && expired {
        let refresh_res = refresh_oauth_token(client, conn, db).await;
        if refresh_res.success {
            refreshed = true;
            if let Some(new_tok) = refresh_res.new_access_token {
                access_token = new_tok;
            }
            if let Ok(Some(updated_conn)) = find_connection_by_id(db, &conn.id) {
                *conn = updated_conn;
            }
        } else {
            return (
                false,
                Some("Token expired and refresh failed".to_string()),
                None,
                false,
                None,
            );
        }
    }

    if check_expiry {
        if refreshed {
            return (true, None, None, true, Some(200));
        }
        if expired {
            return (false, Some("Token expired".to_string()), None, false, None);
        }
        return (true, None, None, false, Some(200));
    }

    if provider == "gemini-cli" || provider == "antigravity" {
        let ua = if provider == "antigravity" {
            "antigravity/hub/2.24.0 darwin/arm64"
        } else {
            "google-api-nodejs-client/9.15.1 gemini-cli/0.34.0"
        };
        let url = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
        let body = json!({
            "metadata": {
                "ideType": "IDE_UNSPECIFIED",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI"
            }
        });
        if let Ok(r) = client
            .post(url)
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(header::USER_AGENT, ua)
            .json(&body)
            .send()
            .await
        {
            let st = r.status().as_u16();
            if r.status().is_success() {
                return (true, None, None, refreshed, Some(st));
            }
            if st == 401 && !refreshed {
                let rr = refresh_oauth_token(client, conn, db).await;
                if rr.success {
                    if let Some(new_tok) = rr.new_access_token {
                        if let Ok(r2) = client
                            .post(url)
                            .header(header::AUTHORIZATION, format!("Bearer {new_tok}"))
                            .header(header::USER_AGENT, ua)
                            .json(&body)
                            .send()
                            .await
                        {
                            let st2 = r2.status().as_u16();
                            if r2.status().is_success() {
                                return (true, None, None, true, Some(st2));
                            }
                            return (
                                false,
                                Some(format!("API returned {st2}")),
                                None,
                                true,
                                Some(st2),
                            );
                        }
                    }
                }
                return (
                    false,
                    Some("Token invalid or revoked".to_string()),
                    None,
                    false,
                    Some(401),
                );
            }
            return (false, Some(format!("API returned {st}")), None, refreshed, Some(st));
        }
        return (false, Some("Failed to probe Cloud Code Assist".to_string()), None, refreshed, None);
    }

    // Standard OAuth probes
    let (url, method, accept_status, soft_msg, probe_body, extra_headers): (
        String,
        reqwest::Method,
        Option<u16>,
        Option<&'static str>,
        Option<Value>,
        Vec<(&'static str, &'static str)>,
    ) = match provider {
        "codex" => (
            "https://chatgpt.com/backend-api/codex/responses".to_string(),
            reqwest::Method::POST,
            Some(400),
            None,
            Some(json!({ "model": "gpt-5.3-codex", "input": [], "stream": false, "store": false })),
            vec![("originator", "codex_cli_rs"), ("User-Agent", "codex_cli_rs/0.167.1")],
        ),
        "github" => (
            "https://api.github.com/user".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![("User-Agent", "9Router"), ("Accept", "application/vnd.github+json")],
        ),
        "iflow" => (
            format!(
                "https://iflow.cn/api/oauth/getUserInfo?accessToken={}",
                urlencoding_component(&access_token)
            ),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![],
        ),
        "qoder" => (
            "https://openapi.qoder.sh/api/v1/userinfo".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![],
        ),
        "qoder-cn" => (
            "https://openapi.qoder.com.cn/api/v1/userinfo".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![],
        ),
        "kilocode" => (
            "https://api.kilo.ai/api/profile".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![],
        ),
        "cline" => (
            "https://api.cline.bot/api/v1/users/me".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![("Accept", "application/json")],
        ),
        "gitlab" => (
            "https://gitlab.com/api/v4/user".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![],
        ),
        "kimchi" => (
            "https://api.cast.ai/v1/llm/openai/supported-providers".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![("Accept", "application/json"), ("User-Agent", "kimchi/0.1.40")],
        ),
        "grok-cli" => (
            "https://cli-chat-proxy.grok.com/v1/user".to_string(),
            reqwest::Method::GET,
            Some(402),
            Some("Connected, but Grok Build credits are exhausted (spending limit). Add credits or upgrade SuperGrok."),
            None,
            vec![
                ("Accept", "application/json"),
                ("User-Agent", "grok-shell/1.0.44 (linux; x86_64)"),
                ("x-grok-client-identifier", "grok-shell"),
                ("x-grok-client-version", "1.0.44"),
            ],
        ),
        "muse" => (
            "https://api.meta.ai/v1/models".to_string(),
            reqwest::Method::GET,
            None,
            None,
            None,
            vec![("x-api-version", "1.0.0")],
        ),
        _ => (
            "https://inference-api.nousresearch.com/v1/chat/completions".to_string(),
            reqwest::Method::POST,
            None,
            None,
            Some(json!({
                "model": "Hermes-3-Llama-3.1-70B",
                "messages": [{ "role": "user", "content": "ping" }],
                "max_tokens": 1,
                "stream": false
            })),
            vec![],
        ),
    };

    let mut req = client.request(method, &url);
    if provider != "iflow" {
        req = req.header(header::AUTHORIZATION, format!("Bearer {access_token}"));
    }
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }
    if let Some(b) = probe_body {
        req = req.json(&b);
    }

    match req.send().await {
        Ok(r) => {
            let st = r.status().as_u16();
            if r.status().is_success() {
                return (true, None, None, refreshed, Some(st));
            }
            if accept_status == Some(st) {
                if let Some(msg) = soft_msg {
                    return (
                        true,
                        Some(msg.to_string()),
                        Some(msg.to_string()),
                        refreshed,
                        Some(st),
                    );
                }
                return (true, None, None, refreshed, Some(st));
            }
            let err = match st {
                401 => "Token invalid or revoked".to_string(),
                403 => "Access denied".to_string(),
                _ => format!("API returned {st}"),
            };
            (false, Some(err), None, refreshed, Some(st))
        }
        Err(e) => (false, Some(e.to_string()), None, refreshed, None),
    }
}

fn urlencoding_component(input: &str) -> String {
    percent_encoding::utf8_percent_encode(input, percent_encoding::NON_ALPHANUMERIC).to_string()
}

async fn test_single_connection(db: &Database, id: &str) -> Option<ConnectionProbeOutcome> {
    let mut conn = find_connection_by_id(db, id).ok()??;
    let psd = get_psd_object(&conn);
    let effective_proxy = resolve_effective_proxy(db, &psd);

    let client = match build_client_for_proxy(&effective_proxy, 15) {
        Ok(c) => c,
        Err(err_msg) => {
            let now = Utc::now().to_rfc3339();
            set_connection_json_field(&mut conn, "testStatus", Some(json!("error")));
            set_connection_json_field(&mut conn, "lastError", Some(json!(err_msg)));
            set_connection_json_field(&mut conn, "lastErrorAt", Some(json!(now)));
            conn.updated_at = now.clone();
            let _ = db.save_provider_connection(&conn);
            return Some(ConnectionProbeOutcome {
                valid: false,
                error: Some(err_msg),
                refreshed: false,
                status_code: None,
                latency_ms: 0,
                tested_at: now,
            });
        }
    };

    if effective_proxy.connection_proxy_enabled
        && !effective_proxy.connection_proxy_url.is_empty()
        && effective_proxy.vercel_relay_url.is_empty()
    {
        if let Err(err) = client.get("https://api.ipify.org/").send().await {
            let proxy_err = format!("Proxy test failed: {err}");
            let now = Utc::now().to_rfc3339();
            set_connection_json_field(&mut conn, "testStatus", Some(json!("error")));
            set_connection_json_field(&mut conn, "lastError", Some(json!(proxy_err)));
            set_connection_json_field(&mut conn, "lastErrorAt", Some(json!(now)));
            conn.updated_at = now.clone();
            let _ = db.save_provider_connection(&conn);
            return Some(ConnectionProbeOutcome {
                valid: false,
                error: Some(proxy_err),
                refreshed: false,
                status_code: None,
                latency_ms: 0,
                tested_at: now,
            });
        }
    }

    let started = Instant::now();
    let (valid, error, warning, refreshed, status_code) = if conn
        .auth_type
        .eq_ignore_ascii_case("apikey")
        || conn.auth_type.eq_ignore_ascii_case("cookie")
    {
        let (v, e, st) = test_api_key_connection(&conn, &client).await;
        (v, e, None, false, st)
    } else {
        test_oauth_connection(db, &mut conn, &client).await
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    let now = Utc::now().to_rfc3339();

    let soft_warning = if valid {
        warning.as_ref().or(error.as_ref()).cloned()
    } else {
        None
    };

    set_connection_json_field(
        &mut conn,
        "testStatus",
        Some(json!(if valid { "active" } else { "error" })),
    );
    if valid {
        set_connection_json_field(
            &mut conn,
            "lastError",
            soft_warning.as_ref().map(|w| json!(w)).or(Some(Value::Null)),
        );
        set_connection_json_field(
            &mut conn,
            "lastErrorAt",
            soft_warning.as_ref().map(|_| json!(now)).or(Some(Value::Null)),
        );
    } else {
        set_connection_json_field(
            &mut conn,
            "lastError",
            error.as_ref().map(|e| json!(e)).or(Some(Value::Null)),
        );
        set_connection_json_field(&mut conn, "lastErrorAt", Some(json!(now)));
    }
    conn.updated_at = now.clone();
    let _ = db.save_provider_connection(&conn);

    Some(ConnectionProbeOutcome {
        valid,
        error,
        refreshed,
        status_code,
        latency_ms,
        tested_at: now,
    })
}

/// `POST /api/providers/{id}/test`
pub async fn provider_test_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    match test_single_connection(&state.db, &id).await {
        Some(res) => Json(json!({
            "valid": res.valid,
            "error": res.error,
            "refreshed": res.refreshed,
            "latencyMs": res.latency_ms,
            "testedAt": res.tested_at,
        }))
        .into_response(),
        None => json_err(StatusCode::NOT_FOUND, "Connection not found"),
    }
}

// ============================================================================
// 11. POST /api/providers/{id}/test-models
// ============================================================================

fn build_internal_ping_headers(db: &Database) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );

    if let Ok(keys) = db.list_api_keys() {
        let active: Vec<_> = keys.into_iter().filter(|k| k.is_active).collect();
        let preferred = active
            .iter()
            .find(|k| k.allowed_models == "*" && k.blocked_models.is_empty())
            .or_else(|| active.first());
        if let Some(k) = preferred {
            if let Ok(hv) = header::HeaderValue::from_str(&format!("Bearer {}", k.key)) {
                headers.insert(header::AUTHORIZATION, hv);
            }
        }
    }

    let admin_tok = resolve_configured_admin_token();
    if !admin_tok.is_empty() {
        if let Ok(hv) = header::HeaderValue::from_str(&admin_tok) {
            headers.insert("x-9r-cli-token", hv);
        }
    }

    headers
}

async fn ping_model_by_kind(
    client: &Client,
    base_url: &str,
    headers: &HeaderMap,
    model: &str,
    kind: &str,
) -> Value {
    let start = Instant::now();
    let res = match kind {
        "embedding" => {
            client
                .post(format!("{base_url}/api/v1/embeddings"))
                .headers(headers.clone())
                .json(&json!({ "model": model, "input": "test" }))
                .send()
                .await
        }
        "image" => {
            client
                .post(format!("{base_url}/api/v1/images/generations"))
                .headers(headers.clone())
                .json(&json!({ "model": model, "prompt": "test" }))
                .send()
                .await
        }
        "systemone" => {
            client
                .post(format!("{base_url}/api/v1/systemone"))
                .headers(headers.clone())
                .json(&json!({
                    "model": model,
                    "state": "Customer: I was charged twice for my order this morning.",
                    "questions": {
                        "probe": { "type": "noul", "instructions": "Is the customer reporting a billing problem?" }
                    }
                }))
                .send()
                .await
        }
        _ => {
            client
                .post(format!("{base_url}/api/v1/chat/completions"))
                .headers(headers.clone())
                .json(&json!({
                    "model": model,
                    "max_tokens": 1024,
                    "stream": false,
                    "messages": [{ "role": "user", "content": "hi" }]
                }))
                .send()
                .await
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    match res {
        Err(e) => json!({
            "ok": false,
            "latencyMs": latency_ms,
            "status": 500,
            "error": e.to_string(),
        }),
        Ok(resp) => {
            let status = resp.status().as_u16();
            let is_ok = resp.status().is_success();
            let raw_text = resp.text().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&raw_text).ok();

            if !is_ok {
                let detail = parsed
                    .as_ref()
                    .and_then(|p| {
                        p.get("error")
                            .and_then(|e| e.get("message").or(Some(e)))
                            .or_else(|| p.get("msg"))
                            .or_else(|| p.get("message"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or(&raw_text);
                let preview: String = detail.chars().take(240).collect();
                let err_msg = if preview.is_empty() {
                    format!("HTTP {status}")
                } else {
                    format!("HTTP {status}: {preview}")
                };
                return json!({
                    "ok": false,
                    "latencyMs": latency_ms,
                    "status": status,
                    "error": err_msg,
                });
            }

            if kind == "embedding" {
                let has_embed = parsed
                    .as_ref()
                    .and_then(|p| p.get("data")?.get(0)?.get("embedding")?.as_array())
                    .is_some();
                if !has_embed {
                    return json!({
                        "ok": false,
                        "latencyMs": latency_ms,
                        "status": status,
                        "error": "Provider returned no embedding data",
                    });
                }
            } else if kind == "image" {
                let has_img = parsed
                    .as_ref()
                    .and_then(|p| p.get("data")?.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !has_img {
                    return json!({
                        "ok": false,
                        "latencyMs": latency_ms,
                        "status": status,
                        "error": "Provider returned no image data for this model",
                    });
                }
            } else if kind == "systemone" {
                let has_ans = parsed
                    .as_ref()
                    .and_then(|p| p.get("answers")?.as_object())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !has_ans {
                    return json!({
                        "ok": false,
                        "latencyMs": latency_ms,
                        "status": status,
                        "error": "Provider returned no answers for this model",
                    });
                }
            } else {
                if let Some(err_val) = parsed.as_ref().and_then(|p| p.get("error")) {
                    if !err_val.is_null() {
                        let msg = err_val
                            .get("message")
                            .and_then(|v| v.as_str())
                            .or_else(|| err_val.as_str())
                            .unwrap_or("Provider returned an error");
                        return json!({
                            "ok": false,
                            "latencyMs": latency_ms,
                            "status": status,
                            "error": msg.chars().take(240).collect::<String>(),
                        });
                    }
                }
                let has_choices = parsed
                    .as_ref()
                    .and_then(|p| p.get("choices")?.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !has_choices {
                    return json!({
                        "ok": false,
                        "latencyMs": latency_ms,
                        "status": status,
                        "error": "Provider returned no completion choices for this model",
                    });
                }
            }

            json!({
                "ok": true,
                "latencyMs": latency_ms,
                "error": Value::Null,
                "status": status,
            })
        }
    }
}

/// `POST /api/providers/{id}/test-models`
pub async fn provider_test_models_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let conn = match find_connection_by_id(&state.db, &id) {
        Ok(Some(c)) => c,
        Ok(None) => return json_err(StatusCode::NOT_FOUND, "Connection not found"),
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let provider_id = conn.provider.clone();
    let alias = ID_TO_ALIAS
        .get(provider_id.as_str())
        .copied()
        .unwrap_or(provider_id.as_str())
        .to_string();

    let mut models: Vec<(String, String, String)> = match alias.as_str() {
        "cu" => CU_PRESET_MODELS
            .iter()
            .map(|m| ((*m).to_string(), (*m).to_string(), "llm".to_string()))
            .collect(),
        "cc" => CC_PRESET_MODELS
            .iter()
            .map(|m| ((*m).to_string(), (*m).to_string(), "llm".to_string()))
            .collect(),
        "openai" => vec![
            ("gpt-4o".into(), "GPT-4o".into(), "llm".into()),
            ("gpt-4o-mini".into(), "GPT-4o Mini".into(), "llm".into()),
        ],
        "anthropic" => vec![
            ("claude-sonnet-4-20250514".into(), "Claude Sonnet 4".into(), "llm".into()),
            ("claude-3-5-sonnet-20241022".into(), "Claude 3.5 Sonnet".into(), "llm".into()),
        ],
        "gemini" => vec![
            ("gemini-2.5-pro".into(), "Gemini 2.5 Pro".into(), "llm".into()),
            ("gemini-2.5-flash".into(), "Gemini 2.5 Flash".into(), "llm".into()),
        ],
        "deepseek" => vec![
            ("deepseek-chat".into(), "DeepSeek Chat".into(), "llm".into()),
            ("deepseek-reasoner".into(), "DeepSeek Reasoner".into(), "llm".into()),
        ],
        _ => Vec::new(),
    };

    let base_url = format!("http://127.0.0.1:{}", state.config.port);
    if is_compatible_provider(&provider_id) && models.is_empty() {
        let fetched = fetch_compatible_model_entries(&conn).await;
        for item in fetched {
            if let Some(mid) = item.get("id").and_then(|v| v.as_str()) {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(mid)
                    .to_string();
                models.push((mid.to_string(), name, "llm".to_string()));
            }
        }
    }

    if models.is_empty() {
        if let Some(dm) = conn.data.default_model.as_deref().filter(|s| !s.is_empty()) {
            models.push((dm.to_string(), dm.to_string(), "llm".to_string()));
        }
    }

    if models.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "No models configured for this provider",
        );
    }

    let internal_headers = build_internal_ping_headers(&state.db);
    let client = http_client(15);

    let (first, rest) = models.split_at(1);
    let (first_id, first_name, first_kind) = &first[0];
    let first_ping = ping_model_by_kind(
        &client,
        &base_url,
        &internal_headers,
        &format!("{alias}/{first_id}"),
        first_kind,
    )
    .await;

    let mut results = Vec::with_capacity(models.len());
    let mut first_entry = first_ping.as_object().cloned().unwrap_or_default();
    first_entry.insert("modelId".to_string(), json!(first_id));
    first_entry.insert("name".to_string(), json!(first_name));
    results.push(Value::Object(first_entry));

    if !rest.is_empty() {
        let sem = Arc::new(tokio::sync::Semaphore::new(4));
        let mut handles = Vec::with_capacity(rest.len());
        for (mid, mname, mkind) in rest {
            let sem_c = Arc::clone(&sem);
            let client_c = client.clone();
            let base_c = base_url.clone();
            let hdrs_c = internal_headers.clone();
            let alias_c = alias.clone();
            let mid_c = mid.clone();
            let mname_c = mname.clone();
            let mkind_c = mkind.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem_c.acquire().await;
                let ping = ping_model_by_kind(
                    &client_c,
                    &base_c,
                    &hdrs_c,
                    &format!("{alias_c}/{mid_c}"),
                    &mkind_c,
                )
                .await;
                let mut entry = ping.as_object().cloned().unwrap_or_default();
                entry.insert("modelId".to_string(), json!(mid_c));
                entry.insert("name".to_string(), json!(mname_c));
                Value::Object(entry)
            }));
        }
        for h in handles {
            if let Ok(v) = h.await {
                results.push(v);
            }
        }
    }

    Json(json!({
        "provider": provider_id,
        "connectionId": id,
        "results": results,
    }))
    .into_response()
}

// ============================================================================
// 12. GET /api/providers/client
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ProvidersClientQuery {
    pub provider: Option<String>,
    #[serde(rename = "accountStatus")]
    pub account_status: Option<String>,
    pub sort: Option<String>,
    pub page: Option<String>,
    #[serde(rename = "pageSize")]
    pub page_size: Option<String>,
}

fn mask_client_name(name: &str) -> String {
    if name.len() <= 16 {
        return name.to_string();
    }
    let mut run = 0usize;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            run += 1;
            if run >= 32 {
                let prefix: String = name.chars().take(8).collect();
                return format!("{prefix}***");
            }
        } else {
            run = 0;
        }
    }
    name.to_string()
}

fn sanitize_client_connection(conn: &ProviderConnection) -> Value {
    let raw = parse_raw_json_obj(&conn.raw_data);
    let mut safe = Map::new();

    for &field in CLIENT_SAFE_FIELDS {
        let val = match field {
            "id" => Some(json!(conn.id)),
            "provider" => Some(json!(conn.provider)),
            "authType" => Some(json!(conn.auth_type)),
            "name" => conn.name.as_ref().map(|n| json!(mask_client_name(n))),
            "email" => conn.email.as_ref().map(|e| json!(e)),
            "priority" => conn.priority.map(|p| json!(p)),
            "isActive" => Some(json!(conn.is_active)),
            "defaultModel" => conn.data.default_model.as_ref().map(|d| json!(d)),
            "createdAt" => Some(json!(conn.created_at)),
            "updatedAt" => Some(json!(conn.updated_at)),
            other => conn
                .data
                .extra
                .get(other)
                .cloned()
                .or_else(|| raw.get(other).cloned()),
        };
        if let Some(v) = val {
            if !v.is_null() || matches!(field, "lastError" | "lastErrorAt") {
                safe.insert(field.to_string(), v);
            }
        }
    }

    let psd = get_psd_object(conn);
    if !psd.is_empty() {
        let mut safe_psd = Map::new();
        for &f in CLIENT_SAFE_PSD_FIELDS {
            if let Some(v) = psd.get(f) {
                safe_psd.insert(f.to_string(), v.clone());
            }
        }
        safe.insert("providerSpecificData".to_string(), Value::Object(safe_psd));
    }

    Value::Object(safe)
}

fn is_usage_eligible(conn: &ProviderConnection) -> bool {
    USAGE_SUPPORTED_PROVIDERS.contains(&conn.provider.as_str())
        && (conn.auth_type.eq_ignore_ascii_case("oauth")
            || conn.auth_type == "mcode-lease"
            || USAGE_APIKEY_PROVIDERS.contains(&conn.provider.as_str()))
}

fn parse_pos_usize(raw: Option<&str>, fallback: usize) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}

/// `GET /api/providers/client`
pub async fn providers_client_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProvidersClientQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let all = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to fetch providers: {e}"),
            )
        }
    };

    let eligible: Vec<ProviderConnection> = all.into_iter().filter(is_usage_eligible).collect();
    let eligible_count = eligible.len();

    let mut provider_options: Vec<String> = eligible
        .iter()
        .map(|c| c.provider.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    provider_options.sort();

    let provider_filter = query
        .provider
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("all");
    let account_status = query
        .account_status
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("all");
    let sort = query.sort.as_deref().unwrap_or("priority");
    let page = parse_pos_usize(query.page.as_deref(), 1);
    let page_size = parse_pos_usize(query.page_size.as_deref(), 20).min(500);

    let provider_filtered: Vec<ProviderConnection> = eligible
        .into_iter()
        .filter(|c| provider_filter == "all" || c.provider == provider_filter)
        .collect();
    let provider_filtered_count = provider_filtered.len();

    let mut account_filtered: Vec<ProviderConnection> = provider_filtered
        .into_iter()
        .filter(|c| match account_status {
            "active" => c.is_active,
            "inactive" => !c.is_active,
            _ => true,
        })
        .collect();

    if sort == "provider" {
        account_filtered.sort_by(|a, b| {
            let idx_a = USAGE_SUPPORTED_PROVIDERS
                .iter()
                .position(|&p| p == a.provider)
                .unwrap_or(usize::MAX);
            let idx_b = USAGE_SUPPORTED_PROVIDERS
                .iter()
                .position(|&p| p == b.provider)
                .unwrap_or(usize::MAX);
            idx_a.cmp(&idx_b).then_with(|| a.provider.cmp(&b.provider))
        });
    } else {
        account_filtered.sort_by(|a, b| {
            let pa = a.priority.unwrap_or(i64::MAX);
            let pb = b.priority.unwrap_or(i64::MAX);
            pa.cmp(&pb).then_with(|| a.provider.cmp(&b.provider))
        });
    }

    let total = account_filtered.len();
    let total_pages = (total.max(1) + page_size - 1) / page_size;
    let current_page = page.min(total_pages);
    let offset = (current_page - 1) * page_size;
    let page_connections: Vec<Value> = account_filtered
        .into_iter()
        .skip(offset)
        .take(page_size)
        .map(|c| sanitize_client_connection(&c))
        .collect();

    Json(json!({
        "connections": page_connections,
        "providerOptions": provider_options,
        "pagination": {
            "page": current_page,
            "pageSize": page_size,
            "total": total,
            "totalPages": total_pages,
        },
        "totals": {
            "eligibleConnections": eligible_count,
            "providerFilteredConnections": provider_filtered_count,
        },
    }))
    .into_response()
}

// ============================================================================
// 13. GET /api/providers/kilo/free-models
// ============================================================================

/// `GET /api/providers/kilo/free-models`
pub async fn providers_kilo_free_models_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    {
        let guard = KILO_MODELS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref cache) = *guard {
            if cache.fetched_at.elapsed() < KILO_CACHE_TTL {
                return Json(json!({ "models": cache.models, "cached": true })).into_response();
            }
        }
    }

    let client = http_client(10);
    let fetch_res = client
        .get(KILO_MODELS_URL)
        .header(header::ACCEPT, "application/json")
        .send()
        .await;

    match fetch_res {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(json_val) => {
                let free_models: Vec<Value> = json_val
                    .get("data")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter(|m| m.get("isFree").and_then(|v| v.as_bool()) == Some(true))
                            .map(|m| {
                                json!({
                                    "id": m.get("id").cloned().unwrap_or(Value::Null),
                                    "name": m.get("name").cloned().unwrap_or(Value::Null),
                                    "isFree": true,
                                    "context_length": m.get("context_length").and_then(|v| v.as_u64()).unwrap_or(0),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let mut guard = KILO_MODELS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(KiloModelsCache {
                    models: free_models.clone(),
                    fetched_at: Instant::now(),
                });
                Json(json!({ "models": free_models, "cached": false })).into_response()
            }
            Err(e) => kilo_fallback_or_error(&e.to_string()),
        },
        Ok(resp) => kilo_fallback_or_error(&format!("Kilo API returned {}", resp.status().as_u16())),
        Err(e) => kilo_fallback_or_error(&e.to_string()),
    }
}

fn kilo_fallback_or_error(err_msg: &str) -> Response {
    let guard = KILO_MODELS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref cache) = *guard {
        return Json(json!({
            "models": cache.models,
            "cached": true,
            "warning": err_msg,
        }))
        .into_response();
    }
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "models": [],
            "error": format!("Failed to fetch Kilo models: {err_msg}"),
        })),
    )
        .into_response()
}

// ============================================================================
// 14. POST /api/providers/test-batch
// ============================================================================

fn get_auth_group(provider_id: &str, conn: Option<&ProviderConnection>) -> String {
    if let Some(c) = conn {
        if !c.auth_type.is_empty() {
            if c.auth_type.eq_ignore_ascii_case("oauth") {
                if FREE_PROVIDER_IDS.contains(&provider_id) {
                    return "free".to_string();
                }
                return "oauth".to_string();
            }
            return c.auth_type.to_ascii_lowercase();
        }
    }
    if FREE_PROVIDER_IDS.contains(&provider_id) {
        return "free".to_string();
    }
    if OAUTH_PROVIDER_IDS.contains(&provider_id) {
        return "oauth".to_string();
    }
    if is_compatible_provider(provider_id) {
        return "compatible".to_string();
    }
    "apikey".to_string()
}

/// `POST /api/providers/test-batch`
pub async fn providers_test_batch_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let mode = body
        .get("mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if mode.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "mode is required");
    }

    let provider_id = body
        .get("providerId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    let active_connections = match state.db.get_active_provider_connections(None) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let targets: Vec<ProviderConnection> = match mode {
        "provider" if provider_id.is_some() => {
            let pid = provider_id.as_deref().unwrap_or("");
            active_connections
                .into_iter()
                .filter(|c| c.provider == pid)
                .collect()
        }
        "oauth" => active_connections
            .into_iter()
            .filter(|c| get_auth_group(&c.provider, Some(c)) == "oauth")
            .collect(),
        "free" => active_connections
            .into_iter()
            .filter(|c| get_auth_group(&c.provider, Some(c)) == "free")
            .collect(),
        "apikey" => active_connections
            .into_iter()
            .filter(|c| get_auth_group(&c.provider, Some(c)) == "apikey")
            .collect(),
        "compatible" => active_connections
            .into_iter()
            .filter(|c| is_compatible_provider(&c.provider))
            .collect(),
        "all" => active_connections,
        _ => {
            return json_err(
                StatusCode::BAD_REQUEST,
                "Invalid mode. Use: provider, oauth, free, apikey, compatible, all",
            )
        }
    };

    if targets.is_empty() {
        return Json(json!({
            "mode": mode,
            "providerId": provider_id,
            "results": [],
            "summary": { "total": 0, "passed": 0, "failed": 0 },
            "testedAt": Utc::now().to_rfc3339(),
        }))
        .into_response();
    }

    let mut results = Vec::with_capacity(targets.len());
    for conn in targets {
        let auth_group = if !conn.auth_type.is_empty() {
            conn.auth_type.clone()
        } else {
            get_auth_group(&conn.provider, Some(&conn))
        };
        let conn_name = conn
            .name
            .clone()
            .or_else(|| conn.email.clone())
            .unwrap_or_else(|| conn.provider.clone());
        match test_single_connection(&state.db, &conn.id).await {
            Some(outcome) => {
                results.push(json!({
                    "provider": conn.provider,
                    "connectionId": conn.id,
                    "connectionName": conn_name,
                    "authType": auth_group,
                    "valid": outcome.valid,
                    "latencyMs": outcome.latency_ms,
                    "error": outcome.error,
                    "diagnosis": Value::Null,
                    "statusCode": outcome.status_code,
                    "testedAt": outcome.tested_at,
                }));
            }
            None => {
                results.push(json!({
                    "provider": conn.provider,
                    "connectionId": conn.id,
                    "connectionName": conn_name,
                    "authType": auth_group,
                    "valid": false,
                    "latencyMs": 0,
                    "error": "Connection not found",
                    "diagnosis": {
                        "type": "network_error",
                        "source": "local",
                        "code": Value::Null,
                        "message": "Connection not found"
                    },
                    "statusCode": Value::Null,
                    "testedAt": Utc::now().to_rfc3339(),
                }));
            }
        }
    }

    let passed = results
        .iter()
        .filter(|r| r.get("valid").and_then(|v| v.as_bool()) == Some(true))
        .count();
    let total = results.len();

    Json(json!({
        "mode": mode,
        "providerId": provider_id,
        "results": results,
        "testedAt": Utc::now().to_rfc3339(),
        "summary": {
            "total": total,
            "passed": passed,
            "failed": total - passed,
        },
    }))
    .into_response()
}

// ============================================================================
// 15. /api/providers/web-login (GET, POST, DELETE)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct WebLoginQuery {
    pub provider: Option<String>,
}

fn sanitize_profile_name(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

fn list_web_login_profiles(data_dir: &FsPath) -> Vec<String> {
    let root = data_dir.join("browser-profiles");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut profiles: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name != "web-login")
        .collect();
    profiles.sort();
    profiles
}

fn resolve_chrome_executable() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CHROME_PATH") {
        let pb = PathBuf::from(p.trim());
        if pb.is_file() {
            return Some(pb);
        }
    }
    let candidates = [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// `GET /api/providers/web-login`
pub async fn providers_web_login_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WebLoginQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let Some(provider) = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Json(json!({
            "providers": WEB_LOGIN_PROVIDERS,
            "profiles": list_web_login_profiles(&state.config.data_dir),
        }))
        .into_response();
    };

    let Some(mut session) = WEB_LOGIN_SESSIONS.get(provider).map(|s| s.clone()) else {
        return json_err(StatusCode::NOT_FOUND, "No login session for this provider");
    };

    if session.status == "success" && session.saved_connection_id.is_none() {
        let cred = session.credential.clone().unwrap_or_default();
        let identity = fetch_web_identity(provider, &cred).await;
        if identity.status == "invalid" {
            session.status = "error".to_string();
            session.error = Some(
                identity
                    .error
                    .unwrap_or_else(|| "The captured session was rejected".to_string()),
            );
            WEB_LOGIN_SESSIONS.insert(provider.to_string(), session.clone());
            return Json(json!({
                "provider": provider,
                "status": session.status,
                "profile": session.profile,
                "error": session.error,
            }))
            .into_response();
        }

        let email = identity.email;
        let name = if email.is_empty() {
            provider.to_string()
        } else {
            email.clone()
        };
        session.account_email = Some(email.clone());

        let mut psd = Map::new();
        psd.insert("authMethod".to_string(), json!("web-login"));
        if provider == "deepseek-web" {
            psd.insert("tokenSource".to_string(), json!("localStorage"));
        }

        let all_conns = state.db.load_provider_connections().unwrap_or_default();
        let mut web_rows: Vec<ProviderConnection> = all_conns
            .into_iter()
            .filter(|c| {
                c.provider == provider
                    && get_psd_object(c)
                        .get("authMethod")
                        .and_then(|v| v.as_str())
                        == Some("web-login")
            })
            .collect();
        web_rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let existing = if !email.is_empty() {
            web_rows
                .iter()
                .find(|c| c.email.as_deref() == Some(email.as_str()))
                .cloned()
        } else if web_rows.len() == 1 {
            web_rows.first().cloned()
        } else {
            None
        };

        let now = Utc::now().to_rfc3339();
        if let Some(mut existing_conn) = existing {
            let mut merged_psd = get_psd_object(&existing_conn);
            for (k, v) in psd {
                merged_psd.insert(k, v);
            }
            existing_conn.name = Some(name);
            if !email.is_empty() {
                existing_conn.email = Some(email.clone());
            }
            existing_conn.is_active = true;
            existing_conn.data.api_key = Some(cred.clone());
            set_connection_json_field(&mut existing_conn, "apiKey", Some(json!(cred)));
            set_connection_json_field(
                &mut existing_conn,
                "providerSpecificData",
                Some(Value::Object(merged_psd)),
            );
            set_connection_json_field(&mut existing_conn, "testStatus", Some(json!("active")));
            set_connection_json_field(&mut existing_conn, "lastError", Some(Value::Null));
            set_connection_json_field(&mut existing_conn, "lastErrorAt", Some(Value::Null));
            existing_conn.updated_at = now;
            if state.db.save_provider_connection(&existing_conn).is_ok() {
                session.saved_connection_id = Some(existing_conn.id.clone());
                if !email.is_empty() {
                    for dup in web_rows {
                        if dup.id != existing_conn.id && dup.email.as_deref() == Some(email.as_str()) {
                            let _ = state.db.delete_provider_connection(&dup.id);
                        }
                    }
                }
                session.credential = None;
            }
        } else {
            let new_id = crate::proxy_pools::generate_uuid();
            let raw_data = json!({
                "apiKey": cred,
                "providerSpecificData": Value::Object(psd),
                "testStatus": "active",
            })
            .to_string();
            let conn = ProviderConnection {
                id: new_id.clone(),
                provider: provider.to_string(),
                auth_type: "cookie".to_string(),
                name: Some(name),
                email: if email.is_empty() { None } else { Some(email) },
                priority: Some(1),
                is_active: true,
                data: crate::db::ConnectionData {
                    api_key: Some(cred),
                    ..Default::default()
                },
                raw_data,
                created_at: now.clone(),
                updated_at: now,
            };
            if state.db.save_provider_connection(&conn).is_ok() {
                session.saved_connection_id = Some(new_id);
                session.credential = None;
            }
        }
        WEB_LOGIN_SESSIONS.insert(provider.to_string(), session.clone());
    }

    let preview = session
        .credential
        .as_deref()
        .map(|c| format!("{}…", c.chars().take(10).collect::<String>()));

    Json(json!({
        "provider": provider,
        "status": session.status,
        "profile": session.profile,
        "error": session.error,
        "connectionId": session.saved_connection_id,
        "email": session.account_email,
        "credentialPreview": preview,
    }))
    .into_response()
}

/// `POST /api/providers/web-login`
pub async fn providers_web_login_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if !WEB_LOGIN_PROVIDERS.contains(&provider) {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("Web login not supported for \"{}\"", provider),
        );
    }

    let profile = sanitize_profile_name(
        body.get("profile")
            .and_then(|v| v.as_str())
            .unwrap_or("default"),
    );

    if let Some(existing) = WEB_LOGIN_SESSIONS.get(provider) {
        if existing.status == "running" {
            return (
                StatusCode::ACCEPTED,
                Json(json!({
                    "provider": provider,
                    "profile": existing.profile,
                    "status": existing.status,
                })),
            )
                .into_response();
        }
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    let session = WebLoginSession {
        profile: profile.clone(),
        status: "running".to_string(),
        error: None,
        credential: None,
        saved_connection_id: None,
        account_email: None,
        cancelled: Arc::clone(&cancelled),
    };
    WEB_LOGIN_SESSIONS.insert(provider.to_string(), session.clone());

    let provider_owned = provider.to_string();
    let data_dir = state.config.data_dir.clone();
    tokio::spawn(async move {
        let Some(_chrome) = resolve_chrome_executable() else {
            if let Some(mut entry) = WEB_LOGIN_SESSIONS.get_mut(&provider_owned) {
                entry.status = "error".to_string();
                entry.error = Some(
                    "No Chrome/Chromium executable found — set CHROME_PATH or install Google Chrome"
                        .to_string(),
                );
            }
            return;
        };

        let profile_dir = data_dir.join("browser-profiles").join(&profile);
        let _ = std::fs::create_dir_all(&profile_dir);

        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            if cancelled.load(Ordering::Relaxed) {
                if let Some(mut entry) = WEB_LOGIN_SESSIONS.get_mut(&provider_owned) {
                    entry.status = "cancelled".to_string();
                }
                return;
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        if let Some(mut entry) = WEB_LOGIN_SESSIONS.get_mut(&provider_owned) {
            if entry.status == "running" {
                entry.status = "timeout".to_string();
                entry.error =
                    Some("Login window timed out after 5 minutes — try again".to_string());
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "provider": provider,
            "profile": session.profile,
            "status": session.status,
        })),
    )
        .into_response()
}

/// `DELETE /api/providers/web-login`
pub async fn providers_web_login_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WebLoginQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let Some(provider) = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return json_err(StatusCode::BAD_REQUEST, "provider is required");
    };

    let cancelled = if let Some(mut session) = WEB_LOGIN_SESSIONS.get_mut(provider) {
        if session.status == "running" {
            session.cancelled.store(true, Ordering::Relaxed);
            session.status = "cancelled".to_string();
            true
        } else {
            false
        }
    } else {
        false
    };

    Json(json!({ "cancelled": cancelled })).into_response()
}

// ============================================================================
// 16. /api/combos/presets (GET, POST)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ComboPresetsQuery {
    pub source: Option<String>,
}

fn is_valid_combo_preset_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

fn build_preset_items(source: &str, existing_names: &HashSet<String>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut items: Vec<(String, Vec<String>)> = Vec::new();

    let mut push_item = |name: &str, models: Vec<String>| {
        if !is_valid_combo_preset_name(name) || models.is_empty() || !seen.insert(name.to_string())
        {
            return;
        }
        items.push((name.to_string(), models));
    };

    if source == "cursor" {
        for &id in CU_PRESET_MODELS {
            push_item(id, vec![format!("cu/{id}")]);
        }
    } else if source == "claude" {
        for &id in CC_PRESET_MODELS {
            push_item(id, vec![format!("cc/{id}")]);
        }
        for &(alias, target) in CLAUDE_CLI_DEFAULT_ALIASES {
            push_item(alias, vec![target.to_string()]);
        }
    }

    items
        .into_iter()
        .map(|(name, models)| {
            let exists = existing_names.contains(&name);
            json!({
                "name": name,
                "models": models,
                "exists": exists,
            })
        })
        .collect()
}

/// `GET /api/combos/presets`
pub async fn combos_presets_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ComboPresetsQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let source = query.source.as_deref().unwrap_or("");
    if source != "cursor" && source != "claude" {
        return json_err(StatusCode::BAD_REQUEST, "source must be 'cursor' or 'claude'");
    }

    let combos = match state.db.load_combos() {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let existing: HashSet<String> = combos.into_iter().map(|c| c.name).collect();
    let items = build_preset_items(source, &existing);
    let to_skip = items
        .iter()
        .filter(|i| i.get("exists").and_then(|v| v.as_bool()) == Some(true))
        .count();
    let to_create = items.len() - to_skip;

    Json(json!({
        "source": source,
        "items": items,
        "toCreate": to_create,
        "toSkip": to_skip,
    }))
    .into_response()
}

/// `POST /api/combos/presets`
pub async fn combos_presets_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| json!({}));
    let source = body.get("source").and_then(|v| v.as_str()).unwrap_or("");
    if source != "cursor" && source != "claude" {
        return json_err(StatusCode::BAD_REQUEST, "source must be 'cursor' or 'claude'");
    }

    let combos = match state.db.load_combos() {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let existing: HashSet<String> = combos.into_iter().map(|c| c.name).collect();
    let items = build_preset_items(source, &existing);

    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let exists = item.get("exists").and_then(|v| v.as_bool()) == Some(true);
        if exists {
            skipped.push(name);
            continue;
        }
        let models: Vec<String> = item
            .get("models")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let now = Utc::now().to_rfc3339();
        let combo = Combo {
            id: crate::proxy_pools::generate_uuid(),
            name: name.clone(),
            kind: None,
            models,
            strategy: Some("fallback".to_string()),
            match_rules: Some("[]".to_string()),
            custom_prompt: None,
            created_at: now.clone(),
            updated_at: now,
        };
        if let Err(e) = state.db.save_combo(&combo) {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
        created.push(json!(combo));
    }

    let created_count = created.len();
    let skipped_count = skipped.len();
    Json(json!({
        "source": source,
        "created": created,
        "skipped": skipped,
        "createdCount": created_count,
        "skippedCount": skipped_count,
    }))
    .into_response()
}

// ============================================================================
// 17. /api/proxy-pools/health (GET, POST)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ProxyHealthQuery {
    #[serde(rename = "proxyId")]
    pub proxy_id: Option<String>,
}

fn get_or_init_proxy_stats(proxy_id: &str) -> ProxyHealthStats {
    PROXY_HEALTH_STATS
        .entry(proxy_id.to_string())
        .or_default()
        .clone()
}

fn update_proxy_stats(proxy_id: &str, ok: bool, latency_ms: u64) -> ProxyHealthStats {
    let now_ms = Utc::now().timestamp_millis();
    let mut entry = PROXY_HEALTH_STATS.entry(proxy_id.to_string()).or_default();
    entry.last_check = Some(now_ms);
    entry.total_checks += 1;
    if ok {
        entry.successes += 1;
        entry.failures = 0;
        entry.last_success = Some(now_ms);
        let lat = latency_ms as f64;
        entry.avg_latency = if entry.total_checks == 1 {
            lat
        } else {
            entry.avg_latency * 0.7 + lat * 0.3
        };
    } else {
        entry.failures += 1;
        entry.last_failure = Some(now_ms);
    }
    entry.clone()
}

async fn check_single_proxy_health(pool: &ProxyPool, timeout_ms: u64) -> (bool, u64, u16, Option<String>) {
    let started = Instant::now();
    let proxy_url = pool.proxy_url().unwrap_or("");
    let Ok(proxy) = reqwest::Proxy::all(proxy_url) else {
        return (
            false,
            started.elapsed().as_millis() as u64,
            400,
            Some("Invalid proxy URL".to_string()),
        );
    };
    let Ok(client) = Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    else {
        return (
            false,
            started.elapsed().as_millis() as u64,
            500,
            Some("Failed to build proxy client".to_string()),
        );
    };

    match client.get("https://api.ipify.org/").send().await {
        Ok(r) => {
            let st = r.status().as_u16();
            let ok = r.status().is_success() || r.status().is_redirection();
            (ok, started.elapsed().as_millis() as u64, st, None)
        }
        Err(e) => (
            false,
            started.elapsed().as_millis() as u64,
            500,
            Some(e.to_string()),
        ),
    }
}

fn cleanup_dead_proxies(db: &Database, immediate: bool) -> anyhow::Result<usize> {
    let settings = db.load_settings().unwrap_or_else(|_| json!({}));
    let delete_after_ms = settings
        .get("proxyDeleteInactiveAfterMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_DELETE_INACTIVE_AFTER_MS) as i64;
    let now_ms = Utc::now().timestamp_millis();

    let pools = db.load_proxy_pools()?;
    let mut deleted = 0usize;
    for pool in pools.into_iter().filter(|p| !p.is_active) {
        if immediate {
            if db.delete_proxy_pool(&pool.id)? {
                PROXY_HEALTH_STATS.remove(&pool.id);
                deleted += 1;
            }
            continue;
        }
        let stats = PROXY_HEALTH_STATS.get(&pool.id).map(|s| s.clone());
        let should_delete = match stats {
            None => true,
            Some(st) => {
                (st.last_failure.map(|lf| now_ms - lf > delete_after_ms).unwrap_or(false)
                    && st.last_success.is_none())
                    || (st.last_check.map(|lc| now_ms - lc > delete_after_ms).unwrap_or(false)
                        && st.failures > 0)
            }
        };
        if should_delete && db.delete_proxy_pool(&pool.id)? {
            PROXY_HEALTH_STATS.remove(&pool.id);
            deleted += 1;
        }
    }
    Ok(deleted)
}

async fn run_health_check_tick(db: &Database) -> Value {
    if PROXY_HEALTH_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return json!({ "skipped": true, "reason": "already-running" });
    }

    let result = async {
        let settings = db.load_settings().unwrap_or_else(|_| json!({}));
        let timeout_ms = settings
            .get("proxyHealthCheckTimeoutMs")
            .and_then(|v| v.as_u64())
            .unwrap_or(8000);
        let sources_cfg = settings.get("proxyPoolProviders");
        let pools = db.load_proxy_pools()?;

        let mut by_source: HashMap<String, Vec<ProxyPool>> = HashMap::new();
        for pool in pools.into_iter().filter(|p| p.is_active) {
            let src = pool.source().unwrap_or("manual").to_string();
            by_source.entry(src).or_default().push(pool);
        }

        let mut checked = 0usize;
        let mut healthy = 0usize;
        let mut unhealthy = 0usize;
        let mut results = Vec::new();

        for (source, source_pools) in by_source {
            let enabled = sources_cfg
                .and_then(|sc| sc.get(&source))
                .and_then(|s| s.get("healthCheckEnabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !enabled {
                continue;
            }

            let mut src_checked = 0usize;
            for mut pool in source_pools {
                let (ok, latency_ms, _st, _err) = check_single_proxy_health(&pool, timeout_ms).await;
                let stats = update_proxy_stats(&pool.id, ok, latency_ms);
                src_checked += 1;
                checked += 1;
                if ok {
                    healthy += 1;
                } else {
                    unhealthy += 1;
                }

                if stats.failures >= DEFAULT_MAX_CONSECUTIVE_FAILURES && pool.is_active {
                    let now = Utc::now().to_rfc3339();
                    let mut data = pool.data.as_object().cloned().unwrap_or_default();
                    data.insert("isActive".to_string(), json!(false));
                    data.insert("testStatus".to_string(), json!("inactive"));
                    data.insert(
                        "lastError".to_string(),
                        json!(format!("Health check failed {}x consecutively", stats.failures)),
                    );
                    pool.is_active = false;
                    pool.test_status = Some("inactive".to_string());
                    pool.data = Value::Object(data);
                    pool.updated_at = now;
                    let _ = db.save_proxy_pool(&pool);
                } else if ok && !pool.is_active {
                    let now = Utc::now().to_rfc3339();
                    let mut data = pool.data.as_object().cloned().unwrap_or_default();
                    data.insert("isActive".to_string(), json!(true));
                    data.insert("testStatus".to_string(), json!("active"));
                    data.insert("lastError".to_string(), Value::Null);
                    pool.is_active = true;
                    pool.test_status = Some("active".to_string());
                    pool.data = Value::Object(data);
                    pool.updated_at = now;
                    let _ = db.save_proxy_pool(&pool);
                }
            }
            results.push(json!({ "source": source, "checked": src_checked }));
        }

        let deleted = cleanup_dead_proxies(db, false).unwrap_or(0);
        Ok::<Value, anyhow::Error>(json!({
            "skipped": false,
            "checked": checked,
            "healthy": healthy,
            "unhealthy": unhealthy,
            "results": results,
            "deleted": deleted,
        }))
    }
    .await;

    PROXY_HEALTH_RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok(v) => v,
        Err(e) => json!({ "skipped": false, "error": e.to_string() }),
    }
}

/// `GET /api/proxy-pools/health`
pub async fn proxy_pools_health_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProxyHealthQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    if let Some(pid) = query
        .proxy_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let stats = get_or_init_proxy_stats(pid);
        return Json(json!({ "proxyId": pid, "stats": stats })).into_response();
    }

    let mut stats_map = Map::new();
    for entry in PROXY_HEALTH_STATS.iter() {
        stats_map.insert(entry.key().clone(), json!(entry.value()));
    }
    Json(json!({
        "running": PROXY_HEALTH_RUNNING.load(Ordering::SeqCst),
        "hasInterval": false,
        "intervalMs": DEFAULT_HEALTH_CHECK_INTERVAL_MS,
        "stats": Value::Object(stats_map),
    }))
    .into_response()
}

/// `POST /api/proxy-pools/health`
pub async fn proxy_pools_health_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "run" => Json(run_health_check_tick(&state.db).await).into_response(),
        "cleanup" => {
            let immediate = body
                .get("immediate")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match cleanup_dead_proxies(&state.db, immediate) {
                Ok(deleted) => Json(json!({ "deleted": deleted })).into_response(),
                Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
        "reset" => {
            if let Some(pid) = body
                .get("proxyId")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                PROXY_HEALTH_STATS.remove(pid);
                return Json(json!({ "ok": true })).into_response();
            }
            json_err(StatusCode::BAD_REQUEST, "Invalid action")
        }
        _ => json_err(StatusCode::BAD_REQUEST, "Invalid action"),
    }
}

// ============================================================================
// Router Assembly
// ============================================================================

/// Builds the Axum router containing all Wave 2 Part A endpoints.
pub fn providers_extra_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/models/availability",
            get(models_availability_get_handler)
                .post(models_availability_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/catalog-sync",
            get(models_catalog_sync_get_handler)
                .post(models_catalog_sync_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/models/refresh",
            get(models_refresh_get_handler)
                .post(models_refresh_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/provider-nodes/validate",
            post(provider_nodes_validate_post_handler).options(options_handler),
        )
        .route(
            "/api/providers/client",
            get(providers_client_get_handler).options(options_handler),
        )
        .route(
            "/api/providers/kilo/free-models",
            get(providers_kilo_free_models_get_handler).options(options_handler),
        )
        .route(
            "/api/providers/test-batch",
            post(providers_test_batch_post_handler).options(options_handler),
        )
        .route(
            "/api/providers/web-login",
            get(providers_web_login_get_handler)
                .post(providers_web_login_post_handler)
                .delete(providers_web_login_delete_handler)
                .options(options_handler),
        )
        .route(
            "/api/providers/{id}",
            get(provider_get_handler)
                .put(provider_put_handler)
                .delete(provider_delete_handler)
                .options(options_handler),
        )
        .route(
            "/api/providers/{id}/overrides",
            get(provider_overrides_get_handler)
                .put(provider_overrides_put_handler)
                .options(options_handler),
        )
        .route(
            "/api/providers/{id}/test",
            post(provider_test_post_handler).options(options_handler),
        )
        .route(
            "/api/providers/{id}/test-models",
            post(provider_test_models_post_handler).options(options_handler),
        )
        .route(
            "/api/combos/presets",
            get(combos_presets_get_handler)
                .post(combos_presets_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/proxy-pools/health",
            get(proxy_pools_health_get_handler)
                .post(proxy_pools_health_post_handler)
                .options(options_handler),
        )
}

// ============================================================================
// Unit & Router Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::put;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    fn make_test_state() -> (AppState, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "ninerouter_providers_extra_test_{}_{}",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(crate::config::Config {
            data_dir: dir.clone(),
            db_path: dir.join("test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let rate_limiter = Arc::new(crate::ratelimit::RateLimiter::from_config(&config));
        let state = AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: crate::routes::ConsoleLogBuffer::new(),
            rate_limiter,
        };
        (state, dir)
    }

    fn build_full_test_router(state: AppState) -> Router {
        providers_extra_router()
            .route(
                "/api/models",
                get(crate::routes::dashboard_models_handler)
                    .put(models_put_handler)
                    .options(options_handler),
            )
            .route(
                "/api/pricing",
                get(crate::routes::get_pricing_handler)
                    .patch(pricing_patch_handler)
                    .delete(pricing_delete_handler)
                    .options(options_handler),
            )
            .route(
                "/api/provider-nodes/{id}",
                put(provider_node_put_handler)
                    .delete(crate::routes::delete_provider_node_handler)
                    .options(options_handler),
            )
            .with_state(state)
    }

    async fn read_json_body(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn spawn_mock_upstream(response_body: &'static str, status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..4 {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let reply = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                        response_body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
            }
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn test_models_put_and_availability_routes() {
        let (state, dir) = make_test_state();
        let app = build_full_test_router(state.clone());

        // PUT /api/models validation
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/models")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"model":"openai/gpt-4o"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // PUT /api/models success
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/models")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"model":"openai/gpt-4o","alias":"fast-gpt"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = read_json_body(resp).await;
        assert_eq!(j["success"], true);
        assert_eq!(j["alias"], "fast-gpt");

        // Conflict when alias points to another model
        state.db.set_model_alias("other-key", "shared-alias").unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/models")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"model":"anthropic/claude","alias":"shared-alias"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Seed connection with active cooldown lock + unavailable connection
        let future = (Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
        let conn = ProviderConnection {
            id: "conn-avail-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some("OpenAI Main".into()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: crate::db::ConnectionData::default(),
            raw_data: json!({
                "modelLock_gpt-4o": future,
                "testStatus": "unavailable",
                "lastError": "rate limited"
            })
            .to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/models/availability")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let avail = read_json_body(resp).await;
        assert_eq!(avail["unavailableCount"], 1);
        assert_eq!(avail["models"][0]["model"], "gpt-4o");
        assert_eq!(avail["models"][0]["status"], "cooldown");

        // Clear cooldown
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/models/availability")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"action":"clearCooldown","provider":"openai","model":"gpt-4o"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/models/availability")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let avail_after = read_json_body(resp).await;
        assert_eq!(avail_after["unavailableCount"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_pricing_patch_and_delete_routes() {
        let (state, dir) = make_test_state();
        let app = build_full_test_router(state.clone());

        // Invalid pricing value -> 400
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/pricing")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"openai":{"gpt-4o":{"input":-1}}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Valid PATCH -> persists and returns user pricing
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/pricing")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"openai":{"gpt-4o":{"input":9.5,"output":19.0}}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = read_json_body(resp).await;
        assert_eq!(j["openai"]["gpt-4o"]["input"], 9.5);

        // DELETE specific model resets to default
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/pricing?provider=openai&model=gpt-4o")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let merged = read_json_body(resp).await;
        assert_eq!(merged["openai"]["gpt-4o"]["input"], 2.5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_provider_nodes_put_and_validate() {
        let (state, dir) = make_test_state();
        let app = build_full_test_router(state.clone());

        let node = ProviderNode {
            id: "anthropic-compatible-1".into(),
            node_type: Some("anthropic-compatible".into()),
            name: Some("Old Node".into()),
            prefix: "old".into(),
            base_url: "https://example.com".into(),
            api_type: None,
            data: json!({ "prefix": "old", "baseUrl": "https://example.com" }),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_node(&node).unwrap();

        let conn = ProviderConnection {
            id: "conn-node-1".into(),
            provider: "anthropic-compatible-1".into(),
            auth_type: "apikey".into(),
            name: Some("Node Conn".into()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: crate::db::ConnectionData::default(),
            raw_data: json!({ "providerSpecificData": { "prefix": "old" } }).to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/provider-nodes/anthropic-compatible-1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"name":"New Node","prefix":"newp","baseUrl":"https://api.example.com/v1/messages/"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = read_json_body(resp).await;
        assert_eq!(j["node"]["name"], "New Node");
        assert_eq!(j["node"]["baseUrl"], "https://api.example.com/v1");

        // Verify cascade to connection providerSpecificData
        let updated_conn = find_connection_by_id(&state.db, "conn-node-1").unwrap().unwrap();
        let psd = get_psd_object(&updated_conn);
        assert_eq!(psd["prefix"], "newp");
        assert_eq!(psd["nodeName"], "New Node");

        // Validate against mock local upstream (with localhost Host header)
        let mock_base = spawn_mock_upstream(r#"{"data":[{"id":"m1"}]}"#, "200 OK").await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/provider-nodes/validate")
                    .header(header::HOST, "127.0.0.1:20127")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "baseUrl": mock_base, "apiKey": "sk-test" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let val_json = read_json_body(resp).await;
        assert_eq!(val_json["valid"], true);

        // SSRF check blocks localhost when not local request
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/provider-nodes/validate")
                    .header(header::HOST, "public.example.com")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"baseUrl":"http://127.0.0.1:8080/v1","apiKey":"sk-test"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_providers_crud_overrides_client_test_and_batch() {
        let (state, dir) = make_test_state();
        let app = build_full_test_router(state.clone());

        let mock_base = spawn_mock_upstream(r#"{"data":[{"id":"gpt-custom-1"}]}"#, "200 OK").await;

        let conn = ProviderConnection {
            id: "conn-p1".into(),
            provider: "openai-compatible-demo".into(),
            auth_type: "apikey".into(),
            name: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: crate::db::ConnectionData {
                api_key: Some("secret-key-123".into()),
                base_url: Some(mock_base.clone()),
                ..Default::default()
            },
            raw_data: json!({
                "apiKey": "secret-key-123",
                "providerSpecificData": { "baseUrl": mock_base }
            })
            .to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        let oauth_conn = ProviderConnection {
            id: "conn-c1".into(),
            provider: "cursor".into(),
            auth_type: "oauth".into(),
            name: Some("Cursor Acc".into()),
            email: Some("dev@example.com".into()),
            priority: Some(1),
            is_active: true,
            data: crate::db::ConnectionData::default(),
            raw_data: json!({ "accessToken": "tok-cursor" }).to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&oauth_conn).unwrap();

        // GET /api/providers/{id} strips secrets
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/conn-p1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = read_json_body(resp).await;
        assert!(j["connection"].get("apiKey").is_none());

        // PUT /api/providers/{id}
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/providers/conn-p1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Updated Name","priority":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let put_j = read_json_body(resp).await;
        assert_eq!(put_j["connection"]["name"], "Updated Name");
        assert_eq!(put_j["connection"]["priority"], 2);

        // GET / PUT /api/providers/{id}/overrides
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/providers/cc/overrides")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"headers":{"X-Custom":"1"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ov = read_json_body(resp).await;
        assert_eq!(ov["headers"]["X-Custom"], "1");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/cc/overrides")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ov_get = read_json_body(resp).await;
        assert_eq!(ov_get["headers"]["X-Custom"], "1");
        assert_eq!(ov_get["builtinHeaders"]["Anthropic-Version"], "2023-06-01");

        // Blocked override header -> 400
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/providers/cc/overrides")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"headers":{"Authorization":"Bearer bad"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // POST /api/providers/{id}/test
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/conn-p1/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let test_j = read_json_body(resp).await;
        assert_eq!(test_j["valid"], true);

        // POST /api/providers/test-batch (mode=compatible)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/test-batch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"mode":"compatible"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let batch_j = read_json_body(resp).await;
        assert_eq!(batch_j["summary"]["total"], 1);
        assert_eq!(batch_j["summary"]["passed"], 1);

        // GET /api/providers/client
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/client")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let client_j = read_json_body(resp).await;
        assert_eq!(client_j["connections"].as_array().unwrap().len(), 1);
        assert_eq!(client_j["connections"][0]["provider"], "cursor");

        // POST /api/models/refresh for compatible provider
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/models/refresh")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"provider":"openai-compatible-demo"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ref_j = read_json_body(resp).await;
        assert_eq!(ref_j["results"][0]["refreshed"], true);
        assert_eq!(ref_j["results"][0]["count"], 1);

        // GET /api/models/refresh?provider=...
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/models/refresh?provider=openai-compatible-demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ref_get = read_json_body(resp).await;
        assert_eq!(ref_get["providers"][0]["ids"][0], "gpt-custom-1");

        // DELETE /api/providers/{id}
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/providers/conn-p1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(find_connection_by_id(&state.db, "conn-p1").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_presets_health_web_login_catalog_sync_and_test_models() {
        let (state, dir) = make_test_state();
        let app = build_full_test_router(state.clone());

        // GET /api/combos/presets?source=claude
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/combos/presets?source=claude")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let preview = read_json_body(resp).await;
        assert!(preview["toCreate"].as_u64().unwrap() >= 8);

        // POST /api/combos/presets
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/combos/presets")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"source":"claude"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let created = read_json_body(resp).await;
        assert!(created["createdCount"].as_u64().unwrap() >= 8);

        // Second POST skips existing
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/combos/presets")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"source":"claude"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let skipped = read_json_body(resp).await;
        assert_eq!(skipped["createdCount"], 0);
        assert!(skipped["skippedCount"].as_u64().unwrap() >= 8);

        // Proxy pools health GET & POST cleanup/reset
        let inactive_pool = ProxyPool {
            id: "pool-dead-1".into(),
            is_active: false,
            test_status: Some("inactive".into()),
            data: json!({ "proxyUrl": "http://127.0.0.1:9999" }),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_proxy_pool(&inactive_pool).unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/proxy-pools/health?proxyId=pool-dead-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/proxy-pools/health")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"action":"cleanup","immediate":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let clean_j = read_json_body(resp).await;
        assert_eq!(clean_j["deleted"], 1);

        // Web-login GET / POST / DELETE
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/web-login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let wl_list = read_json_body(resp).await;
        assert!(wl_list["providers"].as_array().unwrap().len() >= 6);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/web-login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"provider":"grok-web","profile":"work"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/providers/web-login?provider=grok-web")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Catalog sync GET
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/models/catalog-sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // test-models 404 when missing, 400 when no models configured
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/missing-id/test-models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let empty_conn = ProviderConnection {
            id: "conn-nomodels".into(),
            provider: "custom-empty".into(),
            auth_type: "apikey".into(),
            name: None,
            email: None,
            priority: Some(1),
            is_active: true,
            data: crate::db::ConnectionData::default(),
            raw_data: "{}".into(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&empty_conn).unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/conn-nomodels/test-models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_kilo_free_models_cache_hit() {
        let (state, dir) = make_test_state();
        {
            let mut guard = KILO_MODELS_CACHE.lock().unwrap();
            *guard = Some(KiloModelsCache {
                models: vec![json!({
                    "id": "kilo/free-test",
                    "name": "Free Test",
                    "isFree": true,
                    "context_length": 32768
                })],
                fetched_at: Instant::now(),
            });
        }
        let app = build_full_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/kilo/free-models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = read_json_body(resp).await;
        assert_eq!(j["cached"], true);
        assert_eq!(j["models"][0]["id"], "kilo/free-test");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_auth_guard_embedding_validate_and_ping_models() {
        let (state, dir) = make_test_state();

        // Auth guard when requireLogin is true
        state
            .db
            .save_settings(&json!({ "requireLogin": true }))
            .unwrap();
        let app = build_full_test_router(state.clone());
        for (method, uri) in [
            ("GET", "/api/models/availability"),
            ("GET", "/api/providers/client"),
            ("GET", "/api/combos/presets?source=cursor"),
            ("GET", "/api/proxy-pools/health"),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        state
            .db
            .save_settings(&json!({ "requireLogin": false }))
            .unwrap();

        // Custom-embedding validation with mock server
        let embed_base =
            spawn_mock_upstream(r#"{"data":[{"embedding":[0.1,0.2,0.3]}]}"#, "200 OK").await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/provider-nodes/validate")
                    .header(header::HOST, "127.0.0.1:20127")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "type": "custom-embedding",
                            "baseUrl": embed_base,
                            "apiKey": "sk-emb",
                            "modelId": "text-embedding-3-small"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let emb_j = read_json_body(resp).await;
        assert_eq!(emb_j["valid"], true);
        assert_eq!(emb_j["method"], "embeddings");
        assert_eq!(emb_j["dimensions"], 3);

        // ping_model_by_kind against mock local chat server
        let chat_base =
            spawn_mock_upstream(r#"{"choices":[{"message":{"content":"ok"}}]}"#, "200 OK").await;
        let hdrs = build_internal_ping_headers(&state.db);
        let client = http_client(5);
        let ping_res = ping_model_by_kind(&client, &chat_base, &hdrs, "openai/gpt-4o", "llm").await;
        assert_eq!(ping_res["ok"], true);
        assert_eq!(ping_res["status"], 200);

        // Client name masking unit check
        assert_eq!(
            mask_client_name("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "aaaaaaaa***"
        );
        assert_eq!(mask_client_name("My OpenAI Account"), "My OpenAI Account");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
