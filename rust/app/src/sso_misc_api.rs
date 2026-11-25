use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use futures_util::StreamExt;
use ring::digest;
use ring::hmac;
use ring::rand::SecureRandom;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth;
use crate::db::ProviderConnection;
use crate::media::audio;
use crate::proxy;
use crate::responses::{self, ResponsesRequest};
use crate::routes::{self, AppState};
use crate::token_refresh;
use crate::translator::{self, ChatCompletionRequest, ClaudeMessageRequest};

// ============================================================================
// Constants & Static Caches
// ============================================================================

const DEFAULT_OIDC_SCOPES: &str = "openid profile email";
const SESSION_MAX_AGE_SECS: i64 = 24 * 60 * 60;
const SSO_COOKIE_MAX_AGE_SECS: i64 = 10 * 60;
const LOCALE_COOKIE_MAX_AGE_SECS: i64 = 365 * 24 * 60 * 60;
const SAML_CLOCK_SKEW_SECS: i64 = 60;
const VOICES_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const EDGE_TTS_VOICES_URL: &str =
    "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const EDGE_TTS_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const DEEPGRAM_MODELS_URL: &str = "https://api.deepgram.com/v1/models";
const ELEVENLABS_VOICES_URL: &str = "https://api.elevenlabs.io/v1/voices";
const INWORLD_VOICES_URL: &str = "https://api.inworld.ai/tts/v1/voices";
const MINIMAX_VOICES_URL_GLOBAL: &str = "https://api.minimax.io/v1/get_voice";
const MINIMAX_VOICES_URL_CN: &str = "https://api.minimaxi.com/v1/get_voice";

const TRANSLATOR_ALLOWED_FILES: &[&str] = &[
    "1_req_client.json",
    "2_req_source.json",
    "3_req_openai.json",
    "4_req_target.json",
    "5_res_provider.txt",
    "6_res_openai.txt",
    "7_res_client.txt",
    "7_res_client.json",
];

const SUPPORTED_LOCALES: &[&str] = &[
    "en", "vi", "zh-CN", "zh-TW", "ja", "pt-BR", "pt-PT", "ko", "es", "de", "fr", "he", "ar",
    "ru", "pl", "cs", "nl", "tr", "uk", "tl", "id", "km", "th", "hi", "bn", "ur", "ro", "sv",
    "it", "el", "hu", "fi", "da", "no", "fa",
];

const PREBUILT_GEMINI_VOICES: &[(&str, &str, &str)] = &[
    ("Zephyr", "en", "Female"),
    ("Puck", "en", "Male"),
    ("Charon", "en", "Male"),
    ("Kore", "en", "Female"),
    ("Fenrir", "en", "Male"),
    ("Leda", "en", "Female"),
    ("Orus", "en", "Male"),
    ("Aoede", "en", "Female"),
    ("Callirrhoe", "en", "Female"),
    ("Autonoe", "en", "Female"),
    ("Enceladus", "en", "Male"),
    ("Iapetus", "en", "Male"),
    ("Umbriel", "en", "Male"),
    ("Algieba", "en", "Male"),
    ("Despina", "en", "Female"),
    ("Erinome", "en", "Female"),
    ("Algenib", "en", "Male"),
    ("Rasalgethi", "en", "Male"),
    ("Laomedeia", "en", "Female"),
    ("Achernar", "en", "Female"),
    ("Alnilam", "en", "Male"),
    ("Schedar", "en", "Male"),
    ("Gacrux", "en", "Female"),
    ("Pulcherrima", "en", "Female"),
    ("Achird", "en", "Male"),
    ("Zubenelgenubi", "en", "Male"),
    ("Vindemiatrix", "en", "Female"),
    ("Sadachbia", "en", "Male"),
    ("Sadaltager", "en", "Male"),
    ("Sulafat", "en", "Female"),
];

const OLLAMA_TAGS_JSON: &str = r#"{
  "models": [
    {
      "name": "llama3.2",
      "modified_at": "2025-12-26T00:00:00Z",
      "size": 2000000000,
      "digest": "abc123def456",
      "details": {
        "format": "gguf",
        "family": "llama",
        "parameter_size": "3B",
        "quantization_level": "Q4_K_M"
      }
    },
    {
      "name": "qwen2.5",
      "modified_at": "2025-12-26T00:00:00Z",
      "size": 4000000000,
      "digest": "def456abc123",
      "details": {
        "format": "gguf",
        "family": "qwen",
        "parameter_size": "7B",
        "quantization_level": "Q4_K_M"
      }
    }
  ]
}"#;

// Login lockout tracker for SAML ACS (mirrors src/lib/auth/loginLimiter.js)
const MAX_FAILS_BEFORE_LOCK: u32 = 5;
const LOCK_STEPS_MS: &[u64] = &[30_000, 120_000, 600_000, 1_800_000];
const FAIL_WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Default)]
struct LockEntry {
    fails: u32,
    lock_until: Option<Instant>,
    lock_level: usize,
    last_fail_at: Option<Instant>,
}

static SAML_LOGIN_ATTEMPTS: OnceLock<Mutex<HashMap<String, LockEntry>>> = OnceLock::new();
static EDGE_VOICES_CACHE: OnceLock<Mutex<Option<(Instant, Vec<Value>)>>> = OnceLock::new();
static ELEVENLABS_VOICES_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> =
    OnceLock::new();

// ============================================================================
// Router
// ============================================================================

pub fn sso_misc_router() -> Router<AppState> {
    Router::new()
        // Auth SSO & Password Reset
        .route(
            "/api/auth/oidc/start",
            get(oidc_start_handler).options(routes::options_handler),
        )
        .route(
            "/api/auth/oidc/callback",
            get(oidc_callback_handler).options(routes::options_handler),
        )
        .route(
            "/api/auth/saml/start",
            get(saml_start_handler).options(routes::options_handler),
        )
        .route(
            "/api/auth/saml/acs",
            post(saml_acs_handler).options(routes::options_handler),
        )
        .route(
            "/api/auth/saml/metadata",
            get(saml_metadata_handler).options(routes::options_handler),
        )
        .route(
            "/api/auth/reset-password",
            post(reset_password_handler).options(routes::options_handler),
        )
        // Media Providers TTS Voices & Public /v1/audio/voices
        .route(
            "/api/media-providers/tts/voices",
            get(tts_voices_handler).options(routes::options_handler),
        )
        .route(
            "/api/media-providers/tts/deepgram/voices",
            get(deepgram_voices_handler).options(routes::options_handler),
        )
        .route(
            "/api/media-providers/tts/elevenlabs/voices",
            get(elevenlabs_voices_handler).options(routes::options_handler),
        )
        .route(
            "/api/media-providers/tts/inworld/voices",
            get(inworld_voices_handler).options(routes::options_handler),
        )
        .route(
            "/api/media-providers/tts/minimax/voices",
            get(minimax_voices_handler).options(routes::options_handler),
        )
        .route(
            "/v1/audio/voices",
            get(v1_audio_voices_handler).options(v1_audio_voices_options_handler),
        )
        .route(
            "/audio/voices",
            get(v1_audio_voices_handler).options(v1_audio_voices_options_handler),
        )
        .route(
            "/api/v1/audio/voices",
            get(v1_audio_voices_handler).options(v1_audio_voices_options_handler),
        )
        // Misc endpoints
        .route(
            "/api/tags",
            get(tags_handler).options(tags_options_handler),
        )
        .route(
            "/api/locale",
            post(locale_handler).options(routes::options_handler),
        )
        .route(
            "/api/init",
            get(init_handler).options(routes::options_handler),
        )
        .route(
            "/api/cron/flush",
            get(cron_flush_handler)
                .post(cron_flush_handler)
                .options(routes::options_handler),
        )
        .route(
            "/api/shutdown",
            post(dev_shutdown_handler).options(routes::options_handler),
        )
        .route(
            "/api/version/update",
            post(version_update_handler).options(routes::options_handler),
        )
        // /v1 root (alias for /v1/models)
        .route(
            "/v1",
            get(routes::models_handler).options(routes::options_handler),
        )
        .route(
            "/api/v1",
            get(routes::models_handler).options(routes::options_handler),
        )
        // Translator playground helpers
        .route(
            "/api/translator/load",
            get(translator_load_handler).options(routes::options_handler),
        )
        .route(
            "/api/translator/save",
            post(translator_save_handler).options(routes::options_handler),
        )
        .route(
            "/api/translator/send",
            post(translator_send_handler).options(routes::options_handler),
        )
        .route(
            "/api/translator/translate",
            post(translator_translate_handler).options(routes::options_handler),
        )
}

// ============================================================================
// Common HTTP / Cookie / Origin / JWT Helpers
// ============================================================================

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(body),
    )
        .into_response()
}

fn trim_trailing_slashes(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

pub fn get_public_origin(headers: &HeaderMap, uri: &Uri, default_port: u16) -> String {
    if let Ok(base) = std::env::var("BASE_URL") {
        let trimmed = trim_trailing_slashes(&base);
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    if let Ok(base) = std::env::var("NEXT_PUBLIC_BASE_URL") {
        let trimmed = trim_trailing_slashes(&base);
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let host = forwarded_host.or_else(|| {
        headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });

    if let Some(h) = host {
        let proto = forwarded_proto
            .or_else(|| uri.scheme_str())
            .unwrap_or("http")
            .trim_end_matches(':');
        return trim_trailing_slashes(&format!("{}://{}", proto, h));
    }

    if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
        return trim_trailing_slashes(&format!("{}://{}", scheme, authority));
    }

    format!("http://localhost:{}", default_port)
}

fn get_saml_base_url(headers: &HeaderMap, uri: &Uri, settings: &Value, default_port: u16) -> String {
    if let Some(base) = settings.get("baseUrl").and_then(|v| v.as_str()) {
        let trimmed = trim_trailing_slashes(base);
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    get_public_origin(headers, uri, default_port)
}

fn should_use_secure_cookie(headers: &HeaderMap) -> bool {
    let force_secure = std::env::var("AUTH_COOKIE_SECURE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    force_secure || forwarded_https
}

fn format_cookie(name: &str, value: &str, max_age: i64, http_only: bool, secure: bool) -> String {
    let mut s = format!(
        "{}={}; Path=/; Max-Age={}; SameSite=Lax",
        name, value, max_age
    );
    if http_only {
        s.push_str("; HttpOnly");
    }
    if secure {
        s.push_str("; Secure");
    }
    s
}

fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == name {
                let val = v.trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn redirect_with_cookies(target: &str, set_cookies: Vec<String>) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, target);
    for c in set_cookies {
        builder = builder.header(header::SET_COOKIE, c);
    }
    builder
        .body(Body::empty())
        .unwrap_or_else(|_| Redirect::temporary(target).into_response())
}

fn random_base64url(num_bytes: usize) -> String {
    let rng = ring::rand::SystemRandom::new();
    let mut buf = vec![0u8; num_bytes];
    let _ = rng.fill(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

fn random_hex(num_bytes: usize) -> String {
    let rng = ring::rand::SystemRandom::new();
    let mut buf = vec![0u8; num_bytes];
    let _ = rng.fill(&mut buf);
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn create_dashboard_jwt_with_claims(jwt_secret: &str, extra_claims: Value) -> String {
    let now = chrono::Utc::now().timestamp();
    let exp = now + SESSION_MAX_AGE_SECS;
    let header_json = r#"{"alg":"HS256","typ":"JWT"}"#;

    let mut payload = serde_json::Map::new();
    payload.insert("authenticated".to_string(), json!(true));
    if let Value::Object(extra) = extra_claims {
        for (k, v) in extra {
            payload.insert(k, v);
        }
    }
    payload.insert("iat".to_string(), json!(now));
    payload.insert("exp".to_string(), json!(exp));

    let header_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Value::Object(payload).to_string().as_bytes());
    let signing_input = format!("{}.{}", header_b64, payload_b64);

    let key = hmac::Key::new(hmac::HMAC_SHA256, jwt_secret.as_bytes());
    let tag = hmac::sign(&key, signing_input.as_bytes());
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref());

    format!("{}.{}", signing_input, sig_b64)
}

fn decode_b64_url(input: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(input))
        .map_err(|e| format!("Invalid base64url: {}", e))
}

fn url_encode_component(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn is_loopback_hostname(raw: &str) -> bool {
    let mut name = raw.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if name.starts_with('[') {
        if let Some(end) = name.find(']') {
            name = name[1..end].to_string();
        } else {
            return false;
        }
    } else if let Some(colon) = name.find(':') {
        if name.rfind(':') == Some(colon) {
            name = name[..colon].to_string();
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
    if let Ok(peer_token) = std::env::var("NINEROUTER_PEER_TOKEN") {
        if !peer_token.is_empty() {
            let supplied = headers
                .get("x-9r-peer-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !auth::constant_time_eq(supplied.as_bytes(), peer_token.as_bytes()) {
                return false;
            }
            let real_ip = headers
                .get("x-9r-real-ip")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !is_loopback_hostname(real_ip) {
                return false;
            }
        }
    }

    if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        if !is_loopback_hostname(host) {
            return false;
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if let Ok(parsed) = url::Url::parse(origin) {
            if !is_loopback_hostname(parsed.host_str().unwrap_or("")) {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

fn verify_always_protected_auth(headers: &HeaderMap, jwt_secret: &str) -> bool {
    if auth::verify_admin_api_token(headers) {
        return true;
    }
    if let Some(token) = auth::extract_session_token(headers) {
        if auth::verify_dashboard_jwt(&token, jwt_secret) {
            return true;
        }
    }
    false
}

// ============================================================================
// 1. Auth SSO: OIDC Start & Callback
// ============================================================================

#[derive(Debug, Clone)]
pub struct OidcRuntimeConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
}

pub fn get_oidc_runtime_config(settings: &Value) -> Option<OidcRuntimeConfig> {
    let auth_mode = settings
        .get("authMode")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if auth_mode != "oidc" && auth_mode != "both" {
        return None;
    }

    let issuer_url = trim_trailing_slashes(
        settings
            .get("oidcIssuerUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let client_id = settings
        .get("oidcClientId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let client_secret = settings
        .get("oidcClientSecret")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if issuer_url.is_empty() || client_id.is_empty() || client_secret.is_empty() {
        return None;
    }

    let scopes = settings
        .get("oidcScopes")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_OIDC_SCOPES)
        .to_string();

    Some(OidcRuntimeConfig {
        issuer_url,
        client_id,
        client_secret,
        scopes,
    })
}

pub fn create_pkce_pair() -> (String, String) {
    let verifier = random_base64url(32);
    let hash = digest::digest(&digest::SHA256, verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.as_ref());
    (verifier, challenge)
}

pub fn build_oidc_authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &str,
    state: &str,
    nonce: &str,
    code_challenge: &str,
) -> Result<String, String> {
    let mut url = url::Url::parse(authorization_endpoint)
        .map_err(|e| format!("Invalid authorization_endpoint: {}", e))?;
    let normalized_scopes = if scopes.trim().is_empty() {
        DEFAULT_OIDC_SCOPES
    } else {
        scopes.trim()
    };
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", normalized_scopes)
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

async fn fetch_oidc_discovery(client: &reqwest::Client, issuer_url: &str) -> Result<Value, String> {
    let discovery_url = format!("{}/.well-known/openid-configuration", trim_trailing_slashes(issuer_url));
    let res = client
        .get(&discovery_url)
        .header(header::CACHE_CONTROL, "no-store")
        .send()
        .await
        .map_err(|e| format!("Failed to load OIDC discovery document from {}: {}", discovery_url, e))?;
    if !res.status().is_success() {
        return Err(format!(
            "Failed to load OIDC discovery document from {}",
            discovery_url
        ));
    }
    res.json::<Value>()
        .await
        .map_err(|e| format!("Invalid OIDC discovery JSON: {}", e))
}

async fn exchange_oidc_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<Value, String> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    if !client_secret.is_empty() {
        params.push(("client_secret", client_secret));
    }

    let res = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("OIDC token exchange failed: {}", e))?;
    let status = res.status();
    let data: Value = res.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        let msg = data
            .get("error_description")
            .or_else(|| data.get("error"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("OIDC token exchange failed ({})", status.as_u16()));
        return Err(msg);
    }
    Ok(data)
}

fn verify_jwk_signature(
    jwk: &Value,
    alg: &str,
    signing_input: &[u8],
    sig_bytes: &[u8],
) -> Result<(), String> {
    match alg {
        "RS256" | "RS384" | "RS512" => {
            let n_b64 = jwk
                .get("n")
                .and_then(|v| v.as_str())
                .ok_or("Missing RSA 'n' in JWK")?;
            let e_b64 = jwk
                .get("e")
                .and_then(|v| v.as_str())
                .ok_or("Missing RSA 'e' in JWK")?;
            let n = decode_b64_url(n_b64)?;
            let e = decode_b64_url(e_b64)?;
            let verify_alg: &signature::RsaParameters = match alg {
                "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
                _ => &signature::RSA_PKCS1_2048_8192_SHA512,
            };
            RsaPublicKeyComponents { n: &n, e: &e }
                .verify(verify_alg, signing_input, sig_bytes)
                .map_err(|_| format!("Invalid {} signature on id_token", alg))
        }
        "ES256" | "ES384" => {
            let coord_len = if alg == "ES256" { 32 } else { 48 };
            let x_b64 = jwk
                .get("x")
                .and_then(|v| v.as_str())
                .ok_or("Missing EC 'x' in JWK")?;
            let y_b64 = jwk
                .get("y")
                .and_then(|v| v.as_str())
                .ok_or("Missing EC 'y' in JWK")?;
            let x = decode_b64_url(x_b64)?;
            let y = decode_b64_url(y_b64)?;
            if x.len() > coord_len || y.len() > coord_len {
                return Err("Invalid EC coordinate length in JWK".to_string());
            }
            let mut point = Vec::with_capacity(1 + coord_len * 2);
            point.push(0x04);
            point.extend(std::iter::repeat_n(0u8, coord_len - x.len()));
            point.extend_from_slice(&x);
            point.extend(std::iter::repeat_n(0u8, coord_len - y.len()));
            point.extend_from_slice(&y);

            let verify_alg: &dyn signature::VerificationAlgorithm = if alg == "ES256" {
                &signature::ECDSA_P256_SHA256_FIXED
            } else {
                &signature::ECDSA_P384_SHA384_FIXED
            };
            UnparsedPublicKey::new(verify_alg, &point)
                .verify(signing_input, sig_bytes)
                .map_err(|_| format!("Invalid {} signature on id_token", alg))
        }
        other => Err(format!("Unsupported OIDC id_token algorithm: {}", other)),
    }
}

pub async fn verify_oidc_id_token(
    client: &reqwest::Client,
    id_token: &str,
    issuer: &str,
    audience: &str,
    jwks_uri: Option<&str>,
    client_secret: &str,
    expected_nonce: &str,
) -> Result<Value, String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return Err("Malformed OIDC id_token JWT".to_string());
    }

    let header_bytes = decode_b64_url(parts[0])?;
    let header_val: Value =
        serde_json::from_slice(&header_bytes).map_err(|_| "Invalid id_token header JSON")?;
    let payload_bytes = decode_b64_url(parts[1])?;
    let payload: Value =
        serde_json::from_slice(&payload_bytes).map_err(|_| "Invalid id_token payload JSON")?;
    let sig_bytes = decode_b64_url(parts[2])?;
    let signing_input = format!("{}.{}", parts[0], parts[1]);

    let alg = header_val
        .get("alg")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'alg' in id_token header")?;

    if alg == "HS256" {
        if client_secret.is_empty() {
            return Err("Missing client_secret for HS256 id_token verification".to_string());
        }
        let key = hmac::Key::new(hmac::HMAC_SHA256, client_secret.as_bytes());
        hmac::verify(&key, signing_input.as_bytes(), &sig_bytes)
            .map_err(|_| "Invalid HS256 signature on id_token")?;
    } else {
        let jwks_url = jwks_uri
            .filter(|s| !s.trim().is_empty())
            .ok_or("OIDC discovery document missing jwks_uri")?;
        let jwks_res = client
            .get(jwks_url)
            .header(header::CACHE_CONTROL, "no-store")
            .send()
            .await
            .map_err(|e| format!("Failed to fetch JWKS: {}", e))?;
        if !jwks_res.status().is_success() {
            return Err(format!("JWKS endpoint returned {}", jwks_res.status()));
        }
        let jwks: Value = jwks_res
            .json()
            .await
            .map_err(|e| format!("Invalid JWKS JSON: {}", e))?;
        let keys = jwks
            .get("keys")
            .and_then(|v| v.as_array())
            .ok_or("JWKS missing 'keys' array")?;

        let kid = header_val.get("kid").and_then(|v| v.as_str());
        let candidates: Vec<&Value> = if let Some(target_kid) = kid {
            keys.iter()
                .filter(|k| k.get("kid").and_then(|v| v.as_str()) == Some(target_kid))
                .collect()
        } else {
            keys.iter()
                .filter(|k| {
                    k.get("use")
                        .and_then(|v| v.as_str())
                        .map(|u| u == "sig")
                        .unwrap_or(true)
                })
                .collect()
        };

        if candidates.is_empty() {
            return Err("No matching JWK found for id_token".to_string());
        }

        let mut verified = false;
        let mut last_err = String::from("Signature verification failed");
        for jwk in candidates {
            match verify_jwk_signature(jwk, alg, signing_input.as_bytes(), &sig_bytes) {
                Ok(()) => {
                    verified = true;
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        if !verified {
            return Err(last_err);
        }
    }

    // Validate standard OIDC claims
    let iss = payload
        .get("iss")
        .and_then(|v| v.as_str())
        .ok_or("id_token missing 'iss'")?;
    if trim_trailing_slashes(iss) != trim_trailing_slashes(issuer) {
        return Err(format!(
            "OIDC issuer mismatch: expected {}, got {}",
            issuer, iss
        ));
    }

    let aud_matches = match payload.get("aud") {
        Some(Value::String(s)) => s == audience,
        Some(Value::Array(arr)) => arr.iter().any(|v| v.as_str() == Some(audience)),
        _ => false,
    };
    if !aud_matches {
        return Err("OIDC audience mismatch".to_string());
    }

    let now = chrono::Utc::now().timestamp();
    let exp = payload
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or("id_token missing 'exp'")?;
    if now > exp + SAML_CLOCK_SKEW_SECS {
        return Err("OIDC id_token expired".to_string());
    }

    if let Some(nbf) = payload.get("nbf").and_then(|v| v.as_i64()) {
        if now + SAML_CLOCK_SKEW_SECS < nbf {
            return Err("OIDC id_token not yet valid".to_string());
        }
    }

    if !expected_nonce.is_empty() {
        let token_nonce = payload
            .get("nonce")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if token_nonce != expected_nonce {
            return Err("OIDC nonce mismatch".to_string());
        }
    }

    Ok(payload)
}

fn pick_oidc_display_name(payload: &Value) -> String {
    for key in ["preferred_username", "email", "name", "given_name", "sub"] {
        if let Some(val) = payload.get(key).and_then(|v| v.as_str()) {
            if !val.trim().is_empty() {
                return val.to_string();
            }
        }
    }
    "OIDC user".to_string()
}

fn pick_oidc_email(payload: &Value) -> String {
    payload
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub async fn oidc_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = get_public_origin(&headers, &uri, state.config.port);
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let Some(cfg) = get_oidc_runtime_config(&settings) else {
        return Redirect::temporary(&format!("{}/login?error=oidc_not_configured", origin))
            .into_response();
    };

    let discovery = match fetch_oidc_discovery(state.proxy.client(), &cfg.issuer_url).await {
        Ok(d) => d,
        Err(err) => {
            return Redirect::temporary(&format!(
                "{}/login?error={}",
                origin,
                url_encode_component(&err)
            ))
            .into_response();
        }
    };

    let Some(auth_endpoint) = discovery
        .get("authorization_endpoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Redirect::temporary(&format!(
            "{}/login?error={}",
            origin,
            url_encode_component("OIDC discovery missing authorization_endpoint")
        ))
        .into_response();
    };

    let oidc_state = random_base64url(16);
    let nonce = random_base64url(16);
    let (verifier, challenge) = create_pkce_pair();
    let redirect_uri = format!("{}/api/auth/oidc/callback", origin);

    let auth_url = match build_oidc_authorization_url(
        auth_endpoint,
        &cfg.client_id,
        &redirect_uri,
        &cfg.scopes,
        &oidc_state,
        &nonce,
        &challenge,
    ) {
        Ok(u) => u,
        Err(err) => {
            return Redirect::temporary(&format!(
                "{}/login?error={}",
                origin,
                url_encode_component(&err)
            ))
            .into_response();
        }
    };

    let secure = should_use_secure_cookie(&headers);
    redirect_with_cookies(
        &auth_url,
        vec![
            format_cookie("oidc_state", &oidc_state, SSO_COOKIE_MAX_AGE_SECS, true, secure),
            format_cookie("oidc_nonce", &nonce, SSO_COOKIE_MAX_AGE_SECS, true, secure),
            format_cookie(
                "oidc_code_verifier",
                &verifier,
                SSO_COOKIE_MAX_AGE_SECS,
                true,
                secure,
            ),
        ],
    )
}

fn clear_oidc_cookies(secure: bool) -> Vec<String> {
    vec![
        format_cookie("oidc_state", "", 0, true, secure),
        format_cookie("oidc_nonce", "", 0, true, secure),
        format_cookie("oidc_code_verifier", "", 0, true, secure),
    ]
}

#[derive(Debug, Deserialize, Default)]
pub struct OidcCallbackQuery {
    pub error: Option<String>,
    pub code: Option<String>,
    pub state: Option<String>,
}

pub async fn oidc_callback_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Query(query): Query<OidcCallbackQuery>,
) -> Response {
    let origin = get_public_origin(&headers, &uri, state.config.port);
    let secure = should_use_secure_cookie(&headers);

    if let Some(err) = query.error.as_deref().filter(|s| !s.is_empty()) {
        return Redirect::temporary(&format!(
            "{}/login?error={}",
            origin,
            url_encode_component(err)
        ))
        .into_response();
    }

    let (Some(code), Some(req_state)) = (
        query.code.as_deref().filter(|s| !s.is_empty()),
        query.state.as_deref().filter(|s| !s.is_empty()),
    ) else {
        return Redirect::temporary(&format!("{}/login?error=oidc_missing_code", origin))
            .into_response();
    };

    let stored_state = get_cookie(&headers, "oidc_state");
    let stored_nonce = get_cookie(&headers, "oidc_nonce");
    let code_verifier = get_cookie(&headers, "oidc_code_verifier");

    if stored_state.as_deref() != Some(req_state)
        || stored_nonce.is_none()
        || code_verifier.is_none()
    {
        return redirect_with_cookies(
            &format!("{}/login?error=oidc_invalid_state", origin),
            clear_oidc_cookies(secure),
        );
    }

    let stored_nonce = stored_nonce.unwrap_or_default();
    let code_verifier = code_verifier.unwrap_or_default();

    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let Some(cfg) = get_oidc_runtime_config(&settings) else {
        return redirect_with_cookies(
            &format!("{}/login?error=oidc_not_configured", origin),
            clear_oidc_cookies(secure),
        );
    };

    let discovery = match fetch_oidc_discovery(state.proxy.client(), &cfg.issuer_url).await {
        Ok(d) => d,
        Err(err) => {
            return redirect_with_cookies(
                &format!("{}/login?error={}", origin, url_encode_component(&err)),
                clear_oidc_cookies(secure),
            );
        }
    };

    let discovered_issuer = discovery
        .get("issuer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&cfg.issuer_url);
    let token_endpoint = match discovery
        .get("token_endpoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(ep) => ep,
        None => {
            return redirect_with_cookies(
                &format!(
                    "{}/login?error={}",
                    origin,
                    url_encode_component("OIDC discovery missing token_endpoint")
                ),
                clear_oidc_cookies(secure),
            );
        }
    };

    let redirect_uri = format!("{}/api/auth/oidc/callback", origin);
    let token_data = match exchange_oidc_code(
        state.proxy.client(),
        token_endpoint,
        &cfg.client_id,
        &cfg.client_secret,
        code,
        &redirect_uri,
        &code_verifier,
    )
    .await
    {
        Ok(data) => data,
        Err(err) => {
            return redirect_with_cookies(
                &format!("{}/login?error={}", origin, url_encode_component(&err)),
                clear_oidc_cookies(secure),
            );
        }
    };

    let Some(id_token) = token_data
        .get("id_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return redirect_with_cookies(
            &format!(
                "{}/login?error={}",
                origin,
                url_encode_component("OIDC provider did not return an id_token")
            ),
            clear_oidc_cookies(secure),
        );
    };

    let jwks_uri = discovery.get("jwks_uri").and_then(|v| v.as_str());
    let mut payload = match verify_oidc_id_token(
        state.proxy.client(),
        id_token,
        discovered_issuer,
        &cfg.client_id,
        jwks_uri,
        &cfg.client_secret,
        &stored_nonce,
    )
    .await
    {
        Ok(p) => p,
        Err(err) => {
            return redirect_with_cookies(
                &format!("{}/login?error={}", origin, url_encode_component(&err)),
                clear_oidc_cookies(secure),
            );
        }
    };

    // Best-effort userinfo enrichment if email is missing in id_token
    if pick_oidc_email(&payload).is_empty() {
        if let (Some(userinfo_ep), Some(access_token)) = (
            discovery.get("userinfo_endpoint").and_then(|v| v.as_str()),
            token_data.get("access_token").and_then(|v| v.as_str()),
        ) {
            if let Ok(resp) = state
                .proxy
                .client()
                .get(userinfo_ep)
                .bearer_auth(access_token)
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(Value::Object(info)) = resp.json::<Value>().await {
                        if let Some(obj) = payload.as_object_mut() {
                            for (k, v) in info {
                                obj.entry(k).or_insert(v);
                            }
                        }
                    }
                }
            }
        }
    }

    let email = pick_oidc_email(&payload);
    let session_jwt = create_dashboard_jwt_with_claims(
        &state.config.jwt_secret,
        json!({
            "oidc": true,
            "oidcSub": payload.get("sub").cloned().unwrap_or(Value::Null),
            "oidcEmail": if email.is_empty() { Value::Null } else { json!(email) },
            "oidcName": pick_oidc_display_name(&payload),
        }),
    );

    let mut cookies_out = clear_oidc_cookies(secure);
    cookies_out.push(format_cookie(
        "auth_token",
        &session_jwt,
        SESSION_MAX_AGE_SECS,
        true,
        secure,
    ));

    redirect_with_cookies(&format!("{}/dashboard", origin), cookies_out)
}

// ============================================================================
// 2. Auth SSO: SAML Start, Metadata, ACS
// ============================================================================

pub fn format_x509_certificate(cert_str: &str) -> String {
    let cleaned: String = cert_str
        .replace("-----BEGIN CERTIFICATE-----", "")
        .replace("-----END CERTIFICATE-----", "")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    if cleaned.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = cleaned
        .as_bytes()
        .chunks(64)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .collect();
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
        lines.join("\n")
    )
}

pub fn is_saml_configured(settings: &Value) -> bool {
    let entry_point = settings
        .get("samlEntryPoint")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let cert = settings
        .get("samlCert")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    !entry_point.is_empty() && !cert.is_empty()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn generate_saml_metadata(origin: &str, settings: &Value) -> String {
    let issuer = settings
        .get("samlIssuer")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("urn:9router:sp");
    let acs_url = format!("{}/api/auth/saml/acs", trim_trailing_slashes(origin));

    format!(
        concat!(
            r#"<?xml version="1.0"?>"#,
            "\n",
            r#"<EntityDescriptor xmlns="urn:oasis:names:tc:SAML:2.0:metadata" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" entityID="{issuer}">"#,
            "\n",
            r#"  <SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">"#,
            "\n",
            r#"    <NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified</NameIDFormat>"#,
            "\n",
            r#"    <AssertionConsumerService index="1" isDefault="true" Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="{acs_url}"/>"#,
            "\n",
            r#"  </SPSSODescriptor>"#,
            "\n",
            r#"</EntityDescriptor>"#
        ),
        issuer = xml_escape(issuer),
        acs_url = xml_escape(&acs_url),
    )
}

/// Encodes raw bytes into RFC 1951 DEFLATE stored blocks (BTYPE=00), compatible with all `inflateRaw` decoders.
pub fn deflate_raw_stored(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![0x01, 0x00, 0x00, 0xFF, 0xFF];
    }
    let mut out = Vec::with_capacity(data.len() + (data.len() / 65535 + 1) * 5);
    let chunks: Vec<&[u8]> = data.chunks(65535).collect();
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx + 1 == chunks.len();
        out.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        let nlen = !len;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

pub fn build_saml_authorize_url(
    origin: &str,
    settings: &Value,
) -> Result<(String, String), String> {
    let entry_point = settings
        .get("samlEntryPoint")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("SAML Entry Point is required")?;
    let issuer = settings
        .get("samlIssuer")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("urn:9router:sp");
    let acs_url = format!("{}/api/auth/saml/acs", trim_trailing_slashes(origin));
    let request_id = format!("_{}", random_hex(16));
    let issue_instant = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let authn_request_xml = format!(
        concat!(
            r#"<samlp:AuthnRequest xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" "#,
            r#"ID="{id}" Version="2.0" IssueInstant="{instant}" "#,
            r#"ProtocolBinding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" "#,
            r#"AssertionConsumerServiceURL="{acs_url}" Destination="{dest}">"#,
            r#"<saml:Issuer xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion">{issuer}</saml:Issuer>"#,
            r#"<samlp:NameIDPolicy Format="urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified" AllowCreate="true"/>"#,
            r#"</samlp:AuthnRequest>"#,
        ),
        id = xml_escape(&request_id),
        instant = xml_escape(&issue_instant),
        acs_url = xml_escape(&acs_url),
        dest = xml_escape(entry_point),
        issuer = xml_escape(issuer),
    );

    let deflated = deflate_raw_stored(authn_request_xml.as_bytes());
    let saml_request_b64 = base64::engine::general_purpose::STANDARD.encode(&deflated);

    let mut url = url::Url::parse(entry_point)
        .map_err(|e| format!("Invalid samlEntryPoint URL: {}", e))?;
    url.query_pairs_mut()
        .append_pair("SAMLRequest", &saml_request_b64);

    Ok((url.to_string(), request_id))
}

pub async fn saml_metadata_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let origin = get_saml_base_url(&headers, &uri, &settings, state.config.port);
    let metadata_xml = generate_saml_metadata(&origin, &settings);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/xml"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        metadata_xml,
    )
        .into_response()
}

pub async fn saml_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let origin = get_saml_base_url(&headers, &uri, &settings, state.config.port);

    if !is_saml_configured(&settings) {
        return Redirect::temporary(&format!("{}/login?error=saml_not_configured", origin))
            .into_response();
    }

    match build_saml_authorize_url(&origin, &settings) {
        Ok((authorize_url, request_id)) => {
            let secure = should_use_secure_cookie(&headers);
            redirect_with_cookies(
                &authorize_url,
                vec![format_cookie(
                    "saml_state",
                    &request_id,
                    SSO_COOKIE_MAX_AGE_SECS,
                    true,
                    secure,
                )],
            )
        }
        Err(err) => Redirect::temporary(&format!(
            "{}/login?error={}",
            origin,
            url_encode_component(&err)
        ))
        .into_response(),
    }
}

// --- SAML Login Limiter Helpers ---

fn get_client_ip(headers: &HeaderMap) -> String {
    if let Ok(peer_token) = std::env::var("NINEROUTER_PEER_TOKEN") {
        if !peer_token.is_empty() {
            let supplied = headers
                .get("x-9r-peer-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if auth::constant_time_eq(supplied.as_bytes(), peer_token.as_bytes()) {
                if let Some(ip) = headers.get("x-9r-real-ip").and_then(|v| v.to_str().ok()) {
                    if !ip.trim().is_empty() {
                        return ip.trim().to_string();
                    }
                }
            }
        }
    }
    if std::env::var("TRUST_PROXY").map(|v| v == "true").unwrap_or(false) {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next().map(str::trim).filter(|s| !s.is_empty()) {
                return first.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn check_saml_lock(ip: &str) -> Option<u64> {
    let map = SAML_LOGIN_ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().ok()?;
    let now = Instant::now();
    if let Some(entry) = guard.get(ip) {
        if let (Some(last), lock_until) = (entry.last_fail_at, entry.lock_until) {
            if now.duration_since(last) > FAIL_WINDOW
                && lock_until.map(|u| now >= u).unwrap_or(true)
            {
                guard.remove(ip);
                return None;
            }
        }
        if let Some(until) = entry.lock_until {
            if until > now {
                let secs = until.duration_since(now).as_secs_f64().ceil() as u64;
                return Some(secs.max(1));
            }
        }
    }
    None
}

fn record_saml_fail(ip: &str) {
    let map = SAML_LOGIN_ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        let now = Instant::now();
        let entry = guard.entry(ip.to_string()).or_default();
        if let Some(last) = entry.last_fail_at {
            if now.duration_since(last) > FAIL_WINDOW
                && entry.lock_until.map(|u| now >= u).unwrap_or(true)
            {
                *entry = LockEntry::default();
            }
        }
        entry.fails += 1;
        entry.last_fail_at = Some(now);
        if entry.fails >= MAX_FAILS_BEFORE_LOCK {
            let step_ms = LOCK_STEPS_MS[entry.lock_level.min(LOCK_STEPS_MS.len() - 1)];
            entry.lock_until = Some(now + Duration::from_millis(step_ms));
            entry.lock_level += 1;
            entry.fails = 0;
        }
    }
}

fn record_saml_success(ip: &str) {
    if let Some(map) = SAML_LOGIN_ATTEMPTS.get() {
        if let Ok(mut guard) = map.lock() {
            guard.remove(ip);
        }
    }
}

// --- ASN.1 DER X.509 Certificate Public Key Extraction & XMLDSig Verification ---

#[derive(Debug, Clone, Copy)]
struct DerTlv<'a> {
    tag: u8,
    content: &'a [u8],
    total_len: usize,
}

fn read_der_tlv(input: &[u8]) -> Result<DerTlv<'_>, String> {
    if input.len() < 2 {
        return Err("Truncated ASN.1 DER element".to_string());
    }
    let tag = input[0];
    let len_byte = input[1];
    let (len, header_len) = if len_byte & 0x80 == 0 {
        (len_byte as usize, 2)
    } else {
        let num_octets = (len_byte & 0x7F) as usize;
        if num_octets == 0 || num_octets > 4 || input.len() < 2 + num_octets {
            return Err("Invalid ASN.1 DER length".to_string());
        }
        let mut len = 0usize;
        for b in &input[2..2 + num_octets] {
            len = (len << 8) | (*b as usize);
        }
        (len, 2 + num_octets)
    };
    let total_len = header_len + len;
    if input.len() < total_len {
        return Err("Truncated ASN.1 DER content".to_string());
    }
    Ok(DerTlv {
        tag,
        content: &input[header_len..total_len],
        total_len,
    })
}

#[derive(Debug, Clone)]
enum CertPublicKey {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    EcP256(Vec<u8>),
    EcP384(Vec<u8>),
}

fn cert_pem_to_der(cert_str: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = cert_str
        .replace("-----BEGIN CERTIFICATE-----", "")
        .replace("-----END CERTIFICATE-----", "")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    if cleaned.is_empty() {
        return Err("Empty X.509 certificate".to_string());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| format!("Invalid X.509 certificate base64: {}", e))
}

fn extract_public_key_from_cert_der(der: &[u8]) -> Result<CertPublicKey, String> {
    let cert_seq = read_der_tlv(der)?;
    if cert_seq.tag != 0x30 {
        return Err("Certificate is not an ASN.1 SEQUENCE".to_string());
    }
    let tbs = read_der_tlv(cert_seq.content)?;
    if tbs.tag != 0x30 {
        return Err("tbsCertificate is not an ASN.1 SEQUENCE".to_string());
    }

    let mut rest = tbs.content;
    let first = read_der_tlv(rest)?;
    if first.tag == 0xA0 {
        // Optional [0] version
        rest = &rest[first.total_len..];
    }
    // Skip: serialNumber, signature, issuer, validity, subject (5 elements)
    for _ in 0..5 {
        let elem = read_der_tlv(rest)?;
        rest = &rest[elem.total_len..];
    }
    let spki = read_der_tlv(rest)?;
    if spki.tag != 0x30 {
        return Err("subjectPublicKeyInfo is not an ASN.1 SEQUENCE".to_string());
    }

    let alg_seq = read_der_tlv(spki.content)?;
    if alg_seq.tag != 0x30 {
        return Err("AlgorithmIdentifier is not an ASN.1 SEQUENCE".to_string());
    }
    let alg_oid = read_der_tlv(alg_seq.content)?;
    if alg_oid.tag != 0x06 {
        return Err("AlgorithmIdentifier missing OID".to_string());
    }
    let spk_bit_string = read_der_tlv(&spki.content[alg_seq.total_len..])?;
    if spk_bit_string.tag != 0x03 || spk_bit_string.content.is_empty() {
        return Err("subjectPublicKey is not a valid BIT STRING".to_string());
    }
    let key_bytes = &spk_bit_string.content[1..]; // skip unused-bits count

    // OID 1.2.840.113549.1.1.1 (rsaEncryption)
    const OID_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
    // OID 1.2.840.10045.2.1 (ecPublicKey)
    const OID_EC: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
    // OID 1.2.840.10045.3.1.7 (prime256v1 / P-256)
    const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
    // OID 1.3.132.0.34 (secp384r1 / P-384)
    const OID_P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];

    if alg_oid.content == OID_RSA {
        let rsa_seq = read_der_tlv(key_bytes)?;
        if rsa_seq.tag != 0x30 {
            return Err("RSAPublicKey is not an ASN.1 SEQUENCE".to_string());
        }
        let modulus = read_der_tlv(rsa_seq.content)?;
        let exponent = read_der_tlv(&rsa_seq.content[modulus.total_len..])?;
        if modulus.tag != 0x02 || exponent.tag != 0x02 {
            return Err("RSAPublicKey missing INTEGER fields".to_string());
        }
        let n = modulus
            .content
            .iter()
            .position(|b| *b != 0)
            .map(|i| modulus.content[i..].to_vec())
            .unwrap_or_else(|| modulus.content.to_vec());
        let e = exponent
            .content
            .iter()
            .position(|b| *b != 0)
            .map(|i| exponent.content[i..].to_vec())
            .unwrap_or_else(|| exponent.content.to_vec());
        Ok(CertPublicKey::Rsa { n, e })
    } else if alg_oid.content == OID_EC {
        let curve_oid = read_der_tlv(&alg_seq.content[alg_oid.total_len..])?;
        if curve_oid.content == OID_P256 {
            Ok(CertPublicKey::EcP256(key_bytes.to_vec()))
        } else if curve_oid.content == OID_P384 {
            Ok(CertPublicKey::EcP384(key_bytes.to_vec()))
        } else {
            Err("Unsupported EC curve in X.509 certificate".to_string())
        }
    } else {
        Err("Unsupported public key algorithm in X.509 certificate".to_string())
    }
}

fn find_xml_tag_block<'a>(xml: &'a str, local_name: &str) -> Option<&'a str> {
    let mut search_from = 0usize;
    while let Some(rel_open) = xml[search_from..].find('<') {
        let open_idx = search_from + rel_open;
        let after = &xml[open_idx + 1..];
        if after.starts_with('/') || after.starts_with('?') || after.starts_with('!') {
            search_from = open_idx + 1;
            continue;
        }
        let name_end = after
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(after.len());
        let full_name = &after[..name_end];
        let matched_local = full_name.split(':').next_back().unwrap_or(full_name);
        if matched_local == local_name {
            let tag_close = open_idx + 1 + after.find('>')?;
            if xml.as_bytes().get(tag_close - 1) == Some(&b'/') {
                return Some(&xml[open_idx..=tag_close]);
            }
            // Find matching closing tag </...:local_name>
            let mut depth = 1usize;
            let mut cursor = tag_close + 1;
            while cursor < xml.len() {
                let Some(next_lt) = xml[cursor..].find('<') else {
                    break;
                };
                let lt = cursor + next_lt;
                let rem = &xml[lt + 1..];
                if let Some(after_slash) = rem.strip_prefix('/') {
                    let end_gt = lt + 1 + rem.find('>')?;
                    let close_name = after_slash[..end_gt - (lt + 2)].trim();
                    let close_local = close_name.split(':').next_back().unwrap_or(close_name);
                    if close_local == local_name {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&xml[open_idx..=end_gt]);
                        }
                    }
                    cursor = end_gt + 1;
                } else if !rem.starts_with('?') && !rem.starts_with('!') {
                    let n_end = rem
                        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                        .unwrap_or(rem.len());
                    let open_name = &rem[..n_end];
                    let open_local = open_name.split(':').next_back().unwrap_or(open_name);
                    let end_gt = lt + 1 + rem.find('>')?;
                    if open_local == local_name && xml.as_bytes().get(end_gt - 1) != Some(&b'/') {
                        depth += 1;
                    }
                    cursor = end_gt + 1;
                } else {
                    cursor = lt + 1;
                }
            }
        }
        search_from = open_idx + 1;
    }
    None
}

fn find_all_xml_tag_blocks<'a>(xml: &'a str, local_name: &str) -> Vec<&'a str> {
    let mut results = Vec::new();
    let mut cursor = 0usize;
    while cursor < xml.len() {
        if let Some(block) = find_xml_tag_block(&xml[cursor..], local_name) {
            let start = block.as_ptr() as usize - xml.as_ptr() as usize;
            results.push(block);
            cursor = start + block.len();
        } else {
            break;
        }
    }
    results
}

fn extract_xml_attr<'a>(tag_or_block: &'a str, attr_name: &str) -> Option<&'a str> {
    let tag_end = tag_or_block.find('>')?;
    let header = &tag_or_block[..tag_end];
    for quote in ['"', '\''] {
        let needle = format!("{}={}", attr_name, quote);
        if let Some(pos) = header.find(&needle) {
            let after = &header[pos + needle.len()..];
            if let Some(end_q) = after.find(quote) {
                return Some(&after[..end_q]);
            }
        }
    }
    None
}

fn extract_xml_inner_text(block: &str) -> String {
    let Some(start) = block.find('>') else {
        return String::new();
    };
    let Some(end) = block.rfind('<') else {
        return String::new();
    };
    if end <= start + 1 {
        return String::new();
    }
    block[start + 1..end]
        .trim()
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn verify_cert_signature(
    pub_key: &CertPublicKey,
    sig_method_uri: &str,
    data: &[u8],
    sig_bytes: &[u8],
) -> Result<(), String> {
    let lower = sig_method_uri.to_ascii_lowercase();
    match pub_key {
        CertPublicKey::Rsa { n, e } => {
            let alg: &signature::RsaParameters = if lower.contains("sha512") {
                &signature::RSA_PKCS1_2048_8192_SHA512
            } else if lower.contains("sha384") {
                &signature::RSA_PKCS1_2048_8192_SHA384
            } else if lower.contains("sha1") {
                &signature::RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY
            } else {
                &signature::RSA_PKCS1_2048_8192_SHA256
            };
            RsaPublicKeyComponents { n, e }
                .verify(alg, data, sig_bytes)
                .map_err(|_| "SAML RSA signature verification failed".to_string())
        }
        CertPublicKey::EcP256(point) => {
            let alg: &dyn signature::VerificationAlgorithm = if sig_bytes.len() == 64 {
                &signature::ECDSA_P256_SHA256_FIXED
            } else {
                &signature::ECDSA_P256_SHA256_ASN1
            };
            UnparsedPublicKey::new(alg, point)
                .verify(data, sig_bytes)
                .map_err(|_| "SAML ECDSA P-256 signature verification failed".to_string())
        }
        CertPublicKey::EcP384(point) => {
            let alg: &dyn signature::VerificationAlgorithm = if sig_bytes.len() == 96 {
                &signature::ECDSA_P384_SHA384_FIXED
            } else {
                &signature::ECDSA_P384_SHA384_ASN1
            };
            UnparsedPublicKey::new(alg, point)
                .verify(data, sig_bytes)
                .map_err(|_| "SAML ECDSA P-384 signature verification failed".to_string())
        }
    }
}

fn inject_inherited_xmlns(signed_info: &str, context_xml: &str) -> String {
    let Some(first_gt) = signed_info.find('>') else {
        return signed_info.to_string();
    };
    let open_tag = &signed_info[..first_gt];
    let prefix = open_tag
        .trim_start_matches('<')
        .split_once(':')
        .map(|(p, _)| p)
        .unwrap_or("");
    let xmlns_attr = if prefix.is_empty() {
        "xmlns=".to_string()
    } else {
        format!("xmlns:{}=", prefix)
    };
    if open_tag.contains(&xmlns_attr) {
        return signed_info.to_string();
    }
    // Check if an enclosing element (such as <ds:Signature xmlns:ds="...">) declared it
    if let Some(pos) = context_xml.find(&xmlns_attr) {
        let after = &context_xml[pos + xmlns_attr.len()..];
        if let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') {
            if let Some(end) = after[1..].find(q) {
                let ns_uri = &after[1..1 + end];
                let injected = format!(" {}{}{}{}", xmlns_attr, q, ns_uri, q);
                return format!("{}{}{}", open_tag, injected, &signed_info[first_gt..]);
            }
        }
    }
    signed_info.to_string()
}

pub fn validate_saml_response_xml(
    xml: &str,
    expected_request_id: Option<&str>,
    settings: &Value,
) -> Result<Value, String> {
    let saml_cert = settings
        .get("samlCert")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("IdP X.509 Certificate (samlCert) is missing or not configured")?;

    let configured_cert_der = cert_pem_to_der(saml_cert)?;
    let pub_key = extract_public_key_from_cert_der(&configured_cert_der)?;

    // 1. Replay check on InResponseTo
    if let Some(expected_id) = expected_request_id.filter(|s| !s.is_empty()) {
        let in_response_to = extract_xml_attr(xml, "InResponseTo");
        if in_response_to != Some(expected_id) {
            return Err(format!(
                "InResponseTo mismatch: expected {}, received {}",
                expected_id,
                in_response_to.unwrap_or("none")
            ));
        }
    }

    // 2. SAML StatusCode check (when present)
    if let Some(status_block) = find_xml_tag_block(xml, "StatusCode") {
        if let Some(val) = extract_xml_attr(status_block, "Value") {
            if !val.ends_with(":Success") {
                return Err(format!("SAML authentication failed with status {}", val));
            }
        }
    }

    // 3. Conditions NotBefore / NotOnOrAfter / Audience check
    if let Some(cond_block) = find_xml_tag_block(xml, "Conditions") {
        let now = chrono::Utc::now().timestamp();
        if let Some(nb) = extract_xml_attr(cond_block, "NotBefore") {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(nb) {
                if now + SAML_CLOCK_SKEW_SECS < dt.timestamp() {
                    return Err("SAML assertion NotBefore condition not yet satisfied".to_string());
                }
            }
        }
        if let Some(noa) = extract_xml_attr(cond_block, "NotOnOrAfter") {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(noa) {
                if now - SAML_CLOCK_SKEW_SECS >= dt.timestamp() {
                    return Err("SAML assertion expired (NotOnOrAfter)".to_string());
                }
            }
        }
    }

    let sp_issuer = settings
        .get("samlIssuer")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("urn:9router:sp");
    if let Some(aud_block) = find_xml_tag_block(xml, "Audience") {
        let audience = extract_xml_inner_text(aud_block);
        if !audience.is_empty() && audience != sp_issuer {
            return Err(format!(
                "SAML Audience mismatch: expected {}, got {}",
                sp_issuer, audience
            ));
        }
    }

    // 4. Signature & X.509 Certificate validation
    let sig_block = find_xml_tag_block(xml, "Signature")
        .ok_or("SAML response/assertion is missing required XML Signature")?;

    if let Some(x509_block) = find_xml_tag_block(sig_block, "X509Certificate") {
        let embedded_b64 = extract_xml_inner_text(x509_block);
        let embedded_der = cert_pem_to_der(&embedded_b64)?;
        if embedded_der != configured_cert_der {
            return Err("SAML X.509 certificate does not match configured samlCert".to_string());
        }
    }

    let signed_info_raw = find_xml_tag_block(sig_block, "SignedInfo")
        .ok_or("SAML Signature missing SignedInfo")?;
    let sig_value_block = find_xml_tag_block(sig_block, "SignatureValue")
        .ok_or("SAML Signature missing SignatureValue")?;
    let sig_b64: String = extract_xml_inner_text(sig_value_block)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sig_b64)
        .map_err(|e| format!("Invalid SAML SignatureValue base64: {}", e))?;

    let sig_method_uri = find_xml_tag_block(signed_info_raw, "SignatureMethod")
        .and_then(|b| extract_xml_attr(b, "Algorithm"))
        .unwrap_or("http://www.w3.org/2001/04/xmldsig-more#rsa-sha256");

    // Verify SignedInfo against the configured IdP certificate public key, trying both
    // the exact SignedInfo slice and with inherited namespace declaration injected.
    let signed_info_with_ns = inject_inherited_xmlns(signed_info_raw, xml);
    if verify_cert_signature(&pub_key, sig_method_uri, signed_info_raw.as_bytes(), &sig_bytes)
        .is_err()
        && verify_cert_signature(
            &pub_key,
            sig_method_uri,
            signed_info_with_ns.as_bytes(),
            &sig_bytes,
        )
        .is_err()
    {
        return Err("SAML XML signature verification failed".to_string());
    }

    // 5. Extract Assertion claims (NameID + AttributeStatement)
    let assertion_xml = find_xml_tag_block(xml, "Assertion").unwrap_or(xml);
    let mut profile_map = serde_json::Map::new();
    let mut attrs_map = serde_json::Map::new();

    if let Some(name_id_block) = find_xml_tag_block(assertion_xml, "NameID") {
        let name_id = extract_xml_inner_text(name_id_block);
        if !name_id.is_empty() {
            profile_map.insert("nameID".to_string(), json!(name_id));
        }
    }

    for attr_block in find_all_xml_tag_blocks(assertion_xml, "Attribute") {
        let Some(name) = extract_xml_attr(attr_block, "Name") else {
            continue;
        };
        let values: Vec<String> = find_all_xml_tag_blocks(attr_block, "AttributeValue")
            .into_iter()
            .map(extract_xml_inner_text)
            .filter(|s| !s.is_empty())
            .collect();
        if values.is_empty() {
            continue;
        }
        let val = if values.len() == 1 {
            json!(values[0])
        } else {
            json!(values)
        };
        attrs_map.insert(name.to_string(), val.clone());
        profile_map.entry(name.to_string()).or_insert(val);
    }

    profile_map.insert("attributes".to_string(), Value::Object(attrs_map));
    Ok(Value::Object(profile_map))
}

fn value_as_claim_str(val: &Value) -> Option<String> {
    match val {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Array(arr) => arr.first().and_then(value_as_claim_str),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn pick_saml_email(profile: &Value, settings: &Value) -> String {
    if let Some(custom_attr) = settings
        .get("samlAttributeEmail")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(s) = profile.get(custom_attr).and_then(value_as_claim_str) {
            return s;
        }
    }

    let email_keys = [
        "email",
        "emailAddress",
        "mail",
        "nameID",
        "nameId",
        "upn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/upn",
    ];

    for key in email_keys {
        if let Some(s) = profile.get(key).and_then(value_as_claim_str) {
            return s;
        }
    }
    if let Some(attrs) = profile.get("attributes") {
        for key in email_keys {
            if let Some(s) = attrs.get(key).and_then(value_as_claim_str) {
                return s;
            }
        }
    }
    String::new()
}

pub fn pick_saml_display_name(profile: &Value, settings: &Value) -> String {
    if let Some(custom_attr) = settings
        .get("samlAttributeName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(s) = profile.get(custom_attr).and_then(value_as_claim_str) {
            return s;
        }
    }

    let name_keys = [
        "displayName",
        "name",
        "cn",
        "commonName",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/givenname",
    ];

    for key in name_keys {
        if let Some(s) = profile.get(key).and_then(value_as_claim_str) {
            return s;
        }
    }

    let given = profile
        .get("givenName")
        .and_then(value_as_claim_str)
        .unwrap_or_default();
    let surname = profile
        .get("sn")
        .or_else(|| profile.get("surname"))
        .and_then(value_as_claim_str)
        .unwrap_or_default();
    let combined = format!("{} {}", given, surname).trim().to_string();
    if !combined.is_empty() {
        return combined;
    }

    pick_saml_email(profile, settings)
}

fn extract_saml_response_from_body(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ct.contains("application/json") {
        if let Ok(val) = serde_json::from_slice::<Value>(body) {
            return val
                .get("SAMLResponse")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }

    if ct.contains("multipart/form-data") {
        let text = String::from_utf8_lossy(body);
        if let Some(pos) = text.find("name=\"SAMLResponse\"") {
            let after = &text[pos..];
            if let Some(hdr_end) = after.find("\r\n\r\n") {
                let val_start = &after[hdr_end + 4..];
                let val = val_start
                    .split("\r\n--")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }

    // Default: application/x-www-form-urlencoded
    for (k, v) in url::form_urlencoded::parse(body) {
        if k == "SAMLResponse" && !v.is_empty() {
            return Some(v.into_owned());
        }
    }
    None
}

pub async fn saml_acs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    let settings = state.db.load_settings().unwrap_or_else(|_| json!({}));
    let origin = get_saml_base_url(&headers, &uri, &settings, state.config.port);
    let ip = get_client_ip(&headers);
    let secure = should_use_secure_cookie(&headers);

    if let Some(retry_after) = check_saml_lock(&ip) {
        let msg = format!("Too many failed attempts. Try again in {}s.", retry_after);
        return Redirect::temporary(&format!(
            "{}/login?error={}",
            origin,
            url_encode_component(&msg)
        ))
        .into_response();
    }

    let stored_request_id = get_cookie(&headers, "saml_state").unwrap_or_default();
    let clear_saml_cookie = format_cookie("saml_state", "", 0, true, secure);

    let Some(raw_saml_response) = extract_saml_response_from_body(&headers, &body) else {
        record_saml_fail(&ip);
        return redirect_with_cookies(
            &format!("{}/login?error=saml_missing_response", origin),
            vec![clear_saml_cookie],
        );
    };

    if !is_saml_configured(&settings) {
        record_saml_fail(&ip);
        return redirect_with_cookies(
            &format!("{}/login?error=saml_not_configured", origin),
            vec![clear_saml_cookie],
        );
    }

    let decoded_bytes = match base64::engine::general_purpose::STANDARD.decode(
        raw_saml_response
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    ) {
        Ok(b) => b,
        Err(e) => {
            record_saml_fail(&ip);
            return redirect_with_cookies(
                &format!(
                    "{}/login?error={}",
                    origin,
                    url_encode_component(&format!("Invalid SAMLResponse base64: {}", e))
                ),
                vec![clear_saml_cookie],
            );
        }
    };

    let xml = String::from_utf8_lossy(&decoded_bytes);
    let profile = match validate_saml_response_xml(
        &xml,
        if stored_request_id.is_empty() {
            None
        } else {
            Some(stored_request_id.as_str())
        },
        &settings,
    ) {
        Ok(p) => p,
        Err(err) => {
            record_saml_fail(&ip);
            return redirect_with_cookies(
                &format!("{}/login?error={}", origin, url_encode_component(&err)),
                vec![clear_saml_cookie],
            );
        }
    };

    let saml_email = pick_saml_email(&profile, &settings);
    let mut saml_name = pick_saml_display_name(&profile, &settings);
    if saml_name.is_empty() {
        saml_name = "SAML user".to_string();
    }

    record_saml_success(&ip);

    let session_jwt = create_dashboard_jwt_with_claims(
        &state.config.jwt_secret,
        json!({
            "saml": true,
            "samlEmail": if saml_email.is_empty() { Value::Null } else { json!(saml_email) },
            "samlName": saml_name,
        }),
    );

    redirect_with_cookies(
        &format!("{}/dashboard", origin),
        vec![
            clear_saml_cookie,
            format_cookie("auth_token", &session_jwt, SESSION_MAX_AGE_SECS, true, secure),
        ],
    )
}

// ============================================================================
// 3. Auth: Reset Password
// ============================================================================

pub async fn reset_password_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !auth::verify_admin_api_token(&headers) && !is_local_request(&headers) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({ "error": "Local only: CLI token required" }),
        );
    }
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let mut settings = match state.db.load_settings() {
        Ok(s) => s,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            );
        }
    };

    if let Some(obj) = settings.as_object_mut() {
        obj.remove("password");
    }

    match state.db.save_settings(&settings) {
        Ok(()) => json_response(StatusCode::OK, json!({ "success": true })),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    }
}

// ============================================================================
// 4. Media Providers TTS Voices & /v1/audio/voices
// ============================================================================

fn lang_display_name(code: &str) -> String {
    let base = code.split(['-', '_']).next().unwrap_or(code).to_ascii_lowercase();
    match base.as_str() {
        "af" => "Afrikaans",
        "am" => "Amharic",
        "ar" => "Arabic",
        "az" => "Azerbaijani",
        "bg" => "Bulgarian",
        "bn" => "Bengali",
        "bs" => "Bosnian",
        "ca" => "Catalan",
        "cs" => "Czech",
        "cy" => "Welsh",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "en" => "English",
        "es" => "Spanish",
        "et" => "Estonian",
        "fa" => "Persian",
        "fi" => "Finnish",
        "fil" | "tl" => "Filipino",
        "fr" => "French",
        "ga" => "Irish",
        "gl" => "Galician",
        "gu" => "Gujarati",
        "he" | "iw" => "Hebrew",
        "hi" => "Hindi",
        "hr" => "Croatian",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "is" => "Icelandic",
        "it" => "Italian",
        "ja" => "Japanese",
        "jv" => "Javanese",
        "ka" => "Georgian",
        "kk" => "Kazakh",
        "km" => "Khmer",
        "kn" => "Kannada",
        "ko" => "Korean",
        "lo" => "Lao",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "mk" => "Macedonian",
        "ml" => "Malayalam",
        "mn" => "Mongolian",
        "mr" => "Marathi",
        "ms" => "Malay",
        "mt" => "Maltese",
        "my" => "Burmese",
        "nb" | "no" => "Norwegian",
        "ne" => "Nepali",
        "nl" => "Dutch",
        "pl" => "Polish",
        "ps" => "Pashto",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "si" => "Sinhala",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "so" => "Somali",
        "sq" => "Albanian",
        "sr" => "Serbian",
        "su" => "Sundanese",
        "sv" => "Swedish",
        "sw" => "Swahili",
        "ta" => "Tamil",
        "te" => "Telugu",
        "th" => "Thai",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "ur" => "Urdu",
        "uz" => "Uzbek",
        "vi" => "Vietnamese",
        "zh" => "Chinese",
        "zu" => "Zulu",
        _ => return code.to_string(),
    }
    .to_string()
}

fn country_display_name(code: &str) -> String {
    match code.to_ascii_uppercase().as_str() {
        "AE" => "United Arab Emirates",
        "AR" => "Argentina",
        "AT" => "Austria",
        "AU" => "Australia",
        "BE" => "Belgium",
        "BG" => "Bulgaria",
        "BR" => "Brazil",
        "CA" => "Canada",
        "CH" => "Switzerland",
        "CL" => "Chile",
        "CN" => "China",
        "CO" => "Colombia",
        "CZ" => "Czechia",
        "DE" => "Germany",
        "DK" => "Denmark",
        "EG" => "Egypt",
        "ES" => "Spain",
        "FI" => "Finland",
        "FR" => "France",
        "GB" => "United Kingdom",
        "GR" => "Greece",
        "HK" => "Hong Kong",
        "HU" => "Hungary",
        "ID" => "Indonesia",
        "IE" => "Ireland",
        "IL" => "Israel",
        "IN" => "India",
        "IT" => "Italy",
        "JP" => "Japan",
        "KE" => "Kenya",
        "KR" => "South Korea",
        "MX" => "Mexico",
        "MY" => "Malaysia",
        "NG" => "Nigeria",
        "NL" => "Netherlands",
        "NO" => "Norway",
        "NZ" => "New Zealand",
        "PE" => "Peru",
        "PH" => "Philippines",
        "PK" => "Pakistan",
        "PL" => "Poland",
        "PT" => "Portugal",
        "RO" => "Romania",
        "RU" => "Russia",
        "SA" => "Saudi Arabia",
        "SE" => "Sweden",
        "SG" => "Singapore",
        "TH" => "Thailand",
        "TR" => "Turkey",
        "TW" => "Taiwan",
        "UA" => "Ukraine",
        "US" => "United States",
        "VN" => "Vietnam",
        "ZA" => "South Africa",
        _ => return code.to_string(),
    }
    .to_string()
}

fn group_voices_by_lang(voices: Vec<Value>) -> Value {
    let mut by_lang_map: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for v in &voices {
        let code = v
            .get("lang")
            .and_then(|x| x.as_str())
            .unwrap_or("en")
            .to_string();
        let name = v
            .get("langName")
            .and_then(|x| x.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| lang_display_name(&code));
        by_lang_map
            .entry(code)
            .or_insert_with(|| (name, Vec::new()))
            .1
            .push(v.clone());
    }

    let mut languages: Vec<Value> = by_lang_map
        .iter()
        .map(|(code, (name, list))| {
            json!({
                "code": code,
                "name": name,
                "voices": list,
            })
        })
        .collect();
    languages.sort_by(|a, b| {
        let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        na.cmp(nb)
    });

    let mut by_lang_obj = serde_json::Map::new();
    for (code, (name, list)) in by_lang_map {
        by_lang_obj.insert(
            code.clone(),
            json!({
                "code": code,
                "name": name,
                "voices": list,
            }),
        );
    }

    json!({
        "voices": voices,
        "languages": languages,
        "byLang": Value::Object(by_lang_obj),
    })
}

async fn fetch_edge_tts_voices_from_url(
    client: &reqwest::Client,
    url: &str,
    use_cache: bool,
) -> Result<Vec<Value>, String> {
    if use_cache {
        let cache = EDGE_VOICES_CACHE.get_or_init(|| Mutex::new(None));
        if let Ok(guard) = cache.lock() {
            if let Some((ts, ref cached)) = *guard {
                if ts.elapsed() < VOICES_CACHE_TTL {
                    return Ok(cached.clone());
                }
            }
        }
    }

    let res = client
        .get(url)
        .header(header::USER_AGENT, EDGE_TTS_UA)
        .send()
        .await
        .map_err(|e| format!("Edge TTS voices fetch failed: {}", e))?;
    if !res.status().is_success() {
        return Err(format!("Edge TTS voices fetch failed: {}", res.status().as_u16()));
    }
    let raw: Vec<Value> = res
        .json()
        .await
        .map_err(|e| format!("Invalid Edge TTS voices JSON: {}", e))?;

    if use_cache {
        let cache = EDGE_VOICES_CACHE.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((Instant::now(), raw.clone()));
        }
    }
    Ok(raw)
}

async fn fetch_local_device_voices() -> Vec<Value> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = tokio::process::Command::new("say")
            .args(["-v", "?"])
            .output()
            .await
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut voices = Vec::new();
                for line in text.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if let Some(locale_idx) = parts.iter().position(|p| p.len() == 5 && p.as_bytes()[2] == b'_') {
                        let name = parts[..locale_idx].join(" ");
                        let locale = parts[locale_idx];
                        let (lang, country) = locale.split_once('_').unwrap_or((locale, ""));
                        if !name.is_empty() {
                            voices.push(json!({
                                "id": name,
                                "name": name,
                                "locale": locale,
                                "lang": lang,
                                "country": country,
                                "gender": "",
                            }));
                        }
                    }
                }
                return voices;
            }
        }
        return Vec::new();
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

async fn fetch_elevenlabs_voices_from_url(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    use_cache: bool,
) -> Result<Vec<Value>, String> {
    if api_key.trim().is_empty() {
        return Err("ElevenLabs API key required".to_string());
    }

    if use_cache {
        let cache = ELEVENLABS_VOICES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock() {
            if let Some((ts, cached)) = guard.get(api_key) {
                if ts.elapsed() < VOICES_CACHE_TTL {
                    return Ok(cached.clone());
                }
            }
        }
    }

    let res = client
        .get(url)
        .header("xi-api-key", api_key)
        .header(header::CONTENT_TYPE, "application/json")
        .send()
        .await
        .map_err(|e| format!("ElevenLabs voices fetch failed: {}", e))?;
    if !res.status().is_success() {
        return Err(format!(
            "ElevenLabs voices fetch failed: {}",
            res.status().as_u16()
        ));
    }
    let data: Value = res
        .json()
        .await
        .map_err(|e| format!("Invalid ElevenLabs voices JSON: {}", e))?;
    let voices = data
        .get("voices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if use_cache {
        let cache = ELEVENLABS_VOICES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(mut guard) = cache.lock() {
            guard.insert(api_key.to_string(), (Instant::now(), voices.clone()));
        }
    }

    Ok(voices)
}

async fn list_tts_voices_with_urls(
    client: &reqwest::Client,
    provider: &str,
    lang_filter: Option<&str>,
    api_key: Option<&str>,
    edge_url: &str,
    elevenlabs_url: &str,
    use_cache: bool,
) -> Result<Value, (StatusCode, String)> {
    let mut normalized: Vec<Value> = match provider {
        "edge-tts" => {
            let raw = fetch_edge_tts_voices_from_url(client, edge_url, use_cache)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
            raw.into_iter()
                .map(|v| {
                    let short_name = v.get("ShortName").and_then(|x| x.as_str()).unwrap_or("");
                    let friendly = v
                        .get("FriendlyName")
                        .and_then(|x| x.as_str())
                        .unwrap_or(short_name);
                    let locale = v.get("Locale").and_then(|x| x.as_str()).unwrap_or("en-US");
                    let mut parts = locale.split('-');
                    let lang = parts.next().unwrap_or("en");
                    let country = parts.next().unwrap_or("");
                    let clean_name = friendly
                        .replace("Microsoft ", "")
                        .replace(" Online (Natural) - ", " (");
                    json!({
                        "id": short_name,
                        "name": clean_name,
                        "locale": locale,
                        "lang": lang,
                        "country": country,
                        "countryName": country_display_name(if country.is_empty() { lang } else { country }),
                        "langName": lang_display_name(lang),
                        "gender": v.get("Gender").and_then(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        }
        "local-device" => {
            let raw = fetch_local_device_voices().await;
            raw.into_iter()
                .map(|v| {
                    let locale = v
                        .get("locale")
                        .and_then(|x| x.as_str())
                        .unwrap_or("en_US")
                        .replace('_', "-");
                    let lang = v.get("lang").and_then(|x| x.as_str()).unwrap_or("en");
                    let country = v.get("country").and_then(|x| x.as_str()).unwrap_or("");
                    json!({
                        "id": v.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                        "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                        "locale": locale,
                        "lang": lang,
                        "country": country,
                        "countryName": country_display_name(country),
                        "langName": lang_display_name(lang),
                        "gender": v.get("gender").and_then(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        }
        "elevenlabs" => {
            let key = api_key.unwrap_or("");
            let raw = fetch_elevenlabs_voices_from_url(client, elevenlabs_url, key, use_cache)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
            raw.into_iter()
                .map(|v| {
                    let labels = v.get("labels");
                    let full_lang = labels
                        .and_then(|l| l.get("language"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("en");
                    let lang = full_lang.split('-').next().unwrap_or("en");
                    json!({
                        "id": v.get("voice_id").and_then(|x| x.as_str()).unwrap_or(""),
                        "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                        "locale": full_lang,
                        "lang": lang,
                        "country": "",
                        "countryName": "",
                        "langName": lang_display_name(lang),
                        "gender": labels.and_then(|l| l.get("gender")).and_then(|x| x.as_str()).unwrap_or(""),
                        "category": v.get("category").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        }
        "gemini" => PREBUILT_GEMINI_VOICES
            .iter()
            .map(|(id, lang, gender)| {
                json!({
                    "id": id,
                    "name": id,
                    "locale": lang,
                    "lang": lang,
                    "country": "",
                    "countryName": "",
                    "langName": lang_display_name(lang),
                    "gender": gender,
                })
            })
            .collect(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Provider '{}' does not support voice listing", provider),
            ));
        }
    };

    if let Some(lang) = lang_filter.filter(|s| !s.is_empty()) {
        normalized.retain(|v| v.get("lang").and_then(|x| x.as_str()) == Some(lang));
    }

    Ok(group_voices_by_lang(normalized))
}

#[derive(Debug, Deserialize, Default)]
pub struct TtsVoicesQuery {
    pub provider: Option<String>,
    pub lang: Option<String>,
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
}

pub async fn tts_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TtsVoicesQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let provider = query.provider.as_deref().unwrap_or("edge-tts");
    match list_tts_voices_with_urls(
        state.proxy.client(),
        provider,
        query.lang.as_deref(),
        query.api_key.as_deref(),
        EDGE_TTS_VOICES_URL,
        ELEVENLABS_VOICES_URL,
        true,
    )
    .await
    {
        Ok(data) => json_response(StatusCode::OK, data),
        Err((status, msg)) => json_response(status, json!({ "error": msg })),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct LangQuery {
    pub lang: Option<String>,
}

fn format_lang_grouped_response(
    by_lang: BTreeMap<String, (String, Vec<Value>)>,
    lang_filter: Option<&str>,
) -> Value {
    if let Some(lang) = lang_filter.filter(|s| !s.is_empty()) {
        let voices = by_lang
            .get(lang)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        return json!({ "voices": voices });
    }

    let mut languages: Vec<Value> = by_lang
        .iter()
        .map(|(code, (name, list))| json!({ "code": code, "name": name, "voices": list }))
        .collect();
    languages.sort_by(|a, b| {
        let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        na.cmp(nb)
    });

    let mut by_lang_obj = serde_json::Map::new();
    for (code, (name, list)) in by_lang {
        by_lang_obj.insert(
            code.clone(),
            json!({ "code": code, "name": name, "voices": list }),
        );
    }

    json!({
        "languages": languages,
        "byLang": Value::Object(by_lang_obj),
    })
}

async fn fetch_deepgram_grouped_voices(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    lang_filter: Option<&str>,
) -> Result<Value, (StatusCode, String)> {
    let res = client
        .get(url)
        .header(header::AUTHORIZATION, format!("Token {}", api_key))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Deepgram API failed: {}", e)))?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        let msg = if text.is_empty() {
            "Failed".to_string()
        } else {
            text
        };
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Deepgram API {}: {}", status, msg),
        ));
    }

    let data: Value = res
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Invalid Deepgram JSON: {}", e)))?;
    let tts_models = data
        .get("tts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut by_lang: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for m in tts_models {
        let canonical = m
            .get("canonical_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let m_name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let voice_id = if canonical.is_empty() { m_name } else { canonical };
        if voice_id.is_empty() {
            continue;
        }
        let langs: Vec<String> = match m.get("languages").and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect(),
            _ => vec![canonical
                .rsplit('-')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("en")
                .to_string()],
        };
        let gender = m
            .get("metadata")
            .and_then(|md| md.get("tags"))
            .and_then(|t| t.as_array())
            .and_then(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .find(|t| *t == "masculine" || *t == "feminine")
            })
            .unwrap_or("");

        for code in langs {
            let entry = by_lang
                .entry(code.clone())
                .or_insert_with(|| (lang_display_name(&code), Vec::new()));
            if !entry
                .1
                .iter()
                .any(|x| x.get("id").and_then(|v| v.as_str()) == Some(voice_id))
            {
                entry.1.push(json!({
                    "id": voice_id,
                    "name": if m_name.is_empty() { voice_id } else { m_name },
                    "gender": gender,
                    "lang": code,
                }));
            }
        }
    }

    Ok(format_lang_grouped_response(by_lang, lang_filter))
}

async fn fetch_elevenlabs_grouped_voices(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    lang_filter: Option<&str>,
    use_cache: bool,
) -> Result<Value, (StatusCode, String)> {
    let voices = fetch_elevenlabs_voices_from_url(client, url, api_key, use_cache)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    let mut by_lang: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    let mut add_to_lang = |code: &str, v: &Value| {
        let entry = by_lang
            .entry(code.to_string())
            .or_insert_with(|| (lang_display_name(code), Vec::new()));
        let vid = v.get("voice_id").and_then(|x| x.as_str()).unwrap_or("");
        if vid.is_empty()
            || entry
                .1
                .iter()
                .any(|x| x.get("id").and_then(|y| y.as_str()) == Some(vid))
        {
            return;
        }
        let category = v.get("category").and_then(|x| x.as_str()).unwrap_or("");
        let is_owner = v.get("is_owner").and_then(|x| x.as_bool()).unwrap_or(false);
        entry.1.push(json!({
            "id": vid,
            "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(vid),
            "gender": v.get("labels").and_then(|l| l.get("gender")).and_then(|x| x.as_str()).unwrap_or(""),
            "lang": code,
            "free_users_allowed": category == "premade" || is_owner,
        }));
    };

    for v in &voices {
        let primary = v
            .get("labels")
            .and_then(|l| l.get("language"))
            .and_then(|x| x.as_str())
            .unwrap_or("en");
        add_to_lang(primary, v);
        if let Some(verified) = v.get("verified_languages").and_then(|x| x.as_array()) {
            for vl in verified {
                if let Some(lang) = vl.get("language").and_then(|x| x.as_str()) {
                    if !lang.is_empty() && lang != primary {
                        add_to_lang(lang, v);
                    }
                }
            }
        }
    }

    Ok(format_lang_grouped_response(by_lang, lang_filter))
}

async fn fetch_inworld_grouped_voices(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    lang_filter: Option<&str>,
) -> Result<Value, (StatusCode, String)> {
    let res = client
        .get(url)
        .header(header::AUTHORIZATION, format!("Basic {}", api_key))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Inworld API failed: {}", e)))?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        let msg = if text.is_empty() {
            "Failed".to_string()
        } else {
            text
        };
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Inworld API {}: {}", status, msg),
        ));
    }

    let data: Value = res
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Invalid Inworld JSON: {}", e)))?;
    let voices = data
        .get("voices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut by_lang: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for v in voices {
        let voice_id = v.get("voiceId").and_then(|x| x.as_str()).unwrap_or("");
        if voice_id.is_empty() {
            continue;
        }
        let langs: Vec<String> = match v.get("languages").and_then(|x| x.as_array()) {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect(),
            _ => vec!["en".to_string()],
        };
        let display_name = v
            .get("displayName")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(voice_id);
        let gender = v.get("gender").and_then(|x| x.as_str()).unwrap_or("");

        for code in langs {
            let entry = by_lang
                .entry(code.clone())
                .or_insert_with(|| (lang_display_name(&code), Vec::new()));
            if !entry
                .1
                .iter()
                .any(|x| x.get("id").and_then(|y| y.as_str()) == Some(voice_id))
            {
                entry.1.push(json!({
                    "id": voice_id,
                    "name": display_name,
                    "gender": gender,
                    "lang": code,
                }));
            }
        }
    }

    Ok(format_lang_grouped_response(by_lang, lang_filter))
}

fn minimax_infer_language(voice_id: &str) -> String {
    let trimmed = voice_id.trim();
    if let Some((prefix, _)) = trimmed.split_once('_') {
        if !prefix.is_empty() {
            return prefix.to_string();
        }
    }
    "Custom".to_string()
}

pub fn normalize_minimax_voices(data: &Value) -> Value {
    const GROUPS: &[(&str, &str)] = &[
        ("system_voice", "System"),
        ("voice_cloning", "Cloned"),
        ("voice_generation", "Generated"),
        ("music_generation", "Music"),
    ];

    let mut by_lang: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for &(group_key, group_label) in GROUPS {
        let Some(items) = data.get(group_key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let voice_id = item
                .get("voice_id")
                .or_else(|| item.get("voiceId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if voice_id.is_empty() {
                continue;
            }
            let voice_name = item
                .get("voice_name")
                .or_else(|| item.get("voiceName"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(voice_id);
            let lang = if group_key == "system_voice" {
                minimax_infer_language(voice_id)
            } else {
                "Custom".to_string()
            };
            let display = if group_key == "system_voice" {
                voice_name.to_string()
            } else {
                format!("{} · {}", voice_name, group_label)
            };
            let list = by_lang.entry(lang.clone()).or_default();
            if !list
                .iter()
                .any(|x| x.get("id").and_then(|y| y.as_str()) == Some(voice_id))
            {
                list.push(json!({
                    "id": voice_id,
                    "name": display,
                    "lang": lang,
                    "category": group_key,
                }));
            }
        }
    }

    for list in by_lang.values_mut() {
        list.sort_by(|a, b| {
            let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            na.cmp(nb)
        });
    }

    let mut languages: Vec<Value> = by_lang
        .iter()
        .map(|(code, voices)| json!({ "code": code, "name": code, "voices": voices }))
        .collect();
    languages.sort_by(|a, b| {
        let ca = a.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let cb = b.get("code").and_then(|v| v.as_str()).unwrap_or("");
        match (ca == "Custom", cb == "Custom") {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => ca.cmp(cb),
        }
    });

    let mut by_lang_obj = serde_json::Map::new();
    for (code, voices) in by_lang {
        by_lang_obj.insert(
            code.clone(),
            json!({ "code": code, "name": code, "voices": voices }),
        );
    }

    json!({
        "languages": languages,
        "byLang": Value::Object(by_lang_obj),
    })
}

async fn fetch_minimax_grouped_voices(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    voice_type: &str,
    lang_filter: Option<&str>,
) -> Result<Value, (StatusCode, String)> {
    let res = client
        .post(url)
        .bearer_auth(api_key)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({ "voice_type": voice_type }))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("MiniMax API failed: {}", e)))?;

    let status = res.status();
    let raw_text = res.text().await.unwrap_or_default();
    let data: Value = if raw_text.is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw_text).unwrap_or_else(|_| json!({}))
    };

    let base_resp = data
        .get("base_resp")
        .or_else(|| data.get("baseResp"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let status_code = base_resp
        .get("status_code")
        .or_else(|| base_resp.get("statusCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let status_msg = base_resp
        .get("status_msg")
        .or_else(|| base_resp.get("statusMsg"))
        .or_else(|| data.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !status.is_success() {
        let detail = if !status_msg.is_empty() {
            status_msg
        } else if !raw_text.is_empty() {
            &raw_text
        } else {
            "Failed"
        };
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("MiniMax API {}: {}", status.as_u16(), detail),
        ));
    }
    if status_code != 0 {
        let msg = if status_msg.is_empty() {
            "MiniMax voice API error"
        } else {
            status_msg
        };
        return Err((StatusCode::BAD_GATEWAY, msg.to_string()));
    }

    let normalized = normalize_minimax_voices(&data);
    if let Some(lang) = lang_filter.filter(|s| !s.is_empty()) {
        let voices = normalized
            .get("byLang")
            .and_then(|b| b.get(lang))
            .and_then(|g| g.get("voices"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        return Ok(json!({ "voices": voices }));
    }
    Ok(normalized)
}

fn first_active_provider_api_key(db: &crate::db::Database, provider: &str) -> Option<String> {
    let conns = db.get_active_provider_connections(Some(provider)).ok()?;
    conns
        .into_iter()
        .find_map(|c| c.data.api_key.filter(|k| !k.trim().is_empty()))
}

pub async fn deepgram_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(api_key) = first_active_provider_api_key(&state.db, "deepgram") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "No Deepgram connection found" }),
        );
    };
    match fetch_deepgram_grouped_voices(
        state.proxy.client(),
        DEEPGRAM_MODELS_URL,
        &api_key,
        query.lang.as_deref(),
    )
    .await
    {
        Ok(data) => json_response(StatusCode::OK, data),
        Err((status, msg)) => json_response(status, json!({ "error": msg })),
    }
}

pub async fn elevenlabs_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(api_key) = first_active_provider_api_key(&state.db, "elevenlabs") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "No ElevenLabs connection found" }),
        );
    };
    match fetch_elevenlabs_grouped_voices(
        state.proxy.client(),
        ELEVENLABS_VOICES_URL,
        &api_key,
        query.lang.as_deref(),
        true,
    )
    .await
    {
        Ok(data) => json_response(StatusCode::OK, data),
        Err((status, msg)) => json_response(status, json!({ "error": msg })),
    }
}

pub async fn inworld_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LangQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(api_key) = first_active_provider_api_key(&state.db, "inworld") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "No Inworld connection found" }),
        );
    };
    match fetch_inworld_grouped_voices(
        state.proxy.client(),
        INWORLD_VOICES_URL,
        &api_key,
        query.lang.as_deref(),
    )
    .await
    {
        Ok(data) => json_response(StatusCode::OK, data),
        Err((status, msg)) => json_response(status, json!({ "error": msg })),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct MinimaxVoicesQuery {
    pub provider: Option<String>,
    pub voice_type: Option<String>,
    pub lang: Option<String>,
}

pub async fn minimax_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MinimaxVoicesQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider = if query.provider.as_deref() == Some("minimax-cn") {
        "minimax-cn"
    } else {
        "minimax"
    };
    let voice_type = query.voice_type.as_deref().unwrap_or("all");

    let Some(api_key) = first_active_provider_api_key(&state.db, provider) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("No {} connection found", provider) }),
        );
    };
    let url = if provider == "minimax-cn" {
        MINIMAX_VOICES_URL_CN
    } else {
        MINIMAX_VOICES_URL_GLOBAL
    };
    match fetch_minimax_grouped_voices(
        state.proxy.client(),
        url,
        &api_key,
        voice_type,
        query.lang.as_deref(),
    )
    .await
    {
        Ok(data) => json_response(StatusCode::OK, data),
        Err((status, msg)) => json_response(status, json!({ "error": msg })),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct PublicVoicesQuery {
    pub provider: Option<String>,
    pub lang: Option<String>,
}

pub async fn v1_audio_voices_options_handler() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
        ],
    )
}

fn tts_provider_alias(provider: &str) -> &str {
    match provider {
        "elevenlabs" => "el",
        "deepgram" => "dg",
        "inworld" => "inworld",
        "edge-tts" => "edge-tts",
        "local-device" => "local-device",
        _ => provider,
    }
}

pub async fn v1_audio_voices_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PublicVoicesQuery>,
) -> Response {
    if let Err(resp) = audio::check_audio_auth(&headers, &state) {
        return resp;
    }

    let provider = query.provider.as_deref().unwrap_or("");
    let supported = ["elevenlabs", "deepgram", "inworld", "edge-tts", "local-device"];
    if !supported.contains(&provider) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            Json(json!({
                "error": {
                    "message": format!("provider must be one of: {}", supported.join(", ")),
                    "type": "invalid_request_error"
                }
            })),
        )
            .into_response();
    }

    let lang = query.lang.as_deref();
    let result: Result<Value, (StatusCode, String)> = match provider {
        "edge-tts" | "local-device" => {
            list_tts_voices_with_urls(
                state.proxy.client(),
                provider,
                lang,
                None,
                EDGE_TTS_VOICES_URL,
                ELEVENLABS_VOICES_URL,
                true,
            )
            .await
        }
        "elevenlabs" => match first_active_provider_api_key(&state.db, "elevenlabs") {
            Some(key) => {
                fetch_elevenlabs_grouped_voices(
                    state.proxy.client(),
                    ELEVENLABS_VOICES_URL,
                    &key,
                    lang,
                    true,
                )
                .await
            }
            None => Err((
                StatusCode::BAD_REQUEST,
                "No ElevenLabs connection found".to_string(),
            )),
        },
        "deepgram" => match first_active_provider_api_key(&state.db, "deepgram") {
            Some(key) => {
                fetch_deepgram_grouped_voices(
                    state.proxy.client(),
                    DEEPGRAM_MODELS_URL,
                    &key,
                    lang,
                )
                .await
            }
            None => Err((
                StatusCode::BAD_REQUEST,
                "No Deepgram connection found".to_string(),
            )),
        },
        "inworld" => match first_active_provider_api_key(&state.db, "inworld") {
            Some(key) => {
                fetch_inworld_grouped_voices(
                    state.proxy.client(),
                    INWORLD_VOICES_URL,
                    &key,
                    lang,
                )
                .await
            }
            None => Err((
                StatusCode::BAD_REQUEST,
                "No Inworld connection found".to_string(),
            )),
        },
        _ => unreachable!(),
    };

    match result {
        Ok(data) => {
            let raw_voices: Vec<Value> = if lang.filter(|s| !s.is_empty()).is_some() {
                data.get("voices")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
            } else {
                data.get("byLang")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        obj.values()
                            .flat_map(|g| {
                                g.get("voices")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default()
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };

            let alias = tts_provider_alias(provider);
            let out: Vec<Value> = raw_voices
                .into_iter()
                .map(|v| {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    json!({
                        "id": id,
                        "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(id),
                        "lang": v.get("lang").and_then(|x| x.as_str()).unwrap_or(""),
                        "gender": v.get("gender").and_then(|x| x.as_str()).unwrap_or(""),
                        "model": format!("{}/{}", alias, id),
                    })
                })
                .collect();

            (
                StatusCode::OK,
                [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
                Json(json!({
                    "object": "list",
                    "data": out,
                })),
            )
                .into_response()
        }
        Err((status, msg)) => {
            let err_type = if status.is_client_error() {
                "invalid_request_error"
            } else {
                "server_error"
            };
            (
                status,
                [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
                Json(json!({
                    "error": {
                        "message": msg,
                        "type": err_type,
                    }
                })),
            )
                .into_response()
        }
    }
}

// ============================================================================
// 5. Misc Endpoints: tags, locale, init, cron/flush, shutdown, version/update
// ============================================================================

pub async fn tags_options_handler() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
    )
}

pub async fn tags_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
        OLLAMA_TAGS_JSON,
    )
        .into_response()
}

pub fn normalize_locale(locale: &str) -> String {
    if locale == "zh" || locale == "zh-CN" {
        return "zh-CN".to_string();
    }
    if SUPPORTED_LOCALES.contains(&locale) {
        return locale.to_string();
    }
    "en".to_string()
}

pub async fn locale_handler(body: axum::body::Bytes) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "Failed to set locale" }),
            );
        }
    };

    let Some(locale) = payload.get("locale").and_then(|v| v.as_str()) else {
        return json_response(StatusCode::BAD_REQUEST, json!({ "error": "Invalid locale" }));
    };

    if !SUPPORTED_LOCALES.contains(&locale) {
        return json_response(StatusCode::BAD_REQUEST, json!({ "error": "Invalid locale" }));
    }

    let normalized = normalize_locale(locale);
    let cookie = format!(
        "locale={}; Path=/; Max-Age={}",
        normalized, LOCALE_COOKIE_MAX_AGE_SECS
    );

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::SET_COOKIE, cookie),
        ],
        Json(json!({ "success": true, "locale": normalized })),
    )
        .into_response()
}

pub async fn init_handler() -> impl IntoResponse {
    (StatusCode::OK, "Initialized")
}

fn is_cron_authorized(headers: &HeaderMap) -> bool {
    let Ok(secret) = std::env::var("CRON_SECRET") else {
        return true;
    };
    if secret.is_empty() {
        return true;
    }

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-cron-secret").and_then(|v| v.to_str().ok()))
        .unwrap_or("");

    auth::constant_time_eq(token.as_bytes(), secret.as_bytes())
}

pub async fn cron_flush_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !is_cron_authorized(&headers) {
        return json_response(StatusCode::UNAUTHORIZED, json!({ "error": "Unauthorized" }));
    }

    let buffering = state
        .db
        .load_settings()
        .ok()
        .and_then(|s| {
            s.get("lowWriteMode")
                .or_else(|| s.get("analyticsBuffering"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false);

    match state.db.flush_write_behind() {
        Ok(()) => json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "buffering": buffering,
                "analytics": { "before": 0, "after": 0 },
                "detailsFlushed": 0,
            }),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "ok": false, "error": e.to_string() }),
        ),
    }
}

pub async fn dev_shutdown_handler(headers: HeaderMap) -> Response {
    if std::env::var("NODE_ENV").as_deref() == Ok("production") {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({ "success": false, "message": "Not allowed in production" }),
        );
    }

    let secret = std::env::var("SHUTDOWN_SECRET").unwrap_or_default();
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");

    if secret.is_empty() || !auth::constant_time_eq(provided.as_bytes(), secret.as_bytes()) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({ "success": false, "message": "Unauthorized" }),
        );
    }

    #[cfg(not(test))]
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::process::exit(0);
    });

    json_response(
        StatusCode::OK,
        json!({ "success": true, "message": "Shutting down..." }),
    )
}

pub async fn version_update_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !verify_always_protected_auth(&headers, &state.config.jwt_secret) {
        return json_response(StatusCode::UNAUTHORIZED, json!({ "error": "Unauthorized" }));
    }

    if std::env::var("NODE_ENV").as_deref() != Ok("production") {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({
                "success": false,
                "message": "Update is only available in production build (9router CLI)"
            }),
        );
    }

    #[cfg(not(test))]
    {
        // Best-effort updater launch when running outside unit tests
        let updater_path = std::env::var("UPDATER_SCRIPT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("src/lib/updater/updater.js"));
        if updater_path.exists() {
            let _ = std::process::Command::new("node")
                .arg(&updater_path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            std::process::exit(0);
        });
    }

    json_response(
        StatusCode::OK,
        json!({
            "success": true,
            "message": "Updater started. This app will exit shortly."
        }),
    )
}

// ============================================================================
// 6. Translator Playground Helpers (/api/translator/{load,save,send,translate})
// ============================================================================

fn resolve_translator_logs_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NINEROUTER_TRANSLATOR_LOGS_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("logs")
        .join("translator")
}

fn is_allowed_translator_file(file: &str) -> bool {
    TRANSLATOR_ALLOWED_FILES.contains(&file)
}

#[derive(Debug, Deserialize, Default)]
pub struct TranslatorLoadQuery {
    pub file: Option<String>,
}

pub async fn translator_load_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TranslatorLoadQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let Some(file) = query.file.as_deref().filter(|s| !s.is_empty()) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "File parameter required" }),
        );
    };

    if !is_allowed_translator_file(file) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid file name" }),
        );
    }

    let path = resolve_translator_logs_dir().join(file);
    if !path.exists() {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "success": false, "error": "File not found" }),
        );
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => json_response(
            StatusCode::OK,
            json!({ "success": true, "content": content }),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "success": false, "error": e.to_string() }),
        ),
    }
}

pub async fn translator_save_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let file = payload.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let content = payload.get("content").and_then(|v| v.as_str());
    if file.is_empty() || content.is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "File and content required" }),
        );
    }

    if !is_allowed_translator_file(file) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid file name" }),
        );
    }

    let dir = resolve_translator_logs_dir();
    if let Err(e) = save_translator_file_in_dir(&dir, file, content.unwrap_or_default()) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "success": false, "error": e }),
        );
    }

    json_response(StatusCode::OK, json!({ "success": true }))
}

fn save_translator_file_in_dir(dir: &Path, file: &str, content: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(file), content).map_err(|e| e.to_string())
}

fn detect_source_format(body: &Value) -> &'static str {
    if body.get("contents").and_then(|v| v.as_array()).is_some() {
        return "gemini";
    }
    if body.get("input").is_some() && body.get("messages").is_none() {
        return "openai-responses";
    }
    if body.get("anthropic_version").is_some()
        || (body.get("system").is_some() && body.get("messages").is_some())
    {
        return "claude";
    }
    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        let has_claude_blocks = msgs.iter().any(|m| {
            m.get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter().any(|b| {
                        matches!(
                            b.get("type").and_then(|t| t.as_str()),
                            Some("tool_use" | "tool_result" | "thinking")
                        )
                    })
                })
                .unwrap_or(false)
        });
        if has_claude_blocks {
            return "claude";
        }
    }
    "openai"
}

fn target_format_for_provider(provider: &str) -> &'static str {
    let p = provider.to_ascii_lowercase();
    if p == "claude" || p == "anthropic" || p.starts_with("anthropic-compatible") {
        "claude"
    } else if p == "gemini" || p == "gemini-cli" || p == "google" || p == "vertex" {
        "gemini"
    } else {
        "openai"
    }
}

fn resolve_translator_provider_model(db: &crate::db::Database, raw_model: &str) -> (String, String) {
    let aliases = db.load_model_aliases().unwrap_or_default();
    let candidate = aliases
        .get(raw_model)
        .map(String::as_str)
        .unwrap_or(raw_model);

    let parsed = proxy::parse_provider_and_model(candidate);
    if let Some(prov) = parsed.provider {
        return (prov, parsed.model);
    }

    let lower = candidate.to_ascii_lowercase();
    let inferred = if lower.starts_with("claude") {
        "claude"
    } else if lower.contains("gemini") {
        "gemini"
    } else {
        "openai"
    };
    (inferred.to_string(), candidate.to_string())
}

fn build_provider_target_url_and_headers(
    conn: &ProviderConnection,
    provider: &str,
    model: &str,
    stream: bool,
) -> (String, serde_json::Map<String, Value>) {
    let mut hdrs = serde_json::Map::new();
    hdrs.insert("Content-Type".to_string(), json!("application/json"));
    if stream {
        hdrs.insert("Accept".to_string(), json!("text/event-stream"));
    }

    let p = provider.to_ascii_lowercase();
    let base_url = conn.data.base_url.as_deref().unwrap_or("");
    let url = if !base_url.is_empty() {
        if p == "claude" || p == "anthropic" {
            let b = base_url.trim_end_matches('/');
            if b.ends_with("/messages") {
                b.to_string()
            } else {
                format!("{}/messages", b)
            }
        } else {
            proxy::build_chat_completions_url(base_url, model)
        }
    } else if p == "claude" || p == "anthropic" {
        "https://api.anthropic.com/v1/messages".to_string()
    } else if p == "groq" {
        "https://api.groq.com/openai/v1/chat/completions".to_string()
    } else if p == "openrouter" {
        "https://openrouter.ai/api/v1/chat/completions".to_string()
    } else if p == "deepseek" {
        "https://api.deepseek.com/v1/chat/completions".to_string()
    } else if p == "gemini" || p == "google" {
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:{}",
            model, action
        )
    } else {
        "https://api.openai.com/v1/chat/completions".to_string()
    };

    let access_token = token_refresh::extract_access_token(conn);
    if p == "claude" || p == "anthropic" {
        hdrs.insert("anthropic-version".to_string(), json!("2023-06-01"));
        if let Some(ref k) = conn.data.api_key {
            hdrs.insert("x-api-key".to_string(), json!(k));
        } else if let Some(ref tok) = access_token {
            hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", tok)));
        }
    } else if p == "gemini" || p == "google" {
        if let Some(ref k) = conn.data.api_key {
            hdrs.insert("x-goog-api-key".to_string(), json!(k));
        } else if let Some(ref tok) = access_token {
            hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", tok)));
        }
    } else if let Some(ref k) = conn.data.api_key {
        hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", k)));
    } else if let Some(ref tok) = access_token {
        hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", tok)));
    }

    if let Some(ref custom) = conn.data.headers {
        for (k, v) in custom {
            hdrs.insert(k.clone(), json!(v));
        }
    }

    (url, hdrs)
}

pub async fn translator_translate_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let step = payload.get("step").and_then(|v| v.as_i64()).unwrap_or(0);
    let Some(body) = payload.get("body") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Step and body required" }),
        );
    };
    if step == 0 {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Step and body required" }),
        );
    }

    match step {
        1 => {
            let client_body = body.get("body").unwrap_or(body);
            let raw_model = client_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (provider, model) = resolve_translator_provider_model(&state.db, raw_model);
            let source_format = detect_source_format(client_body);
            let target_format = target_format_for_provider(&provider);
            json_response(
                StatusCode::OK,
                json!({
                    "success": true,
                    "result": {
                        "provider": provider,
                        "model": model,
                        "sourceFormat": source_format,
                        "targetFormat": target_format,
                    }
                }),
            )
        }
        2 => {
            let client_body = body.get("body").unwrap_or(body);
            let raw_model = client_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (_provider, model) = resolve_translator_provider_model(&state.db, raw_model);
            let source_format = detect_source_format(client_body);
            let stream = client_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let mut openai_val = match source_format {
                "claude" => {
                    let mut normalized = client_body.clone();
                    if let Some(obj) = normalized.as_object_mut() {
                        if !obj.contains_key("max_tokens") {
                            obj.insert("max_tokens".to_string(), json!(4096));
                        }
                    }
                    match serde_json::from_value::<ClaudeMessageRequest>(normalized) {
                    Ok(mut claude_req) => {
                        if !model.is_empty() {
                            claude_req.model = model;
                        }
                        claude_req.stream = Some(stream);
                        let chat = translator::claude_to_openai_request(&claude_req);
                        serde_json::to_value(chat).unwrap_or_else(|_| client_body.clone())
                    }
                    Err(e) => {
                        return json_response(
                            StatusCode::BAD_REQUEST,
                            json!({ "success": false, "error": format!("Invalid Claude request: {}", e) }),
                        );
                    }
                    }
                }
                "openai-responses" => {
                    match serde_json::from_value::<ResponsesRequest>(client_body.clone()) {
                        Ok(mut resp_req) => {
                            if !model.is_empty() {
                                resp_req.model = model;
                            }
                            responses::normalize_responses_request(&mut serde_json::to_value(&resp_req).unwrap_or_default());
                            match responses::responses_to_chat_request(&resp_req) {
                                Ok((mut chat, _)) => {
                                    chat.stream = Some(stream);
                                    serde_json::to_value(chat).unwrap_or_else(|_| client_body.clone())
                                }
                                Err(e) => {
                                    return json_response(
                                        StatusCode::BAD_REQUEST,
                                        json!({ "success": false, "error": e }),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            return json_response(
                                StatusCode::BAD_REQUEST,
                                json!({ "success": false, "error": format!("Invalid Responses request: {}", e) }),
                            );
                        }
                    }
                }
                _ => {
                    let mut v = client_body.clone();
                    if let Some(obj) = v.as_object_mut() {
                        if !model.is_empty() {
                            obj.insert("model".to_string(), json!(model));
                        }
                        obj.insert("stream".to_string(), json!(stream));
                    }
                    v
                }
            };

            if let Some(obj) = openai_val.as_object_mut() {
                obj.remove("_toolNameMap");
            }

            json_response(
                StatusCode::OK,
                json!({
                    "success": true,
                    "result": { "body": openai_val }
                }),
            )
        }
        3 => {
            let openai_body = body.get("body").unwrap_or(body);
            let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if provider.is_empty() || model.is_empty() {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({ "success": false, "error": "provider and model required" }),
                );
            }

            let conns = state
                .db
                .get_active_provider_connections(Some(provider))
                .unwrap_or_default();
            let Some(conn) = conns.first() else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({
                        "success": false,
                        "error": format!("No active connection for provider: {}", provider)
                    }),
                );
            };

            let target_format = target_format_for_provider(provider);
            let stream = openai_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let final_body = if target_format == "claude" {
                match serde_json::from_value::<ChatCompletionRequest>(openai_body.clone()) {
                    Ok(mut chat_req) => {
                        chat_req.model = model.to_string();
                        chat_req.stream = Some(stream);
                        let claude_req = translator::openai_to_claude_request(&chat_req);
                        serde_json::to_value(claude_req).unwrap_or_else(|_| openai_body.clone())
                    }
                    Err(e) => {
                        return json_response(
                            StatusCode::BAD_REQUEST,
                            json!({ "success": false, "error": format!("Invalid OpenAI request: {}", e) }),
                        );
                    }
                }
            } else {
                let mut v = openai_body.clone();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("model".to_string(), json!(model));
                    obj.insert("stream".to_string(), json!(stream));
                }
                v
            };

            let (url, hdrs) = build_provider_target_url_and_headers(conn, provider, model, stream);
            json_response(
                StatusCode::OK,
                json!({
                    "success": true,
                    "result": {
                        "url": url,
                        "headers": Value::Object(hdrs),
                        "body": final_body,
                    }
                }),
            )
        }
        _ => json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid step (1-3)" }),
        ),
    }
}

pub async fn translator_send_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let Some(body) = payload.get("body") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "provider, model, and body required" }),
        );
    };
    if provider.is_empty() || model.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "provider, model, and body required" }),
        );
    }

    let conns = state
        .db
        .get_active_provider_connections(Some(provider))
        .unwrap_or_default();
    let Some(mut conn) = conns.into_iter().next() else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({
                "success": false,
                "error": format!("No active connection for provider: {}", provider)
            }),
        );
    };

    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(true);
    let client = state.proxy.client();

    let send_once = |c: &ProviderConnection| {
        let (url, hdrs) = build_provider_target_url_and_headers(c, provider, model, stream);
        let mut req = client.post(&url);
        for (k, v) in hdrs {
            if let Some(vs) = v.as_str() {
                req = req.header(k, vs);
            }
        }
        req.json(body).send()
    };

    let mut resp = match send_once(&conn).await {
        Ok(r) => r,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    };

    if resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN {
        let refresh = token_refresh::refresh_oauth_token(client, &conn, &state.db).await;
        if refresh.success {
            if let Ok(updated) = state.db.get_active_provider_connections(Some(provider)) {
                if let Some(found) = updated.into_iter().find(|c| c.id == conn.id) {
                    conn = found;
                }
            }
            if let Ok(retried) = send_once(&conn).await {
                resp = retried;
            }
        }
    }

    if !resp.status().is_success() {
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let details = resp.text().await.unwrap_or_default();
        return json_response(
            status,
            json!({
                "success": false,
                "error": format!("Provider error: {}", status.as_u16()),
                "details": details,
            }),
        );
    }

    let byte_stream = resp
        .bytes_stream()
        .map(|chunk| chunk.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(byte_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// Unit & Router Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::TokenCache;
    use crate::combo_router::ComboTracker;
    use crate::config::Config;
    use crate::db::{ConnectionData, Database, ProviderConnection};
    use crate::original::Original;
    use crate::proxy::ProxyService;
    use crate::ratelimit::RateLimiter;
    use crate::routes::ConsoleLogBuffer;
    use axum::body::to_bytes;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn make_test_state() -> AppState {
        let db = Database::open_in_memory().expect("in-memory db");
        let config = Arc::new(Config {
            data_dir: PathBuf::from("/tmp"),
            db_path: PathBuf::from("/tmp/test-sso-misc.sqlite"),
            port: 20127,
            jwt_secret: "test-jwt-secret-key".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = Original::empty();
        let combo_tracker = ComboTracker::new();
        let token_cache = TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let rate_limiter = Arc::new(RateLimiter::from_config(&config));
        AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: ConsoleLogBuffer::new(),
            rate_limiter,
        }
    }

    fn make_api_key_row(key: &str) -> crate::db::ApiKey {
        crate::db::ApiKey {
            id: "key-1".to_string(),
            key: key.to_string(),
            name: Some("Test Key".to_string()),
            machine_id: None,
            is_active: true,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            rate_limit_requests: 0,
            rate_limit_window_seconds: 0,
            token_limit_daily: 0,
            token_limit_monthly: 0,
            allowed_models: "*".to_string(),
            blocked_models: "[]".to_string(),
            allowed_ips: "[]".to_string(),
            blocked_ips: "[]".to_string(),
            expires_at: String::new(),
            description: String::new(),
            owner_ref: None,
            max_concurrent_requests: 0,
            allowed_providers: "[]".to_string(),
            blocked_providers: "[]".to_string(),
            force_provider: String::new(),
            force_model: String::new(),
            allowed_endpoints: "[]".to_string(),
            blocked_endpoints: "[]".to_string(),
            budget_usd: None,
            budget_reset_hours: None,
            budget_window_started_at: None,
            billing_mode: "free".to_string(),
            byok_provider: None,
            regenerate_version: 0,
            metadata: "{}".to_string(),
            updated_at: None,
        }
    }

    fn insert_api_key(db: &Database, key: &str) {
        db.save_api_key(&make_api_key_row(key))
            .expect("save api key");
    }

    fn insert_provider_conn(db: &Database, id: &str, provider: &str, api_key: &str, base_url: Option<&str>) {
        let conn = ProviderConnection {
            id: id.to_string(),
            provider: provider.to_string(),
            auth_type: "apikey".to_string(),
            name: Some(provider.to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some(api_key.to_string()),
                base_url: base_url.map(ToString::to_string),
                headers: None,
                default_model: None,
                extra: HashMap::new(),
            },
            raw_data: String::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        db.save_provider_connection(&conn).expect("save provider conn");
    }

    #[tokio::test]
    async fn test_misc_init_locale_tags_v1_root() {
        let state = make_test_state();
        insert_api_key(&state.db, "sk-test-v1-root");
        let app = sso_misc_router().with_state(state.clone());

        // GET /api/init
        let resp = app
            .clone()
            .oneshot(Request::get("/api/init").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"Initialized");

        // POST /api/locale valid & invalid
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/locale")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"locale":"zh-CN"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cookie.contains("locale=zh-CN"));

        let bad_resp = app
            .clone()
            .oneshot(
                Request::post("/api/locale")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"locale":"xx-INVALID"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_resp.status(), StatusCode::BAD_REQUEST);

        // GET /api/tags
        let tags_resp = app
            .clone()
            .oneshot(Request::get("/api/tags").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(tags_resp.status(), StatusCode::OK);
        let tags_json: Value =
            serde_json::from_slice(&to_bytes(tags_resp.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(tags_json["models"][0]["name"], "llama3.2");
        assert_eq!(tags_json["models"][1]["name"], "qwen2.5");

        // GET /v1 & /api/v1 root
        let v1_resp = app
            .clone()
            .oneshot(
                Request::get("/v1")
                    .header(header::AUTHORIZATION, "Bearer sk-test-v1-root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(v1_resp.status(), StatusCode::OK);
        let v1_json: Value =
            serde_json::from_slice(&to_bytes(v1_resp.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(v1_json["object"], "list");

        let api_v1_resp = app
            .oneshot(
                Request::get("/api/v1")
                    .header(header::AUTHORIZATION, "Bearer sk-test-v1-root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(api_v1_resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_cron_flush_auth_and_execution() {
        let _guard = lock_env();
        let state = make_test_state();
        let app = sso_misc_router().with_state(state);

        unsafe {
            std::env::set_var("CRON_SECRET", "s3cret-value");
        }
        // Reject missing/wrong/prefix
        for auth_hdr in [None, Some("Bearer nope"), Some("Bearer s3cret"), Some("Bearer s3cret-value-extra")] {
            let mut req = Request::post("/api/cron/flush");
            if let Some(h) = auth_hdr {
                req = req.header(header::AUTHORIZATION, h);
            }
            let resp = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }

        // Accept Bearer and x-cron-secret
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/cron/flush")
                    .header(header::AUTHORIZATION, "Bearer s3cret-value")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["ok"], true);

        let resp2 = app
            .clone()
            .oneshot(
                Request::get("/api/cron/flush")
                    .header("x-cron-secret", "s3cret-value")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        unsafe {
            std::env::remove_var("CRON_SECRET");
        }
        let open_resp = app
            .oneshot(Request::post("/api/cron/flush").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(open_resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_shutdown_version_update_and_reset_password() {
        let _guard = lock_env();
        let state = make_test_state();
        state
            .db
            .save_settings(&json!({
                "requireLogin": true,
                "password": "$2b$10$dummyhashedpassword"
            }))
            .unwrap();
        let jwt = auth::create_dashboard_jwt(&state.config.jwt_secret);
        let auth_cookie = format!("auth_token={}", jwt);
        let app = sso_misc_router().with_state(state.clone());

        // Shutdown: 401 without secret, 200 with secret in non-prod, 403 in prod
        unsafe {
            std::env::set_var("NODE_ENV", "development");
            std::env::set_var("SHUTDOWN_SECRET", "shut-123");
        }
        let unauth_sd = app
            .clone()
            .oneshot(Request::post("/api/shutdown").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauth_sd.status(), StatusCode::UNAUTHORIZED);

        let ok_sd = app
            .clone()
            .oneshot(
                Request::post("/api/shutdown")
                    .header(header::AUTHORIZATION, "Bearer shut-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_sd.status(), StatusCode::OK);

        unsafe {
            std::env::set_var("NODE_ENV", "production");
        }
        let prod_sd = app
            .clone()
            .oneshot(
                Request::post("/api/shutdown")
                    .header(header::AUTHORIZATION, "Bearer shut-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(prod_sd.status(), StatusCode::FORBIDDEN);

        // Version update: 401 without JWT, 403 in dev, 200 in prod
        let unauth_upd = app
            .clone()
            .oneshot(
                Request::post("/api/version/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth_upd.status(), StatusCode::UNAUTHORIZED);

        let ok_upd = app
            .clone()
            .oneshot(
                Request::post("/api/version/update")
                    .header(header::COOKIE, &auth_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_upd.status(), StatusCode::OK);

        unsafe {
            std::env::set_var("NODE_ENV", "development");
        }
        let dev_upd = app
            .clone()
            .oneshot(
                Request::post("/api/version/update")
                    .header(header::COOKIE, &auth_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dev_upd.status(), StatusCode::FORBIDDEN);

        // Reset password: 403 via proxy, 401 without auth, 200 with localhost + cookie clears password
        let via_proxy = app
            .clone()
            .oneshot(
                Request::post("/api/auth/reset-password")
                    .header("x-9r-via-proxy", "1")
                    .header(header::COOKIE, &auth_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(via_proxy.status(), StatusCode::FORBIDDEN);

        let reset_ok = app
            .oneshot(
                Request::post("/api/auth/reset-password")
                    .header(header::HOST, "127.0.0.1:20127")
                    .header(header::COOKIE, &auth_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reset_ok.status(), StatusCode::OK);
        let after = state.db.load_settings().unwrap();
        assert!(after.get("password").is_none());

        unsafe {
            std::env::remove_var("NODE_ENV");
            std::env::remove_var("SHUTDOWN_SECRET");
        }
    }

    #[tokio::test]
    async fn test_oidc_start_and_callback_flow_with_mock_idp() {
        let _guard = lock_env();
        let state = make_test_state();

        // Generate an ECDSA P-256 keypair in ring for JWK + id_token signing
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = signature::EcdsaKeyPair::generate_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .unwrap();
        let key_pair = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8.as_ref(),
            &rng,
        )
        .unwrap();
        use signature::KeyPair;
        let pub_bytes = key_pair.public_key().as_ref(); // 0x04 || x(32) || y(32)
        let x_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&pub_bytes[1..33]);
        let y_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&pub_bytes[33..65]);

        let captured_nonce = Arc::new(Mutex::new(String::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let issuer = format!("http://{}", addr);

        let issuer_disc = issuer.clone();
        let issuer_tok = issuer.clone();
        let nonce_tok = Arc::clone(&captured_nonce);
        let pkcs8_bytes = pkcs8.as_ref().to_vec();

        let idp_app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let iss = issuer_disc.clone();
                    async move {
                        Json(json!({
                            "issuer": iss,
                            "authorization_endpoint": format!("{}/authorize", iss),
                            "token_endpoint": format!("{}/token", iss),
                            "jwks_uri": format!("{}/jwks.json", iss),
                        }))
                    }
                }),
            )
            .route(
                "/jwks.json",
                get(move || {
                    let x = x_b64.clone();
                    let y = y_b64.clone();
                    async move {
                        Json(json!({
                            "keys": [{
                                "kty": "EC",
                                "crv": "P-256",
                                "use": "sig",
                                "alg": "ES256",
                                "kid": "k1",
                                "x": x,
                                "y": y
                            }]
                        }))
                    }
                }),
            )
            .route(
                "/token",
                post(move || {
                    let iss = issuer_tok.clone();
                    let nonce_ref = Arc::clone(&nonce_tok);
                    let key_bytes = pkcs8_bytes.clone();
                    async move {
                        let rng = ring::rand::SystemRandom::new();
                        let kp = signature::EcdsaKeyPair::from_pkcs8(
                            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                            &key_bytes,
                            &rng,
                        )
                        .unwrap();
                        let nonce_val = nonce_ref.lock().unwrap().clone();
                        let now = chrono::Utc::now().timestamp();
                        let hdr = base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .encode(r#"{"alg":"ES256","typ":"JWT","kid":"k1"}"#);
                        let pld = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                            json!({
                                "iss": iss,
                                "aud": "client-123",
                                "sub": "user-42",
                                "email": "alice@example.com",
                                "preferred_username": "alice",
                                "nonce": nonce_val,
                                "iat": now,
                                "exp": now + 600,
                            })
                            .to_string(),
                        );
                        let signing_input = format!("{}.{}", hdr, pld);
                        let sig = kp.sign(&rng, signing_input.as_bytes()).unwrap();
                        let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .encode(sig.as_ref());
                        Json(json!({
                            "access_token": "at-xyz",
                            "token_type": "Bearer",
                            "id_token": format!("{}.{}", signing_input, sig_b64),
                        }))
                    }
                }),
            );

        tokio::spawn(async move {
            let _ = axum::serve(listener, idp_app).await;
        });

        state
            .db
            .save_settings(&json!({
                "authMode": "oidc",
                "oidcIssuerUrl": issuer,
                "oidcClientId": "client-123",
                "oidcClientSecret": "secret-456",
                "oidcScopes": "openid profile email"
            }))
            .unwrap();

        let app = sso_misc_router().with_state(state.clone());

        // 1. /api/auth/oidc/start
        let start_resp = app
            .clone()
            .oneshot(
                Request::get("/api/auth/oidc/start")
                    .header(header::HOST, "localhost:20127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = start_resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(location.contains("/authorize?"));
        assert!(location.contains("code_challenge_method=S256"));

        let mut cookies_kv = Vec::new();
        let mut oidc_state = String::new();
        let mut oidc_nonce = String::new();
        for val in start_resp.headers().get_all(header::SET_COOKIE) {
            let s = val.to_str().unwrap();
            let kv = s.split(';').next().unwrap().to_string();
            if let Some(v) = kv.strip_prefix("oidc_state=") {
                oidc_state = v.to_string();
            }
            if let Some(v) = kv.strip_prefix("oidc_nonce=") {
                oidc_nonce = v.to_string();
            }
            cookies_kv.push(kv);
        }
        *captured_nonce.lock().unwrap() = oidc_nonce;

        // 2. /api/auth/oidc/callback with invalid state -> redirect with oidc_invalid_state
        let bad_cb = app
            .clone()
            .oneshot(
                Request::get("/api/auth/oidc/callback?code=abc&state=wrong")
                    .header(header::HOST, "localhost:20127")
                    .header(header::COOKIE, cookies_kv.join("; "))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_cb.status(), StatusCode::TEMPORARY_REDIRECT);
        assert!(bad_cb
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("oidc_invalid_state"));

        // 3. /api/auth/oidc/callback valid -> issues auth_token cookie and redirects to /dashboard
        let ok_cb = app
            .oneshot(
                Request::get(format!(
                    "/api/auth/oidc/callback?code=auth-code-1&state={}",
                    oidc_state
                ))
                .header(header::HOST, "localhost:20127")
                .header(header::COOKIE, cookies_kv.join("; "))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_cb.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            ok_cb
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "http://localhost:20127/dashboard"
        );

        let issued_jwt = ok_cb
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|s| s.split(';').next()?.strip_prefix("auth_token="))
            .expect("auth_token cookie issued");
        assert!(auth::verify_dashboard_jwt(
            issued_jwt,
            &state.config.jwt_secret
        ));
    }

    #[tokio::test]
    async fn test_saml_metadata_start_and_acs_with_real_signature() {
        let state = make_test_state();

        // Generate real self-signed ECDSA P-256 cert via rcgen and matching ring keypair
        let cert_key = rcgen::generate_simple_self_signed(vec!["idp.example.com".to_string()]).unwrap();
        let cert_pem = cert_key.cert.pem();
        let key_der = cert_key.signing_key.serialize_der();

        let rng = ring::rand::SystemRandom::new();
        let ring_kp = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &key_der,
            &rng,
        )
        .unwrap();

        state
            .db
            .save_settings(&json!({
                "samlEntryPoint": "https://idp.example.com/sso",
                "samlIssuer": "urn:9router:sp",
                "samlCert": cert_pem,
            }))
            .unwrap();

        let app = sso_misc_router().with_state(state.clone());

        // 1. Metadata XML
        let meta_resp = app
            .clone()
            .oneshot(
                Request::get("/api/auth/saml/metadata")
                    .header(header::HOST, "localhost:20127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(meta_resp.status(), StatusCode::OK);
        let meta_xml =
            String::from_utf8(to_bytes(meta_resp.into_body(), usize::MAX).await.unwrap().to_vec())
                .unwrap();
        assert!(meta_xml.contains(r#"entityID="urn:9router:sp""#));
        assert!(meta_xml.contains(r#"Location="http://localhost:20127/api/auth/saml/acs""#));

        // 2. SAML start
        let start_resp = app
            .clone()
            .oneshot(
                Request::get("/api/auth/saml/start")
                    .header(header::HOST, "localhost:20127")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::TEMPORARY_REDIRECT);
        let loc = start_resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(loc.starts_with("https://idp.example.com/sso?SAMLRequest="));
        let saml_state_cookie = start_resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let req_id = saml_state_cookie.strip_prefix("saml_state=").unwrap();

        // 3. Build signed SAMLResponse matching req_id
        let signed_info = r##"<ds:SignedInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/><ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256"/><ds:Reference URI="#_a1"><ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/><ds:DigestValue>AAAA</ds:DigestValue></ds:Reference></ds:SignedInfo>"##;
        let sig = ring_kp.sign(&rng, signed_info.as_bytes()).unwrap();
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.as_ref());
        let cert_b64 = cert_pem
            .replace("-----BEGIN CERTIFICATE-----", "")
            .replace("-----END CERTIFICATE-----", "")
            .replace('\n', "");

        let saml_xml = format!(
            concat!(
                r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" ID="_r1" InResponseTo="{req_id}" Version="2.0">"#,
                r#"<samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>"#,
                r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a1" Version="2.0">"#,
                r#"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">{signed_info}<ds:SignatureValue>{sig_b64}</ds:SignatureValue><ds:KeyInfo><ds:X509Data><ds:X509Certificate>{cert_b64}</ds:X509Certificate></ds:X509Data></ds:KeyInfo></ds:Signature>"#,
                r#"<saml:Subject><saml:NameID>bob@example.com</saml:NameID></saml:Subject>"#,
                r#"<saml:AttributeStatement><saml:Attribute Name="email"><saml:AttributeValue>bob@example.com</saml:AttributeValue></saml:Attribute><saml:Attribute Name="displayName"><saml:AttributeValue>Bob Smith</saml:AttributeValue></saml:Attribute></saml:AttributeStatement>"#,
                r#"</saml:Assertion></samlp:Response>"#,
            ),
            req_id = req_id,
            signed_info = signed_info,
            sig_b64 = sig_b64,
            cert_b64 = cert_b64,
        );
        let saml_b64 = base64::engine::general_purpose::STANDARD.encode(saml_xml.as_bytes());
        let form_body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("SAMLResponse", &saml_b64)
            .finish();

        // 4. Reject replay mismatch
        let bad_acs = app
            .clone()
            .oneshot(
                Request::post("/api/auth/saml/acs")
                    .header(header::HOST, "localhost:20127")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "saml_state=wrong-req-id")
                    .body(Body::from(form_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_acs.status(), StatusCode::TEMPORARY_REDIRECT);
        assert!(bad_acs
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("InResponseTo"));

        // 5. Valid ACS issues auth_token cookie
        let ok_acs = app
            .oneshot(
                Request::post("/api/auth/saml/acs")
                    .header(header::HOST, "localhost:20127")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, &saml_state_cookie)
                    .body(Body::from(form_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_acs.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            ok_acs.headers().get(header::LOCATION).unwrap().to_str().unwrap(),
            "http://localhost:20127/dashboard"
        );
        let issued_jwt = ok_acs
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|s| s.split(';').next()?.strip_prefix("auth_token="))
            .expect("auth_token cookie issued");
        assert!(auth::verify_dashboard_jwt(
            issued_jwt,
            &state.config.jwt_secret
        ));
    }

    #[test]
    fn test_saml_helper_contracts() {
        let formatted = format_x509_certificate("MIIC1234567890123456789012345678901234567890123456789012345678901234567890");
        assert!(formatted.contains("-----BEGIN CERTIFICATE-----"));
        assert!(formatted.contains("\n1234567890\n"));
        assert_eq!(format_x509_certificate("   "), "");

        assert!(is_saml_configured(&json!({
            "samlEntryPoint": "https://idp.example.com/sso",
            "samlCert": "dummy"
        })));
        assert!(!is_saml_configured(&json!({ "samlEntryPoint": "https://idp.example.com/sso" })));

        let profile = json!({
            "email": "user@example.com",
            "displayName": "Jane Doe",
            "customEmail": "custom@example.com",
            "customName": "Custom User",
        });
        assert_eq!(pick_saml_email(&profile, &json!({})), "user@example.com");
        assert_eq!(
            pick_saml_email(&profile, &json!({ "samlAttributeEmail": "customEmail" })),
            "custom@example.com"
        );
        assert_eq!(pick_saml_display_name(&profile, &json!({})), "Jane Doe");
        assert_eq!(
            pick_saml_display_name(&profile, &json!({ "samlAttributeName": "customName" })),
            "Custom User"
        );
    }

    #[tokio::test]
    async fn test_tts_voices_endpoints_and_mock_upstreams() {
        let state = make_test_state();
        insert_api_key(&state.db, "sk-voices-key");
        let app = sso_misc_router().with_state(state.clone());

        // 1. Missing connections return 400
        for path in [
            "/api/media-providers/tts/deepgram/voices",
            "/api/media-providers/tts/elevenlabs/voices",
            "/api/media-providers/tts/inworld/voices",
            "/api/media-providers/tts/minimax/voices",
        ] {
            let r = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "expected 400 for {}", path);
        }

        // 2. Gemini voices via /api/media-providers/tts/voices?provider=gemini
        let gem_resp = app
            .clone()
            .oneshot(
                Request::get("/api/media-providers/tts/voices?provider=gemini")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gem_resp.status(), StatusCode::OK);
        let gem_json: Value =
            serde_json::from_slice(&to_bytes(gem_resp.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(gem_json["voices"].as_array().unwrap().len(), 30);

        // 3. Unknown provider on /v1/audio/voices returns 400
        let bad_pub = app
            .clone()
            .oneshot(
                Request::get("/v1/audio/voices?provider=unknown")
                    .header(header::AUTHORIZATION, "Bearer sk-voices-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_pub.status(), StatusCode::BAD_REQUEST);

        // 4. Auth check on /v1/audio/voices when requireLogin=true
        state.db.save_settings(&json!({ "requireLogin": true })).unwrap();
        let unauth_pub = app
            .clone()
            .oneshot(
                Request::get("/v1/audio/voices?provider=edge-tts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth_pub.status(), StatusCode::UNAUTHORIZED);

        // 5. Mock upstreams for edge-tts, elevenlabs, deepgram, inworld, minimax
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock_base = format!("http://{}", addr);

        let mock_app = Router::new()
            .route(
                "/edge",
                get(|| async {
                    Json(json!([
                        { "ShortName": "zh-CN-XiaoxiaoNeural", "FriendlyName": "Microsoft Xiaoxiao Online (Natural) - Chinese", "Locale": "zh-CN", "Gender": "Female" },
                        { "ShortName": "en-US-JennyNeural", "FriendlyName": "Microsoft Jenny Online (Natural) - English", "Locale": "en-US", "Gender": "Female" }
                    ]))
                }),
            )
            .route(
                "/eleven",
                get(|| async {
                    Json(json!({
                        "voices": [{ "voice_id": "v1", "name": "Rachel", "category": "premade", "labels": { "language": "en", "gender": "female" } }]
                    }))
                }),
            )
            .route(
                "/deepgram",
                get(|| async {
                    Json(json!({
                        "tts": [{ "canonical_name": "aura-2-thalia-en", "name": "Thalia", "languages": ["en"], "metadata": { "tags": ["feminine"] } }]
                    }))
                }),
            )
            .route(
                "/inworld",
                get(|| async {
                    Json(json!({
                        "voices": [{ "voiceId": "Ashley", "displayName": "Ashley", "languages": ["en"], "gender": "Female" }]
                    }))
                }),
            )
            .route(
                "/minimax",
                post(|| async {
                    Json(json!({
                        "system_voice": [
                            { "voice_id": "English_expressive_narrator", "voice_name": "Expressive Narrator" },
                            { "voice_id": "Chinese (Mandarin)_female_beijing", "voice_name": "Female Beijing" }
                        ],
                        "voice_cloning": [{ "voice_id": "clone_123", "voice_name": "My Voice" }],
                        "base_resp": { "status_code": 0, "status_msg": "success" }
                    }))
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, mock_app).await;
        });

        let client = reqwest::Client::new();
        let edge_res = list_tts_voices_with_urls(
            &client,
            "edge-tts",
            Some("zh"),
            None,
            &format!("{}/edge", mock_base),
            &format!("{}/eleven", mock_base),
            false,
        )
        .await
        .unwrap();
        assert_eq!(edge_res["voices"].as_array().unwrap().len(), 1);
        assert_eq!(edge_res["voices"][0]["id"], "zh-CN-XiaoxiaoNeural");

        let dg_res = fetch_deepgram_grouped_voices(
            &client,
            &format!("{}/deepgram", mock_base),
            "dg-key",
            None,
        )
        .await
        .unwrap();
        assert_eq!(dg_res["byLang"]["en"]["voices"][0]["id"], "aura-2-thalia-en");

        let el_res = fetch_elevenlabs_grouped_voices(
            &client,
            &format!("{}/eleven", mock_base),
            "el-key",
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(el_res["byLang"]["en"]["voices"][0]["free_users_allowed"], true);

        let iw_res = fetch_inworld_grouped_voices(
            &client,
            &format!("{}/inworld", mock_base),
            "iw-key",
            None,
        )
        .await
        .unwrap();
        assert_eq!(iw_res["byLang"]["en"]["voices"][0]["id"], "Ashley");

        let mm_res = fetch_minimax_grouped_voices(
            &client,
            &format!("{}/minimax", mock_base),
            "mm-key",
            "all",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            mm_res["byLang"]["English"]["voices"][0]["id"],
            "English_expressive_narrator"
        );
        assert_eq!(
            mm_res["byLang"]["Custom"]["voices"][0]["name"],
            "My Voice · Cloned"
        );
    }

    #[tokio::test]
    async fn test_translator_load_save_translate_and_send() {
        let _guard = lock_env();
        let state = make_test_state();
        let temp_dir = std::env::temp_dir().join(format!("9r-translator-test-{}", random_hex(8)));
        unsafe {
            std::env::set_var("NINEROUTER_TRANSLATOR_LOGS_DIR", &temp_dir);
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock_url = format!("http://{}", addr);

        let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
                )
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, upstream_app).await;
        });

        insert_provider_conn(
            &state.db,
            "conn-openai",
            "openai",
            "sk-upstream",
            Some(&format!("{}/v1", mock_url)),
        );
        insert_provider_conn(
            &state.db,
            "conn-claude",
            "claude",
            "sk-ant-upstream",
            Some("https://api.anthropic.com/v1"),
        );

        let app = sso_misc_router().with_state(state);

        // 1. Disallow path traversal on save/load
        let bad_save = app
            .clone()
            .oneshot(
                Request::post("/api/translator/save")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"file":"../secret.txt","content":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_save.status(), StatusCode::BAD_REQUEST);

        // 2. Save & load roundtrip
        let ok_save = app
            .clone()
            .oneshot(
                Request::post("/api/translator/save")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"file":"1_req_client.json","content":"{\"hello\":1}"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_save.status(), StatusCode::OK);

        let ok_load = app
            .clone()
            .oneshot(
                Request::get("/api/translator/load?file=1_req_client.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_load.status(), StatusCode::OK);
        let load_json: Value =
            serde_json::from_slice(&to_bytes(ok_load.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(load_json["content"], r#"{"hello":1}"#);

        // 3. Translate steps 1, 2, 3
        let step1 = app
            .clone()
            .oneshot(
                Request::post("/api/translator/translate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "step": 1,
                            "body": {
                                "model": "claude/claude-sonnet-4-5",
                                "system": "be brief",
                                "messages": [{ "role": "user", "content": "hi" }]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(step1.status(), StatusCode::OK);
        let s1: Value =
            serde_json::from_slice(&to_bytes(step1.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(s1["result"]["sourceFormat"], "claude");
        assert_eq!(s1["result"]["targetFormat"], "claude");

        let step2 = app
            .clone()
            .oneshot(
                Request::post("/api/translator/translate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "step": 2,
                            "body": {
                                "model": "openai/gpt-4o",
                                "system": "be brief",
                                "messages": [{ "role": "user", "content": "hi" }]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(step2.status(), StatusCode::OK);
        let s2: Value =
            serde_json::from_slice(&to_bytes(step2.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(s2["result"]["body"]["messages"][0]["role"], "system");

        let step3 = app
            .clone()
            .oneshot(
                Request::post("/api/translator/translate")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "step": 3,
                            "body": {
                                "provider": "claude",
                                "model": "claude-sonnet-4-5",
                                "body": {
                                    "model": "claude-sonnet-4-5",
                                    "messages": [{ "role": "user", "content": "hi" }]
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(step3.status(), StatusCode::OK);
        let s3: Value =
            serde_json::from_slice(&to_bytes(step3.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(s3["result"]["url"], "https://api.anthropic.com/v1/messages");

        // 4. Send SSE stream
        let send_resp = app
            .oneshot(
                Request::post("/api/translator/send")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "provider": "openai",
                            "model": "gpt-4o",
                            "body": { "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }], "stream": true }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(send_resp.status(), StatusCode::OK);
        let sse_text =
            String::from_utf8(to_bytes(send_resp.into_body(), usize::MAX).await.unwrap().to_vec())
                .unwrap();
        assert!(sse_text.contains("[DONE]"));

        let _ = std::fs::remove_dir_all(&temp_dir);
        unsafe {
            std::env::remove_var("NINEROUTER_TRANSLATOR_LOGS_DIR");
        }
    }
}
