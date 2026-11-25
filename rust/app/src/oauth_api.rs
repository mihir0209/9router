use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use base64::engine::general_purpose::{
    STANDARD as B64_STANDARD, URL_SAFE as B64_URL, URL_SAFE_NO_PAD as B64_URL_NO_PAD,
};
use base64::Engine;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection as SqliteConnection, OpenFlags};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::auth::verify_internal_auth;
use crate::db::{ConnectionData, Database, ProviderConnection};
use crate::routes::{options_handler, AppState};

// ============================================================================
// Constants & Provider Registry
// ============================================================================

const PKCE_SESSION_TTL_SECS: u64 = 15 * 60;
const PROXY_IDLE_TIMEOUT_SECS: u64 = 300;
const LONG_PROXY_IDLE_TIMEOUT_SECS: u64 = 600;
const MIMO_SESSION_COOKIE: &str = "9r_mimo_login";
const MIMO_SESSION_TTL_MS: i64 = 15 * 60 * 1000;
const MIMO_COOKIE_JSON_BUDGET: usize = 2800;

const JS_ENCODE_URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

// Google / Antigravity public CLI OAuth client credentials (matching open-sse/providers/shared.js)
const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_CLIENT_SECRET: &str = "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";
const ANTIGRAVITY_IDE_USER_AGENT: &str = "antigravity/hub/2.24.0 darwin/arm64";
const ANTIGRAVITY_LOAD_CODE_ASSIST_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ANTIGRAVITY_ONBOARD_USER_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:onboardUser";

const GOOGLE_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GOOGLE_CLIENT_SECRET: &str = "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";

// Claude
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

// Codex
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_FIXED_PORT: u16 = 1455;

// xAI
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const XAI_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_LOOPBACK_PORT: u16 = 56121;
const XAI_PKCE_VERIFIER_BYTES: usize = 96;

// Grok CLI
const GROK_CLI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const GROK_CLI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const GROK_CLI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const GROK_CLI_SCOPE: &str =
    "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";
const GROK_CLI_VERSION: &str = "1.0.44";
const GROK_CLI_PAGER_USER_AGENT: &str = "grok-pager/1.0.44 grok-shell/1.0.44 (linux; x86_64)";

// Muse
const MUSE_CLIENT_ID: &str = "1031625952748946";
const MUSE_DEVICE_CODE_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const MUSE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const MUSE_KEY_URL: &str = "https://api.meta.ai/muse-code/key";

// Nous
const NOUS_CLIENT_ID: &str = "hermes-cli";
const NOUS_DEVICE_CODE_URL: &str = "https://portal.nousresearch.com/api/oauth/device/code";
const NOUS_TOKEN_URL: &str = "https://portal.nousresearch.com/api/oauth/token";
const NOUS_AGENT_KEY_URL: &str = "https://portal.nousresearch.com/api/oauth/agent-key";
const NOUS_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
const NOUS_SCOPE: &str = "inference:invoke inference:mint_agent_key";

// iFlow
const IFLOW_CLIENT_ID: &str = "10009311001";
const IFLOW_CLIENT_SECRET: &str = "4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW";
const IFLOW_AUTHORIZE_URL: &str = "https://iflow.cn/oauth";
const IFLOW_TOKEN_URL: &str = "https://iflow.cn/oauth/token";
const IFLOW_USERINFO_URL: &str = "https://iflow.cn/api/oauth/getUserInfo";

// GitHub
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
const GITHUB_API_VERSION: &str = "2022-11-28";
const GITHUB_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";

// Kimi
const KIMI_DEFAULT_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_DEVICE_CODE_URL: &str = "https://auth.kimi.com/api/oauth/device_authorization";
const KIMI_TOKEN_URL: &str = "https://auth.kimi.com/api/oauth/token";
const KIMI_AUTHORIZE_DEVICE_URL: &str = "https://www.kimi.com/code/authorize_device";

// KiloCode
const KILOCODE_API_BASE: &str = "https://api.kilo.ai";
const KILOCODE_INITIATE_URL: &str = "https://api.kilo.ai/api/device-auth/codes";

// Cline / ClinePass
const CLINE_AUTHORIZE_URL: &str = "https://api.cline.bot/api/v1/auth/authorize";
const CLINE_TOKEN_URL: &str = "https://api.cline.bot/api/v1/auth/token";

// CodeBuddy CN & Intl
const CODEBUDDY_CN_STATE_URL: &str = "https://copilot.tencent.com/v2/plugin/auth/state";
const CODEBUDDY_CN_TOKEN_URL: &str = "https://copilot.tencent.com/v2/plugin/auth/token";
const CODEBUDDY_CN_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";
const CODEBUDDY_INTL_STATE_URL: &str = "https://www.codebuddy.ai/v2/plugin/auth/state";
const CODEBUDDY_INTL_TOKEN_URL: &str = "https://www.codebuddy.ai/v2/plugin/auth/token";
const CODEBUDDY_INTL_UA: &str = "IDE/2.63.2 CodeBuddy/2.63.2";

// Kimchi
const KIMCHI_WEB_APP_URL: &str = "https://app.kimchi.dev";
const KIMCHI_VALIDATION_URL: &str = "https://api.cast.ai/v1/llm/openai/supported-providers";
const KIMCHI_USERINFO_URL: &str = "https://app.kimchi.dev/api/v1/me";

// GLM (ZCode)
const GLM_CLI_INIT_URL: &str = "https://zcode.z.ai/api/v1/oauth/cli/init";
const GLM_CLI_POLL_URL: &str = "https://zcode.z.ai/api/v1/oauth/cli/poll";
const GLM_BUSINESS_LOGIN_URL: &str = "https://api.z.ai/api/auth/z/login";
const GLM_API_BASE_URL: &str = "https://api.z.ai";
const GLM_PLAN_API_KEY_NAME: &str = "zcode-api-key";

// Kiro
const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
const KIRO_START_URL: &str = "https://view.awsapps.com/start";
const KIRO_ISSUER_URL: &str = "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6";
const KIRO_SOCIAL_REDIRECT_URI: &str = "kiro://kiro.kiroAgent/authenticate-success";

// Trae
const TRAE_CLIENT_ID: &str = "ono9krqynydwx5";
const TRAE_CLIENT_SECRET: &str = "-";
const TRAE_USER_AGENT: &str = "Trae/1.0.0 antigravity-cockpit-tools";
const TRAE_DEFAULT_APP_VERSION: &str = "3.5.54";
const TRAE_TOKEN_LIFETIME_SECS: i64 = 14 * 24 * 60 * 60;
const TRAE_GUIDANCE_URLS: &[&str] = &[
    "https://api.marscode.com/cloudide/api/v3/trae/GetLoginGuidance",
    "https://api.trae.ai/cloudide/api/v3/trae/GetLoginGuidance",
    "https://www.trae.ai/cloudide/api/v3/trae/GetLoginGuidance",
];
const TRAE_API_ORIGINS: &[&str] = &[
    "https://api.marscode.com",
    "https://api.trae.ai",
    "https://www.trae.ai",
    "https://www.marscode.com",
];

// Windsurf
const WINDSURF_CLIENT_ID: &str = "3GUryQ7ldAeKEuD2obYnppsnmj58eP5u";
const WINDSURF_AUTH_BASE: &str = "https://www.windsurf.com";
const WINDSURF_REGISTER_BASE: &str = "https://register.windsurf.com";
const WINDSURF_DEFAULT_API_SERVER: &str = "https://server.codeium.com";
const WINDSURF_USER_AGENT: &str = "antigravity-cockpit-tools";

// Zed
const ZED_WEB_BASE_URL: &str = "https://zed.dev";
const ZED_CLOUD_BASE_URL: &str = "https://cloud.zed.dev";
const ZED_DEFAULT_NATIVE_APP_PORT: u16 = 58443;
const ZED_PRIVATE_KEY_PREFIX: &str = "zed-rsa-pkcs1:";
const ZED_KEYRING_LABEL: &str = "zed-github-account";

// MiniMax Code
const MCODE_CLIENT_ID: &str = "mcode-public";
const MCODE_SCOPE: &str = "agent.default";
const MCODE_AUDIENCE: &str = "agent-backend";
const MCODE_MAX_FRAME_BYTES: usize = 64 * 1024;
const MCODE_MAX_MIN_VALIDITY_MS: i64 = 5 * 60 * 1000;

// ============================================================================
// Global State: PKCE / State Store & Callback Proxy Manager
// ============================================================================

#[derive(Debug, Clone)]
pub struct PkceEntry {
    pub provider: String,
    pub state: String,
    pub code_verifier: String,
    pub code_challenge: String,
    pub redirect_uri: Option<String>,
    pub system_id: Option<String>,
    pub created_at: Instant,
}

#[derive(Default)]
struct PkceStore {
    entries: HashMap<String, PkceEntry>,
}

impl PkceStore {
    fn cleanup_expired(&mut self, ttl: Duration) {
        let now = Instant::now();
        self.entries
            .retain(|_, v| now.duration_since(v.created_at) < ttl);
    }

    fn insert(&mut self, entry: PkceEntry, ttl: Duration) {
        self.cleanup_expired(ttl);
        self.entries.insert(entry.state.clone(), entry);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn get(&mut self, state: &str, ttl: Duration) -> Option<PkceEntry> {
        self.cleanup_expired(ttl);
        self.entries.get(state).cloned()
    }

    fn remove(&mut self, state: &str) -> Option<PkceEntry> {
        self.entries.remove(state)
    }
}

#[derive(Debug, Clone)]
struct ProxyExchangeSession {
    code_verifier: String,
    redirect_uri: String,
    status: String,
    connection_id: Option<String>,
    email: Option<String>,
    error: Option<String>,
    created_at: Instant,
}

#[derive(Debug, Clone)]
struct SingletonSession {
    state: String,
    code_verifier: Option<String>,
    system_id: Option<String>,
    status: String,
    connection_id: Option<String>,
    email: Option<String>,
    error: Option<String>,
    created_at: Instant,
}

#[derive(Debug, Clone)]
struct XiaomiMimoSession {
    private_key: [u8; 32],
    status: String,
    uid: Option<String>,
    access_token: Option<String>,
    base_url: Option<String>,
    error: Option<String>,
    created_at: Instant,
}

struct RunningProxy {
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

struct OAuthRuntime {
    pkce_store: Mutex<PkceStore>,
    xai_discovery_cache: Mutex<Option<(String, String)>>,
    codex_sessions: Mutex<HashMap<String, ProxyExchangeSession>>,
    xai_sessions: Mutex<HashMap<String, ProxyExchangeSession>>,
    trae_session: Mutex<Option<SingletonSession>>,
    windsurf_session: Mutex<Option<SingletonSession>>,
    zed_session: Mutex<Option<SingletonSession>>,
    xiaomi_sessions: Mutex<HashMap<String, XiaomiMimoSession>>,
    codex_proxy: Mutex<Option<RunningProxy>>,
    xai_proxy: Mutex<Option<RunningProxy>>,
    trae_proxy: Mutex<Option<RunningProxy>>,
    windsurf_proxy: Mutex<Option<RunningProxy>>,
    zed_proxy: Mutex<Option<RunningProxy>>,
    xiaomi_proxy: Mutex<Option<RunningProxy>>,
}

impl OAuthRuntime {
    fn new() -> Self {
        Self {
            pkce_store: Mutex::new(PkceStore::default()),
            xai_discovery_cache: Mutex::new(None),
            codex_sessions: Mutex::new(HashMap::new()),
            xai_sessions: Mutex::new(HashMap::new()),
            trae_session: Mutex::new(None),
            windsurf_session: Mutex::new(None),
            zed_session: Mutex::new(None),
            xiaomi_sessions: Mutex::new(HashMap::new()),
            codex_proxy: Mutex::new(None),
            xai_proxy: Mutex::new(None),
            trae_proxy: Mutex::new(None),
            windsurf_proxy: Mutex::new(None),
            zed_proxy: Mutex::new(None),
            xiaomi_proxy: Mutex::new(None),
        }
    }
}

static OAUTH_RUNTIME: OnceLock<OAuthRuntime> = OnceLock::new();

fn runtime() -> &'static OAuthRuntime {
    OAUTH_RUNTIME.get_or_init(OAuthRuntime::new)
}

// ============================================================================
// Crypto, Random, Base64, Curve25519 (X25519), RSA, and JWT Helpers
// ============================================================================

fn random_bytes(len: usize) -> Vec<u8> {
    let rng = SystemRandom::new();
    let mut buf = vec![0u8; len];
    rng.fill(&mut buf).expect("SystemRandom failed");
    buf
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let rng = SystemRandom::new();
    rng.fill(&mut b).expect("SystemRandom failed");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex_encode(&b[0..4]),
        hex_encode(&b[4..6]),
        hex_encode(&b[6..8]),
        hex_encode(&b[8..10]),
        hex_encode(&b[10..16])
    )
}

fn b64url_encode(data: &[u8]) -> String {
    B64_URL_NO_PAD.encode(data)
}

fn b64url_decode(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim();
    B64_URL_NO_PAD
        .decode(trimmed.trim_end_matches('='))
        .or_else(|_| B64_URL.decode(trimmed))
        .map_err(|e| format!("base64url decode error: {}", e))
}

fn b64_std_decode(input: &str) -> Result<Vec<u8>, String> {
    let cleaned = input.trim().replace('-', "+").replace('_', "/");
    let pad = (4 - (cleaned.len() % 4)) % 4;
    let padded = format!("{}{}", cleaned, "=".repeat(pad));
    B64_STANDARD
        .decode(padded)
        .map_err(|e| format!("base64 decode error: {}", e))
}

pub fn generate_pkce(verifier_bytes: usize) -> (String, String, String) {
    let verifier = b64url_encode(&random_bytes(verifier_bytes));
    let challenge_digest = digest::digest(&digest::SHA256, verifier.as_bytes());
    let challenge = b64url_encode(challenge_digest.as_ref());
    let state = b64url_encode(&random_bytes(32));
    (verifier, challenge, state)
}

fn encode_uri_component(s: &str) -> String {
    utf8_percent_encode(s, JS_ENCODE_URI_COMPONENT).to_string()
}

fn build_query_uri_component(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

// --- Pure-Rust RFC 7748 Curve25519 (X25519) Montgomery ladder ---
// Used for Xiaomi MiMo ECDH callback decryption so keys can be retried across
// multiple pending sessions without consuming ring::agreement::EphemeralPrivateKey.

type Fe = [i64; 16];

fn fe_zero() -> Fe {
    [0; 16]
}

fn fe_one() -> Fe {
    let mut f = [0; 16];
    f[0] = 1;
    f
}

fn fe_from_bytes(s: &[u8; 32]) -> Fe {
    let mut o = [0i64; 16];
    for i in 0..16 {
        o[i] = (s[2 * i] as i64) + ((s[2 * i + 1] as i64) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn fe_carry(o: &mut Fe) {
    for i in 0..16 {
        o[i] += 1i64 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

fn fe_sel(p: &mut Fe, q: &mut Fe, b: i64) {
    let c = !(b - 1);
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

fn fe_add(a: &Fe, b: &Fe) -> Fe {
    let mut o = [0i64; 16];
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn fe_sub(a: &Fe, b: &Fe) -> Fe {
    let mut o = [0i64; 16];
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn fe_mul(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0i128; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += (a[i] as i128) * (b[j] as i128);
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o = [0i64; 16];
    for i in 0..16 {
        o[i] = t[i] as i64;
    }
    fe_carry(&mut o);
    fe_carry(&mut o);
    o
}

fn fe_sq(a: &Fe) -> Fe {
    fe_mul(a, a)
}

fn fe_inv(i: &Fe) -> Fe {
    let mut c = *i;
    for a in (0..=253).rev() {
        c = fe_sq(&c);
        if a != 2 && a != 4 {
            c = fe_mul(&c, i);
        }
    }
    c
}

fn fe_to_bytes(i: &Fe) -> [u8; 32] {
    let mut t = *i;
    fe_carry(&mut t);
    fe_carry(&mut t);
    fe_carry(&mut t);
    for _ in 0..2 {
        let mut m = [0i64; 16];
        m[0] = t[0] - 0xffed;
        for j in 1..15 {
            m[j] = t[j] - 0xffff - ((m[j - 1] >> 16) & 1);
            m[j - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        fe_sel(&mut t, &mut m, 1 - b);
    }
    let mut o = [0u8; 32];
    for j in 0..16 {
        o[2 * j] = (t[j] & 0xff) as u8;
        o[2 * j + 1] = ((t[j] >> 8) & 0xff) as u8;
    }
    o
}

pub fn x25519_scalarmult(n: &[u8; 32], p: &[u8; 32]) -> [u8; 32] {
    let mut z = *n;
    z[0] &= 248;
    z[31] = (z[31] & 127) | 64;
    let x = fe_from_bytes(p);
    let mut a = fe_one();
    let mut b = x;
    let mut c = fe_zero();
    let mut d = fe_one();
    let mut e;
    let mut f;
    let mut c121665 = fe_zero();
    c121665[0] = 0xdb41;
    c121665[1] = 1;

    for i in (0..=254).rev() {
        let r = ((z[i >> 3] >> (i & 7)) & 1) as i64;
        fe_sel(&mut a, &mut b, r);
        fe_sel(&mut c, &mut d, r);
        e = fe_add(&a, &c);
        a = fe_sub(&a, &c);
        c = fe_add(&b, &d);
        b = fe_sub(&b, &d);
        d = fe_sq(&e);
        f = fe_sq(&a);
        a = fe_mul(&c, &a);
        c = fe_mul(&b, &e);
        e = fe_add(&a, &c);
        a = fe_sub(&a, &c);
        b = fe_sq(&a);
        c = fe_sub(&d, &f);
        a = fe_mul(&c, &c121665);
        a = fe_add(&a, &d);
        c = fe_mul(&c, &a);
        a = fe_mul(&d, &f);
        d = fe_mul(&b, &x);
        b = fe_sq(&e);
        fe_sel(&mut a, &mut b, r);
        fe_sel(&mut c, &mut d, r);
    }
    fe_to_bytes(&fe_mul(&a, &fe_inv(&c)))
}

fn x25519_public_from_private(priv_key: &[u8; 32]) -> [u8; 32] {
    let mut base = [0u8; 32];
    base[0] = 9;
    x25519_scalarmult(priv_key, &base)
}

pub fn generate_xiaomi_x25519_keypair() -> (String, [u8; 32]) {
    let raw = random_bytes(32);
    let mut priv_key = [0u8; 32];
    priv_key.copy_from_slice(&raw);
    priv_key[0] &= 248;
    priv_key[31] = (priv_key[31] & 127) | 64;

    let pub_raw = x25519_public_from_private(&priv_key);
    // X25519 SPKI DER header (12 bytes): 302a300506032b656e032100
    let mut spki = Vec::with_capacity(44);
    spki.extend_from_slice(&[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
    ]);
    spki.extend_from_slice(&pub_raw);
    (B64_STANDARD.encode(&spki), priv_key)
}

#[derive(Debug, Clone)]
pub struct XiaomiCallbackPayload {
    pub uid: Option<String>,
    pub sk: Option<String>,
    pub url: String,
}

pub fn decrypt_xiaomi_callback(
    private_key: &[u8; 32],
    encrypted_b64: &str,
) -> Result<XiaomiCallbackPayload, String> {
    let raw = b64_std_decode(encrypted_b64)?;
    if raw.len() < 12 + 32 + 16 + 1 {
        return Err(format!("Encrypted payload too short: {} bytes", raw.len()));
    }

    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&raw[0..12]);
    let mut eph_pub = [0u8; 32];
    eph_pub.copy_from_slice(&raw[12..44]);
    let mut ciphertext_and_tag = raw[44..].to_vec();

    let shared_secret = x25519_scalarmult(private_key, &eph_pub);
    let derived = digest::digest(&digest::SHA256, &shared_secret);

    let unbound = UnboundKey::new(&AES_256_GCM, derived.as_ref())
        .map_err(|_| "AES-256-GCM key init failed".to_string())?;
    let key = LessSafeKey::new(unbound);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let plaintext = key
        .open_in_place(nonce, Aad::empty(), &mut ciphertext_and_tag)
        .map_err(|_| "AES-256-GCM decryption/authentication failed".to_string())?;

    let parsed: Value = serde_json::from_slice(plaintext)
        .map_err(|e| format!("Decrypted payload is not valid JSON: {}", e))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "Decrypted payload is not a valid object".to_string())?;

    let uid = obj.get("uid").and_then(json_value_as_string);
    let sk = obj.get("sk").and_then(json_value_as_string);
    let url = obj
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.xiaomimimo.com/v1".to_string());

    Ok(XiaomiCallbackPayload { uid, sk, url })
}

fn json_value_as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// --- Zed RSA-2048 helpers (via OpenSSL CLI) ---

pub fn encode_zed_private_key_verifier(private_key_pem: &str) -> String {
    format!(
        "{}{}",
        ZED_PRIVATE_KEY_PREFIX,
        b64url_encode(private_key_pem.as_bytes())
    )
}

pub fn decode_zed_private_key_verifier(verifier: &str) -> Result<String, String> {
    if !verifier.starts_with(ZED_PRIVATE_KEY_PREFIX) {
        return Err("Missing Zed private key verifier; restart the login flow".to_string());
    }
    let b64 = &verifier[ZED_PRIVATE_KEY_PREFIX.len()..];
    let bytes = b64url_decode(b64)?;
    String::from_utf8(bytes).map_err(|e| format!("Invalid Zed private key encoding: {}", e))
}

fn generate_rsa2048_keypair() -> Result<(Vec<u8>, String), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let gen_out = Command::new("openssl")
        .args(["genrsa", "-traditional", "2048"])
        .output()
        .or_else(|_| Command::new("openssl").args(["genrsa", "2048"]).output())
        .map_err(|e| format!("Failed to run openssl genrsa: {}", e))?;

    let priv_pem_bytes = if gen_out.status.success() {
        gen_out.stdout
    } else {
        let fallback = Command::new("openssl")
            .args(["genrsa", "2048"])
            .output()
            .map_err(|e| format!("Failed to run openssl genrsa: {}", e))?;
        if !fallback.status.success() {
            return Err(format!(
                "openssl genrsa failed: {}",
                String::from_utf8_lossy(&fallback.stderr)
            ));
        }
        fallback.stdout
    };

    let priv_pem = String::from_utf8(priv_pem_bytes.clone())
        .map_err(|e| format!("Invalid RSA private key PEM: {}", e))?;

    let mut child = Command::new("openssl")
        .args(["rsa", "-RSAPublicKey_out", "-outform", "DER"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run openssl rsa: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&priv_pem_bytes);
    }
    let pub_out = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for openssl rsa: {}", e))?;
    if !pub_out.status.success() {
        return Err(format!(
            "openssl rsa public key extraction failed: {}",
            String::from_utf8_lossy(&pub_out.stderr)
        ));
    }

    Ok((pub_out.stdout, priv_pem))
}

pub fn create_zed_native_auth_data(
    web_base_url: Option<&str>,
    native_app_port: Option<u16>,
    system_id: Option<&str>,
) -> Result<Value, String> {
    let (pub_der, priv_pem) = generate_rsa2048_keypair()?;
    let port = native_app_port.unwrap_or(ZED_DEFAULT_NATIVE_APP_PORT);
    let sys_id = system_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(uuid_v4);
    let pub_b64url_padded = B64_STANDARD
        .encode(&pub_der)
        .replace('+', "-")
        .replace('/', "_");

    let base = web_base_url
        .unwrap_or(ZED_WEB_BASE_URL)
        .trim_end_matches('/');
    let mut url = url::Url::parse(&format!("{}/native_app_signin", base))
        .map_err(|e| format!("Invalid Zed base URL: {}", e))?;
    url.query_pairs_mut()
        .append_pair("native_app_port", &port.to_string())
        .append_pair("native_app_public_key", &pub_b64url_padded)
        .append_pair("system_id", &sys_id);

    Ok(json!({
        "authUrl": url.to_string(),
        "privateKeyVerifier": encode_zed_private_key_verifier(&priv_pem),
        "nativeAppPort": port,
        "systemId": sys_id,
        "publicKey": pub_b64url_padded,
    }))
}

pub fn decrypt_zed_access_token(
    encrypted_access_token: &str,
    private_key_verifier: &str,
) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let private_key_pem = decode_zed_private_key_verifier(private_key_verifier)?;
    let encrypted = b64url_decode(encrypted_access_token)?;

    let key_path = std::env::temp_dir().join(format!("9r-zed-{}.pem", uuid_v4()));
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&key_path)
                .map_err(|e| format!("Failed to write temp Zed key: {}", e))?;
            f.write_all(private_key_pem.as_bytes())
                .map_err(|e| format!("Failed to write temp Zed key: {}", e))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&key_path, private_key_pem.as_bytes())
                .map_err(|e| format!("Failed to write temp Zed key: {}", e))?;
        }
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(key_path.clone());

    let run_decrypt = |extra_args: &[&str]| -> Result<Vec<u8>, String> {
        let mut child = Command::new("openssl")
            .arg("pkeyutl")
            .arg("-decrypt")
            .arg("-inkey")
            .arg(&key_path)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn openssl pkeyutl: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&encrypted);
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("Failed to wait for openssl pkeyutl: {}", e))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };

    match run_decrypt(&[
        "-pkeyopt",
        "rsa_padding_mode:oaep",
        "-pkeyopt",
        "rsa_oaep_md:sha256",
    ]) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|e| format!("Failed to decrypt Zed access token: {}", e)),
        Err(oaep_err) => {
            if let Ok(bytes) = run_decrypt(&["-pkeyopt", "rsa_padding_mode:pkcs1"]) {
                let text = String::from_utf8_lossy(&bytes).to_string();
                if !text.contains('\u{FFFD}') {
                    return Ok(text);
                }
            }
            Err(format!("Failed to decrypt Zed access token: {}", oaep_err))
        }
    }
}

pub fn parse_zed_callback_payload(input: &str) -> Result<(String, String), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Missing Zed callback URL".to_string());
    }

    let mut data: HashMap<String, String> = HashMap::new();
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(raw) {
        for (k, v) in obj {
            if let Some(s) = json_value_as_string(&v) {
                data.insert(k, s);
            }
        }
    } else {
        let parsed = url::Url::parse(raw).or_else(|_| {
            let query = if let Some(idx) = raw.find('?') {
                &raw[idx + 1..]
            } else {
                raw.trim_start_matches('?')
            };
            url::Url::parse(&format!("http://127.0.0.1/?{}", query))
        });
        let url = parsed.map_err(|_| "Invalid Zed callback URL".to_string())?;
        for (k, v) in url.query_pairs() {
            data.insert(k.into_owned(), v.into_owned());
        }
    }

    let user_id = data
        .get("user_id")
        .or_else(|| data.get("userId"))
        .cloned()
        .unwrap_or_default();
    let encrypted = data
        .get("access_token")
        .or_else(|| data.get("accessToken"))
        .or_else(|| data.get("token"))
        .cloned()
        .unwrap_or_default();

    if user_id.is_empty() || encrypted.is_empty() {
        return Err("Zed callback must include user_id and access_token".to_string());
    }
    Ok((user_id, encrypted))
}

// --- JWT helpers ---

pub fn decode_jwt_payload(jwt: &str) -> Option<Map<String, Value>> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let bytes = b64_std_decode(parts[1]).ok()?;
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

pub fn decode_xai_id_token_email(id_token: &str) -> Option<String> {
    let payload = decode_jwt_payload(id_token)?;
    payload
        .get("email")
        .or_else(|| payload.get("preferred_username"))
        .or_else(|| payload.get("sub"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

pub fn extract_email_from_jwt(token: &str) -> Option<String> {
    decode_xai_id_token_email(token)
}

#[derive(Debug, Default, Clone)]
pub struct CodexAccountInfo {
    pub email: Option<String>,
    pub chatgpt_account_id: Option<String>,
    pub chatgpt_plan_type: Option<String>,
}

pub fn extract_codex_account_info(token: &str) -> CodexAccountInfo {
    let Some(payload) = decode_jwt_payload(token) else {
        return CodexAccountInfo::default();
    };
    let chatgpt = payload
        .get("https://api.openai.com/auth")
        .and_then(|v| v.as_object());
    let email = payload
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chatgpt_account_id = chatgpt
        .and_then(|m| m.get("chatgpt_account_id"))
        .or_else(|| payload.get("account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chatgpt_plan_type = chatgpt
        .and_then(|m| m.get("chatgpt_plan_type"))
        .or_else(|| payload.get("plan_type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    CodexAccountInfo {
        email,
        chatgpt_account_id,
        chatgpt_plan_type,
    }
}

// ============================================================================
// Database Persistence Helpers (connectionsRepo parity)
// ============================================================================

#[derive(Debug, Default)]
struct ConnectionCreateInput {
    provider: String,
    auth_type: String,
    name: Option<String>,
    email: Option<String>,
    display_name: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    api_key: Option<String>,
    expires_at: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    project_id: Option<String>,
    token_type: Option<String>,
    last_refresh_at: Option<String>,
    test_status: Option<String>,
    is_active: Option<bool>,
    priority: Option<i64>,
    provider_specific_data: Option<Map<String, Value>>,
    extra_fields: Map<String, Value>,
}

fn create_provider_connection(
    db: &Database,
    input: ConnectionCreateInput,
) -> Result<ProviderConnection, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let all = db
        .load_provider_connections()
        .map_err(|e| format!("DB load error: {}", e))?;
    let provider_conns: Vec<&ProviderConnection> = all
        .iter()
        .filter(|c| c.provider == input.provider)
        .collect();
    let pool_size = provider_conns.len();

    let incoming_psd = input.provider_specific_data.as_ref();
    let incoming_username = incoming_psd
        .and_then(|m| m.get("username"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let incoming_ws = incoming_psd
        .and_then(|m| m.get("chatgptAccountId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let existing: Option<&ProviderConnection> = if input.auth_type == "oauth" {
        if let Some(ref email) = input.email {
            if email.is_empty() {
                None
            } else {
                provider_conns
                    .iter()
                    .copied()
                    .find(|c| {
                        if c.auth_type != "oauth" || c.email.as_deref() != Some(email.as_str()) {
                            return false;
                        }
                        let raw_val: Value =
                            serde_json::from_str(&c.raw_data).unwrap_or(Value::Null);
                        let existing_psd = raw_val
                            .get("providerSpecificData")
                            .and_then(|v| v.as_object());
                        let existing_ws = existing_psd
                            .and_then(|m| m.get("chatgptAccountId"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty());
                        if input.provider == "codex" {
                            return matches!((incoming_ws, existing_ws), (Some(a), Some(b)) if a == b);
                        }
                        match (incoming_ws, existing_ws) {
                            (Some(a), Some(b)) => return a == b,
                            (Some(_), None) | (None, Some(_)) => return false,
                            (None, None) => {}
                        }
                        let existing_username = existing_psd
                            .and_then(|m| m.get("username"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty());
                        match (incoming_username, existing_username) {
                            (Some(a), Some(b)) => a == b,
                            (Some(_), None) | (None, Some(_)) => false,
                            (None, None) => true,
                        }
                    })
            }
        } else {
            None
        }
    } else if input.auth_type == "apikey" {
        if let Some(ref name) = input.name {
            provider_conns
                .iter()
                .copied()
                .find(|c| c.auth_type == "apikey" && c.name.as_deref() == Some(name.as_str()))
        } else {
            None
        }
    } else {
        None
    };

    let mut raw_map = if let Some(ex) = existing {
        serde_json::from_str::<Value>(&ex.raw_data)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    } else {
        Map::new()
    };

    for (k, v) in input.extra_fields {
        if !matches!(
            k.as_str(),
            "id" | "provider"
                | "authType"
                | "name"
                | "email"
                | "priority"
                | "isActive"
                | "createdAt"
                | "updatedAt"
        ) {
            raw_map.insert(k, v);
        }
    }
    if let Some(ref v) = input.display_name {
        raw_map.insert("displayName".to_string(), json!(v));
    }
    if let Some(ref v) = input.access_token {
        raw_map.insert("accessToken".to_string(), json!(v));
    }
    if let Some(ref v) = input.refresh_token {
        raw_map.insert("refreshToken".to_string(), json!(v));
    }
    if let Some(ref v) = input.id_token {
        raw_map.insert("idToken".to_string(), json!(v));
    }
    if let Some(ref v) = input.api_key {
        raw_map.insert("apiKey".to_string(), json!(v));
    }
    if let Some(ref v) = input.expires_at {
        raw_map.insert("expiresAt".to_string(), json!(v));
    }
    if let Some(v) = input.expires_in {
        raw_map.insert("expiresIn".to_string(), json!(v));
    }
    if let Some(ref v) = input.scope {
        raw_map.insert("scope".to_string(), json!(v));
    }
    if let Some(ref v) = input.project_id {
        raw_map.insert("projectId".to_string(), json!(v));
    }
    if let Some(ref v) = input.token_type {
        raw_map.insert("tokenType".to_string(), json!(v));
    }
    if let Some(ref v) = input.last_refresh_at {
        raw_map.insert("lastRefreshAt".to_string(), json!(v));
    }
    if let Some(psd) = input.provider_specific_data {
        if !psd.is_empty() {
            raw_map.insert("providerSpecificData".to_string(), Value::Object(psd));
        }
    }
    let test_status = input.test_status.as_deref().unwrap_or("active");
    raw_map.insert("testStatus".to_string(), json!(test_status));
    if test_status == "active" {
        raw_map.remove("lastError");
        raw_map.remove("lastErrorAt");
        raw_map.remove("errorCode");
        raw_map.remove("rateLimitedUntil");
        raw_map.insert("backoffLevel".to_string(), json!(0));
        let model_lock_keys: Vec<String> = raw_map
            .keys()
            .filter(|k| k.starts_with("modelLock_"))
            .cloned()
            .collect();
        for k in model_lock_keys {
            raw_map.remove(&k);
        }
    }

    let raw_data = Value::Object(raw_map.clone()).to_string();
    let parsed_data: ConnectionData = serde_json::from_str(&raw_data).unwrap_or_default();

    let conn = if let Some(ex) = existing {
        ProviderConnection {
            id: ex.id.clone(),
            provider: ex.provider.clone(),
            auth_type: input.auth_type,
            name: input.name.or_else(|| ex.name.clone()),
            email: input.email.or_else(|| ex.email.clone()),
            priority: input.priority.or(ex.priority),
            is_active: input.is_active.unwrap_or(ex.is_active),
            data: parsed_data,
            raw_data,
            created_at: ex.created_at.clone(),
            updated_at: now,
        }
    } else {
        let mut conn_name = input.name;
        if conn_name.is_none()
            && (input.auth_type == "oauth" || input.auth_type == "access_token")
        {
            let fallback = input
                .email
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Account {}", pool_size + 1));
            if input.provider == "github" {
                let psd = raw_map
                    .get("providerSpecificData")
                    .and_then(|v| v.as_object());
                conn_name = psd
                    .and_then(|m| m.get("githubLogin"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        psd.and_then(|m| m.get("githubEmail"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| input.email.clone().filter(|s| !s.is_empty()))
                    .or_else(|| {
                        psd.and_then(|m| m.get("githubName"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                    })
                    .or(Some(fallback));
            } else {
                conn_name = Some(fallback);
            }
        }
        let max_priority = provider_conns
            .iter()
            .filter_map(|c| c.priority)
            .max()
            .unwrap_or(0);
        let priority = input.priority.unwrap_or(max_priority + 1);

        ProviderConnection {
            id: uuid_v4(),
            provider: input.provider,
            auth_type: input.auth_type,
            name: conn_name,
            email: input.email,
            priority: Some(priority),
            is_active: input.is_active.unwrap_or(true),
            data: parsed_data,
            raw_data,
            created_at: now.clone(),
            updated_at: now,
        }
    };

    db.save_provider_connection(&conn)
        .map_err(|e| format!("Failed to save connection: {}", e))?;
    Ok(conn)
}

fn input_from_mapped_tokens(
    provider: &str,
    auth_type: &str,
    mapped: &Value,
) -> ConnectionCreateInput {
    let obj = mapped.as_object();
    let str_field = |k: &str| {
        obj.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let expires_in = obj
        .and_then(|m| m.get("expiresIn"))
        .and_then(|v| v.as_i64());
    let expires_at = str_field("expiresAt").or_else(|| {
        expires_in
            .filter(|&s| s > 0)
            .map(|s| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339())
    });
    let psd = obj
        .and_then(|m| m.get("providerSpecificData"))
        .and_then(|v| v.as_object())
        .cloned();

    ConnectionCreateInput {
        provider: provider.to_string(),
        auth_type: auth_type.to_string(),
        name: str_field("name"),
        email: str_field("email"),
        display_name: str_field("displayName"),
        access_token: str_field("accessToken"),
        refresh_token: str_field("refreshToken"),
        id_token: str_field("idToken"),
        api_key: str_field("apiKey"),
        expires_at,
        expires_in,
        scope: str_field("scope"),
        project_id: str_field("projectId"),
        token_type: str_field("tokenType"),
        last_refresh_at: str_field("lastRefreshAt"),
        test_status: Some("active".to_string()),
        is_active: Some(true),
        priority: None,
        provider_specific_data: psd,
        extra_fields: Map::new(),
    }
}

// ============================================================================
// AWS Region & Kiro External IdP Validation
// ============================================================================

pub fn is_valid_aws_region(region: &str) -> bool {
    // ^[a-z]{2}-[a-z]+-\d{1,2}$
    let parts: Vec<&str> = region.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let p0 = parts[0];
    let p1 = parts[1];
    let p2 = parts[2];
    p0.len() == 2
        && p0.bytes().all(|b| b.is_ascii_lowercase())
        && !p1.is_empty()
        && p1.bytes().all(|b| b.is_ascii_lowercase())
        && (1..=2).contains(&p2.len())
        && p2.bytes().all(|b| b.is_ascii_digit())
}

pub fn assert_valid_aws_region(region: &str) -> Result<String, String> {
    if !is_valid_aws_region(region) {
        return Err("Invalid region".to_string());
    }
    Ok(region.to_string())
}

pub fn validate_microsoft_token_endpoint(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("token_endpoint is required".to_string());
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|_| "token_endpoint must be a valid URL".to_string())?;
    if parsed.scheme() != "https" {
        return Err("token_endpoint must use https".to_string());
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "login.microsoftonline.com" | "login.microsoft.com" | "login.windows.net"
    ) {
        return Err("token_endpoint must be a Microsoft login endpoint".to_string());
    }
    Ok(parsed.to_string())
}

pub fn normalize_kiro_external_idp_auth(raw_auth: &Value) -> Result<Value, String> {
    let parsed_storage;
    let obj = if let Some(s) = raw_auth.as_str() {
        parsed_storage = serde_json::from_str::<Value>(s)
            .map_err(|_| "CLIProxyAPI auth JSON is invalid".to_string())?;
        parsed_storage
            .as_object()
            .ok_or_else(|| "CLIProxyAPI auth JSON is required".to_string())?
    } else if let Some(m) = raw_auth.as_object() {
        m
    } else {
        return Err("CLIProxyAPI auth JSON is required".to_string());
    };

    let get_str = |k1: &str, k2: &str| -> String {
        obj.get(k1)
            .or_else(|| obj.get(k2))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };

    let auth_method = get_str("auth_method", "authMethod");
    if !auth_method.is_empty() && auth_method != "external_idp" {
        return Err("Only external_idp Kiro auth is supported by this importer".to_string());
    }

    let access_token = get_str("access_token", "accessToken");
    let refresh_token = get_str("refresh_token", "refreshToken");
    let client_id = get_str("client_id", "clientId");
    let token_endpoint =
        validate_microsoft_token_endpoint(&get_str("token_endpoint", "tokenEndpoint"))?;
    let profile_arn = get_str("profile_arn", "profileArn");
    let region_raw = obj
        .get("region")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let region = if region_raw.is_empty() {
        "us-east-1".to_string()
    } else {
        region_raw
    };

    let scope = match obj.get("scopes").or_else(|| obj.get("scope")) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };

    if access_token.is_empty() {
        return Err("access_token is required".to_string());
    }
    if refresh_token.is_empty() {
        return Err("refresh_token is required".to_string());
    }
    if client_id.is_empty() {
        return Err("client_id is required".to_string());
    }
    if scope.is_empty() {
        return Err("scopes is required".to_string());
    }
    if profile_arn.is_empty() {
        return Err("profile_arn is required".to_string());
    }

    let payload = decode_jwt_payload(&access_token);
    let email = obj
        .get("email")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            payload.as_ref().and_then(|p| {
                p.get("email")
                    .or_else(|| p.get("preferred_username"))
                    .or_else(|| p.get("upn"))
                    .or_else(|| p.get("sub"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        });

    let expires_at = resolve_kiro_external_expires_at(obj, &access_token, payload.as_ref());

    Ok(json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at,
        "email": email,
        "providerSpecificData": {
            "profileArn": profile_arn,
            "region": region,
            "authMethod": "external_idp",
            "provider": "CLIProxyAPI",
            "clientId": client_id,
            "tokenEndpoint": token_endpoint,
            "scope": scope,
        }
    }))
}

fn resolve_kiro_external_expires_at(
    obj: &Map<String, Value>,
    _access_token: &str,
    payload: Option<&Map<String, Value>>,
) -> String {
    for k in ["expired", "expires_at", "expiresAt"] {
        if let Some(v) = obj.get(k) {
            if let Some(s) = v.as_str() {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                    return dt.with_timezone(&chrono::Utc).to_rfc3339();
                }
            } else if let Some(n) = v.as_i64() {
                let secs = if n > 1_000_000_000_000 { n / 1000 } else { n };
                if let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) {
                    return dt.to_rfc3339();
                }
            }
        }
    }
    if let Some(secs) = obj
        .get("expires_in")
        .or_else(|| obj.get("expiresIn"))
        .and_then(|v| v.as_i64())
        .filter(|&s| s > 0)
    {
        return (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339();
    }
    if let Some(exp) = payload
        .and_then(|p| p.get("exp"))
        .and_then(|v| v.as_i64())
    {
        if let Some(dt) = chrono::DateTime::from_timestamp(exp, 0) {
            return dt.to_rfc3339();
        }
    }
    (chrono::Utc::now() + chrono::Duration::seconds(3600)).to_rfc3339()
}

// ============================================================================
// Provider OAuth Flows (28 providers + kimi-coding alias)
// ============================================================================

fn canonical_provider(provider: &str) -> &str {
    if provider == "kimi-coding" {
        "kimi"
    } else {
        provider
    }
}

fn provider_flow_type(provider: &str) -> Result<&'static str, String> {
    match canonical_provider(provider) {
        "claude" | "codex" | "xai" | "gitlab" => Ok("authorization_code_pkce"),
        "antigravity" | "gemini-cli" | "iflow" | "cline" | "clinepass" | "trae" | "windsurf"
        | "zed" => Ok("authorization_code"),
        "grok-cli" | "muse" | "nous" | "qoder" | "qoder-cn" | "github" | "kiro" | "kimi"
        | "kilocode" | "codebuddy-cn" | "codebuddy-intl" | "glm" | "minimax-code"
        | "minimax-code-global" => Ok("device_code"),
        "cursor" => Ok("import_token"),
        "kimchi" => Ok("browser_token"),
        _ => Err(format!("Unknown provider: {}", provider)),
    }
}

fn provider_fixed_port(provider: &str) -> Option<u16> {
    match canonical_provider(provider) {
        "codex" => Some(CODEX_FIXED_PORT),
        "xai" => Some(XAI_LOOPBACK_PORT),
        _ => None,
    }
}

fn provider_callback_path(provider: &str) -> &'static str {
    match canonical_provider(provider) {
        "codex" => "/auth/callback",
        "windsurf" => "/windsurf-auth-callback",
        "zed" => "/",
        _ => "/callback",
    }
}

fn validate_xai_oauth_endpoint(raw_url: &str, field: &str) -> Result<String, String> {
    let value = raw_url.trim();
    if value.is_empty() {
        return Err(format!("xai discovery {} is empty", field));
    }
    let parsed = url::Url::parse(value)
        .map_err(|e| format!("xai discovery {} is invalid: {}", field, e))?;
    if parsed.scheme() != "https" {
        return Err(format!("xai discovery {} must use https: {}", field, value));
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        return Err(format!(
            "xai discovery {} host {} is not on x.ai",
            field, host
        ));
    }
    Ok(value.to_string())
}

async fn discover_xai_endpoints(client: &reqwest::Client) -> (String, String) {
    if let Ok(guard) = runtime().xai_discovery_cache.lock() {
        if let Some(cached) = guard.as_ref() {
            return cached.clone();
        }
    }

    let mut result = (XAI_AUTHORIZE_URL.to_string(), XAI_TOKEN_URL.to_string());
    if let Ok(resp) = client
        .get(XAI_DISCOVERY_URL)
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<Value>().await {
                if let (Some(auth_ep), Some(tok_ep)) = (
                    data.get("authorization_endpoint").and_then(|v| v.as_str()),
                    data.get("token_endpoint").and_then(|v| v.as_str()),
                ) {
                    if let (Ok(a), Ok(t)) = (
                        validate_xai_oauth_endpoint(auth_ep, "authorization_endpoint"),
                        validate_xai_oauth_endpoint(tok_ep, "token_endpoint"),
                    ) {
                        result = (a, t);
                    }
                }
            }
        }
    }

    if let Ok(mut guard) = runtime().xai_discovery_cache.lock() {
        *guard = Some(result.clone());
    }
    result
}

fn extract_json_path(root: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut cur = Some(root);
        for key in *path {
            cur = cur.and_then(|v| v.get(*key));
        }
        if let Some(val) = cur {
            if let Some(s) = val.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            } else if let Some(n) = val.as_i64() {
                return Some(n.to_string());
            } else if let Some(n) = val.as_u64() {
                return Some(n.to_string());
            }
        }
    }
    None
}

async fn fetch_trae_login_guidance(
    client: &reqwest::Client,
    login_trace_id: &str,
) -> Result<String, String> {
    let body = json!({
        "loginTraceID": login_trace_id,
        "login_trace_id": login_trace_id,
    });
    let mut last_err = "no successful response".to_string();
    for url in TRAE_GUIDANCE_URLS {
        match client
            .post(*url)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::USER_AGENT, TRAE_USER_AGENT)
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
        {
            Ok(res) => {
                if !res.status().is_success() {
                    last_err = format!("{} HTTP {}", url, res.status().as_u16());
                    continue;
                }
                if let Ok(data) = res.json::<Value>().await {
                    if let Some(host) = extract_json_path(
                        &data,
                        &[
                            &["Result", "LoginHost"],
                            &["Result", "loginHost"],
                            &["Result", "LoginURL"],
                            &["result", "loginHost"],
                            &["data", "Result", "LoginHost"],
                            &["data", "loginHost"],
                            &["LoginHost"],
                            &["loginHost"],
                        ],
                    ) {
                        return Ok(host);
                    }
                    last_err = format!("{} missing LoginHost", url);
                } else {
                    last_err = format!("{} invalid JSON", url);
                }
            }
            Err(e) => {
                last_err = format!("{} {}", url, e);
            }
        }
    }
    Err(format!("Trae GetLoginGuidance failed: {}", last_err))
}

fn build_trae_verification_url(
    login_host: &str,
    login_trace_id: &str,
    callback_url: &str,
) -> Result<String, String> {
    let normalized = if login_host.starts_with("http") {
        login_host.to_string()
    } else {
        format!("https://{}", login_host.trim_start_matches('/'))
    };
    let mut url = url::Url::parse(&normalized)
        .map_err(|e| format!("Invalid Trae loginHost {}: {}", login_host, e))?;
    url.set_path("/authorization");
    let machine_id = uuid_v4();
    url.query_pairs_mut()
        .append_pair("login_version", "1")
        .append_pair("auth_from", "trae")
        .append_pair("login_channel", "native_ide")
        .append_pair("plugin_version", "local")
        .append_pair("auth_type", "local")
        .append_pair("client_id", TRAE_CLIENT_ID)
        .append_pair("redirect", "0")
        .append_pair("login_trace_id", login_trace_id)
        .append_pair("auth_callback_url", callback_url)
        .append_pair("machine_id", &machine_id)
        .append_pair("device_id", "0")
        .append_pair("x_device_id", "0")
        .append_pair("x_machine_id", &machine_id)
        .append_pair("x_device_brand", "unknown")
        .append_pair("x_device_type", "unknown")
        .append_pair("x_os_version", "unknown")
        .append_pair("x_env", "")
        .append_pair("x_app_version", TRAE_DEFAULT_APP_VERSION)
        .append_pair("x_app_type", "stable");
    Ok(url.to_string())
}

fn oauth_platform_enum() -> u8 {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => 2,
        ("macos", _) => 1,
        ("linux", "aarch64") => 4,
        ("linux", _) => 3,
        ("windows", _) => 5,
        _ => 0,
    }
}

fn kimi_client_id() -> String {
    std::env::var("KIMI_CODING_OAUTH_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("KIMI_OAUTH_CLIENT_ID")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| KIMI_DEFAULT_CLIENT_ID.to_string())
}

fn kimi_headers(device_id: Option<&str>) -> Vec<(&'static str, String)> {
    let os_name = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    let device_model = format!("{} {}", os_name, arch);
    let device_name = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let resolved_id = device_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("kimi-{}", chrono::Utc::now().timestamp_millis()));

    vec![
        ("X-Msh-Platform", "9router".to_string()),
        ("X-Msh-Version", env!("CARGO_PKG_VERSION").to_string()),
        ("X-Msh-Device-Name", device_name),
        ("X-Msh-Device-Model", device_model),
        ("X-Msh-Device-Id", resolved_id),
    ]
}

async fn generate_auth_data(
    client: &reqwest::Client,
    provider: &str,
    redirect_uri: Option<&str>,
    meta: &HashMap<String, String>,
) -> Result<Value, String> {
    let canon = canonical_provider(provider);
    let flow_type = provider_flow_type(canon)?;
    let verifier_bytes = if canon == "xai" {
        XAI_PKCE_VERIFIER_BYTES
    } else {
        32
    };
    let (pkce_verifier, code_challenge, pkce_state) = generate_pkce(verifier_bytes);

    let mut state = pkce_state.clone();
    let mut code_verifier = pkce_verifier.clone();
    let mut system_id: Option<String> = None;

    let auth_url: Option<String> = if flow_type == "device_code" || flow_type == "import_token" {
        None
    } else {
        let ruri = redirect_uri.unwrap_or("http://localhost:8080/callback");
        match canon {
            "claude" => {
                let mut u = url::Url::parse(CLAUDE_AUTHORIZE_URL).unwrap();
                u.query_pairs_mut()
                    .append_pair("code", "true")
                    .append_pair("client_id", CLAUDE_CLIENT_ID)
                    .append_pair("response_type", "code")
                    .append_pair("redirect_uri", ruri)
                    .append_pair("scope", "org:create_api_key user:profile user:inference")
                    .append_pair("code_challenge", &code_challenge)
                    .append_pair("code_challenge_method", "S256")
                    .append_pair("state", &state);
                Some(u.to_string())
            }
            "codex" => {
                let qs = build_query_uri_component(&[
                    ("response_type", "code"),
                    ("client_id", CODEX_CLIENT_ID),
                    ("redirect_uri", ruri),
                    ("scope", "openid profile email offline_access"),
                    ("code_challenge", &code_challenge),
                    ("code_challenge_method", "S256"),
                    ("id_token_add_organizations", "true"),
                    ("codex_cli_simplified_flow", "true"),
                    ("originator", "codex_cli_rs"),
                    ("state", &state),
                ]);
                Some(format!("{}?{}", CODEX_AUTHORIZE_URL, qs))
            }
            "xai" => {
                let (auth_ep, _) = discover_xai_endpoints(client).await;
                let nonce = hex_encode(&random_bytes(16));
                let qs = build_query_uri_component(&[
                    ("response_type", "code"),
                    ("client_id", XAI_CLIENT_ID),
                    ("redirect_uri", ruri),
                    ("scope", XAI_SCOPE),
                    ("code_challenge", &code_challenge),
                    ("code_challenge_method", "S256"),
                    ("state", &state),
                    ("nonce", &nonce),
                    ("plan", "generic"),
                    ("referrer", "cli-proxy-api"),
                ]);
                Some(format!("{}?{}", auth_ep, qs))
            }
            "antigravity" => {
                let mut u = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
                u.query_pairs_mut()
                    .append_pair("client_id", ANTIGRAVITY_CLIENT_ID)
                    .append_pair("response_type", "code")
                    .append_pair("redirect_uri", ruri)
                    .append_pair(
                        "scope",
                        "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs",
                    )
                    .append_pair("state", &state)
                    .append_pair("access_type", "offline")
                    .append_pair("prompt", "consent");
                Some(u.to_string())
            }
            "gemini-cli" => {
                let mut u = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
                u.query_pairs_mut()
                    .append_pair("client_id", GOOGLE_CLIENT_ID)
                    .append_pair("response_type", "code")
                    .append_pair("redirect_uri", ruri)
                    .append_pair(
                        "scope",
                        "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
                    )
                    .append_pair("state", &state)
                    .append_pair("access_type", "offline")
                    .append_pair("prompt", "consent");
                Some(u.to_string())
            }
            "iflow" => {
                let mut u = url::Url::parse(IFLOW_AUTHORIZE_URL).unwrap();
                u.query_pairs_mut()
                    .append_pair("loginMethod", "phone")
                    .append_pair("type", "phone")
                    .append_pair("redirect", ruri)
                    .append_pair("state", &state)
                    .append_pair("client_id", IFLOW_CLIENT_ID);
                Some(u.to_string())
            }
            "cline" | "clinepass" => {
                let mut u = url::Url::parse(CLINE_AUTHORIZE_URL).unwrap();
                u.query_pairs_mut()
                    .append_pair("client_type", "extension")
                    .append_pair("callback_url", ruri)
                    .append_pair("redirect_uri", ruri);
                Some(u.to_string())
            }
            "gitlab" => {
                let base_url = meta
                    .get("baseUrl")
                    .map(|s| s.trim_end_matches('/'))
                    .filter(|s| !s.is_empty())
                    .unwrap_or("https://gitlab.com");
                let client_id = meta.get("clientId").map(|s| s.as_str()).unwrap_or("");
                let mut u = url::Url::parse(&format!("{}/oauth/authorize", base_url))
                    .map_err(|e| format!("Invalid GitLab baseUrl: {}", e))?;
                u.query_pairs_mut()
                    .append_pair("client_id", client_id)
                    .append_pair("redirect_uri", ruri)
                    .append_pair("response_type", "code")
                    .append_pair("state", &state)
                    .append_pair("scope", "api read_user")
                    .append_pair("code_challenge", &code_challenge)
                    .append_pair("code_challenge_method", "S256");
                Some(u.to_string())
            }
            "kimchi" => {
                let base = KIMCHI_WEB_APP_URL.trim_end_matches('/');
                let mut u = url::Url::parse(&format!("{}/cli-auth", base)).unwrap();
                u.query_pairs_mut()
                    .append_pair("callback", ruri)
                    .append_pair("state", &state);
                Some(u.to_string())
            }
            "trae" => {
                let trace_id = uuid_v4();
                let login_host = fetch_trae_login_guidance(client, &trace_id).await?;
                state = trace_id.clone();
                Some(build_trae_verification_url(&login_host, &trace_id, ruri)?)
            }
            "windsurf" => {
                let mut u =
                    url::Url::parse(&format!("{}/windsurf/signin", WINDSURF_AUTH_BASE)).unwrap();
                u.query_pairs_mut()
                    .append_pair("response_type", "token")
                    .append_pair("client_id", WINDSURF_CLIENT_ID)
                    .append_pair("redirect_uri", ruri)
                    .append_pair("state", &state)
                    .append_pair("prompt", "login")
                    .append_pair("redirect_parameters_type", "query")
                    .append_pair("workflow", "onboarding");
                Some(u.to_string())
            }
            "zed" => {
                let port = meta
                    .get("nativeAppPort")
                    .and_then(|s| s.parse::<u16>().ok());
                let zed_data = create_zed_native_auth_data(None, port, None)?;
                code_verifier = zed_data
                    .get("privateKeyVerifier")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                system_id = zed_data
                    .get("systemId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                zed_data
                    .get("authUrl")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            }
            _ => None,
        }
    };

    if let Ok(mut store) = runtime().pkce_store.lock() {
        store.insert(
            PkceEntry {
                provider: canon.to_string(),
                state: state.clone(),
                code_verifier: code_verifier.clone(),
                code_challenge: code_challenge.clone(),
                redirect_uri: redirect_uri.map(|s| s.to_string()),
                system_id: system_id.clone(),
                created_at: Instant::now(),
            },
            Duration::from_secs(PKCE_SESSION_TTL_SECS),
        );
    }

    let mut out = json!({
        "authUrl": auth_url,
        "state": state,
        "codeVerifier": code_verifier,
        "codeChallenge": code_challenge,
        "redirectUri": redirect_uri,
        "flowType": flow_type,
        "fixedPort": provider_fixed_port(canon),
        "callbackPath": provider_callback_path(canon),
    });
    if let Some(sys_id) = system_id {
        out.as_object_mut()
            .unwrap()
            .insert("systemId".to_string(), json!(sys_id));
    }
    Ok(out)
}

async fn exchange_tokens(
    client: &reqwest::Client,
    provider: &str,
    code: &str,
    redirect_uri: Option<&str>,
    code_verifier: Option<&str>,
    state: Option<&str>,
    meta: &Value,
) -> Result<Value, String> {
    let canon = canonical_provider(provider);
    let ruri = redirect_uri.unwrap_or("");
    let verifier = code_verifier.unwrap_or("");

    match canon {
        "claude" => {
            let (auth_code, code_state) = match code.split_once('#') {
                Some((c, s)) => (c, s),
                None => (code, ""),
            };
            let st = if !code_state.is_empty() {
                code_state
            } else {
                state.unwrap_or("")
            };
            let resp = client
                .post(CLAUDE_TOKEN_URL)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .json(&json!({
                    "code": auth_code,
                    "state": st,
                    "grant_type": "authorization_code",
                    "client_id": CLAUDE_CLIENT_ID,
                    "redirect_uri": ruri,
                    "code_verifier": verifier,
                }))
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("Token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("Token exchange invalid JSON: {}", e))?;
            Ok(json!({
                "accessToken": tokens.get("access_token").and_then(|v| v.as_str()),
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "scope": tokens.get("scope").and_then(|v| v.as_str()),
            }))
        }
        "codex" => {
            let form = [
                ("grant_type", "authorization_code"),
                ("client_id", CODEX_CLIENT_ID),
                ("code", code),
                ("redirect_uri", ruri),
                ("code_verifier", verifier),
            ];
            let resp = client
                .post(CODEX_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("Token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("Token exchange invalid JSON: {}", e))?;
            let id_token = tokens.get("id_token").and_then(|v| v.as_str());
            let access_token = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let info = id_token
                .map(extract_codex_account_info)
                .unwrap_or_default();
            let email = info
                .email
                .or_else(|| extract_email_from_jwt(access_token));
            let mut mapped = json!({
                "accessToken": access_token,
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "idToken": id_token,
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "lastRefreshAt": chrono::Utc::now().to_rfc3339(),
            });
            if let Some(em) = email {
                mapped
                    .as_object_mut()
                    .unwrap()
                    .insert("email".to_string(), json!(em));
            }
            if info.chatgpt_account_id.is_some() || info.chatgpt_plan_type.is_some() {
                mapped.as_object_mut().unwrap().insert(
                    "providerSpecificData".to_string(),
                    json!({
                        "chatgptAccountId": info.chatgpt_account_id,
                        "chatgptPlanType": info.chatgpt_plan_type,
                    }),
                );
            }
            Ok(mapped)
        }
        "xai" => {
            let (_, token_ep) = discover_xai_endpoints(client).await;
            let form = [
                ("grant_type", "authorization_code"),
                ("client_id", XAI_CLIENT_ID),
                ("code", code),
                ("redirect_uri", ruri),
                ("code_verifier", verifier),
            ];
            let resp = client
                .post(&token_ep)
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("xAI token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("xAI token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("xAI token exchange invalid JSON: {}", e))?;
            let id_token = tokens.get("id_token").and_then(|v| v.as_str());
            let email = id_token.and_then(decode_xai_id_token_email);
            let mut mapped = json!({
                "accessToken": tokens.get("access_token").and_then(|v| v.as_str()),
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "scope": tokens.get("scope").and_then(|v| v.as_str()),
            });
            if let Some(em) = email {
                mapped
                    .as_object_mut()
                    .unwrap()
                    .insert("email".to_string(), json!(em));
            }
            if let Some(idt) = id_token {
                mapped.as_object_mut().unwrap().insert(
                    "providerSpecificData".to_string(),
                    json!({ "idToken": idt }),
                );
            }
            Ok(mapped)
        }
        "antigravity" | "gemini-cli" => {
            let (cid, csec) = if canon == "antigravity" {
                (ANTIGRAVITY_CLIENT_ID, ANTIGRAVITY_CLIENT_SECRET)
            } else {
                (GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET)
            };
            let form = [
                ("grant_type", "authorization_code"),
                ("client_id", cid),
                ("client_secret", csec),
                ("code", code),
                ("redirect_uri", ruri),
            ];
            let resp = client
                .post("https://oauth2.googleapis.com/token")
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("Token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("Token exchange invalid JSON: {}", e))?;
            let access_token = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let mut ui_req = client
                .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
                .bearer_auth(access_token);
            if canon == "antigravity" {
                ui_req = ui_req.header("x-request-source", "local");
            }
            let user_info = match ui_req.timeout(Duration::from_secs(15)).send().await {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(json!({})),
                _ => json!({}),
            };

            let metadata = json!({
                "ideType": 9,
                "platform": oauth_platform_enum(),
                "pluginType": 2,
            });
            let mut project_id = String::new();
            let mut tier_id = "legacy-tier".to_string();

            let load_body = if canon == "antigravity" {
                json!({ "metadata": metadata })
            } else {
                json!({ "metadata": metadata, "mode": 1 })
            };
            let mut load_req = client
                .post(ANTIGRAVITY_LOAD_CODE_ASSIST_URL)
                .bearer_auth(access_token)
                .header(header::CONTENT_TYPE, "application/json");
            if canon == "antigravity" {
                load_req = load_req
                    .header(header::USER_AGENT, ANTIGRAVITY_IDE_USER_AGENT)
                    .header("x-request-source", "local");
            }
            if let Ok(r) = load_req
                .json(&load_body)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                if r.status().is_success() {
                    if let Ok(data) = r.json::<Value>().await {
                        project_id = data
                            .get("cloudaicompanionProject")
                            .and_then(|v| {
                                v.get("id")
                                    .and_then(|id| id.as_str())
                                    .or_else(|| v.as_str())
                            })
                            .unwrap_or("")
                            .to_string();
                        if canon == "antigravity" {
                            if let Some(tiers) = data.get("allowedTiers").and_then(|v| v.as_array())
                            {
                                for tier in tiers {
                                    if tier.get("isDefault").and_then(|v| v.as_bool()) == Some(true)
                                    {
                                        if let Some(id) = tier.get("id").and_then(|v| v.as_str()) {
                                            tier_id = id.trim().to_string();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Fire-and-forget onboarding for antigravity
            if canon == "antigravity" && !project_id.is_empty() {
                let c = client.clone();
                let tok = access_token.to_string();
                let meta_clone = metadata.clone();
                tokio::spawn(async move {
                    for _ in 0..10 {
                        let res = c
                            .post(ANTIGRAVITY_ONBOARD_USER_URL)
                            .bearer_auth(&tok)
                            .header(header::CONTENT_TYPE, "application/json")
                            .header(header::USER_AGENT, ANTIGRAVITY_IDE_USER_AGENT)
                            .header("x-request-source", "local")
                            .json(&json!({ "tierId": tier_id, "metadata": meta_clone }))
                            .timeout(Duration::from_secs(15))
                            .send()
                            .await;
                        match res {
                            Ok(r) if r.status().is_success() => {
                                if let Ok(v) = r.json::<Value>().await {
                                    if v.get("done").and_then(|d| d.as_bool()) == Some(true) {
                                        break;
                                    }
                                }
                            }
                            _ => break,
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                });
            }

            Ok(json!({
                "accessToken": access_token,
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "scope": tokens.get("scope").and_then(|v| v.as_str()),
                "email": user_info.get("email").and_then(|v| v.as_str()),
                "projectId": project_id,
            }))
        }
        "iflow" => {
            let basic = B64_STANDARD.encode(format!("{}:{}", IFLOW_CLIENT_ID, IFLOW_CLIENT_SECRET));
            let form = [
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", ruri),
                ("client_id", IFLOW_CLIENT_ID),
                ("client_secret", IFLOW_CLIENT_SECRET),
            ];
            let resp = client
                .post(IFLOW_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .header(header::AUTHORIZATION, format!("Basic {}", basic))
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("Token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("Token exchange invalid JSON: {}", e))?;
            let access_token = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let mut ui_url = url::Url::parse(IFLOW_USERINFO_URL).unwrap();
            ui_url
                .query_pairs_mut()
                .append_pair("accessToken", access_token);
            let ui_res = client
                .get(ui_url.as_str())
                .header(header::ACCEPT, "application/json")
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Failed to fetch user info: {}", e))?;
            if !ui_res.status().is_success() {
                let err = ui_res.text().await.unwrap_or_default();
                return Err(format!("Failed to fetch user info: {}", err));
            }
            let result: Value = ui_res
                .json()
                .await
                .map_err(|e| format!("Failed to parse user info: {}", e))?;
            if result.get("success").and_then(|v| v.as_bool()) != Some(true) {
                let msg = result
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error");
                return Err(format!("User info request failed: {}", msg));
            }
            let user_info = result.get("data").cloned().unwrap_or(json!({}));
            let api_key = user_info
                .get("apiKey")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Empty API key returned from iFlow".to_string())?;
            let email = user_info
                .get("email")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    user_info
                        .get("phone")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                })
                .ok_or_else(|| "Missing account email/phone in user info".to_string())?;
            let display_name = user_info
                .get("nickname")
                .or_else(|| user_info.get("name"))
                .and_then(|v| v.as_str());

            Ok(json!({
                "accessToken": access_token,
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "apiKey": api_key,
                "email": email,
                "displayName": display_name,
            }))
        }
        "cline" | "clinepass" => {
            let decoded_token = b64_std_decode(code)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|decoded| {
                    let idx = decoded.rfind('}')?;
                    serde_json::from_str::<Value>(&decoded[..=idx]).ok()
                });

            let token_data = if let Some(td) = decoded_token {
                json!({
                    "access_token": td.get("accessToken").and_then(|v| v.as_str()),
                    "refresh_token": td.get("refreshToken").and_then(|v| v.as_str()),
                    "email": td.get("email").and_then(|v| v.as_str()),
                    "firstName": td.get("firstName").and_then(|v| v.as_str()),
                    "lastName": td.get("lastName").and_then(|v| v.as_str()),
                    "expires_at": td.get("expiresAt").cloned(),
                })
            } else {
                let resp = client
                    .post(CLINE_TOKEN_URL)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json")
                    .json(&json!({
                        "grant_type": "authorization_code",
                        "code": code,
                        "client_type": "extension",
                        "redirect_uri": ruri,
                    }))
                    .timeout(Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| format!("{} token exchange failed: {}", canon, e))?;
                if !resp.status().is_success() {
                    let err = resp.text().await.unwrap_or_default();
                    return Err(format!("{} token exchange failed: {}", canon, err));
                }
                let data: Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("{} token exchange invalid JSON: {}", canon, e))?;
                let d = data.get("data");
                json!({
                    "access_token": d.and_then(|x| x.get("accessToken")).or_else(|| data.get("accessToken")).and_then(|v| v.as_str()),
                    "refresh_token": d.and_then(|x| x.get("refreshToken")).or_else(|| data.get("refreshToken")).and_then(|v| v.as_str()),
                    "email": d.and_then(|x| x.get("userInfo")).and_then(|u| u.get("email")).and_then(|v| v.as_str()).unwrap_or(""),
                    "expires_at": d.and_then(|x| x.get("expiresAt")).or_else(|| data.get("expiresAt")).cloned(),
                })
            };

            let expires_in = token_data
                .get("expires_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp() - chrono::Utc::now().timestamp())
                .unwrap_or(3600);

            Ok(json!({
                "accessToken": token_data.get("access_token"),
                "refreshToken": token_data.get("refresh_token"),
                "expiresIn": expires_in,
                "email": token_data.get("email"),
                "providerSpecificData": {
                    "firstName": token_data.get("firstName"),
                    "lastName": token_data.get("lastName"),
                }
            }))
        }
        "gitlab" => {
            let base_url = meta
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_end_matches('/'))
                .filter(|s| !s.is_empty())
                .unwrap_or("https://gitlab.com");
            let client_id = meta.get("clientId").and_then(|v| v.as_str()).unwrap_or("");
            let client_secret = meta
                .get("clientSecret")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let mut form = vec![
                ("client_id", client_id),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", ruri),
                ("code_verifier", verifier),
            ];
            if !client_secret.is_empty() {
                form.push(("client_secret", client_secret));
            }
            let resp = client
                .post(format!("{}/oauth/token", base_url))
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("GitLab token exchange failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("GitLab token exchange failed: {}", err));
            }
            let tokens: Value = resp
                .json()
                .await
                .map_err(|e| format!("GitLab token exchange invalid JSON: {}", e))?;
            let access_token = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let user = match client
                .get(format!("{}/api/v4/user", base_url))
                .bearer_auth(access_token)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(json!({})),
                _ => json!({}),
            };
            let email = user
                .get("email")
                .or_else(|| user.get("public_email"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(json!({
                "accessToken": access_token,
                "refreshToken": tokens.get("refresh_token").and_then(|v| v.as_str()),
                "expiresIn": tokens.get("expires_in").and_then(|v| v.as_i64()),
                "scope": tokens.get("scope").and_then(|v| v.as_str()),
                "providerSpecificData": {
                    "username": user.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                    "email": email,
                    "name": user.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "baseUrl": base_url,
                    "clientId": client_id,
                    "authKind": "oauth",
                }
            }))
        }
        "kimchi" => {
            let access_token = code.trim();
            if access_token.is_empty() {
                return Err("Missing Kimchi token".to_string());
            }
            let val_res = client
                .get(KIMCHI_VALIDATION_URL)
                .header(header::ACCEPT, "application/json")
                .bearer_auth(access_token)
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Kimchi token validation failed: {}", e))?;
            if !val_res.status().is_success() {
                return Err(format!(
                    "Kimchi token validation failed: {}",
                    val_res.status().as_u16()
                ));
            }
            let user_info = match client
                .get(KIMCHI_USERINFO_URL)
                .header(header::ACCEPT, "application/json")
                .bearer_auth(access_token)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(json!({})),
                _ => json!({}),
            };
            let user_id = user_info
                .get("id")
                .and_then(json_value_as_string)
                .unwrap_or_default();
            let username = user_info
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let email = user_info
                .get("email")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    if user_id.is_empty() {
                        None
                    } else {
                        Some(format!("kimchi-user-{}", user_id))
                    }
                });
            let display_name = user_info
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    if username.is_empty() {
                        None
                    } else {
                        Some(username.clone())
                    }
                });

            Ok(json!({
                "accessToken": access_token,
                "refreshToken": Value::Null,
                "email": email,
                "displayName": display_name,
                "providerSpecificData": {
                    "authMethod": "browser_token",
                    "userId": user_id,
                    "username": username,
                }
            }))
        }
        "trae" => exchange_trae_tokens(client, code).await,
        "windsurf" => exchange_windsurf_tokens(client, code, state).await,
        "zed" => exchange_zed_tokens(client, code, verifier, meta).await,
        _ => Err(format!(
            "Provider {} does not support authorization code exchange",
            provider
        )),
    }
}

async fn exchange_trae_tokens(client: &reqwest::Client, code: &str) -> Result<Value, String> {
    let trimmed = code.trim();
    let looks_callback = (trimmed.contains('?') || trimmed.contains('=') || trimmed.contains('&'))
        && (trimmed.contains("refreshToken") || trimmed.contains("refresh_token"));

    let (access_token, refresh_token, expires_in, expires_at_str, auth_method) = if !looks_callback {
        let clean = trimmed
            .strip_prefix("Cloud-IDE-JWT ")
            .or_else(|| trimmed.strip_prefix("cloud-ide-jwt "))
            .or_else(|| trimmed.strip_prefix("Bearer "))
            .or_else(|| trimmed.strip_prefix("bearer "))
            .unwrap_or(trimmed)
            .trim()
            .to_string();
        (
            clean,
            None,
            Some(TRAE_TOKEN_LIFETIME_SECS),
            None,
            "imported",
        )
    } else {
        let query_str = if let Some(idx) = trimmed.find('?') {
            &trimmed[idx + 1..]
        } else {
            trimmed.trim_start_matches('#')
        };
        let params: HashMap<String, String> = url::form_urlencoded::parse(query_str.as_bytes())
            .into_owned()
            .collect();
        let pick = |keys: &[&str]| -> Option<String> {
            for k in keys {
                if let Some(v) = params.get(*k) {
                    let t = v.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
            None
        };
        if let Some(err) = pick(&["error", "error_code", "errorCode"]) {
            let desc = pick(&["error_description", "error_desc", "message"]);
            return Err(match desc {
                Some(d) => format!("Trae auth failed: {} ({})", err, d),
                None => format!("Trae auth failed: {}", err),
            });
        }
        let rt = pick(&["refreshToken", "refresh_token", "RefreshToken"])
            .ok_or_else(|| "Trae callback missing refreshToken".to_string())?;
        let _lh = pick(&["loginHost", "login_host", "LoginHost", "host", "consoleHost"])
            .ok_or_else(|| "Trae callback missing loginHost".to_string())?;
        let cloudide_token = pick(&[
            "x-cloudide-token",
            "xCloudideToken",
            "accessToken",
            "access_token",
            "token",
        ]);

        let body = json!({
            "ClientID": TRAE_CLIENT_ID,
            "RefreshToken": rt,
            "ClientSecret": TRAE_CLIENT_SECRET,
            "UserID": "",
        });
        let mut last_err = "no successful response".to_string();
        let mut exchanged = None;
        for origin in TRAE_API_ORIGINS {
            let url = format!("{}/cloudide/api/v3/trae/oauth/ExchangeToken", origin);
            let mut req = client
                .post(&url)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::USER_AGENT, TRAE_USER_AGENT);
            if let Some(ref ct) = cloudide_token {
                req = req.header("x-cloudide-token", ct);
            }
            match req.json(&body).timeout(Duration::from_secs(20)).send().await {
                Ok(res) => {
                    let status = res.status();
                    let text = res.text().await.unwrap_or_default();
                    if !status.is_success() {
                        last_err = format!("{} HTTP {}", url, status.as_u16());
                        continue;
                    }
                    let Ok(data) = serde_json::from_str::<Value>(&text) else {
                        last_err = format!("{} invalid JSON", url);
                        continue;
                    };
                    if let Some(at) = extract_json_path(
                        &data,
                        &[
                            &["Result", "AccessToken"],
                            &["Result", "accessToken"],
                            &["result", "access_token"],
                            &["accessToken"],
                        ],
                    ) {
                        let new_rt = extract_json_path(
                            &data,
                            &[
                                &["Result", "RefreshToken"],
                                &["result", "refresh_token"],
                                &["refreshToken"],
                            ],
                        )
                        .unwrap_or_else(|| rt.clone());
                        let exp_at = extract_json_path(
                            &data,
                            &[
                                &["Result", "ExpiresAt"],
                                &["Result", "expiresAt"],
                                &["result", "expires_at"],
                                &["expiresAt"],
                            ],
                        );
                        exchanged = Some((at, Some(new_rt), None, exp_at, "oauth"));
                        break;
                    } else {
                        let msg = extract_json_path(
                            &data,
                            &[&["message"], &["msg"], &["error"], &["Result", "Message"]],
                        )
                        .unwrap_or_else(|| "missing AccessToken".to_string());
                        last_err = format!("{} {}", url, msg);
                    }
                }
                Err(e) => {
                    last_err = format!("{} {}", url, e);
                }
            }
        }
        exchanged.ok_or_else(|| format!("Trae ExchangeToken failed: {}", last_err))?
    };

    // Fetch user info best-effort
    let mut ui_email = None;
    let mut ui_name = None;
    let mut ui_ai_region = None;
    let mut ui_region = None;
    let mut ui_tenant = None;
    let mut ui_user_id = None;
    for origin in TRAE_API_ORIGINS {
        let url = format!("{}/cloudide/api/v3/trae/GetUserInfo", origin);
        if let Ok(res) = client
            .post(&url)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::USER_AGENT, TRAE_USER_AGENT)
            .header("x-cloudide-token", &access_token)
            .json(&json!({}))
            .timeout(Duration::from_secs(15))
            .send()
            .await
        {
            if res.status().is_success() {
                if let Ok(data) = res.json::<Value>().await {
                    ui_email = extract_json_path(
                        &data,
                        &[
                            &["Result", "NonPlainTextEmail"],
                            &["Result", "Email"],
                            &["Result", "email"],
                            &["email"],
                            &["data", "email"],
                        ],
                    );
                    ui_name = extract_json_path(
                        &data,
                        &[
                            &["Result", "ScreenName"],
                            &["Result", "Nickname"],
                            &["Result", "Name"],
                            &["result", "nickname"],
                            &["nickname"],
                            &["name"],
                        ],
                    );
                    ui_ai_region = extract_json_path(
                        &data,
                        &[
                            &["Result", "AIRegion"],
                            &["Result", "aiRegion"],
                            &["aiRegion"],
                        ],
                    );
                    ui_region = extract_json_path(
                        &data,
                        &[&["Result", "Region"], &["Result", "region"], &["region"]],
                    );
                    ui_tenant = extract_json_path(
                        &data,
                        &[
                            &["Result", "TenantID"],
                            &["Result", "tenantId"],
                            &["tenantId"],
                        ],
                    );
                    ui_user_id = extract_json_path(
                        &data,
                        &[&["Result", "UserID"], &["Result", "userId"], &["userId"]],
                    );
                    break;
                }
            }
        }
    }

    let resolved_expires_in = expires_in.unwrap_or_else(|| {
        expires_at_str
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .map(|ts| (ts - chrono::Utc::now().timestamp()).max(60))
            .unwrap_or(TRAE_TOKEN_LIFETIME_SECS)
    });
    let ai_region = ui_ai_region.unwrap_or_else(|| "US-East".to_string());
    let scope = {
        let r = ai_region.to_ascii_lowercase();
        if r == "sg" || r.contains("singapore") {
            "marscode-sg"
        } else if r == "cn" || r.contains("china") {
            "marscode-cn"
        } else {
            "marscode-us"
        }
    };

    Ok(json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresIn": resolved_expires_in,
        "email": ui_email,
        "displayName": ui_name,
        "providerSpecificData": {
            "authMethod": auth_method,
            "aiRegion": ai_region,
            "region": ui_region.unwrap_or_else(|| ai_region.clone()),
            "tenant": ui_tenant.unwrap_or_else(|| "marscode".to_string()),
            "userId": ui_user_id.unwrap_or_default(),
            "scope": scope,
            "webId": "",
            "bizUserId": "",
            "userUniqueId": "",
            "appLanguage": "en",
            "appVersion": TRAE_DEFAULT_APP_VERSION,
            "userRegion": if ai_region == "SG" { "SG" } else { "US" },
            "userIdentity": "Free",
        }
    }))
}

async fn windsurf_seat_request(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let res = client
        .post(&url)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, WINDSURF_USER_AGENT)
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Windsurf {} failed: {}", path, e))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let preview: String = text.chars().take(200).collect();
        return Err(format!(
            "Windsurf {} HTTP {}: {}",
            path,
            status.as_u16(),
            preview
        ));
    }
    serde_json::from_str(&text).map_err(|_| format!("Windsurf {} invalid JSON", path))
}

async fn exchange_windsurf_tokens(
    client: &reqwest::Client,
    code: &str,
    expected_state: Option<&str>,
) -> Result<Value, String> {
    let trimmed = code.trim();
    let looks_callback = trimmed.contains('?') || trimmed.contains("access_token=");

    let (api_key, api_server_url, firebase_id_token, auth_method) = if !looks_callback {
        let clean = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .unwrap_or(trimmed)
            .trim()
            .to_string();
        if clean.starts_with("sk-ws-") {
            (
                clean,
                WINDSURF_DEFAULT_API_SERVER.to_string(),
                None,
                "imported",
            )
        } else {
            let data = windsurf_seat_request(
                client,
                WINDSURF_REGISTER_BASE,
                "/exa.seat_management_pb.SeatManagementService/RegisterUser",
                json!({ "firebase_id_token": clean }),
            )
            .await?;
            let api_key = extract_json_path(&data, &[&["apiKey"], &["api_key"]])
                .ok_or_else(|| "Windsurf RegisterUser missing apiKey".to_string())?;
            let api_server_url =
                extract_json_path(&data, &[&["apiServerUrl"], &["api_server_url"]])
                    .unwrap_or_else(|| WINDSURF_DEFAULT_API_SERVER.to_string());
            (api_key, api_server_url, Some(clean), "imported")
        }
    } else {
        let query_str = if let Some(idx) = trimmed.find('?') {
            &trimmed[idx + 1..]
        } else {
            trimmed.trim_start_matches('#')
        };
        let params: HashMap<String, String> = url::form_urlencoded::parse(query_str.as_bytes())
            .into_owned()
            .collect();
        if let Some(err) = params.get("error").filter(|s| !s.trim().is_empty()) {
            let desc = params.get("error_description").map(|s| s.trim());
            return Err(match desc {
                Some(d) if !d.is_empty() => format!("Windsurf auth failed: {} ({})", err, d),
                _ => format!("Windsurf auth failed: {}", err),
            });
        }
        let fb_token = params
            .get("access_token")
            .or_else(|| params.get("token"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Windsurf callback missing access_token".to_string())?;
        if let (Some(exp), Some(got)) = (expected_state, params.get("state")) {
            if !got.is_empty() && got != exp {
                return Err("Windsurf callback state mismatch".to_string());
            }
        }
        let data = windsurf_seat_request(
            client,
            WINDSURF_REGISTER_BASE,
            "/exa.seat_management_pb.SeatManagementService/RegisterUser",
            json!({ "firebase_id_token": fb_token }),
        )
        .await?;
        let api_key = extract_json_path(&data, &[&["apiKey"], &["api_key"]])
            .ok_or_else(|| "Windsurf RegisterUser missing apiKey".to_string())?;
        let api_server_url =
            extract_json_path(&data, &[&["apiServerUrl"], &["api_server_url"]])
                .unwrap_or_else(|| WINDSURF_DEFAULT_API_SERVER.to_string());
        (api_key, api_server_url, Some(fb_token), "oauth")
    };

    let mut email = None;
    let mut name = None;
    if let Some(ref fb) = firebase_id_token {
        if let Ok(auth_res) = windsurf_seat_request(
            client,
            &api_server_url,
            "/exa.seat_management_pb.SeatManagementService/GetOneTimeAuthToken",
            json!({ "firebaseIdToken": fb }),
        )
        .await
        {
            if let Some(auth_token) = extract_json_path(&auth_res, &[&["authToken"], &["auth_token"]])
            {
                if let Ok(user_res) = windsurf_seat_request(
                    client,
                    &api_server_url,
                    "/exa.seat_management_pb.SeatManagementService/GetCurrentUser",
                    json!({ "authToken": auth_token, "includeSubscription": true }),
                )
                .await
                {
                    let user = user_res.get("user").unwrap_or(&user_res);
                    email = extract_json_path(user, &[&["email"]]);
                    name = extract_json_path(user, &[&["name"]]);
                }
            }
        }
    }

    Ok(json!({
        "accessToken": api_key,
        "refreshToken": Value::Null,
        "expiresIn": Value::Null,
        "email": email,
        "displayName": name,
        "providerSpecificData": {
            "authMethod": auth_method,
            "apiServerUrl": api_server_url,
            "firebaseIdToken": firebase_id_token,
        }
    }))
}

async fn fetch_zed_authenticated_user(
    client: &reqwest::Client,
    user_id: &str,
    access_token: &str,
    system_id: Option<&str>,
) -> Result<Value, String> {
    let mut req = client
        .get(format!("{}/client/users/me", ZED_CLOUD_BASE_URL))
        .header(header::ACCEPT, "application/json")
        .header(header::AUTHORIZATION, format!("{} {}", user_id, access_token));
    if let Some(sid) = system_id {
        if !sid.trim().is_empty() {
            req = req.header("x-zed-system-id", sid.trim());
        }
    }
    let res = req
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Zed user validation request failed: {}", e))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let data: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    if !status.is_success() {
        let msg = data
            .get("message")
            .or_else(|| data.get("error").and_then(|e| e.get("message")))
            .or_else(|| data.get("error"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("HTTP {}", status.as_u16())
                } else {
                    text
                }
            });
        return Err(msg);
    }
    Ok(data)
}

fn normalize_zed_org_id(val: Option<&Value>) -> String {
    match val {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        Some(Value::Object(obj)) => obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn resolve_zed_organization_id(psd: Option<&Map<String, Value>>, user_info: Option<&Value>) -> String {
    if let Some(m) = psd {
        let explicit = normalize_zed_org_id(
            m.get("organizationId")
                .or_else(|| m.get("defaultOrganizationId")),
        );
        if !explicit.is_empty() {
            return explicit;
        }
    }
    if let Some(ui) = user_info {
        let from_user = normalize_zed_org_id(
            ui.get("default_organization_id")
                .or_else(|| ui.get("defaultOrganizationId")),
        );
        if !from_user.is_empty() {
            return from_user;
        }
        if let Some(orgs) = ui.get("organizations").and_then(|v| v.as_array()) {
            let picked = orgs
                .iter()
                .find(|o| o.get("is_personal").and_then(|v| v.as_bool()) == Some(true))
                .or_else(|| orgs.first());
            if let Some(org) = picked {
                return normalize_zed_org_id(org.get("id"));
            }
        }
    }
    String::new()
}

async fn exchange_zed_tokens(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
    meta: &Value,
) -> Result<Value, String> {
    let (user_id, encrypted) = parse_zed_callback_payload(code)?;
    let access_token = decrypt_zed_access_token(&encrypted, code_verifier)?;
    let system_id = meta
        .get("systemId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(uuid_v4);
    let user_info = fetch_zed_authenticated_user(client, &user_id, &access_token, Some(&system_id))
        .await
        .ok();
    let org_id = resolve_zed_organization_id(None, user_info.as_ref());
    let email = user_info
        .as_ref()
        .and_then(|u| u.get("email"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let display_name = user_info.as_ref().and_then(|u| {
        u.get("name")
            .or_else(|| u.get("display_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    Ok(json!({
        "accessToken": access_token,
        "refreshToken": Value::Null,
        "expiresIn": Value::Null,
        "email": email,
        "displayName": display_name,
        "providerSpecificData": {
            "authMethod": "oauth",
            "userId": user_id,
            "systemId": system_id,
            "organizationId": org_id,
        }
    }))
}

async fn fetch_kiro_profile_arn(client: &reqwest::Client, access_token: &str) -> Option<String> {
    if access_token.is_empty() {
        return None;
    }
    let resp = client
        .post("https://codewhisperer.us-east-1.amazonaws.com/ListAvailableProfiles")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .bearer_auth(access_token)
        .json(&json!({ "maxResults": 10 }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;
    data.get("profiles")?
        .as_array()?
        .iter()
        .find_map(|p| {
            p.get("arn")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

async fn request_device_code(
    client: &reqwest::Client,
    provider: &str,
    code_challenge: Option<&str>,
    options: &Value,
) -> Result<Value, String> {
    let canon = canonical_provider(provider);
    if provider_flow_type(canon)? != "device_code" {
        return Err(format!("Provider {} does not support device code flow", provider));
    }

    match canon {
        "github" => {
            let resp = client
                .post(GITHUB_DEVICE_CODE_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[("client_id", GITHUB_CLIENT_ID), ("scope", "read:user")])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Device code request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Device code request failed: {}", err));
            }
            resp.json::<Value>()
                .await
                .map_err(|e| format!("Device code invalid JSON: {}", e))
        }
        "grok-cli" => {
            let resp = client
                .post(GROK_CLI_DEVICE_CODE_URL)
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, GROK_CLI_PAGER_USER_AGENT)
                .form(&[
                    ("client_id", GROK_CLI_CLIENT_ID),
                    ("scope", GROK_CLI_SCOPE),
                    ("referrer", "grok-build"),
                ])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Grok CLI device code request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Grok CLI device code request failed: {}", err));
            }
            resp.json::<Value>()
                .await
                .map_err(|e| format!("Grok CLI device code invalid JSON: {}", e))
        }
        "muse" => {
            let resp = client
                .post(MUSE_DEVICE_CODE_URL)
                .header(header::ACCEPT, "application/json")
                .header("x-api-version", "1.0.0")
                .form(&[("client_id", MUSE_CLIENT_ID)])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Muse Code device code request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Muse Code device code request failed: {}", err));
            }
            resp.json::<Value>()
                .await
                .map_err(|e| format!("Muse Code device code invalid JSON: {}", e))
        }
        "nous" => {
            let resp = client
                .post(NOUS_DEVICE_CODE_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[("client_id", NOUS_CLIENT_ID), ("scope", NOUS_SCOPE)])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Nous device code request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Nous device code request failed: {}", err));
            }
            let data: Value = resp
                .json()
                .await
                .map_err(|e| format!("Nous device code invalid JSON: {}", e))?;
            Ok(json!({
                "device_code": data.get("device_code"),
                "user_code": data.get("user_code"),
                "verification_uri": data.get("verification_uri"),
                "verification_uri_complete": data.get("verification_uri_complete"),
                "expires_in": data.get("expires_in"),
                "interval": data.get("interval"),
            }))
        }
        "kimi" => {
            let device_id = uuid_v4();
            let client_id = kimi_client_id();
            let mut req = client
                .post(KIMI_DEVICE_CODE_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[("client_id", client_id.as_str())]);
            for (k, v) in kimi_headers(Some(&device_id)) {
                req = req.header(k, v);
            }
            let resp = req
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Device code request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Device code request failed: {}", err));
            }
            let data: Value = resp
                .json()
                .await
                .map_err(|e| format!("Device code invalid JSON: {}", e))?;
            let user_code = data.get("user_code").and_then(|v| v.as_str()).unwrap_or("");
            let ver_uri = data
                .get("verification_uri")
                .and_then(|v| v.as_str())
                .unwrap_or(KIMI_AUTHORIZE_DEVICE_URL);
            let ver_complete = data
                .get("verification_uri_complete")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}?user_code={}", KIMI_AUTHORIZE_DEVICE_URL, user_code));
            Ok(json!({
                "device_code": data.get("device_code"),
                "user_code": user_code,
                "verification_uri": ver_uri,
                "verification_uri_complete": ver_complete,
                "expires_in": data.get("expires_in"),
                "interval": data.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
                "_kimiDeviceId": device_id,
            }))
        }
        "kilocode" => {
            let resp = client
                .post(KILOCODE_INITIATE_URL)
                .header(header::CONTENT_TYPE, "application/json")
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Device auth initiation failed: {}", e))?;
            if !resp.status().is_success() {
                if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                    return Err(
                        "Too many pending authorization requests. Please try again later."
                            .to_string(),
                    );
                }
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("Device auth initiation failed: {}", err));
            }
            let data: Value = resp
                .json()
                .await
                .map_err(|e| format!("Device auth invalid JSON: {}", e))?;
            Ok(json!({
                "device_code": data.get("code"),
                "user_code": data.get("code"),
                "verification_uri": data.get("verificationUrl"),
                "verification_uri_complete": data.get("verificationUrl"),
                "expires_in": data.get("expiresIn").and_then(|v| v.as_i64()).unwrap_or(300),
                "interval": 3,
            }))
        }
        "codebuddy-cn" | "codebuddy-intl" => {
            let (state_url, domain, ua) = if canon == "codebuddy-cn" {
                (
                    format!("{}?platform=CLI", CODEBUDDY_CN_STATE_URL),
                    "copilot.tencent.com",
                    CODEBUDDY_CN_UA,
                )
            } else {
                (
                    format!("{}?platform=ide", CODEBUDDY_INTL_STATE_URL),
                    "www.codebuddy.ai",
                    CODEBUDDY_INTL_UA,
                )
            };
            let resp = client
                .post(&state_url)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, ua)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("X-Domain", domain)
                .header("X-No-Authorization", "true")
                .header("X-No-User-Id", "true")
                .header("X-Product", "SaaS")
                .body("{}")
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("CodeBuddy state request failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("CodeBuddy state request failed: {}", err));
            }
            let data: Value = resp
                .json()
                .await
                .map_err(|e| format!("CodeBuddy state invalid JSON: {}", e))?;
            let code_ok = data.get("code").and_then(|v| v.as_i64()) == Some(0);
            let st = data
                .get("data")
                .and_then(|d| d.get("state"))
                .and_then(|v| v.as_str());
            let auth_url = data
                .get("data")
                .and_then(|d| d.get("authUrl"))
                .and_then(|v| v.as_str());
            match (code_ok, st, auth_url) {
                (true, Some(s), Some(u)) => Ok(json!({
                    "device_code": s,
                    "verification_uri": u,
                    "user_code": "",
                    "interval": 5,
                    "_isCodeBuddy": true,
                })),
                _ => {
                    let msg = data
                        .get("msg")
                        .and_then(|v| v.as_str())
                        .unwrap_or("missing state/authUrl");
                    Err(format!("CodeBuddy state error: {}", msg))
                }
            }
        }
        "qoder" | "qoder-cn" => {
            let (verifier, challenge, _) = generate_pkce(32);
            let nonce = uuid_v4();
            let machine_id = uuid_v4();
            let login_url = if canon == "qoder-cn" {
                "https://qoder.com.cn/device/selectAccounts"
            } else {
                "https://qoder.com/device/selectAccounts"
            };
            let mut u = url::Url::parse(login_url).unwrap();
            u.query_pairs_mut()
                .append_pair("challenge", &challenge)
                .append_pair("challenge_method", "S256")
                .append_pair("machine_id", &machine_id)
                .append_pair("nonce", &nonce);
            let short_code: String = nonce.chars().take(8).collect::<String>().to_ascii_uppercase();
            Ok(json!({
                "device_code": nonce,
                "user_code": short_code,
                "verification_uri": login_url,
                "verification_uri_complete": u.to_string(),
                "expires_in": 300,
                "interval": 2,
                "codeVerifier": verifier,
                "_qoderNonce": nonce,
                "_qoderMachineId": machine_id,
            }))
        }
        "kiro" => {
            let region = options
                .get("region")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "us-east-1".to_string());
            assert_valid_aws_region(&region)?;
            let start_url = options
                .get("startUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| KIRO_START_URL.to_string());
            let auth_method = if options.get("authMethod").and_then(|v| v.as_str()) == Some("idc") {
                "idc"
            } else {
                "builder-id"
            };

            let reg_res = client
                .post(format!("https://oidc.{}.amazonaws.com/client/register", region))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .json(&json!({
                    "clientName": "kiro-oauth-client",
                    "clientType": "public",
                    "scopes": [
                        "codewhisperer:completions",
                        "codewhisperer:analysis",
                        "codewhisperer:conversations"
                    ],
                    "grantTypes": [
                        "urn:ietf:params:oauth:grant-type:device_code",
                        "refresh_token"
                    ],
                    "issuerUrl": KIRO_ISSUER_URL,
                }))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Client registration failed: {}", e))?;
            if !reg_res.status().is_success() {
                let err = reg_res.text().await.unwrap_or_default();
                return Err(format!("Client registration failed: {}", err));
            }
            let client_info: Value = reg_res
                .json()
                .await
                .map_err(|e| format!("Client registration invalid JSON: {}", e))?;
            let client_id = client_info
                .get("clientId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let client_secret = client_info
                .get("clientSecret")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let dev_res = client
                .post(format!(
                    "https://oidc.{}.amazonaws.com/device_authorization",
                    region
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .json(&json!({
                    "clientId": client_id,
                    "clientSecret": client_secret,
                    "startUrl": start_url,
                }))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Device authorization failed: {}", e))?;
            if !dev_res.status().is_success() {
                let err = dev_res.text().await.unwrap_or_default();
                return Err(format!("Device authorization failed: {}", err));
            }
            let device_data: Value = dev_res
                .json()
                .await
                .map_err(|e| format!("Device authorization invalid JSON: {}", e))?;

            Ok(json!({
                "device_code": device_data.get("deviceCode"),
                "user_code": device_data.get("userCode"),
                "verification_uri": device_data.get("verificationUri"),
                "verification_uri_complete": device_data.get("verificationUriComplete"),
                "expires_in": device_data.get("expiresIn"),
                "interval": device_data.get("interval").and_then(|v| v.as_i64()).unwrap_or(5),
                "_clientId": client_id,
                "_clientSecret": client_secret,
                "_region": region,
                "_authMethod": auth_method,
                "_startUrl": start_url,
            }))
        }
        "glm" => {
            let poll_token = hex_encode(&random_bytes(32));
            let resp = client
                .post(GLM_CLI_INIT_URL)
                .header(header::CONTENT_TYPE, "application/json")
                .bearer_auth(&poll_token)
                .json(&json!({ "provider": "zai" }))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("ZCode OAuth init failed: {}", e))?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                return Err(format!("ZCode OAuth init failed: {}", err));
            }
            let payload: Value = resp
                .json()
                .await
                .map_err(|e| format!("ZCode OAuth init invalid JSON: {}", e))?;
            if !is_glm_success_code(payload.get("code")) || payload.get("data").is_none() {
                let msg = payload
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ZCode OAuth init returned no data");
                return Err(msg.to_string());
            }
            let data = &payload["data"];
            let flow_id = data
                .get("flow_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let auth_url = data
                .get("authorize_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if flow_id.is_empty() || auth_url.is_empty() {
                return Err("ZCode OAuth init response missing flow_id/authorize_url".to_string());
            }
            let expires_in = glm_relative_seconds(data.get("expires_at")).unwrap_or(300);
            let interval = data
                .get("poll_interval_sec")
                .and_then(|v| v.as_i64())
                .unwrap_or(3);
            Ok(json!({
                "device_code": flow_id,
                "verification_uri": auth_url,
                "expires_in": expires_in,
                "interval": interval,
                "_zcodePollToken": poll_token,
            }))
        }
        "minimax-code" | "minimax-code-global" => {
            let challenge = code_challenge
                .ok_or_else(|| "MiniMax Code sign-in requires a PKCE code challenge".to_string())?;
            let account_base = if canon == "minimax-code" {
                "https://account.minimax.cn"
            } else {
                "https://account.minimax.io"
            };
            let form = [
                ("client_id", MCODE_CLIENT_ID),
                ("scope", MCODE_SCOPE),
                ("audience", MCODE_AUDIENCE),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
            ];
            let resp = client
                .post(format!("{}/oauth2/device/code", account_base))
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("MiniMax device code request failed: {}", e))?;
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));

            let non_empty_str = |k: &str| {
                body.get(k)
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            };
            let user_code = non_empty_str("user_code");
            let device_code = non_empty_str("device_code");
            let uri = non_empty_str("verification_uri_complete")
                .or_else(|| non_empty_str("verification_uri"))
                .or_else(|| non_empty_str("verification_url"));

            let (Some(uc), Some(u)) = (user_code, uri) else {
                return Err(format!(
                    "MiniMax device code response unusable (HTTP {})",
                    status
                ));
            };
            let poll_by_user = device_code.is_none();
            let dc = device_code.unwrap_or_else(|| uc.clone());
            let expires_in = body
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .filter(|&v| v > 0)
                .or_else(|| {
                    body.get("expired_in").and_then(|v| v.as_i64()).and_then(|v| {
                        if v <= 0 {
                            None
                        } else if v < 1_000_000_000_000 {
                            Some(v)
                        } else {
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            Some(((v - now_ms + 999) / 1000).max(1))
                        }
                    })
                })
                .unwrap_or(600);
            let interval = body
                .get("interval")
                .and_then(|v| v.as_i64())
                .filter(|&v| v > 0)
                .map(|iv| {
                    if poll_by_user && iv >= 60 {
                        (iv + 500) / 1000
                    } else {
                        iv
                    }
                })
                .unwrap_or(5);

            Ok(json!({
                "device_code": dc,
                "user_code": uc,
                "verification_uri": u,
                "expires_in": expires_in,
                "interval": interval,
                "_minimaxPollByUser": poll_by_user,
            }))
        }
        _ => Err(format!(
            "Provider {} does not support device code flow",
            provider
        )),
    }
}

fn is_glm_success_code(code: Option<&Value>) -> bool {
    match code {
        None | Some(Value::Null) => true,
        Some(Value::Number(n)) => matches!(n.as_i64(), Some(0) | Some(200)),
        Some(Value::String(s)) => s == "0" || s == "200",
        _ => false,
    }
}

fn glm_relative_seconds(val: Option<&Value>) -> Option<i64> {
    let raw = val.and_then(|v| v.as_i64())?;
    if raw <= 0 {
        return None;
    }
    let ms = if raw > 1_000_000_000_000 {
        raw
    } else {
        raw * 1000
    };
    let secs = (ms - chrono::Utc::now().timestamp_millis()) / 1000;
    if secs > 0 {
        Some(secs)
    } else {
        None
    }
}

async fn glm_fetch_business_json(
    client: &reqwest::Client,
    url: &str,
    auth_token: Option<&str>,
    body: Option<Value>,
    label: &str,
) -> Result<Value, String> {
    let mut req = if let Some(b) = body {
        client.post(url).json(&b)
    } else {
        client.get(url)
    };
    req = req.header(header::CONTENT_TYPE, "application/json");
    if let Some(tok) = auth_token {
        req = req.bearer_auth(tok);
    }
    let resp = req
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Z.ai {} request failed: {}", label, e))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let preview: String = text.chars().take(200).collect();
        return Err(format!(
            "Z.ai {} request failed (HTTP {}): {}",
            label,
            status.as_u16(),
            preview
        ));
    }
    let payload: Value = serde_json::from_str(&text)
        .map_err(|_| format!("Z.ai {} response is not valid JSON", label))?;
    if !is_glm_success_code(payload.get("code"))
        || payload.get("success").and_then(|v| v.as_bool()) == Some(false)
    {
        let msg = payload
            .get("msg")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Z.ai {} returned business error", label));
        return Err(msg);
    }
    Ok(payload.get("data").cloned().unwrap_or(payload))
}

async fn poll_for_token(
    client: &reqwest::Client,
    provider: &str,
    device_code: &str,
    code_verifier: Option<&str>,
    extra_data: Option<&Value>,
) -> Result<Value, String> {
    let canon = canonical_provider(provider);
    if provider_flow_type(canon)? != "device_code" {
        return Err(format!("Provider {} does not support device code flow", provider));
    }

    match canon {
        "github" => {
            let resp = client
                .post(GITHUB_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[
                    ("client_id", GITHUB_CLIENT_ID),
                    ("device_code", device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("GitHub poll failed: {}", e))?;
            let ok = resp.status().is_success();
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": text }));
            if !ok {
                return Ok(json!({
                    "success": false,
                    "error": data.get("error"),
                    "errorDescription": data.get("error_description"),
                }));
            }
            let Some(access_token) = data.get("access_token").and_then(|v| v.as_str()) else {
                let err = data.get("error").and_then(|v| v.as_str()).unwrap_or("no_access_token");
                return Ok(json!({
                    "success": false,
                    "error": err,
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": err == "authorization_pending",
                }));
            };

            let copilot_token = match client
                .get(GITHUB_COPILOT_TOKEN_URL)
                .bearer_auth(access_token)
                .header(header::ACCEPT, "application/json")
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
                .header(header::USER_AGENT, GITHUB_USER_AGENT)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(json!({})),
                _ => json!({}),
            };
            let user_info = match client
                .get(GITHUB_USER_URL)
                .bearer_auth(access_token)
                .header(header::ACCEPT, "application/json")
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
                .header(header::USER_AGENT, GITHUB_USER_AGENT)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(json!({})),
                _ => json!({}),
            };
            let login = user_info.get("login").and_then(|v| v.as_str());
            let uname = user_info.get("name").and_then(|v| v.as_str());
            let uemail = user_info.get("email").and_then(|v| v.as_str());

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": data.get("refresh_token").and_then(|v| v.as_str()),
                    "expiresIn": data.get("expires_in").and_then(|v| v.as_i64()),
                    "name": login.or(uname),
                    "displayName": uname.or(login),
                    "email": uemail,
                    "providerSpecificData": {
                        "copilotToken": copilot_token.get("token"),
                        "copilotTokenExpiresAt": copilot_token.get("expires_at"),
                        "githubUserId": user_info.get("id"),
                        "githubLogin": login,
                        "githubName": uname,
                        "githubEmail": uemail,
                    }
                }
            }))
        }
        "grok-cli" => {
            let resp = client
                .post(GROK_CLI_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, GROK_CLI_PAGER_USER_AGENT)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_code),
                    ("client_id", GROK_CLI_CLIENT_ID),
                ])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Grok CLI poll failed: {}", e))?;
            let status_ok = resp.status().is_success();
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": text }));
            let err_str = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let pending = err_str == "authorization_pending" || err_str == "slow_down";
            if !status_ok && !pending {
                return Ok(json!({
                    "success": false,
                    "error": data.get("error"),
                    "errorDescription": data.get("error_description"),
                }));
            }
            let Some(access_token) = data.get("access_token").and_then(|v| v.as_str()) else {
                return Ok(json!({
                    "success": false,
                    "error": if err_str.is_empty() { "no_access_token" } else { err_str },
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": err_str == "authorization_pending",
                }));
            };

            let user = match client
                .get("https://cli-chat-proxy.grok.com/v1/user")
                .bearer_auth(access_token)
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, GROK_CLI_PAGER_USER_AGENT)
                .header("x-xai-token-auth", "xai-grok-cli")
                .header("x-grok-client-version", GROK_CLI_VERSION)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => None,
            };

            let id_token = data.get("id_token").and_then(|v| v.as_str());
            let email = id_token
                .and_then(decode_xai_id_token_email)
                .or_else(|| extract_email_from_jwt(access_token))
                .or_else(|| {
                    user.as_ref()
                        .and_then(|u| u.get("email"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
            let user_id = user.as_ref().and_then(|u| {
                u.get("userId")
                    .or_else(|| u.get("principalId"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
            let display_name = user.as_ref().and_then(|u| {
                let first = u.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
                let last = u.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
                let joined = format!("{} {}", first, last).trim().to_string();
                if joined.is_empty() {
                    None
                } else {
                    Some(joined)
                }
            });
            let expires_in = data.get("expires_in").and_then(|v| v.as_i64());
            let expires_at = expires_in
                .map(|s| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339());

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": data.get("refresh_token").and_then(|v| v.as_str()),
                    "expiresIn": expires_in,
                    "expiresAt": expires_at,
                    "scope": data.get("scope").and_then(|v| v.as_str()),
                    "email": email,
                    "displayName": display_name,
                    "providerSpecificData": {
                        "authMethod": "device_code",
                        "idToken": id_token,
                        "email": email,
                        "userId": user_id,
                        "hasGrokCodeAccess": user.as_ref().and_then(|u| u.get("hasGrokCodeAccess")).cloned(),
                        "subscriptionTier": user.as_ref().and_then(|u| u.get("subscriptionTier")).cloned(),
                    }
                }
            }))
        }
        "muse" => {
            let resp = client
                .post(MUSE_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .header("x-api-version", "1.0.0")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_code),
                    ("client_id", MUSE_CLIENT_ID),
                ])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Muse poll failed: {}", e))?;
            let status_ok = resp.status().is_success();
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": text }));
            let err_str = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let pending = err_str == "authorization_pending" || err_str == "slow_down";
            if !status_ok && !pending {
                return Ok(json!({
                    "success": false,
                    "error": data.get("error"),
                    "errorDescription": data.get("error_description"),
                }));
            }
            let Some(access_token) = data.get("access_token").and_then(|v| v.as_str()) else {
                return Ok(json!({
                    "success": false,
                    "error": if err_str.is_empty() { "no_access_token" } else { err_str },
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": err_str == "authorization_pending",
                }));
            };

            // Mint Model API key (with retry on 429 / 5xx)
            let no_redirect_client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|_| client.clone());
            let mut key_resp = None;
            for attempt in 0..3u64 {
                let r = no_redirect_client
                    .post(MUSE_KEY_URL)
                    .header(header::ACCEPT, "application/json")
                    .bearer_auth(access_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-api-version", "1.0.0")
                    .json(&json!({ "onboard": true }))
                    .timeout(Duration::from_secs(20))
                    .send()
                    .await;
                match r {
                    Ok(resp_ok) => {
                        let st = resp_ok.status().as_u16();
                        let transient = st == 429 || st >= 500;
                        key_resp = Some(resp_ok);
                        if !transient || attempt >= 2 {
                            break;
                        }
                    }
                    Err(e) => {
                        return Ok(json!({
                            "success": false,
                            "error": "exchange_failed",
                            "errorDescription": format!("Muse Code key mint failed: {}", e),
                            "fatal": true,
                        }));
                    }
                }
                tokio::time::sleep(Duration::from_secs(5 * (attempt + 1))).await;
            }
            let Some(kr) = key_resp else {
                return Ok(json!({
                    "success": false,
                    "error": "exchange_failed",
                    "errorDescription": "Muse Code key mint failed",
                    "fatal": true,
                }));
            };
            let kr_status = kr.status();
            let kr_text = kr.text().await.unwrap_or_default();
            if !kr_status.is_success() {
                let preview: String = kr_text.chars().take(200).collect();
                let mut msg = format!("{} {}", kr_status.as_u16(), preview);
                if let Ok(err_json) = serde_json::from_str::<Value>(&kr_text) {
                    let title = err_json.get("title").and_then(|v| v.as_str());
                    let detail = err_json.get("detail").and_then(|v| v.as_str());
                    let parts: Vec<&str> = [title, detail].into_iter().flatten().collect();
                    if !parts.is_empty() {
                        msg = parts.join(": ");
                        if let Some(au) = err_json.get("action_url").and_then(|v| v.as_str()) {
                            msg.push_str(&format!(" — {}", au));
                        }
                    }
                }
                return Ok(json!({
                    "success": false,
                    "error": "exchange_failed",
                    "errorDescription": format!("Muse Code key mint failed: {}", msg),
                    "fatal": true,
                }));
            }
            let Ok(payload) = serde_json::from_str::<Value>(&kr_text) else {
                return Ok(json!({
                    "success": false,
                    "error": "exchange_failed",
                    "errorDescription": "Muse Code key mint returned invalid JSON",
                    "fatal": true,
                }));
            };
            if payload.get("is_subs_active").and_then(|v| v.as_bool()) == Some(false) {
                return Ok(json!({
                    "success": false,
                    "error": "exchange_failed",
                    "errorDescription": "Muse Code subscription is inactive — activate it on muse.ai first",
                    "fatal": true,
                }));
            }
            let api_key = payload
                .get("api_key")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let Some(api_key) = api_key else {
                return Ok(json!({
                    "success": false,
                    "error": "exchange_failed",
                    "errorDescription": "Muse Code key response is missing api_key",
                    "fatal": true,
                }));
            };
            let email = payload
                .get("user_email")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty());

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": api_key,
                    "refreshToken": Value::Null,
                    "expiresIn": Value::Null,
                    "email": email,
                    "providerSpecificData": {
                        "authMethod": "device_code",
                        "oauthAccessToken": access_token,
                        "subscriptionTier": payload.get("subs_tier_name").cloned(),
                    }
                }
            }))
        }
        "nous" => {
            let resp = client
                .post(NOUS_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", NOUS_CLIENT_ID),
                    ("device_code", device_code),
                ])
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Nous poll failed: {}", e))?;
            let status_ok = resp.status().is_success();
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": text }));
            let err_str = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            let pending = err_str == "authorization_pending" || err_str == "slow_down";
            if !status_ok && !pending {
                return Ok(json!({
                    "success": false,
                    "error": data.get("error"),
                    "errorDescription": data.get("error_description"),
                }));
            }
            let Some(access_token) = data.get("access_token").and_then(|v| v.as_str()) else {
                return Ok(json!({
                    "success": false,
                    "error": if err_str.is_empty() { "no_access_token" } else { err_str },
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": err_str == "authorization_pending",
                }));
            };

            let agent_key_res = match client
                .post(NOUS_AGENT_KEY_URL)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .bearer_auth(access_token)
                .json(&json!({ "min_ttl_seconds": 7200 }))
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => None,
            };
            let agent_key = agent_key_res
                .as_ref()
                .and_then(|d| d.get("api_key"))
                .and_then(|v| v.as_str())
                .unwrap_or(access_token);
            let mut psd = Map::new();
            psd.insert("inferenceBaseUrl".to_string(), json!(NOUS_INFERENCE_BASE_URL));
            if let Some(ref ak) = agent_key_res {
                if let Some(id) = ak.get("key_id").and_then(|v| v.as_str()) {
                    psd.insert("agentKeyId".to_string(), json!(id));
                }
                if let Some(exp) = ak.get("expires_at") {
                    if !exp.is_null() {
                        psd.insert("agentKeyExpiresAt".to_string(), exp.clone());
                    }
                }
            }

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": data.get("refresh_token").and_then(|v| v.as_str()),
                    "expiresIn": data.get("expires_in").and_then(|v| v.as_i64()),
                    "scope": data.get("scope").and_then(|v| v.as_str()),
                    "apiKey": agent_key,
                    "providerSpecificData": Value::Object(psd),
                }
            }))
        }
        "kimi" => {
            let device_id = extra_data
                .and_then(|e| e.get("_kimiDeviceId"))
                .and_then(|v| v.as_str());
            let client_id = kimi_client_id();
            let mut req = client
                .post(KIMI_TOKEN_URL)
                .header(header::ACCEPT, "application/json")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", client_id.as_str()),
                    ("device_code", device_code),
                ]);
            for (k, v) in kimi_headers(device_id) {
                req = req.header(k, v);
            }
            let resp = req
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Kimi poll failed: {}", e))?;
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": "non-json token response" }));
            let err_str = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            if err_str == "authorization_pending" || err_str == "slow_down" {
                return Ok(json!({
                    "success": false,
                    "error": err_str,
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": err_str == "authorization_pending",
                }));
            }
            let Some(access_token) = data.get("access_token").and_then(|v| v.as_str()) else {
                return Ok(json!({
                    "success": false,
                    "error": if err_str.is_empty() { "no_access_token" } else { err_str },
                    "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                    "pending": false,
                }));
            };
            let mut psd = Map::new();
            psd.insert("authMethod".to_string(), json!("device_code"));
            if let Some(did) = device_id {
                psd.insert("deviceId".to_string(), json!(did));
            }
            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": data.get("refresh_token").and_then(|v| v.as_str()),
                    "expiresIn": data.get("expires_in").and_then(|v| v.as_i64()),
                    "providerSpecificData": Value::Object(psd),
                }
            }))
        }
        "kilocode" => {
            let resp = client
                .get(format!("{}/{}", KILOCODE_INITIATE_URL, device_code))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("KiloCode poll failed: {}", e))?;
            let status = resp.status();
            if status == StatusCode::ACCEPTED {
                return Ok(json!({ "success": false, "error": "authorization_pending", "pending": true }));
            }
            if status == StatusCode::FORBIDDEN {
                return Ok(json!({ "success": false, "error": "access_denied", "errorDescription": "Authorization denied by user" }));
            }
            if status == StatusCode::GONE {
                return Ok(json!({ "success": false, "error": "expired_token", "errorDescription": "Authorization code expired" }));
            }
            if !status.is_success() {
                return Ok(json!({ "success": false, "error": "poll_failed", "errorDescription": format!("Poll failed: {}", status.as_u16()) }));
            }
            let data: Value = resp.json().await.unwrap_or(json!({}));
            if data.get("status").and_then(|v| v.as_str()) == Some("approved") {
                if let Some(tok) = data.get("token").and_then(|v| v.as_str()) {
                    let org_id = match client
                        .get(format!("{}/api/profile", KILOCODE_API_BASE))
                        .bearer_auth(tok)
                        .timeout(Duration::from_secs(15))
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => r
                            .json::<Value>()
                            .await
                            .ok()
                            .and_then(|p| {
                                p.get("organizations")?
                                    .as_array()?
                                    .first()?
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            }),
                        _ => None,
                    };
                    let mut tokens = json!({
                        "accessToken": tok,
                        "refreshToken": Value::Null,
                        "expiresIn": Value::Null,
                        "email": data.get("userEmail").and_then(|v| v.as_str()),
                    });
                    if let Some(oid) = org_id {
                        tokens.as_object_mut().unwrap().insert(
                            "providerSpecificData".to_string(),
                            json!({ "orgId": oid }),
                        );
                    }
                    return Ok(json!({ "success": true, "tokens": tokens }));
                }
            }
            Ok(json!({ "success": false, "error": "authorization_pending", "pending": true }))
        }
        "codebuddy-cn" | "codebuddy-intl" => {
            let (token_url, domain, ua) = if canon == "codebuddy-cn" {
                (CODEBUDDY_CN_TOKEN_URL, "copilot.tencent.com", CODEBUDDY_CN_UA)
            } else {
                (CODEBUDDY_INTL_TOKEN_URL, "www.codebuddy.ai", CODEBUDDY_INTL_UA)
            };
            let mut u = url::Url::parse(token_url).unwrap();
            u.query_pairs_mut().append_pair("state", device_code);
            let resp = client
                .get(u.as_str())
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, ua)
                .header("X-Requested-With", "XMLHttpRequest")
                .header("X-Domain", domain)
                .header("X-No-Authorization", "true")
                .header("X-No-User-Id", "true")
                .header("X-No-Enterprise-Id", "true")
                .header("X-No-Department-Info", "true")
                .header("X-Product", "SaaS")
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("CodeBuddy poll failed: {}", e))?;
            if !resp.status().is_success() {
                return Ok(json!({ "success": false, "error": "request_failed" }));
            }
            let data: Value = resp.json().await.unwrap_or(json!({}));
            let code = data.get("code").and_then(|v| v.as_i64());
            if code == Some(0) {
                if let Some(at) = data
                    .get("data")
                    .and_then(|d| d.get("accessToken"))
                    .and_then(|v| v.as_str())
                {
                    let d = &data["data"];
                    return Ok(json!({
                        "success": true,
                        "tokens": {
                            "accessToken": at,
                            "refreshToken": d.get("refreshToken").and_then(|v| v.as_str()).unwrap_or(""),
                            "expiresIn": d.get("expiresIn").and_then(|v| v.as_i64()).unwrap_or(86400),
                            "providerSpecificData": {},
                        }
                    }));
                }
            }
            if code == Some(11217) {
                return Ok(json!({ "success": false, "error": "authorization_pending", "pending": true }));
            }
            Ok(json!({
                "success": false,
                "error": data.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown_error"),
            }))
        }
        "qoder" | "qoder-cn" => {
            let verifier = code_verifier
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Missing code verifier".to_string())?;
            let (poll_base, userinfo_url) = if canon == "qoder-cn" {
                (
                    "https://openapi.qoder.com.cn/api/v1/deviceToken/poll",
                    "https://openapi.qoder.com.cn/api/v1/userinfo",
                )
            } else {
                (
                    "https://openapi.qoder.sh/api/v1/deviceToken/poll",
                    "https://openapi.qoder.sh/api/v1/userinfo",
                )
            };
            let mut u = url::Url::parse(poll_base).unwrap();
            u.query_pairs_mut()
                .append_pair("nonce", device_code)
                .append_pair("verifier", verifier)
                .append_pair("challenge_method", "S256");

            let resp = client
                .get(u.as_str())
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, "Go-http-client/2.0")
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            let Ok(resp) = resp else {
                return Ok(json!({
                    "success": false,
                    "error": "poll_failed",
                    "errorDescription": resp.unwrap_err().to_string(),
                }));
            };
            let st = resp.status();
            if st == StatusCode::ACCEPTED || st == StatusCode::NOT_FOUND {
                return Ok(json!({ "success": false, "error": "authorization_pending", "pending": true }));
            }
            let text = resp.text().await.unwrap_or_default();
            if !st.is_success() {
                let mut msg = format!("Qoder device token poll failed: HTTP {}", st.as_u16());
                if let Ok(body) = serde_json::from_str::<Value>(&text) {
                    if let Some(m) = body.get("message").and_then(|v| v.as_str()) {
                        msg = format!("Qoder device token poll failed: {}", m);
                    }
                }
                return Ok(json!({ "success": false, "error": "poll_failed", "errorDescription": msg }));
            }
            let Ok(body) = serde_json::from_str::<Value>(&text) else {
                return Ok(json!({ "success": false, "error": "poll_failed", "errorDescription": "Qoder device token poll: invalid JSON response" }));
            };
            let Some(access_token) = body.get("token").and_then(|v| v.as_str()) else {
                return Ok(json!({ "success": false, "error": "poll_failed", "errorDescription": "Qoder device token poll returned 200 but no token" }));
            };

            let (name, raw_email, org_id) = match client
                .get(userinfo_url)
                .bearer_auth(access_token)
                .header(header::ACCEPT, "application/json")
                .header(header::USER_AGENT, "Go-http-client/2.0")
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let ub: Value = r.json().await.unwrap_or(json!({}));
                    (
                        ub.get("name")
                            .or_else(|| ub.get("username"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        ub.get("email")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        ub.get("organization_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                    )
                }
                _ => (String::new(), String::new(), String::new()),
            };

            let user_id = body
                .get("user_id")
                .and_then(json_value_as_string)
                .unwrap_or_default();
            let expire_ms = parse_qoder_expiry(body.get("expires_at"), body.get("expires_in"));
            let remaining_secs = (expire_ms - chrono::Utc::now().timestamp_millis()) / 1000;
            let expires_in = remaining_secs.max(24 * 60 * 60);
            let email = if !raw_email.is_empty() {
                Some(raw_email)
            } else if !user_id.is_empty() {
                Some(format!("qoder-user-{}", user_id))
            } else {
                None
            };
            let machine_id = extra_data
                .and_then(|e| e.get("_qoderMachineId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": body.get("refresh_token").and_then(|v| v.as_str()),
                    "expiresIn": expires_in,
                    "email": email,
                    "displayName": if name.is_empty() { None } else { Some(name) },
                    "providerSpecificData": {
                        "authMethod": "device",
                        "userId": user_id,
                        "machineId": machine_id,
                        "organizationId": org_id,
                    }
                }
            }))
        }
        "kiro" => {
            let region = extra_data
                .and_then(|e| e.get("_region"))
                .and_then(|v| v.as_str())
                .unwrap_or("us-east-1");
            assert_valid_aws_region(region)?;
            let client_id = extra_data
                .and_then(|e| e.get("_clientId"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let client_secret = extra_data
                .and_then(|e| e.get("_clientSecret"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let resp = client
                .post(format!("https://oidc.{}.amazonaws.com/token", region))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .json(&json!({
                    "clientId": client_id,
                    "clientSecret": client_secret,
                    "deviceCode": device_code,
                    "grantType": "urn:ietf:params:oauth:grant-type:device_code",
                }))
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("Kiro poll failed: {}", e))?;
            let text = resp.text().await.unwrap_or_default();
            let data: Value = serde_json::from_str(&text)
                .unwrap_or_else(|_| json!({ "error": "invalid_response", "error_description": text }));

            if let Some(access_token) = data.get("accessToken").and_then(|v| v.as_str()) {
                let mut profile_arn = data
                    .get("profileArn")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if profile_arn.is_none() {
                    profile_arn = fetch_kiro_profile_arn(client, access_token).await;
                }
                let email = extract_email_from_jwt(access_token);
                let auth_method = extra_data
                    .and_then(|e| e.get("_authMethod"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("builder-id");
                let start_url = extra_data
                    .and_then(|e| e.get("_startUrl"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(KIRO_START_URL);
                return Ok(json!({
                    "success": true,
                    "tokens": {
                        "accessToken": access_token,
                        "refreshToken": data.get("refreshToken").and_then(|v| v.as_str()),
                        "expiresIn": data.get("expiresIn").and_then(|v| v.as_i64()),
                        "email": email,
                        "providerSpecificData": {
                            "profileArn": profile_arn,
                            "clientId": client_id,
                            "clientSecret": client_secret,
                            "region": region,
                            "authMethod": auth_method,
                            "startUrl": start_url,
                        }
                    }
                }));
            }

            let err = data
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("authorization_pending");
            Ok(json!({
                "success": false,
                "error": err,
                "errorDescription": data.get("error_description").or_else(|| data.get("message")),
                "pending": err == "authorization_pending" || err == "slow_down",
            }))
        }
        "glm" => {
            let poll_token = extra_data
                .and_then(|e| e.get("_zcodePollToken"))
                .and_then(|v| v.as_str());
            let Some(poll_token) = poll_token else {
                return Ok(json!({
                    "success": false,
                    "error": "access_denied",
                    "errorDescription": "Missing ZCode poll token — restart the login flow",
                }));
            };

            let resp = client
                .get(format!("{}/{}", GLM_CLI_POLL_URL, encode_uri_component(device_code)))
                .bearer_auth(poll_token)
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| format!("ZCode poll failed: {}", e))?;
            if !resp.status().is_success() {
                return Ok(json!({
                    "success": false,
                    "error": "access_denied",
                    "errorDescription": format!("ZCode poll failed (HTTP {})", resp.status().as_u16()),
                }));
            }
            let payload: Value = resp.json().await.unwrap_or(json!({}));
            if !is_glm_success_code(payload.get("code")) {
                return Ok(json!({
                    "success": false,
                    "error": "access_denied",
                    "errorDescription": payload.get("msg").and_then(|v| v.as_str()).unwrap_or("ZCode poll failed"),
                }));
            }
            let data = payload.get("data").cloned().unwrap_or(json!({}));
            let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "pending" {
                return Ok(json!({ "success": false, "error": "authorization_pending", "pending": true }));
            }
            if status == "failed" {
                return Ok(json!({
                    "success": false,
                    "error": "access_denied",
                    "errorDescription": "ZCode authorization failed or was cancelled",
                }));
            }
            if status != "ready" {
                return Ok(json!({
                    "success": false,
                    "error": "authorization_pending",
                    "errorDescription": format!("Unknown status: {}", status),
                    "pending": true,
                }));
            }
            let pdata = data
                .get("zai")
                .or_else(|| {
                    data.get("providerId")
                        .and_then(|v| v.as_str())
                        .and_then(|pid| data.get(pid))
                })
                .and_then(|v| v.as_object());
            let zai_access_token = pdata
                .and_then(|m| m.get("access_token").or_else(|| m.get("accessToken")))
                .or_else(|| data.get("accessToken"))
                .or_else(|| data.get("access_token"))
                .and_then(|v| v.as_str());
            let Some(zai_access_token) = zai_access_token else {
                return Ok(json!({
                    "success": false,
                    "error": "access_denied",
                    "errorDescription": "ZCode poll response missing access token",
                }));
            };

            let biz_payload = glm_fetch_business_json(
                client,
                GLM_BUSINESS_LOGIN_URL,
                None,
                Some(json!({ "token": zai_access_token })),
                "Z.ai business login",
            )
            .await?;
            let business_token = biz_payload
                .get("access_token")
                .or_else(|| biz_payload.get("accessToken"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Z.ai business login response is missing access_token".to_string())?;

            let customer_info = glm_fetch_business_json(
                client,
                &format!("{}/api/biz/customer/getCustomerInfo", GLM_API_BASE_URL),
                Some(&business_token),
                None,
                "customer info",
            )
            .await?;

            let (org_id, proj_id) = pick_glm_org_and_project(&customer_info)
                .ok_or_else(|| {
                    "Unable to resolve Z.ai organization and project for the coding plan"
                        .to_string()
                })?;

            let list_url = format!(
                "{}/api/biz/v1/organization/{}/projects/{}/api_keys",
                GLM_API_BASE_URL, org_id, proj_id
            );
            let keys = glm_fetch_business_json(
                client,
                &list_url,
                Some(&business_token),
                None,
                "api keys",
            )
            .await?;
            let mut key_entry = keys
                .as_array()
                .and_then(|arr| {
                    arr.iter().find(|item| {
                        item.get("name").and_then(|v| v.as_str()) == Some(GLM_PLAN_API_KEY_NAME)
                    })
                })
                .cloned();
            if key_entry.is_none() {
                key_entry = Some(
                    glm_fetch_business_json(
                        client,
                        &list_url,
                        Some(&business_token),
                        Some(json!({ "name": GLM_PLAN_API_KEY_NAME })),
                        "api key create",
                    )
                    .await?,
                );
            }
            let api_key = key_entry
                .as_ref()
                .and_then(|e| e.get("apiKey"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Z.ai api_keys response is missing apiKey".to_string())?;
            let secret = glm_fetch_business_json(
                client,
                &format!("{}/copy/{}", list_url, encode_uri_component(api_key)),
                Some(&business_token),
                None,
                "api key copy",
            )
            .await?;
            let secret_key = secret
                .get("secretKey")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Z.ai api key copy response is missing secretKey".to_string())?;

            let plan_api_key = format!("{}.{}", api_key, secret_key);
            let user = data.get("user").cloned().unwrap_or(json!({}));
            let email = user.get("email").and_then(|v| v.as_str());
            let uname = user.get("name").and_then(|v| v.as_str());
            let display_name = uname.or(email);
            let zai_rt = pdata
                .and_then(|m| m.get("refresh_token").or_else(|| m.get("refreshToken")))
                .or_else(|| data.get("refresh_token"))
                .or_else(|| data.get("refreshToken"))
                .and_then(|v| v.as_str());

            let mut psd = json!({
                "authMethod": "cli_poll",
                "username": uname,
                "userId": user.get("user_id"),
                "zcodeJwtToken": data.get("token").and_then(|v| v.as_str()).unwrap_or(""),
                "zaiBusinessToken": business_token,
            });
            if let Some(rt) = zai_rt {
                if !rt.is_empty() {
                    psd.as_object_mut()
                        .unwrap()
                        .insert("zaiRefreshToken".to_string(), json!(rt));
                }
            }

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": plan_api_key,
                    "refreshToken": Value::Null,
                    "email": email,
                    "displayName": display_name,
                    "providerSpecificData": psd,
                }
            }))
        }
        "minimax-code" | "minimax-code-global" => {
            let verifier = code_verifier
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Missing code verifier".to_string())?;
            let account_base = if canon == "minimax-code" {
                "https://account.minimax.cn"
            } else {
                "https://account.minimax.io"
            };
            let poll_by_user = extra_data
                .and_then(|e| e.get("_minimaxPollByUser"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let code_field = if poll_by_user {
                ("user_code", device_code)
            } else {
                ("device_code", device_code)
            };
            let form = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                code_field,
                ("client_id", MCODE_CLIENT_ID),
                ("code_verifier", verifier),
            ];
            let resp = client
                .post(format!("{}/oauth2/token", account_base))
                .header(header::ACCEPT, "application/json")
                .form(&form)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("MiniMax poll failed: {}", e))?;
            let text = resp.text().await.unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));

            if let Some(status) = body.get("status").and_then(|v| v.as_str()).map(|s| s.trim()) {
                if !status.is_empty() {
                    let err = match status {
                        "pending" => "authorization_pending",
                        "slow_down" => "slow_down",
                        "denied" | "access_denied" => "access_denied",
                        "expired" | "expired_token" => "expired_token",
                        other => return Ok(json!({
                            "success": false,
                            "error": format!("unexpected_status:{}", other),
                            "pending": false,
                        })),
                    };
                    return Ok(json!({
                        "success": false,
                        "error": err,
                        "pending": err == "authorization_pending" || err == "slow_down",
                    }));
                }
            }
            if let Some(err) = body.get("error").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                return Ok(json!({
                    "success": false,
                    "error": err,
                    "errorDescription": body.get("error_description").and_then(|v| v.as_str()),
                    "pending": err == "authorization_pending" || err == "slow_down",
                }));
            }
            let Some(access_token) = body
                .get("access_token")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            else {
                return Ok(json!({ "success": false, "error": "no_access_token", "pending": false }));
            };

            let expires_in = body
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .filter(|&v| v > 0)
                .unwrap_or(3600);
            let refresh_token = body
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .unwrap_or("");

            Ok(json!({
                "success": true,
                "tokens": {
                    "accessToken": access_token,
                    "refreshToken": refresh_token,
                    "expiresIn": expires_in,
                }
            }))
        }
        _ => Err(format!(
            "Provider {} does not support device code flow",
            provider
        )),
    }
}

fn pick_glm_org_and_project(customer_info: &Value) -> Option<(String, String)> {
    let orgs = customer_info.get("organizations")?.as_array()?;
    let is_default_name = |name: Option<&str>| -> bool {
        let n = name.unwrap_or("").trim().to_ascii_lowercase();
        n.contains("默认机构") || n.contains("默认项目") || n == "default"
    };
    let mut candidates: Vec<(&Value, Vec<&Value>)> = Vec::new();
    for org in orgs {
        let oid = org.get("organizationId").and_then(json_value_as_string);
        if oid.is_none() {
            continue;
        }
        let projects: Vec<&Value> = org
            .get("projects")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|p| {
                        p.get("projectType")
                            .and_then(json_value_as_string)
                            .as_deref()
                            != Some("2")
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !projects.is_empty() {
            candidates.push((org, projects));
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let (org, projects) = candidates
        .iter()
        .find(|(o, _)| is_default_name(o.get("organizationName").and_then(|v| v.as_str())))
        .unwrap_or(&candidates[0]);
    let proj = projects
        .iter()
        .find(|p| is_default_name(p.get("projectName").and_then(|v| v.as_str())))
        .unwrap_or(&projects[0]);
    let oid = org.get("organizationId").and_then(json_value_as_string)?;
    let pid = proj.get("projectId").and_then(json_value_as_string)?;
    Some((oid, pid))
}

fn parse_qoder_expiry(expires_at: Option<&Value>, expires_in: Option<&Value>) -> i64 {
    if let Some(v) = expires_at {
        if let Some(ms) = v.as_i64().filter(|&n| n > 0) {
            return ms;
        }
        if let Some(s) = v.as_str().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if s.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(ms) = s.parse::<i64>() {
                    if ms > 0 {
                        return ms;
                    }
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                return dt.timestamp_millis();
            }
        }
    }
    if let Some(secs) = expires_in.and_then(|v| v.as_i64()).filter(|&n| n >= 0) {
        return chrono::Utc::now().timestamp_millis() + secs * 1000;
    }
    chrono::Utc::now().timestamp_millis() + 30 * 24 * 60 * 60 * 1000
}

// ============================================================================
// Local Callback Proxy Servers (Codex, xAI, Trae, Windsurf, Zed, Xiaomi MiMo)
// ============================================================================

fn is_loopback_origin(origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Ok(u) = url::Url::parse(origin) else {
        return false;
    };
    u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1") | Some("localhost"))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_result_page(success: bool, message: &str) -> Html<String> {
    let color = if success { "#22c55e" } else { "#ef4444" };
    let icon = if success { "&#10003;" } else { "&#10007;" };
    let title = if success {
        "Authentication Successful"
    } else {
        "Authentication Failed"
    };
    let safe_msg = escape_html(message);
    Html(format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title}</title>
<style>body{{font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0;background:#f5f5f5}}.c{{text-align:center;padding:2rem;background:#fff;border-radius:8px;box-shadow:0 2px 10px rgba(0,0,0,.1)}}.i{{color:{color};font-size:3rem}}h1{{margin:1rem 0}}p{{color:#666}}</style>
</head><body><div class="c"><div class="i">{icon}</div><h1>{title}</h1><p>{safe_msg}</p><p>Closing in <span id="cd">3</span>s...</p>
<script>let n=3;const c=document.getElementById("cd");const t=setInterval(()=>{{n--;c.textContent=n;if(n<=0){{clearInterval(t);window.close();}}}},1000);</script>
</div></body></html>"#
    ))
}

fn stop_proxy_slot(slot: &Mutex<Option<RunningProxy>>) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(mut running) = guard.take() {
            if let Some(tx) = running.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }
}

async fn start_fixed_oauth_proxy(
    provider: &'static str,
    port: u16,
    app_port: u16,
    state: AppState,
) -> Value {
    let slot = if provider == "xai" {
        &runtime().xai_proxy
    } else {
        &runtime().codex_proxy
    };

    {
        if let Ok(guard) = slot.lock() {
            if guard.is_some() {
                return json!({ "success": true });
            }
        }
    }

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                return json!({ "success": false, "reason": "port_busy" });
            }
            return json!({ "success": false, "reason": e.to_string() });
        }
    };

    let (tx, rx) = oneshot::channel::<()>();
    {
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(RunningProxy {
                port,
                shutdown_tx: Some(tx),
            });
        }
    }

    let callback_handler = move |Query(params): Query<HashMap<String, String>>| {
        let state = state.clone();
        async move {
            let code = params.get("code").cloned();
            let st = params.get("state").cloned();
            let error_param = params.get("error").cloned();
            let error_desc = params.get("error_description").cloned();

            let sessions_lock = if provider == "xai" {
                &runtime().xai_sessions
            } else {
                &runtime().codex_sessions
            };

            let session = st.as_ref().and_then(|s| {
                sessions_lock
                    .lock()
                    .ok()
                    .and_then(|m| m.get(s).cloned())
            });

            if let (Some(st_key), Some(sess)) = (st.as_ref(), session) {
                let res: Result<(String, Option<String>), String> = async {
                    if let Some(ep) = error_param {
                        return Err(error_desc.unwrap_or(ep));
                    }
                    let code_val = code
                        .filter(|c| !c.is_empty())
                        .ok_or_else(|| "No authorization code received".to_string())?;
                    let token_data = exchange_tokens(
                        state.proxy.client(),
                        provider,
                        &code_val,
                        Some(&sess.redirect_uri),
                        Some(&sess.code_verifier),
                        Some(st_key),
                        &json!({}),
                    )
                    .await?;
                    let input = input_from_mapped_tokens(provider, "oauth", &token_data);
                    let conn = create_provider_connection(&state.db, input)?;
                    Ok((conn.id, conn.email))
                }
                .await;

                let html = match res {
                    Ok((conn_id, email)) => {
                        if let Ok(mut m) = sessions_lock.lock() {
                            if let Some(entry) = m.get_mut(st_key) {
                                entry.status = "done".to_string();
                                entry.connection_id = Some(conn_id);
                                entry.email = email;
                            }
                        }
                        render_result_page(true, "You can close this window.")
                    }
                    Err(err) => {
                        if let Ok(mut m) = sessions_lock.lock() {
                            if let Some(entry) = m.get_mut(st_key) {
                                entry.status = "error".to_string();
                                entry.error = Some(err.clone());
                            }
                        }
                        render_result_page(false, &err)
                    }
                };
                stop_proxy_slot(if provider == "xai" {
                    &runtime().xai_proxy
                } else {
                    &runtime().codex_proxy
                });
                return html.into_response();
            }

            // Mode B: legacy channel fallback 302 redirect
            let qs: String = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params.iter())
                .finish();
            let redirect_url = if qs.is_empty() {
                format!("http://localhost:{}/callback", app_port)
            } else {
                format!("http://localhost:{}/callback?{}", app_port, qs)
            };
            stop_proxy_slot(if provider == "xai" {
                &runtime().xai_proxy
            } else {
                &runtime().codex_proxy
            });
            Redirect::temporary(&redirect_url).into_response()
        }
    };

    let router = Router::new()
        .route("/callback", get(callback_handler.clone()))
        .route("/auth/callback", get(callback_handler));

    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            tokio::select! {
                _ = rx => {},
                _ = tokio::time::sleep(Duration::from_secs(PROXY_IDLE_TIMEOUT_SECS)) => {
                    stop_proxy_slot(if provider == "xai" { &runtime().xai_proxy } else { &runtime().codex_proxy });
                }
            }
        });
        let _ = server.await;
    });

    json!({ "success": true })
}

async fn start_dynamic_callback_proxy(
    provider: &'static str,
    preferred_port: u16,
    state: AppState,
) -> Value {
    let slot = match provider {
        "trae" => &runtime().trae_proxy,
        "windsurf" => &runtime().windsurf_proxy,
        "zed" => &runtime().zed_proxy,
        "xiaomi-mimo" => &runtime().xiaomi_proxy,
        _ => return json!({ "success": false, "reason": "unsupported provider" }),
    };

    if let Ok(guard) = slot.lock() {
        if let Some(running) = guard.as_ref() {
            let cb_path = if provider == "trae" {
                "/callback"
            } else if provider == "windsurf" {
                "/windsurf-auth-callback"
            } else {
                "/"
            };
            return json!({
                "success": true,
                "port": running.port,
                "callbackUrl": format!("http://127.0.0.1:{}{}", running.port, cb_path),
            });
        }
    }

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", preferred_port)).await {
        Ok(l) => l,
        Err(_) if preferred_port != 0 => {
            match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                Ok(l) => l,
                Err(e) => return json!({ "success": false, "reason": e.to_string() }),
            }
        }
        Err(e) => return json!({ "success": false, "reason": e.to_string() }),
    };

    let bound_port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => return json!({ "success": false, "reason": e.to_string() }),
    };

    let (tx, rx) = oneshot::channel::<()>();
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(RunningProxy {
            port: bound_port,
            shutdown_tx: Some(tx),
        });
    }

    let cb_path = if provider == "trae" {
        "/callback"
    } else if provider == "windsurf" {
        "/windsurf-auth-callback"
    } else {
        "/"
    };
    let timeout_secs = if provider == "xiaomi-mimo" {
        PROXY_IDLE_TIMEOUT_SECS
    } else {
        LONG_PROXY_IDLE_TIMEOUT_SECS
    };

    let handler = move |headers: HeaderMap, Query(params): Query<HashMap<String, String>>| {
        let state = state.clone();
        async move {
            let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
            if !is_loopback_origin(origin) {
                return (
                    StatusCode::FORBIDDEN,
                    render_result_page(false, "Cross-origin callback rejected"),
                )
                    .into_response();
            }

            match provider {
                "xiaomi-mimo" => {
                    let Some(u_param) = params.get("u").filter(|s| !s.is_empty()) else {
                        return (
                            StatusCode::BAD_REQUEST,
                            render_result_page(false, "Missing encrypted payload (u parameter)."),
                        )
                            .into_response();
                    };
                    let pending: Vec<(String, [u8; 32])> = runtime()
                        .xiaomi_sessions
                        .lock()
                        .map(|m| {
                            m.iter()
                                .filter(|(_, s)| s.status == "pending")
                                .map(|(k, s)| (k.clone(), s.private_key))
                                .collect()
                        })
                        .unwrap_or_default();
                    if pending.is_empty() {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            render_result_page(
                                false,
                                "No active OAuth session. Please restart the login flow.",
                            ),
                        )
                            .into_response();
                    }
                    let mut matched: Option<(String, XiaomiCallbackPayload)> = None;
                    for (st, pk) in &pending {
                        if let Ok(dec) = decrypt_xiaomi_callback(pk, u_param) {
                            matched = Some((st.clone(), dec));
                            break;
                        }
                    }
                    match matched {
                        Some((st, dec)) => {
                            if let Some(sk) = dec.sk.filter(|s| !s.is_empty()) {
                                if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                                    if let Some(sess) = m.get_mut(&st) {
                                        sess.status = "done".to_string();
                                        sess.uid = dec.uid.clone();
                                        sess.access_token = Some(sk);
                                        sess.base_url = Some(dec.url);
                                    }
                                }
                                return (
                                    StatusCode::OK,
                                    render_result_page(
                                        true,
                                        "Xiaomi account linked. You can close this tab.",
                                    ),
                                )
                                    .into_response();
                            }
                            let msg = "Decrypted payload missing sk (API key)";
                            if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                                for (st, _) in &pending {
                                    if let Some(s) = m.get_mut(st) {
                                        s.status = "error".to_string();
                                        s.error = Some(msg.to_string());
                                    }
                                }
                            }
                            (StatusCode::BAD_REQUEST, render_result_page(false, msg))
                                .into_response()
                        }
                        None => {
                            let msg = "Could not decrypt with any pending session key";
                            if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                                for (st, _) in &pending {
                                    if let Some(s) = m.get_mut(st) {
                                        s.status = "error".to_string();
                                        s.error = Some(msg.to_string());
                                    }
                                }
                            }
                            (
                                StatusCode::BAD_REQUEST,
                                render_result_page(false, &format!("Decryption failed: {}", msg)),
                            )
                                .into_response()
                        }
                    }
                }
                "trae" | "windsurf" => {
                    let sess_mutex = if provider == "trae" {
                        &runtime().trae_session
                    } else {
                        &runtime().windsurf_session
                    };
                    let session = sess_mutex.lock().ok().and_then(|g| g.clone());
                    let Some(sess) = session else {
                        return render_result_page(
                            false,
                            &format!("No active {} login session", provider),
                        )
                        .into_response();
                    };
                    let cb_state = params.get("state").map(|s| s.as_str());
                    if provider == "windsurf" && cb_state != Some(sess.state.as_str()) {
                        if let Ok(mut g) = sess_mutex.lock() {
                            if let Some(s) = g.as_mut() {
                                s.status = "error".to_string();
                                s.error = Some("Windsurf callback state mismatch".to_string());
                            }
                        }
                        stop_proxy_slot(&runtime().windsurf_proxy);
                        return render_result_page(false, "Windsurf callback state mismatch")
                            .into_response();
                    }
                    if provider == "trae" {
                        if let Some(cs) = cb_state {
                            if cs != sess.state {
                                if let Ok(mut g) = sess_mutex.lock() {
                                    if let Some(s) = g.as_mut() {
                                        s.status = "error".to_string();
                                        s.error = Some("Trae callback state mismatch".to_string());
                                    }
                                }
                                stop_proxy_slot(&runtime().trae_proxy);
                                return render_result_page(false, "Trae callback state mismatch")
                                    .into_response();
                            }
                        }
                    }
                    let qs: String = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(params.iter())
                        .finish();
                    let raw_cb = format!("{}?{}", cb_path, qs);
                    let res = async {
                        let token_data = exchange_tokens(
                            state.proxy.client(),
                            provider,
                            &raw_cb,
                            None,
                            None,
                            Some(&sess.state),
                            &json!({}),
                        )
                        .await?;
                        let auth_type = if provider == "windsurf" {
                            "api_key"
                        } else {
                            "oauth"
                        };
                        let input = input_from_mapped_tokens(provider, auth_type, &token_data);
                        create_provider_connection(&state.db, input)
                    }
                    .await;
                    let html = match res {
                        Ok(conn) => {
                            if let Ok(mut g) = sess_mutex.lock() {
                                if let Some(s) = g.as_mut() {
                                    s.status = "done".to_string();
                                    s.connection_id = Some(conn.id);
                                    s.email = conn.email;
                                }
                            }
                            render_result_page(true, "You can close this window.")
                        }
                        Err(e) => {
                            if let Ok(mut g) = sess_mutex.lock() {
                                if let Some(s) = g.as_mut() {
                                    s.status = "error".to_string();
                                    s.error = Some(e.clone());
                                }
                            }
                            render_result_page(false, &e)
                        }
                    };
                    stop_proxy_slot(if provider == "trae" {
                        &runtime().trae_proxy
                    } else {
                        &runtime().windsurf_proxy
                    });
                    html.into_response()
                }
                "zed" => {
                    let session = runtime().zed_session.lock().ok().and_then(|g| g.clone());
                    let Some(sess) = session else {
                        return render_result_page(false, "No active Zed login session")
                            .into_response();
                    };
                    let has_zed_params = params.contains_key("user_id")
                        || params.contains_key("userId")
                        || params.contains_key("access_token")
                        || params.contains_key("accessToken")
                        || params.contains_key("token");
                    if !has_zed_params {
                        return render_result_page(
                            false,
                            "Waiting for Zed sign-in — this request carried no login data.",
                        )
                        .into_response();
                    }
                    let qs: String = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(params.iter())
                        .finish();
                    let raw_cb = format!("/?{}", qs);
                    let meta = sess
                        .system_id
                        .as_ref()
                        .map(|sid| json!({ "systemId": sid }))
                        .unwrap_or_else(|| json!({}));
                    let res = async {
                        let token_data = exchange_tokens(
                            state.proxy.client(),
                            "zed",
                            &raw_cb,
                            None,
                            sess.code_verifier.as_deref(),
                            Some(&sess.state),
                            &meta,
                        )
                        .await?;
                        let input = input_from_mapped_tokens("zed", "oauth", &token_data);
                        create_provider_connection(&state.db, input)
                    }
                    .await;
                    match res {
                        Ok(conn) => {
                            if let Ok(mut g) = runtime().zed_session.lock() {
                                if let Some(s) = g.as_mut() {
                                    s.status = "done".to_string();
                                    s.connection_id = Some(conn.id);
                                    s.email = conn.email;
                                }
                            }
                            stop_proxy_slot(&runtime().zed_proxy);
                            render_result_page(true, "You can close this window.").into_response()
                        }
                        Err(e) => {
                            if let Ok(mut g) = runtime().zed_session.lock() {
                                if let Some(s) = g.as_mut() {
                                    s.status = "error".to_string();
                                    s.error = Some(e.clone());
                                }
                            }
                            render_result_page(false, &e).into_response()
                        }
                    }
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    };

    let router = match provider {
        "trae" => Router::new()
            .route("/callback", get(handler.clone()))
            .route("/auth/callback", get(handler)),
        "windsurf" => Router::new().route("/windsurf-auth-callback", get(handler)),
        "zed" => Router::new()
            .route("/", get(handler.clone()))
            .route("/callback", get(handler)),
        _ => Router::new().route("/", get(handler)),
    };

    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            tokio::select! {
                _ = rx => {},
                _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                    match provider {
                        "trae" => stop_proxy_slot(&runtime().trae_proxy),
                        "windsurf" => stop_proxy_slot(&runtime().windsurf_proxy),
                        "zed" => stop_proxy_slot(&runtime().zed_proxy),
                        "xiaomi-mimo" => {
                            stop_proxy_slot(&runtime().xiaomi_proxy);
                            if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                                m.clear();
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        let _ = server.await;
    });

    json!({
        "success": true,
        "port": bound_port,
        "callbackUrl": format!("http://127.0.0.1:{}{}", bound_port, cb_path),
    })
}

// ============================================================================
// Local File / SQLite / Keyring / Lease Auto-Import Helpers
// ============================================================================

fn resolve_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn commandcode_auto_import_from_home(home: &Path, app_data: Option<&Path>) -> Value {
    let mut candidates = vec![home.join(".commandcode").join("auth.json")];
    if cfg!(windows) {
        let ad = app_data
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"));
        candidates.push(ad.join("commandcode").join("auth.json"));
    }

    let found_path = candidates.iter().find(|p| p.is_file()).cloned();
    let Some(auth_path) = found_path else {
        let joined = candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return json!({
            "found": false,
            "error": format!("Command Code auth file not found. Checked:\n{}\n\nRun 'commandcode login' in your terminal first.", joined),
        });
    };

    let Ok(raw) = std::fs::read_to_string(&auth_path) else {
        return json!({
            "found": false,
            "error": "Failed to read Command Code auth.json",
        });
    };
    let Ok(auth) = serde_json::from_str::<Value>(&raw) else {
        return json!({
            "found": false,
            "error": "auth.json is not valid JSON. Please run 'commandcode login' again.",
        });
    };
    let api_key = auth
        .get("apiKey")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .unwrap_or("");
    if api_key.is_empty() {
        return json!({
            "found": false,
            "error": "No apiKey found in ~/.commandcode/auth.json. Please run 'commandcode login'.",
        });
    }

    json!({
        "found": true,
        "apiKey": api_key,
        "userId": auth.get("userId").cloned().unwrap_or(Value::Null),
        "userName": auth.get("userName").cloned().unwrap_or(Value::Null),
        "keyName": auth.get("keyName").cloned().unwrap_or(Value::Null),
        "authenticatedAt": auth.get("authenticatedAt").cloned().unwrap_or(Value::Null),
        "source": auth_path.display().to_string(),
    })
}

fn normalize_cursor_value(val: &str) -> String {
    if let Ok(Value::String(s)) = serde_json::from_str::<Value>(val) {
        s
    } else {
        val.to_string()
    }
}

pub fn extract_cursor_tokens_from_sqlite(
    db_path: &Path,
    allow_fuzzy: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let conn = SqliteConnection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;

    let select_by_keys = |keys: &[&str]| -> Option<String> {
        let placeholders = keys.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT key, value FROM itemTable WHERE key IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql).ok()?;
        let params: Vec<&dyn rusqlite::ToSql> =
            keys.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                let k: String = row.get(0)?;
                let v: String = row.get(1)?;
                Ok((k, v))
            })
            .ok()?;
        let mut map = HashMap::new();
        for (k, v) in rows.flatten() {
            map.insert(k, v);
        }
        for key in keys {
            if let Some(v) = map.get(*key) {
                if !v.is_empty() {
                    return Some(normalize_cursor_value(v));
                }
            }
        }
        None
    };

    let mut access_token = select_by_keys(&["cursorAuth/accessToken", "cursorAuth/token"]);
    let mut machine_id = select_by_keys(&[
        "storage.serviceMachineId",
        "storage.machineId",
        "telemetry.machineId",
    ]);

    if allow_fuzzy && (access_token.is_none() || machine_id.is_none()) {
        let fuzzy_lookup = |needles: &[&str]| -> Option<String> {
            let mut stmt = conn
                .prepare("SELECT key, value FROM itemTable")
                .ok()?;
            let rows = stmt
                .query_map([], |row| {
                    let k: String = row.get(0)?;
                    let v: String = row.get(1)?;
                    Ok((k, v))
                })
                .ok()?;
            let items: Vec<(String, String)> = rows.flatten().collect();
            for needle in needles {
                for (k, v) in &items {
                    if k.to_ascii_lowercase().contains(needle) && !v.is_empty() {
                        return Some(normalize_cursor_value(v));
                    }
                }
            }
            None
        };
        if access_token.is_none() {
            access_token = fuzzy_lookup(&["accesstoken", "access_token"]);
        }
        if machine_id.is_none() {
            machine_id = fuzzy_lookup(&["machineid", "machine_id"]);
        }
    }

    Ok((access_token, machine_id))
}

pub fn cursor_auto_import_from_home(home: &Path) -> (StatusCode, Value) {
    let os = std::env::consts::OS;
    let candidates: Vec<PathBuf> = if os == "macos" {
        vec![
            home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            home.join("Library/Application Support/Cursor - Insiders/User/globalStorage/state.vscdb"),
        ]
    } else if os == "windows" {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        vec![
            app_data.join("Cursor/User/globalStorage/state.vscdb"),
            app_data.join("Cursor - Insiders/User/globalStorage/state.vscdb"),
            local_app_data.join("Cursor/User/globalStorage/state.vscdb"),
            local_app_data.join("Programs/Cursor/User/globalStorage/state.vscdb"),
        ]
    } else if os == "linux" {
        vec![home.join(".config/Cursor/User/globalStorage/state.vscdb")]
    } else {
        return (
            StatusCode::BAD_REQUEST,
            json!({ "found": false, "error": "Unsupported platform" }),
        );
    };

    let single_known = candidates.len() == 1;
    let db_path = if single_known {
        candidates[0].clone()
    } else {
        match candidates.iter().find(|p| p.is_file()) {
            Some(p) => p.clone(),
            None => {
                let label = if os == "macos" { "macOS" } else { "Windows" };
                let joined = candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                return (
                    StatusCode::OK,
                    json!({
                        "found": false,
                        "error": format!("Cursor database not found in known {} locations:\n{}\n\nMake sure Cursor IDE is installed and opened at least once.", label, joined),
                    }),
                );
            }
        }
    };

    match extract_cursor_tokens_from_sqlite(&db_path, os == "macos") {
        Ok((Some(at), Some(mid))) => (
            StatusCode::OK,
            json!({ "found": true, "accessToken": at, "machineId": mid }),
        ),
        Ok(_) => {
            if single_known {
                (
                    StatusCode::OK,
                    json!({
                        "found": false,
                        "error": "Cursor database not found. Make sure Cursor IDE is installed and you are logged in.",
                    }),
                )
            } else {
                (
                    StatusCode::OK,
                    json!({
                        "found": false,
                        "error": "Could not find Cursor tokens in the database. Please login to Cursor IDE first, then retry.",
                        "windowsManual": true,
                        "dbPath": db_path.display().to_string(),
                    }),
                )
            }
        }
        Err(err) => {
            if !single_known {
                (
                    StatusCode::OK,
                    json!({
                        "found": false,
                        "error": format!("Found a Cursor database at {} but could not open it: {}", db_path.display(), err),
                        "dbPath": db_path.display().to_string(),
                    }),
                )
            } else {
                (
                    StatusCode::OK,
                    json!({
                        "found": false,
                        "error": "Cursor database not found. Make sure Cursor IDE is installed and you are logged in.",
                    }),
                )
            }
        }
    }
}

pub fn kiro_auto_import_from_home(home: &Path, app_data: Option<&Path>) -> Value {
    let cache_path = home.join(".aws/sso/cache");
    let Ok(entries) = std::fs::read_dir(&cache_path) else {
        return json!({
            "found": false,
            "error": "AWS SSO cache not found. Please login to Kiro IDE first.",
        });
    };

    let mut files: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    files.sort();

    let mut refresh_token: Option<String> = None;
    let mut found_file: Option<String> = None;
    let mut token_data: Option<Value> = None;

    let try_file = |file: &str| -> Option<(String, Value)> {
        let content = std::fs::read_to_string(cache_path.join(file)).ok()?;
        let data: Value = serde_json::from_str(&content).ok()?;
        let rt = data.get("refreshToken").and_then(|v| v.as_str())?;
        if rt.starts_with("aorAAAAAG") {
            Some((rt.to_string(), data))
        } else {
            None
        }
    };

    if files.iter().any(|f| f == "kiro-auth-token.json") {
        if let Some((rt, data)) = try_file("kiro-auth-token.json") {
            refresh_token = Some(rt);
            found_file = Some("kiro-auth-token.json".to_string());
            token_data = Some(data);
        }
    }

    if refresh_token.is_none() {
        for file in &files {
            if !file.ends_with(".json") {
                continue;
            }
            if let Some((rt, data)) = try_file(file) {
                refresh_token = Some(rt);
                found_file = Some(file.clone());
                token_data = Some(data);
                break;
            }
        }
    }

    let Some(rt) = refresh_token else {
        return json!({
            "found": false,
            "error": "Kiro token not found in AWS SSO cache. Please login to Kiro IDE first.",
        });
    };

    let mut client_id: Option<String> = None;
    let mut client_secret: Option<String> = None;
    let region = token_data
        .as_ref()
        .and_then(|d| d.get("region"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let auth_method = token_data
        .as_ref()
        .and_then(|d| d.get("authMethod"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(hash) = token_data
        .as_ref()
        .and_then(|d| d.get("clientIdHash"))
        .and_then(|v| v.as_str())
    {
        if let Ok(content) = std::fs::read_to_string(cache_path.join(format!("{}.json", hash))) {
            if let Ok(cdata) = serde_json::from_str::<Value>(&content) {
                if let (Some(cid), Some(csec)) = (
                    cdata.get("clientId").and_then(|v| v.as_str()),
                    cdata.get("clientSecret").and_then(|v| v.as_str()),
                ) {
                    client_id = Some(cid.to_string());
                    client_secret = Some(csec.to_string());
                }
            }
        }
    }

    let ad = app_data
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Roaming"));
    let profile_paths = [
        ad.join("Kiro/User/globalStorage/kiro.kiroagent/profile.json"),
        home.join(".config/Kiro/User/globalStorage/kiro.kiroagent/profile.json"),
    ];
    let mut profile_arn: Option<String> = None;
    for p in &profile_paths {
        if let Ok(content) = std::fs::read_to_string(p) {
            if let Ok(pdata) = serde_json::from_str::<Value>(&content) {
                if let Some(arn) = pdata.get("arn").and_then(|v| v.as_str()) {
                    // Normalize arn:aws:codewhisperer:<region>: -> us-east-1
                    if let Some(rest) = arn.strip_prefix("arn:aws:codewhisperer:") {
                        if let Some((_, tail)) = rest.split_once(':') {
                            profile_arn = Some(format!("arn:aws:codewhisperer:us-east-1:{}", tail));
                        } else {
                            profile_arn = Some(arn.to_string());
                        }
                    } else {
                        profile_arn = Some(arn.to_string());
                    }
                    break;
                }
            }
        }
    }

    json!({
        "found": true,
        "refreshToken": rt,
        "source": found_file,
        "clientId": client_id,
        "clientSecret": client_secret,
        "region": region,
        "authMethod": auth_method,
        "profileArn": profile_arn,
    })
}

pub fn read_desktop_pass_token_from_home(
    home: &Path,
) -> Option<(String, Option<String>, Option<String>)> {
    let src = if cfg!(windows) {
        home.join("AppData/Roaming/Xiaomi MiMo/Partitions/xiaomi-account/Network/Cookies")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Xiaomi MiMo/Partitions/xiaomi-account/Network/Cookies")
    } else {
        home.join(".config/Xiaomi MiMo/Partitions/xiaomi-account/Network/Cookies")
    };
    if !src.is_file() {
        return None;
    }

    let tmp = std::env::temp_dir().join(format!("9r-mimo-cookies-{}.db", uuid_v4()));
    if std::fs::copy(&src, &tmp).is_err() {
        return None;
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(tmp.clone());

    let conn = SqliteConnection::open_with_flags(
        &tmp,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let mut stmt = conn
        .prepare("SELECT name, value FROM cookies WHERE host_key = ?")
        .ok()?;
    let rows = stmt
        .query_map([".account.xiaomi.com"], |row| {
            let k: String = row.get(0)?;
            let v: String = row.get(1)?;
            Ok((k, v))
        })
        .ok()?;
    let mut jar = HashMap::new();
    for (k, v) in rows.flatten() {
        jar.insert(k, v);
    }
    let pass_token = jar.get("passToken").filter(|s| !s.is_empty())?.clone();
    let user_id = jar.get("userId").filter(|s| !s.is_empty()).cloned();
    let c_user_id = jar.get("cUserId").filter(|s| !s.is_empty()).cloned();
    Some((pass_token, user_id, c_user_id))
}

pub fn xiaomi_mimo_auto_import_from_home(home: &Path, app_data: Option<&Path>) -> Value {
    let mut candidates = vec![home.join(".local/share/mimocode/auth.json")];
    if cfg!(windows) {
        let ad = app_data
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        candidates.push(ad.join("Xiaomi MiMo/auth.json"));
    }
    if cfg!(target_os = "macos") {
        candidates.push(home.join("Library/Application Support/mimocode/auth.json"));
    }

    let Some(auth_path) = candidates.iter().find(|p| p.is_file()).cloned() else {
        let joined = candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return json!({
            "found": false,
            "error": format!("Xiaomi MiMo Desktop auth file not found. Checked:\n{}\n\nMake sure Xiaomi MiMo Desktop is installed and you are signed in.", joined),
        });
    };

    let Ok(raw) = std::fs::read_to_string(&auth_path) else {
        return json!({
            "found": false,
            "error": "Failed to read Xiaomi MiMo auth.json",
        });
    };
    let Ok(auth) = serde_json::from_str::<Value>(&raw) else {
        return json!({
            "found": false,
            "error": "auth.json is not valid JSON. Please sign in to Xiaomi MiMo Desktop again.",
        });
    };
    let Some(xiaomi) = auth.get("xiaomi").and_then(|v| v.as_object()) else {
        return json!({
            "found": false,
            "error": "No Xiaomi credentials found in auth.json. Please sign in to Xiaomi MiMo Desktop.",
        });
    };
    let key = xiaomi
        .get("key")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if key.is_empty() {
        return json!({
            "found": false,
            "error": "No Xiaomi credentials found in auth.json. Please sign in to Xiaomi MiMo Desktop.",
        });
    }
    if !key.starts_with("sk-") {
        return json!({
            "found": false,
            "error": "Xiaomi key does not appear to be a valid API key (expected sk- prefix).",
        });
    }

    let metadata = xiaomi.get("metadata").and_then(|v| v.as_object());
    let uid = metadata
        .and_then(|m| m.get("uid"))
        .and_then(json_value_as_string);
    let base_url = metadata
        .and_then(|m| m.get("base_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "https://api.xiaomimimo.com/v1".to_string());

    let (mimo_pass_token, mimo_user_id, mimo_c_user_id) =
        match read_desktop_pass_token_from_home(home) {
            Some((pt, uid, cuid)) => (Some(pt), uid, cuid),
            None => (None, None, None),
        };

    json!({
        "found": true,
        "apiKey": key,
        "uid": uid,
        "baseUrl": base_url,
        "source": auth_path.display().to_string(),
        "mimoPassToken": mimo_pass_token,
        "mimoUserId": mimo_user_id,
        "mimoCUserId": mimo_c_user_id,
    })
}

fn zed_settings_paths(home: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Zed/settings.json")]
    } else if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        vec![local.join("Zed/settings.json")]
    } else {
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        vec![xdg.join("zed/settings.json")]
    }
}

fn resolve_zed_credentials_url(home: &Path) -> String {
    for path in zed_settings_paths(home) {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(settings) = serde_json::from_str::<Value>(&raw) {
                let u = settings
                    .get("credentials_url")
                    .or_else(|| settings.get("server_url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim_end_matches('/').to_string())
                    .filter(|s| !s.is_empty());
                if let Some(url) = u {
                    return url;
                }
            }
        }
    }
    ZED_WEB_BASE_URL.to_string()
}

fn zed_global_db_paths(home: &Path) -> Vec<PathBuf> {
    let channels = ["0-global", "0-stable", "0-preview"];
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Zed/db")
    } else if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        local.join("Zed/db")
    } else {
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        xdg.join("zed/db")
    };
    channels
        .iter()
        .map(|c| base.join(c).join("db.sqlite"))
        .collect()
}

pub fn read_zed_system_id_from_home(home: &Path) -> Option<String> {
    for db_path in zed_global_db_paths(home) {
        if !db_path.is_file() {
            continue;
        }
        if let Ok(conn) = SqliteConnection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            if let Ok(val) = conn.query_row(
                "SELECT value FROM kv_store WHERE key = 'system_id' LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            ) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }
    None
}

fn parse_secret_tool_search(text: &str) -> Vec<(Option<String>, Option<String>, Option<String>)> {
    let mut username = None;
    let mut label = None;
    let mut secret = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("attribute.username") {
            if let Some((_, val)) = rest.split_once('=') {
                let v = val.trim();
                if !v.is_empty() {
                    username = Some(v.to_string());
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("label") {
            if let Some((_, val)) = rest.split_once('=') {
                let v = val.trim();
                if !v.is_empty() {
                    label = Some(v.to_string());
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("secret") {
            if let Some((_, val)) = rest.split_once('=') {
                secret = Some(val.trim().to_string());
            }
        }
    }
    if username.is_none() && label.is_none() && secret.is_none() {
        vec![]
    } else {
        vec![(label, username, secret)]
    }
}

async fn read_zed_keyring_credentials(credentials_url: &str) -> Result<(String, String), String> {
    if cfg!(target_os = "linux") {
        let out = tokio::process::Command::new("secret-tool")
            .args(["search", "--all", "url", credentials_url])
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "secret-tool not found (install libsecret / secret-tools)".to_string()
                } else {
                    e.to_string()
                }
            })?;
        let merged = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        let items = parse_secret_tool_search(&merged);
        if let Some((_, Some(uid), Some(sec))) = items
            .iter()
            .find(|(lbl, u, s)| lbl.as_deref() == Some(ZED_KEYRING_LABEL) && u.is_some() && s.is_some())
            .or_else(|| items.iter().find(|(_, u, s)| u.is_some() && s.is_some()))
            .cloned()
        {
            if !uid.is_empty() && !sec.is_empty() {
                return Ok((uid, sec));
            }
        }
        let lookup = tokio::process::Command::new("secret-tool")
            .args(["lookup", "url", credentials_url])
            .output()
            .await
            .map_err(|e| e.to_string())?;
        let token = String::from_utf8_lossy(&lookup.stdout).trim().to_string();
        if token.is_empty() {
            return Err("Zed IDE session not found in the system keyring. Sign in to Zed, then retry.".to_string());
        }
        let uid = items
            .into_iter()
            .find_map(|(_, u, _)| u)
            .ok_or_else(|| "Zed keyring entry missing username (user id)".to_string())?;
        Ok((uid, token))
    } else if cfg!(target_os = "macos") {
        let stripped = credentials_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        for server in [credentials_url, stripped, "zed.dev"] {
            if let Ok(out) = tokio::process::Command::new("security")
                .args(["find-internet-password", "-s", server, "-g"])
                .output()
                .await
            {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let password = combined.lines().find_map(|l| {
                    let t = l.trim();
                    t.strip_prefix("password:")
                        .map(|r| r.trim().trim_matches('"').to_string())
                });
                let account = combined.lines().find_map(|l| {
                    if let Some(pos) = l.find("\"acct\"<blob>=") {
                        let rest = &l[pos + "\"acct\"<blob>=".len()..];
                        if let Some(start) = rest.find('"') {
                            let tail = &rest[start + 1..];
                            if let Some(end) = tail.find('"') {
                                return Some(tail[..end].to_string());
                            }
                        }
                    }
                    None
                });
                if let (Some(pw), Some(acct)) = (password, account) {
                    if !pw.is_empty() && !acct.is_empty() {
                        return Ok((acct, pw));
                    }
                }
            }
        }
        Err("Zed credentials not found in macOS Keychain".to_string())
    } else {
        Err(format!(
            "Zed auto-import is not supported on platform {}",
            std::env::consts::OS
        ))
    }
}

async fn detect_ide_installed(provider: &str) -> (bool, Option<String>) {
    let os = std::env::consts::OS;
    let paths: Vec<&str> = match (provider, os) {
        ("trae", "macos") => vec!["/Applications/Trae.app"],
        ("trae", "linux") => vec!["/usr/bin/trae", "/usr/local/bin/trae", "/opt/trae", "/opt/Trae"],
        ("windsurf", "macos") => vec!["/Applications/Windsurf.app"],
        ("windsurf", "linux") => vec![
            "/usr/bin/windsurf",
            "/usr/local/bin/windsurf",
            "/opt/windsurf",
            "/opt/Windsurf",
        ],
        _ => vec![],
    };
    for p in paths {
        if Path::new(p).exists() {
            return (true, Some(p.to_string()));
        }
    }
    let bin = match provider {
        "trae" => "trae",
        "windsurf" => "windsurf",
        _ => return (false, None),
    };
    let cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(out) = tokio::process::Command::new(cmd).arg(bin).output().await {
        if out.status.success() {
            return (true, Some(bin.to_string()));
        }
    }
    (false, None)
}

// --- MiniMax Code Lease Protocol (Unix domain socket + capability file) ---

fn resolve_mcode_data_dir(override_dir: Option<&str>) -> PathBuf {
    if let Some(d) = override_dir.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return PathBuf::from(d);
    }
    if let Ok(env_dir) = std::env::var("MINIMAX_DESKTOP_DATA_DIR") {
        if !env_dir.trim().is_empty() {
            return PathBuf::from(env_dir.trim());
        }
    }
    resolve_home_dir().join(".minimax")
}

fn resolve_mcode_lease_paths(data_dir: &Path) -> Result<(PathBuf, PathBuf), (&'static str, String)> {
    if !data_dir.is_absolute() {
        return Err((
            "INVALID_REQUEST",
            "dataDir must be an absolute path.".to_string(),
        ));
    }
    let run_dir = data_dir.join("run");
    Ok((
        run_dir.join("mcode-auth-lease-v1.sock"),
        run_dir.join("mcode-auth-lease-v1.cap"),
    ))
}

fn read_mcode_capability(cap_path: &Path) -> Result<String, (&'static str, String)> {
    let cap = std::fs::read_to_string(cap_path).map_err(|e| {
        (
            "BROKER_UNAVAILABLE",
            format!(
                "Cannot read capability file {}. Is the MiniMax Code desktop app installed and logged in on this machine? ({})",
                cap_path.display(),
                e
            ),
        )
    })?;
    let trimmed = cap.trim().to_string();
    if trimmed.len() != 43
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err((
            "BROKER_UNAVAILABLE",
            "Capability file has unexpected content.".to_string(),
        ));
    }
    Ok(trimmed)
}

async fn send_mcode_lease_request(
    data_dir: &Path,
    request_payload: Value,
    timeout_ms: u64,
) -> Result<Value, (&'static str, String)> {
    let (sock_path, cap_path) = resolve_mcode_lease_paths(data_dir)?;
    let capability = read_mcode_capability(&cap_path)?;
    let mut payload = request_payload;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("version".to_string(), json!(1));
        obj.insert("capability".to_string(), json!(capability));
    }
    let json_bytes = serde_json::to_vec(&payload)
        .map_err(|e| ("INVALID_REQUEST", format!("JSON encode error: {}", e)))?;
    if json_bytes.is_empty() || json_bytes.len() > MCODE_MAX_FRAME_BYTES {
        return Err(("INVALID_REQUEST", "Frame payload out of bounds.".to_string()));
    }

    let mut frame = Vec::with_capacity(4 + json_bytes.len());
    frame.extend_from_slice(&(json_bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&json_bytes);

    #[cfg(unix)]
    {
        let io_fut = async {
            let mut stream = tokio::net::UnixStream::connect(&sock_path)
                .await
                .map_err(|e| {
                    (
                        "BROKER_UNAVAILABLE",
                        format!(
                            "Cannot reach auth broker at {}: {}. Is the MiniMax Code desktop app running?",
                            sock_path.display(),
                            e
                        ),
                    )
                })?;
            stream
                .write_all(&frame)
                .await
                .map_err(|e| ("BROKER_UNAVAILABLE", format!("Broker write error: {}", e)))?;
            let mut len_buf = [0u8; 4];
            stream
                .read_exact(&mut len_buf)
                .await
                .map_err(|_| ("BROKER_UNAVAILABLE", "Broker closed the connection early.".to_string()))?;
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 || len > MCODE_MAX_FRAME_BYTES {
                return Err((
                    "PROTOCOL_MISMATCH",
                    "Broker sent an invalid frame length.".to_string(),
                ));
            }
            let mut body_buf = vec![0u8; len];
            stream
                .read_exact(&mut body_buf)
                .await
                .map_err(|_| ("BROKER_UNAVAILABLE", "Broker closed the connection early.".to_string()))?;
            let resp: Value = serde_json::from_slice(&body_buf)
                .map_err(|_| ("PROTOCOL_MISMATCH", "Broker sent a non-JSON frame.".to_string()))?;
            if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let code = resp
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("INTERNAL_ERROR");
                let static_code = match code {
                    "AUTH_REQUIRED" => "AUTH_REQUIRED",
                    "BROKER_UNAVAILABLE" => "BROKER_UNAVAILABLE",
                    "CAPABILITY_REJECTED" => "CAPABILITY_REJECTED",
                    "PROTOCOL_MISMATCH" => "PROTOCOL_MISMATCH",
                    "INVALID_REQUEST" => "INVALID_REQUEST",
                    _ => "INTERNAL_ERROR",
                };
                return Err((static_code, format!("Desktop authentication error: {}", static_code)));
            }
            Ok(resp.get("result").cloned().unwrap_or(json!({})))
        };
        tokio::time::timeout(Duration::from_millis(timeout_ms), io_fut)
            .await
            .map_err(|_| {
                (
                    "BROKER_UNAVAILABLE",
                    format!("Broker did not respond within {}ms.", timeout_ms),
                )
            })?
    }
    #[cfg(not(unix))]
    {
        let _ = (sock_path, frame, timeout_ms);
        Err((
            "BROKER_UNAVAILABLE",
            "Named pipe lease is not supported on this platform".to_string(),
        ))
    }
}

// ============================================================================
// Xiaomi MiMo Login Session Cookie Helpers
// ============================================================================

#[derive(Debug, Clone)]
struct MimoCookieItem {
    name: String,
    value: String,
    domain: String,
    path: String,
}

#[derive(Debug, Clone)]
struct MimoLoginSession {
    state: String,
    region: String,
    proxy_url: Option<String>,
    jar: HashMap<String, MimoCookieItem>,
    created_at_ms: i64,
    upstream_base: String,
}

fn resolve_mimo_region_base(region: &str) -> &'static str {
    match region.to_ascii_lowercase().as_str() {
        "cn" => "https://mimo-server-cn.xiaomimimo.com",
        "ams" => "https://mimo-server-ams.xiaomimimo.com",
        "ru" => "https://mimo-server-ru.xiaomimimo.com",
        "in" => "https://mimo-server-in.xiaomimimo.com",
        _ => "https://mimo-server-sgp.xiaomimimo.com",
    }
}

fn encode_mimo_session_cookie(sess: &MimoLoginSession) -> String {
    let entries: Vec<[&str; 4]> = sess
        .jar
        .values()
        .map(|c| [c.name.as_str(), c.value.as_str(), c.domain.as_str(), c.path.as_str()])
        .collect();
    let mut payload = json!({
        "s": sess.state,
        "r": sess.region,
        "t": sess.created_at_ms,
        "p": sess.proxy_url.as_deref().unwrap_or(""),
        "j": entries,
    })
    .to_string();
    if payload.len() > MIMO_COOKIE_JSON_BUDGET {
        let filtered: Vec<[&str; 4]> = entries
            .into_iter()
            .filter(|[name, _, _, _]| {
                matches!(
                    *name,
                    "passToken" | "userId" | "cUserId" | "serviceToken" | "deviceId"
                ) || name.ends_with("_serviceToken")
                    || name.ends_with("_ph")
                    || name.ends_with("_slh")
            })
            .collect();
        payload = json!({
            "s": sess.state,
            "r": sess.region,
            "t": sess.created_at_ms,
            "p": sess.proxy_url.as_deref().unwrap_or(""),
            "j": filtered,
        })
        .to_string();
    }
    format!("v2.{}", b64url_encode(payload.as_bytes()))
}

fn decode_mimo_session_cookie(raw_val: &str) -> Option<MimoLoginSession> {
    let payload_str = if let Some(b64) = raw_val.strip_prefix("v2.") {
        let bytes = b64url_decode(b64).ok()?;
        String::from_utf8(bytes).ok()?
    } else if let Some(enc) = raw_val.strip_prefix("v1.") {
        percent_encoding::percent_decode_str(enc)
            .decode_utf8()
            .ok()?
            .to_string()
    } else {
        return None;
    };

    let payload: Value = serde_json::from_str(&payload_str).ok()?;
    let created_at_ms = payload.get("t")?.as_i64()?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms - created_at_ms > MIMO_SESSION_TTL_MS {
        return None;
    }

    let state = payload.get("s")?.as_str()?.to_string();
    let region_raw = payload
        .get("r")
        .and_then(|v| v.as_str())
        .unwrap_or("sgp");
    let region = match region_raw {
        "cn" | "sgp" | "ams" | "ru" | "in" => region_raw.to_string(),
        _ => "sgp".to_string(),
    };
    let proxy_url = payload
        .get("p")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());

    let mut jar = HashMap::new();
    if let Some(arr) = payload.get("j").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(parts) = item.as_array() {
                if parts.len() >= 4 {
                    if let (Some(n), Some(v), Some(d), Some(p)) = (
                        parts[0].as_str(),
                        parts[1].as_str(),
                        parts[2].as_str(),
                        parts[3].as_str(),
                    ) {
                        let key = format!("{}|{}|{}", n, d, p);
                        jar.insert(
                            key,
                            MimoCookieItem {
                                name: n.to_string(),
                                value: v.to_string(),
                                domain: d.to_string(),
                                path: p.to_string(),
                            },
                        );
                    }
                }
            }
        }
    }

    Some(MimoLoginSession {
        state,
        region: region.clone(),
        proxy_url,
        jar,
        created_at_ms,
        upstream_base: resolve_mimo_region_base(&region).to_string(),
    })
}

fn absorb_mimo_set_cookies(sess: &mut MimoLoginSession, resp: &reqwest::Response, req_url: &str) {
    let Ok(parsed_url) = url::Url::parse(req_url) else {
        return;
    };
    let default_domain = parsed_url.host_str().unwrap_or("account.xiaomi.com");
    let default_path = "/";
    for val in resp.headers().get_all(header::SET_COOKIE) {
        let Ok(raw) = val.to_str() else {
            continue;
        };
        let mut parts = raw.split(';');
        let Some(nv) = parts.next() else {
            continue;
        };
        let Some((name_raw, value_raw)) = nv.trim().split_once('=') else {
            continue;
        };
        let name = name_raw.trim().to_string();
        let value = value_raw.trim().to_string();
        let mut domain = default_domain.to_string();
        let mut path = default_path.to_string();
        let mut expired = value == "EXPIRED";

        for attr in parts {
            let trimmed = attr.trim();
            let (k, v) = match trimmed.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
                None => (trimmed.to_ascii_lowercase(), ""),
            };
            if k == "domain" && !v.is_empty() {
                domain = v.trim_start_matches('.').to_string();
            } else if k == "path" && !v.is_empty() {
                path = v.to_string();
            } else if k == "max-age" {
                if let Ok(ma) = v.parse::<i64>() {
                    if ma <= 0 {
                        expired = true;
                    }
                }
            } else if k == "expires" && v.to_ascii_lowercase().contains("expired") {
                expired = true;
            }
        }

        let key = format!("{}|{}|{}", name, domain, path);
        if expired || value.is_empty() {
            sess.jar.remove(&key);
        } else {
            sess.jar.insert(
                key,
                MimoCookieItem {
                    name,
                    value,
                    domain,
                    path,
                },
            );
        }
    }
}

fn read_mimo_session_identity(sess: &MimoLoginSession) -> Option<(String, Option<String>, Option<String>)> {
    let pick = |name: &str| -> Option<String> {
        sess.jar.values().find_map(|c| {
            let d = c.domain.trim_start_matches('.').to_ascii_lowercase();
            if c.name == name && (d == "account.xiaomi.com" || d.ends_with(".account.xiaomi.com")) {
                Some(c.value.clone())
            } else {
                None
            }
        })
    };
    let pass_token = pick("passToken")?;
    if pass_token == "EXPIRED" || pass_token.is_empty() {
        return None;
    }
    Some((pass_token, pick("userId"), pick("cUserId")))
}

fn rewrite_mimo_to_proxy(text: &str, origin: &str) -> String {
    let proxy_base = format!("{}/__mimo_login/mimo", origin);
    let mut out = text.to_string();
    let bases = [
        "https://mimo-server-cn.xiaomimimo.com",
        "https://mimo-server-sgp.xiaomimimo.com",
        "https://mimo-server-ams.xiaomimimo.com",
        "https://mimo-server-ru.xiaomimimo.com",
        "https://mimo-server-in.xiaomimimo.com",
    ];
    for b in bases {
        let http_b = b.replacen("https://", "http://", 1);
        let proto_rel = b.trim_start_matches("https:");
        out = out.replace(b, &proxy_base);
        out = out.replace(&http_b, &proxy_base.replacen("https://", "http://", 1));
        out = out.replace(proto_rel, &proxy_base);
        out = out.replace(&encode_uri_component(b), &encode_uri_component(&proxy_base));
    }
    for acct in [
        "https://account.xiaomi.com",
        "http://account.xiaomi.com",
        "//account.xiaomi.com",
    ] {
        out = out.replace(acct, origin);
        out = out.replace(&encode_uri_component(acct), &encode_uri_component(origin));
    }
    out
}

async fn probe_local_http_proxy() -> Option<String> {
    let ports = [10808u16, 10809, 7890, 7891, 1080, 1081, 8080, 8888];
    let mut handles = Vec::new();
    for port in ports {
        handles.push(tokio::spawn(async move {
            let fut = async {
                let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .ok()?;
                let req = format!(
                    "CONNECT account.xiaomi.com:443 HTTP/1.1\r\nHost: account.xiaomi.com:443\r\n\r\n"
                );
                stream.write_all(req.as_bytes()).await.ok()?;
                let mut buf = [0u8; 64];
                let n = stream.read(&mut buf).await.ok()?;
                let head = String::from_utf8_lossy(&buf[..n]);
                if head.contains(" 200 ") || head.contains(" 202 ") {
                    Some(format!("http://127.0.0.1:{}", port))
                } else {
                    None
                }
            };
            tokio::time::timeout(Duration::from_millis(500), fut)
                .await
                .ok()
                .flatten()
        }));
    }
    for h in handles {
        if let Ok(Some(url)) = h.await {
            return Some(url);
        }
    }
    None
}

// ============================================================================
// HTTP Route Handlers
// ============================================================================

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

// 1. POST /api/oauth/codex/bulk-import
pub async fn codex_bulk_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("Invalid JSON body: {}", e)),
    };

    let accounts: Vec<Value> = match parsed {
        Value::Array(arr) => arr,
        Value::Object(ref obj) if obj.get("accounts").and_then(|v| v.as_array()).is_some() => {
            obj["accounts"].as_array().cloned().unwrap_or_default()
        }
        Value::Object(_) => vec![parsed],
        _ => return json_error(StatusCode::BAD_REQUEST, "No accounts provided"),
    };
    if accounts.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "No accounts provided");
    }

    let mut results = Vec::with_capacity(accounts.len());
    let mut success = 0usize;
    let mut failed = 0usize;

    for (i, raw) in accounts.into_iter().enumerate() {
        let res = (|| -> Result<String, String> {
            let obj = raw
                .as_object()
                .ok_or_else(|| "Item is not an object".to_string())?;
            let access_token = obj
                .get("accessToken")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Missing accessToken".to_string())?
                .to_string();

            let id_token = obj
                .get("idToken")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mut email = obj
                .get("email")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let mut psd = obj
                .get("providerSpecificData")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();

            let needs_email = email.is_none();
            let needs_acct = !psd.contains_key("chatgptAccountId");
            let needs_plan = !psd.contains_key("chatgptPlanType");
            if needs_email || needs_acct || needs_plan {
                let info = extract_codex_account_info(
                    id_token.as_deref().unwrap_or(&access_token),
                );
                if needs_email {
                    email = info.email;
                }
                if needs_acct {
                    if let Some(aid) = info.chatgpt_account_id {
                        psd.insert("chatgptAccountId".to_string(), json!(aid));
                    }
                }
                if needs_plan {
                    if let Some(pt) = info.chatgpt_plan_type {
                        psd.insert("chatgptPlanType".to_string(), json!(pt));
                    }
                }
            }

            let expires_in = obj.get("expiresIn").and_then(|v| v.as_i64());
            let expires_at = obj
                .get("expiresAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    expires_in
                        .filter(|&s| s > 0)
                        .map(|s| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339())
                });

            let mut extra = Map::new();
            for (k, v) in obj {
                if !matches!(
                    k.as_str(),
                    "id" | "provider"
                        | "authType"
                        | "createdAt"
                        | "updatedAt"
                        | "name"
                        | "email"
                        | "displayName"
                        | "accessToken"
                        | "refreshToken"
                        | "idToken"
                        | "expiresAt"
                        | "expiresIn"
                        | "testStatus"
                        | "isActive"
                        | "priority"
                        | "lastRefreshAt"
                        | "providerSpecificData"
                ) {
                    extra.insert(k.clone(), v.clone());
                }
            }

            let conn = create_provider_connection(
                &state.db,
                ConnectionCreateInput {
                    provider: "codex".to_string(),
                    auth_type: "oauth".to_string(),
                    name: obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    email,
                    display_name: obj
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    access_token: Some(access_token),
                    refresh_token: obj
                        .get("refreshToken")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    id_token,
                    expires_at,
                    expires_in,
                    last_refresh_at: Some(
                        obj.get("lastRefreshAt")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
                    ),
                    test_status: Some(
                        obj.get("testStatus")
                            .and_then(|v| v.as_str())
                            .unwrap_or("active")
                            .to_string(),
                    ),
                    is_active: Some(
                        obj.get("isActive")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                    ),
                    priority: obj.get("priority").and_then(|v| v.as_i64()),
                    provider_specific_data: if psd.is_empty() { None } else { Some(psd) },
                    extra_fields: extra,
                    ..Default::default()
                },
            )?;
            Ok(conn.id)
        })();

        match res {
            Ok(id) => {
                success += 1;
                results.push(json!({ "index": i, "ok": true, "id": id }));
            }
            Err(err) => {
                failed += 1;
                results.push(json!({ "index": i, "ok": false, "error": err }));
            }
        }
    }

    Json(json!({
        "success": success,
        "failed": failed,
        "results": results,
    }))
    .into_response()
}

// 2. POST /api/oauth/codex/import-token
pub async fn codex_import_token_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(raw_token) = body.get("accessToken").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "Access token is required");
    };
    let token = raw_token.trim();
    if token.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Access token is required");
    }

    let mut email: Option<String> = None;
    let mut psd = Map::new();
    psd.insert("authMethod".to_string(), json!("access_token"));

    if let Some(payload) = decode_jwt_payload(token) {
        let auth = payload
            .get("https://api.openai.com/auth")
            .and_then(|v| v.as_object());
        let profile = payload
            .get("https://api.openai.com/profile")
            .and_then(|v| v.as_object());
        email = profile
            .and_then(|p| p.get("email"))
            .or_else(|| payload.get("email"))
            .or_else(|| payload.get("preferred_username"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(aid) = auth
            .and_then(|a| a.get("chatgpt_account_id"))
            .and_then(|v| v.as_str())
        {
            psd.insert("chatgptAccountId".to_string(), json!(aid));
        }
        if let Some(pt) = auth
            .and_then(|a| a.get("chatgpt_plan_type"))
            .and_then(|v| v.as_str())
        {
            psd.insert("chatgptPlanType".to_string(), json!(pt));
        }
        if let Some(exp) = payload.get("exp").and_then(|v| v.as_i64()) {
            psd.insert("jwtExp".to_string(), json!(exp));
        }
    }

    if email.is_none() {
        let info = extract_codex_account_info(token);
        if let Some(em) = info.email {
            email = Some(em);
        }
        if let Some(aid) = info.chatgpt_account_id {
            psd.insert("chatgptAccountId".to_string(), json!(aid));
        }
        if let Some(pt) = info.chatgpt_plan_type {
            psd.insert("chatgptPlanType".to_string(), json!(pt));
        }
    }

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| email.clone())
        .unwrap_or_else(|| "ChatGPT Access Token".to_string());

    let workspace = psd
        .get("chatgptAccountId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let plan = psd
        .get("chatgptPlanType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "codex".to_string(),
            auth_type: "access_token".to_string(),
            name: Some(name),
            email,
            access_token: Some(token.to_string()),
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
                "name": conn.name,
                "workspace": workspace,
                "plan": plan,
            }
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 3. GET /api/oauth/commandcode/auto-import
pub async fn commandcode_auto_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let home = resolve_home_dir();
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    Json(commandcode_auto_import_from_home(&home, app_data.as_deref())).into_response()
}

// 4. GET /api/oauth/cursor/auto-import
pub async fn cursor_auto_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let home = resolve_home_dir();
    let (status, body) = cursor_auto_import_from_home(&home);
    (status, Json(body)).into_response()
}

// 5a. GET /api/oauth/cursor/import
pub async fn cursor_import_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    Json(json!({
        "provider": "cursor",
        "method": "import_token",
        "instructions": {
            "title": "How to get your Cursor token",
            "steps": [
                "1. Open Cursor IDE and make sure you're logged in",
                "2. Find the state.vscdb file:",
                "   - Linux: ~/.config/Cursor/User/globalStorage/state.vscdb",
                "   - macOS: /Users/<user>/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
                "   - Windows: %APPDATA%\\Cursor\\User\\globalStorage\\state.vscdb",
                "3. Open the database with SQLite browser or CLI:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='cursorAuth/accessToken'\"",
                "4. Also get the machine ID:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='storage.serviceMachineId'\"",
                "5. Paste both values in the form below",
            ],
            "alternativeMethod": [
                "Or use this one-liner to get both values:",
                "sqlite3 state.vscdb \"SELECT key, value FROM itemTable WHERE key IN ('cursorAuth/accessToken', 'storage.serviceMachineId')\"",
            ],
        },
        "requiredFields": [
            {
                "name": "accessToken",
                "label": "Access Token",
                "description": "From cursorAuth/accessToken in state.vscdb",
                "type": "textarea",
            },
            {
                "name": "machineId",
                "label": "Machine ID",
                "description": "From storage.serviceMachineId in state.vscdb",
                "type": "text",
            },
        ],
    }))
    .into_response()
}

// 5b. POST /api/oauth/cursor/import
pub async fn cursor_import_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(access_token) = body.get("accessToken").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "Access token is required");
    };
    let Some(machine_id) = body.get("machineId").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "Machine ID is required");
    };
    let token = access_token.trim();
    let mid = machine_id.trim();

    if token.len() < 50 {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid token format. Token appears too short.",
        );
    }
    let hex_only: String = mid.chars().filter(|&c| c != '-').collect();
    if hex_only.len() < 32 || !hex_only.chars().all(|c| c.is_ascii_hexdigit()) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid machine ID format. Expected UUID format.",
        );
    }

    let payload = decode_jwt_payload(token);
    let email = payload.as_ref().and_then(|p| {
        p.get("email")
            .or_else(|| p.get("sub"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    let user_id = payload.as_ref().and_then(|p| {
        p.get("sub")
            .or_else(|| p.get("user_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let mut psd = Map::new();
    psd.insert("machineId".to_string(), json!(mid));
    psd.insert("authMethod".to_string(), json!("imported"));
    psd.insert("provider".to_string(), json!("Imported"));
    if let Some(uid) = user_id {
        psd.insert("userId".to_string(), json!(uid));
    }

    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(86400)).to_rfc3339();
    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "cursor".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some(token.to_string()),
            expires_at: Some(expires_at),
            email,
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
            }
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 6. POST /api/oauth/gitlab/pat
pub async fn gitlab_pat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid request body"),
    };
    let token = parsed
        .get("token")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .unwrap_or("");
    if token.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Personal Access Token is required");
    }
    let base = parsed
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("https://gitlab.com")
        .trim_end_matches('/');

    let resp = match state
        .proxy
        .client()
        .get(format!("{}/api/v4/user", base))
        .header("Private-Token", token)
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return json_error(
            StatusCode::UNAUTHORIZED,
            &format!("GitLab token verification failed: {}", err),
        );
    }

    let user: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let email = user
        .get("email")
        .or_else(|| user.get("public_email"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let username = user
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = user
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let display_name = if !name.is_empty() {
        name.clone()
    } else if !username.is_empty() {
        username.clone()
    } else {
        email.clone()
    };

    let mut psd = Map::new();
    psd.insert("username".to_string(), json!(username));
    psd.insert("email".to_string(), json!(email));
    psd.insert("name".to_string(), json!(name));
    psd.insert("baseUrl".to_string(), json!(base));
    psd.insert("authKind".to_string(), json!("personal_access_token"));

    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "gitlab".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some(token.to_string()),
            email: Some(email),
            display_name: Some(display_name),
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 7. POST /api/oauth/grok-cli/bulk-import
pub async fn grok_cli_bulk_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("Invalid JSON body: {}", e)),
    };

    let accounts: Vec<Value> = match parsed {
        Value::Array(arr) => arr,
        Value::Object(ref obj) if obj.get("accounts").and_then(|v| v.as_array()).is_some() => {
            obj["accounts"].as_array().cloned().unwrap_or_default()
        }
        Value::Object(_) => vec![parsed],
        _ => return json_error(StatusCode::BAD_REQUEST, "No accounts provided"),
    };
    if accounts.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "No accounts provided");
    }

    let total = accounts.len();
    let mut results = Vec::with_capacity(total);
    let mut success = 0usize;
    let mut failed = 0usize;

    for (i, raw) in accounts.into_iter().enumerate() {
        let res = (|| -> Result<(String, Option<String>), String> {
            let obj = raw
                .as_object()
                .ok_or_else(|| "Item is not an object".to_string())?;
            let access_token = obj
                .get("access_token")
                .or_else(|| obj.get("accessToken"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Missing access_token / accessToken".to_string())?
                .to_string();
            let refresh_token = obj
                .get("refresh_token")
                .or_else(|| obj.get("refreshToken"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let id_token = obj
                .get("id_token")
                .or_else(|| obj.get("idToken"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mut email = obj
                .get("email")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if email.is_none() {
                email = id_token
                    .as_deref()
                    .and_then(decode_xai_id_token_email)
                    .or_else(|| extract_email_from_jwt(&access_token));
            }
            let expires_in = obj
                .get("expires_in")
                .or_else(|| obj.get("expiresIn"))
                .and_then(|v| v.as_i64());
            let expires_at = obj
                .get("expires_at")
                .or_else(|| obj.get("expiresAt"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    expires_in
                        .filter(|&s| s > 0)
                        .map(|s| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339())
                });

            let mut psd = Map::new();
            psd.insert("authMethod".to_string(), json!("device_code"));
            if let Some(ref idt) = id_token {
                psd.insert("idToken".to_string(), json!(idt));
            }
            if let Some(ref em) = email {
                psd.insert("email".to_string(), json!(em));
            }
            if let Some(extra_psd) = obj
                .get("providerSpecificData")
                .and_then(|v| v.as_object())
            {
                for (k, v) in extra_psd {
                    psd.insert(k.clone(), v.clone());
                }
            }

            let conn = create_provider_connection(
                &state.db,
                ConnectionCreateInput {
                    provider: "grok-cli".to_string(),
                    auth_type: "oauth".to_string(),
                    display_name: obj
                        .get("displayName")
                        .or_else(|| obj.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    email,
                    access_token: Some(access_token),
                    refresh_token,
                    expires_at,
                    test_status: Some("active".to_string()),
                    provider_specific_data: Some(psd),
                    ..Default::default()
                },
            )?;
            Ok((conn.id, conn.email))
        })();

        match res {
            Ok((id, email)) => {
                success += 1;
                results.push(json!({ "index": i, "ok": true, "id": id, "email": email }));
            }
            Err(err) => {
                failed += 1;
                results.push(json!({ "index": i, "ok": false, "error": err }));
            }
        }
    }

    Json(json!({
        "total": total,
        "success": success,
        "failed": failed,
        "results": results,
    }))
    .into_response()
}

// 8. POST /api/oauth/iflow/cookie
pub async fn iflow_cookie_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(cookie_raw) = body.get("cookie").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "Cookie is required");
    };
    let trimmed = cookie_raw.trim();
    if !trimmed.contains("BXAuth=") {
        return json_error(StatusCode::BAD_REQUEST, "Cookie must contain BXAuth field");
    }
    let normalized_cookie = if trimmed.ends_with(';') {
        trimmed.to_string()
    } else {
        format!("{};", trimmed)
    };

    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";
    let get_res = match state
        .proxy
        .client()
        .get("https://platform.iflow.cn/api/openapi/apikey")
        .header(header::COOKIE, &normalized_cookie)
        .header(header::ACCEPT, "application/json, text/plain, */*")
        .header(header::USER_AGENT, ua)
        .header(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    if !get_res.status().is_success() {
        let st = get_res.status();
        let err_txt = get_res.text().await.unwrap_or_default();
        return json_error(
            st,
            &format!("Failed to fetch API key info: {}", err_txt),
        );
    }
    let get_result: Value = match get_res.json().await {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if get_result.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let msg = get_result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("API key fetch failed: {}", msg),
        );
    }
    let key_name = get_result
        .get("data")
        .and_then(|d| d.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if key_name.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing name in API key info");
    }

    let post_res = match state
        .proxy
        .client()
        .post("https://platform.iflow.cn/api/openapi/apikey")
        .header(header::COOKIE, &normalized_cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/plain, */*")
        .header(header::USER_AGENT, ua)
        .header(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(header::ORIGIN, "https://platform.iflow.cn")
        .header(header::REFERER, "https://platform.iflow.cn/")
        .json(&json!({ "name": key_name }))
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if !post_res.status().is_success() {
        let st = post_res.status();
        let err_txt = post_res.text().await.unwrap_or_default();
        return json_error(
            st,
            &format!("Failed to refresh API key: {}", err_txt),
        );
    }
    let post_result: Value = match post_res.json().await {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if post_result.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let msg = post_result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("API key refresh failed: {}", msg),
        );
    }
    let refreshed = post_result.get("data").cloned().unwrap_or(json!({}));
    let api_key = refreshed
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if api_key.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing API key in response");
    }

    let bx_auth = normalized_cookie
        .split(';')
        .find_map(|p| p.trim().strip_prefix("BXAuth="))
        .unwrap_or("");
    let cookie_to_save = if bx_auth.is_empty() {
        String::new()
    } else {
        format!("BXAuth={};", bx_auth)
    };
    let resolved_name = refreshed
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(key_name)
        .to_string();
    let expire_time = refreshed.get("expireTime").cloned().unwrap_or(Value::Null);

    let mut psd = Map::new();
    psd.insert("cookie".to_string(), json!(cookie_to_save));
    psd.insert("expireTime".to_string(), expire_time.clone());

    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "iflow".to_string(),
            auth_type: "cookie".to_string(),
            name: Some(resolved_name.clone()),
            email: Some(resolved_name),
            api_key: Some(api_key.to_string()),
            test_status: Some("active".to_string()),
            is_active: Some(true),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => {
            let masked: String = api_key.chars().take(10).collect();
            Json(json!({
                "success": true,
                "connection": {
                    "id": conn.id,
                    "provider": conn.provider,
                    "email": conn.email,
                    "apiKey": format!("{}...", masked),
                    "expireTime": expire_time,
                }
            }))
            .into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 9. POST /api/oauth/kiro/api-key
pub async fn kiro_api_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(raw_key) = body.get("apiKey").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "API key is required");
    };
    let trimmed = raw_key.trim();
    if trimmed.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "API key is required");
    }

    let region = body
        .get("region")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1");
    if assert_valid_aws_region(region).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key validation failed");
    }

    let endpoint = format!("https://q.{}.amazonaws.com/ListAvailableModels?origin=AI_EDITOR", region);
    let ok = match state
        .proxy
        .client()
        .get(&endpoint)
        .bearer_auth(trimmed)
        .header("TokenType", "API_KEY")
        .header(header::ACCEPT, "application/json")
        .header(header::USER_AGENT, "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0")
        .header("X-Amz-User-Agent", "aws-sdk-js/3.0.0 kiro-ide/1.0.0")
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|d| d.get("models").and_then(|m| m.as_array()).map(|a| !a.is_empty()))
            .unwrap_or(false),
        _ => false,
    };

    if !ok {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key validation failed");
    }

    let email = extract_email_from_jwt(trimmed);
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339();
    let mut psd = Map::new();
    psd.insert("region".to_string(), json!(region));
    psd.insert("authMethod".to_string(), json!("api_key"));
    psd.insert("provider".to_string(), json!("API Key"));

    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "kiro".to_string(),
            auth_type: "api_key".to_string(),
            access_token: Some(trimmed.to_string()),
            expires_at: Some(expires_at),
            email,
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
            }
        }))
        .into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key validation failed"),
    }
}

// 10. GET /api/oauth/kiro/auto-import
pub async fn kiro_auto_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let home = resolve_home_dir();
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    Json(kiro_auto_import_from_home(&home, app_data.as_deref())).into_response()
}

// 11. POST /api/oauth/kiro/import-cli-proxy
pub async fn kiro_import_cli_proxy_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let raw_auth = body
        .get("cliProxyAuth")
        .or_else(|| body.get("auth"))
        .or_else(|| body.get("json"))
        .unwrap_or(&body);
    let token_data = match normalize_kiro_external_idp_auth(raw_auth) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &e),
    };

    let input = input_from_mapped_tokens("kiro", "oauth", &token_data);
    match create_provider_connection(&state.db, input) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
            }
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &e),
    }
}

// 12. POST /api/oauth/kiro/import
pub async fn kiro_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let Some(refresh_token) = body.get("refreshToken").and_then(|v| v.as_str()) else {
        return json_error(StatusCode::BAD_REQUEST, "Refresh token is required");
    };
    let rt = refresh_token.trim();
    if rt.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Refresh token is required");
    }

    let client_id = body
        .get("clientId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let client_secret = body
        .get("clientSecret")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let region = body
        .get("region")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1");
    let is_idc = client_id.is_some() && client_secret.is_some();

    let refresh_res: Result<Value, String> = if is_idc {
        assert_valid_aws_region(region)
            .map_err(|e| e)
            .and_then(|_| Ok(()))
            .ok();
        if let Err(e) = assert_valid_aws_region(region) {
            Err(e)
        } else {
            let endpoint = format!("https://oidc.{}.amazonaws.com/token", region);
            match state
                .proxy
                .client()
                .post(&endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .json(&json!({
                    "clientId": client_id.unwrap(),
                    "clientSecret": client_secret.unwrap(),
                    "refreshToken": rt,
                    "grantType": "refresh_token",
                }))
                .timeout(Duration::from_secs(20))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r
                    .json::<Value>()
                    .await
                    .map_err(|e| format!("Token refresh invalid JSON: {}", e)),
                Ok(r) => Err(format!(
                    "Token refresh failed: {}",
                    r.text().await.unwrap_or_default()
                )),
                Err(e) => Err(format!("Token refresh failed: {}", e)),
            }
        }
    } else {
        match state
            .proxy
            .client()
            .post(format!("{}/refreshToken", KIRO_AUTH_SERVICE))
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({ "refreshToken": rt }))
            .timeout(Duration::from_secs(20))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .map_err(|e| format!("Token refresh invalid JSON: {}", e)),
            Ok(r) => Err(format!(
                "Token refresh failed: {}",
                r.text().await.unwrap_or_default()
            )),
            Err(e) => Err(format!("Token refresh failed: {}", e)),
        }
    };

    let token_data = match refresh_res {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let access_token = token_data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let new_rt = token_data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .unwrap_or(rt);
    let expires_in = token_data
        .get("expiresIn")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let email = extract_email_from_jwt(access_token);
    let resolved_profile_arn = body
        .get("profileArn")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            token_data
                .get("profileArn")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let mut psd = Map::new();
    psd.insert(
        "profileArn".to_string(),
        resolved_profile_arn
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    psd.insert(
        "authMethod".to_string(),
        json!(if is_idc { "idc" } else { "imported" }),
    );
    psd.insert(
        "provider".to_string(),
        json!(if is_idc { "Enterprise" } else { "Imported" }),
    );
    if is_idc {
        psd.insert("clientId".to_string(), json!(client_id.unwrap()));
        psd.insert("clientSecret".to_string(), json!(client_secret.unwrap()));
        psd.insert("region".to_string(), json!(region));
    }

    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339();
    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "kiro".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some(access_token.to_string()),
            refresh_token: Some(new_rt.to_string()),
            expires_at: Some(expires_at),
            email,
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
            }
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 13. GET /api/oauth/kiro/social-authorize
pub async fn kiro_social_authorize_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let provider = query.get("provider").map(|s| s.as_str()).unwrap_or("");
    if provider != "google" && provider != "github" {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Invalid provider. Use 'google' or 'github'",
        );
    }

    let (code_verifier, code_challenge, pkce_state) = generate_pkce(32);
    let idp = if provider == "google" {
        "Google"
    } else {
        "Github"
    };
    let auth_url = format!(
        "{}/login?idp={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&prompt=select_account",
        KIRO_AUTH_SERVICE,
        idp,
        encode_uri_component(KIRO_SOCIAL_REDIRECT_URI),
        code_challenge,
        pkce_state
    );

    Json(json!({
        "authUrl": auth_url,
        "state": pkce_state,
        "codeVerifier": code_verifier,
        "codeChallenge": code_challenge,
        "provider": provider,
    }))
    .into_response()
}

// 14. POST /api/oauth/kiro/social-exchange
pub async fn kiro_social_exchange_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let code_verifier = body
        .get("codeVerifier")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if code.is_empty() || code_verifier.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing required fields");
    }
    if provider != "google" && provider != "github" {
        return json_error(StatusCode::BAD_REQUEST, "Invalid provider");
    }

    let resp = match state
        .proxy
        .client()
        .post(format!("{}/oauth/token", KIRO_AUTH_SERVICE))
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "code": code,
            "code_verifier": code_verifier,
            "redirect_uri": KIRO_SOCIAL_REDIRECT_URI,
        }))
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Token exchange failed: {}", err),
        );
    }
    let token_data: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let access_token = token_data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let refresh_token = token_data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let expires_in = token_data
        .get("expiresIn")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let email = extract_email_from_jwt(access_token);
    let provider_cap = if provider == "google" {
        "Google"
    } else {
        "Github"
    };

    let mut psd = Map::new();
    psd.insert(
        "profileArn".to_string(),
        token_data.get("profileArn").cloned().unwrap_or(Value::Null),
    );
    psd.insert("authMethod".to_string(), json!(provider));
    psd.insert("provider".to_string(), json!(provider_cap));

    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339();
    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "kiro".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some(access_token.to_string()),
            refresh_token,
            expires_at: Some(expires_at),
            email,
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
            }
        }))
        .into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 15. POST /api/oauth/xiaomi-mimo/api-key
pub async fn xiaomi_mimo_api_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let key = body
        .get("apiKey")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let uid = body
        .get("uid")
        .and_then(json_value_as_string);
    let base_url = body
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.xiaomimimo.com/v1".to_string());
    let mimo_pass_token = body
        .get("mimoPassToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let mimo_user_id = body
        .get("mimoUserId")
        .and_then(json_value_as_string);
    let mimo_c_user_id = body
        .get("mimoCUserId")
        .and_then(json_value_as_string);
    let norm_region = body
        .get("region")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let session_only = key.is_empty() && mimo_pass_token.is_some();
    if key.is_empty() && mimo_pass_token.is_none() {
        return json_error(StatusCode::BAD_REQUEST, "API key is required");
    }
    if !key.is_empty() && !key.starts_with("sk-") {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Invalid key format — expected sk- prefix",
        );
    }

    let mut validated = false;
    let mut model_count = 0usize;
    if !key.is_empty() {
        if let Ok(resp) = state
            .proxy
            .client()
            .get(format!("{}/models", base_url))
            .bearer_auth(&key)
            .header("X-Mimo-Source", "mimocode-cli")
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(data) = resp.json::<Value>().await {
                    model_count = data
                        .get("data")
                        .and_then(|d| d.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    validated = true;
                }
            }
        }
    }

    let all_conns = match state.db.load_provider_connections() {
        Ok(c) => c,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key import failed"),
    };

    let existing = all_conns.into_iter().find(|c| {
        if c.provider != "xiaomi-mimo" {
            return false;
        }
        let raw_val: Value = serde_json::from_str(&c.raw_data).unwrap_or(Value::Null);
        let psd = raw_val
            .get("providerSpecificData")
            .and_then(|v| v.as_object());
        let existing_token = raw_val.get("accessToken").and_then(|v| v.as_str());
        let uid_match = uid
            .as_ref()
            .map(|u| c.email.as_deref() == Some(&format!("{}@xiaomi", u)))
            .unwrap_or(false);
        let key_match = !key.is_empty() && existing_token == Some(key.as_str());
        let sess_match = session_only
            && mimo_user_id.as_ref().is_some_and(|muid| {
                let existing_muid = psd
                    .and_then(|m| m.get("mimoUserId"))
                    .and_then(|v| v.as_str());
                let existing_reg = psd
                    .and_then(|m| m.get("region"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("cn");
                existing_muid == Some(muid.as_str())
                    && norm_region
                        .as_deref()
                        .map(|nr| existing_reg == nr)
                        .unwrap_or(true)
            });
        uid_match || key_match || sess_match
    });

    if let Some(mut ex) = existing {
        let mut raw_map = serde_json::from_str::<Value>(&ex.raw_data)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut psd = raw_map
            .get("providerSpecificData")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if !key.is_empty() {
            raw_map.insert("accessToken".to_string(), json!(key));
        }
        if let Some(ref u) = uid {
            psd.insert("uid".to_string(), json!(u));
        }
        if !key.is_empty() || !psd.contains_key("baseUrl") {
            psd.insert("baseUrl".to_string(), json!(base_url));
        }
        if let Some(ref nr) = norm_region {
            psd.insert("region".to_string(), json!(nr));
        } else if !psd.contains_key("region") {
            psd.insert("region".to_string(), json!("cn"));
        }
        if session_only {
            psd.insert("authMethod".to_string(), json!("session"));
        } else if !psd.contains_key("authMethod") {
            psd.insert("authMethod".to_string(), json!("api_key"));
        }
        if let Some(ref pt) = mimo_pass_token {
            psd.insert("mimoPassToken".to_string(), json!(pt));
        }
        if let Some(ref muid) = mimo_user_id {
            psd.insert("mimoUserId".to_string(), json!(muid));
        }
        if let Some(ref mcuid) = mimo_c_user_id {
            psd.insert("mimoCUserId".to_string(), json!(mcuid));
        }
        psd.insert("modelCount".to_string(), json!(model_count));
        raw_map.insert("providerSpecificData".to_string(), Value::Object(psd));
        if validated {
            raw_map.insert("testStatus".to_string(), json!("active"));
        }
        let display_name = raw_map
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        ex.raw_data = Value::Object(raw_map).to_string();
        ex.data = serde_json::from_str(&ex.raw_data).unwrap_or_default();
        ex.updated_at = chrono::Utc::now().to_rfc3339();
        if state.db.save_provider_connection(&ex).is_err() {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key import failed");
        }
        return Json(json!({
            "success": true,
            "validated": validated,
            "modelCount": model_count,
            "updated": true,
            "connection": {
                "id": ex.id,
                "provider": ex.provider,
                "email": ex.email,
                "displayName": display_name,
            }
        }))
        .into_response();
    }

    let email = uid.as_ref().map(|u| format!("{}@xiaomi", u));
    let display_name = match &uid {
        Some(u) => format!("Xiaomi {}{}", u, if session_only { " (Session)" } else { "" }),
        None => "Xiaomi MiMo".to_string(),
    };
    let mut psd = Map::new();
    psd.insert("uid".to_string(), uid.map(Value::String).unwrap_or(Value::Null));
    psd.insert("baseUrl".to_string(), json!(base_url));
    psd.insert(
        "authMethod".to_string(),
        json!(if session_only { "session" } else { "api_key" }),
    );
    psd.insert(
        "provider".to_string(),
        json!(if session_only { "Session Login" } else { "API Key" }),
    );
    psd.insert(
        "region".to_string(),
        json!(norm_region.as_deref().unwrap_or("cn")),
    );
    psd.insert("modelCount".to_string(), json!(model_count));
    psd.insert(
        "mimoPassToken".to_string(),
        mimo_pass_token.map(Value::String).unwrap_or(Value::Null),
    );
    psd.insert(
        "mimoUserId".to_string(),
        mimo_user_id.map(Value::String).unwrap_or(Value::Null),
    );
    psd.insert(
        "mimoCUserId".to_string(),
        mimo_c_user_id.map(Value::String).unwrap_or(Value::Null),
    );

    let test_status = if validated || session_only {
        "active"
    } else {
        "untested"
    };
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339();
    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "xiaomi-mimo".to_string(),
            auth_type: if session_only {
                "oauth".to_string()
            } else {
                "api_key".to_string()
            },
            access_token: if key.is_empty() { None } else { Some(key) },
            expires_at: Some(expires_at),
            email,
            display_name: Some(display_name.clone()),
            test_status: Some(test_status.to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => Json(json!({
            "success": true,
            "validated": validated,
            "modelCount": model_count,
            "connection": {
                "id": conn.id,
                "provider": conn.provider,
                "email": conn.email,
                "displayName": display_name,
            }
        }))
        .into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "API key import failed"),
    }
}

// 16. GET /api/oauth/xiaomi-mimo/auto-import
pub async fn xiaomi_mimo_auto_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let home = resolve_home_dir();
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    Json(xiaomi_mimo_auto_import_from_home(&home, app_data.as_deref())).into_response()
}

// 17. POST /api/oauth/xiaomi-mimo/login/start
pub async fn xiaomi_mimo_login_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let mut region = "cn".to_string();
    if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(&body) {
        if let Some(r) = obj.get("region").and_then(|v| v.as_str()) {
            let low = r.to_ascii_lowercase();
            if matches!(low.as_str(), "cn" | "sgp" | "ams" | "ru" | "in") {
                region = low;
            }
        }
    }

    let egress = if region != "cn" {
        probe_local_http_proxy().await
    } else {
        None
    };

    let mut sess = MimoLoginSession {
        state: uuid_v4(),
        region: region.clone(),
        proxy_url: egress.clone(),
        jar: HashMap::new(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        upstream_base: resolve_mimo_region_base(&region).to_string(),
    };

    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15));
    if let Some(ref p) = egress {
        if let Ok(proxy) = reqwest::Proxy::all(p) {
            builder = builder.proxy(proxy);
        }
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let api_ua = "miNative PC/Normal Windows_NT/10.0.19045 SDKV/1.0.0 DEVT/PC DEVS/Windows APP/miaccount_desktop APPV/0.1.0";
    let sso_ua = "MiClaw/1.0";

    let me_url = format!("{}/api/user/xiaomi/me", sess.upstream_base);
    let me_res = match client
        .get(&me_url)
        .header(header::USER_AGENT, api_ua)
        .header(header::ACCEPT, "text/html,application/json,*/*")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    absorb_mimo_set_cookies(&mut sess, &me_res, &me_url);
    let sso_loc = me_res
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let Some(sso_loc) = sso_loc.filter(|s| s.contains("account.xiaomi.com")) else {
        return json_error(
            StatusCode::BAD_GATEWAY,
            &format!(
                "Unexpected me response ({}) — no account redirect",
                me_res.status().as_u16()
            ),
        );
    };

    let login_res = match client
        .get(&sso_loc)
        .header(header::USER_AGENT, sso_ua)
        .header(header::ACCEPT, "text/html,application/json,*/*")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    absorb_mimo_set_cookies(&mut sess, &login_res, &sso_loc);
    let page_loc = login_res
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let Some(page_loc) = page_loc else {
        return json_error(
            StatusCode::BAD_GATEWAY,
            &format!(
                "Unexpected serviceLogin response ({})",
                login_res.status().as_u16()
            ),
        );
    };

    let base_acct = url::Url::parse("https://account.xiaomi.com").unwrap();
    let page_url = base_acct
        .join(&page_loc)
        .unwrap_or(base_acct);
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:20131");
    let origin = format!("{}://{}", proto, host);
    let path_and_query = match page_url.query() {
        Some(q) => format!("{}?{}", page_url.path(), q),
        None => page_url.path().to_string(),
    };
    let proxied_path = rewrite_mimo_to_proxy(&path_and_query, &origin);

    let cookie_val = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=900",
        MIMO_SESSION_COOKIE,
        encode_mimo_session_cookie(&sess)
    );

    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie_val)],
        Json(json!({
            "success": true,
            "state": sess.state,
            "pageUrl": proxied_path,
            "region": region,
        })),
    )
        .into_response()
}

// 18. GET /api/oauth/xiaomi-mimo/login/status
pub async fn xiaomi_mimo_login_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let state_param = query.get("state").map(|s| s.as_str()).unwrap_or("");
    let raw_cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cs| {
            cs.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix(&format!("{}=", MIMO_SESSION_COOKIE))
                    .map(|v| v.trim().to_string())
            })
        });

    let Some(sess) = raw_cookie.as_deref().and_then(decode_mimo_session_cookie) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "status": "expired" }))).into_response();
    };
    if !state_param.is_empty() && sess.state != state_param {
        return (StatusCode::NOT_FOUND, Json(json!({ "status": "expired" }))).into_response();
    }

    if let Some((pass_token, user_id, c_user_id)) = read_mimo_session_identity(&sess) {
        let clear_cookie = format!("{}=; Path=/; HttpOnly; Max-Age=0", MIMO_SESSION_COOKIE);
        return (
            StatusCode::OK,
            [(header::SET_COOKIE, clear_cookie)],
            Json(json!({
                "status": "done",
                "region": sess.region,
                "passToken": pass_token,
                "userId": user_id,
                "cUserId": c_user_id,
            })),
        )
            .into_response();
    }

    let rearm_cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        MIMO_SESSION_COOKIE,
        encode_mimo_session_cookie(&sess),
        MIMO_SESSION_TTL_MS / 1000
    );
    (
        StatusCode::OK,
        [(header::SET_COOKIE, rearm_cookie)],
        Json(json!({
            "status": "pending",
            "region": sess.region,
        })),
    )
        .into_response()
}

// 19. GET /api/oauth/zed/auto-import
pub async fn zed_auto_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let home = resolve_home_dir();
    let credentials_url = resolve_zed_credentials_url(&home);
    match read_zed_keyring_credentials(&credentials_url).await {
        Ok((user_id, access_token)) => {
            let system_id = read_zed_system_id_from_home(&home).unwrap_or_else(uuid_v4);
            Json(json!({
                "found": true,
                "userId": user_id,
                "accessToken": access_token,
                "systemId": system_id,
                "credentialsUrl": credentials_url,
            }))
            .into_response()
        }
        Err(err) => Json(json!({
            "found": false,
            "error": err,
            "credentialsUrl": credentials_url,
        }))
        .into_response(),
    }
}

// 20. POST /api/oauth/zed/import
pub async fn zed_import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let access_token = body
        .get("accessToken")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let user_id = body
        .get("userId")
        .and_then(json_value_as_string)
        .unwrap_or_default();
    let system_id = body
        .get("systemId")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(uuid_v4);

    if access_token.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Access token is required");
    }
    if user_id.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "User id is required");
    }

    let user_info = match fetch_zed_authenticated_user(
        state.proxy.client(),
        &user_id,
        &access_token,
        Some(&system_id),
    )
    .await
    {
        Ok(u) => u,
        Err(e) => return json_error(StatusCode::UNAUTHORIZED, &e),
    };

    let org_id = resolve_zed_organization_id(None, Some(&user_info));
    let email = user_info
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let display_name = user_info
        .get("name")
        .or_else(|| user_info.get("display_name"))
        .or_else(|| user_info.get("username"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("Zed {}", user_id));

    let mut psd = Map::new();
    psd.insert("authMethod".to_string(), json!("imported"));
    psd.insert("userId".to_string(), json!(user_id));
    psd.insert("systemId".to_string(), json!(system_id));
    psd.insert("organizationId".to_string(), json!(org_id));

    match create_provider_connection(
        &state.db,
        ConnectionCreateInput {
            provider: "zed".to_string(),
            auth_type: "oauth".to_string(),
            access_token: Some(access_token),
            email,
            display_name: Some(display_name),
            test_status: Some("active".to_string()),
            provider_specific_data: Some(psd),
            ..Default::default()
        },
    ) {
        Ok(conn) => {
            let dn = serde_json::from_str::<Value>(&conn.raw_data)
                .ok()
                .and_then(|v| v.get("displayName").and_then(|d| d.as_str()).map(|s| s.to_string()));
            Json(json!({
                "success": true,
                "connection": {
                    "id": conn.id,
                    "provider": conn.provider,
                    "email": conn.email,
                    "displayName": dn,
                }
            }))
            .into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// 21a. GET /api/oauth/{provider}/{action}
pub async fn oauth_dynamic_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((provider, action)): AxumPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    match action.as_str() {
        "authorize" => {
            if provider == "xiaomi-mimo" {
                let (pub_key, priv_key) = generate_xiaomi_x25519_keypair();
                let st = query
                    .get("state")
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(uuid_v4);
                let proxy_res =
                    start_dynamic_callback_proxy("xiaomi-mimo", 0, state.clone()).await;
                if proxy_res.get("success").and_then(|v| v.as_bool()) != Some(true) {
                    let reason = proxy_res
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("Failed to start callback server: {}", reason),
                    );
                }
                if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                    m.insert(
                        st.clone(),
                        XiaomiMimoSession {
                            private_key: priv_key,
                            status: "pending".to_string(),
                            uid: None,
                            access_token: None,
                            base_url: None,
                            error: None,
                            created_at: Instant::now(),
                        },
                    );
                }
                let redirect_uri = proxy_res
                    .get("callbackUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let platform_url = std::env::var("MIMO_PLATFORM_URL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "https://platform.xiaomimimo.com".to_string());
                let host_id = std::env::var("HOSTNAME")
                    .or_else(|_| std::env::var("COMPUTERNAME"))
                    .unwrap_or_else(|_| "unknown".to_string());
                let mid_digest = digest::digest(
                    &digest::SHA256,
                    format!("{}-{}", std::env::consts::OS, host_id).as_bytes(),
                );
                let key_name = format!("9router-xmd-{}", &hex_encode(mid_digest.as_ref())[..8]);

                let mut auth_u = url::Url::parse(&format!(
                    "{}/authorize",
                    platform_url.trim_end_matches('/')
                ))
                .unwrap();
                auth_u
                    .query_pairs_mut()
                    .append_pair("pk", &pub_key)
                    .append_pair("redirect_uri", redirect_uri)
                    .append_pair("kn", "mimocode")
                    .append_pair("key_name", &key_name);

                return Json(json!({
                    "state": st,
                    "authorizeUrl": auth_u.to_string(),
                    "redirectUri": redirect_uri,
                    "port": proxy_res.get("port"),
                }))
                .into_response();
            }

            let redirect_uri = query
                .get("redirect_uri")
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("http://localhost:8080/callback");
            let mut meta: HashMap<String, String> = query
                .iter()
                .filter(|(k, _)| k.as_str() != "redirect_uri")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if provider == "zed" {
                if let Ok(u) = url::Url::parse(redirect_uri) {
                    if let Some(p) = u.port() {
                        meta.insert("nativeAppPort".to_string(), p.to_string());
                    }
                }
            }

            match generate_auth_data(state.proxy.client(), &provider, Some(redirect_uri), &meta)
                .await
            {
                Ok(data) => Json(data).into_response(),
                Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        "start-proxy" => match provider.as_str() {
            "trae" => Json(start_dynamic_callback_proxy("trae", 0, state).await).into_response(),
            "windsurf" => {
                Json(start_dynamic_callback_proxy("windsurf", 0, state).await).into_response()
            }
            "zed" => {
                let port = query
                    .get("native_app_port")
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(ZED_DEFAULT_NATIVE_APP_PORT);
                Json(start_dynamic_callback_proxy("zed", port, state).await).into_response()
            }
            "xiaomi-mimo" => {
                Json(start_dynamic_callback_proxy("xiaomi-mimo", 0, state).await).into_response()
            }
            "codex" | "xai" => {
                let Some(app_port) = query.get("app_port").and_then(|s| s.parse::<u16>().ok())
                else {
                    return json_error(StatusCode::BAD_REQUEST, "Missing app_port");
                };
                let fixed = if provider == "xai" {
                    XAI_LOOPBACK_PORT
                } else {
                    CODEX_FIXED_PORT
                };
                let prov_static: &'static str = if provider == "xai" { "xai" } else { "codex" };
                let result = start_fixed_oauth_proxy(prov_static, fixed, app_port, state).await;
                let mut server_side = false;
                if result.get("success").and_then(|v| v.as_bool()) == Some(true) {
                    if let (Some(st), Some(cv), Some(ru)) = (
                        query.get("state").filter(|s| !s.is_empty()),
                        query.get("code_verifier").filter(|s| !s.is_empty()),
                        query.get("redirect_uri").filter(|s| !s.is_empty()),
                    ) {
                        let sessions_lock = if prov_static == "xai" {
                            &runtime().xai_sessions
                        } else {
                            &runtime().codex_sessions
                        };
                        if let Ok(mut m) = sessions_lock.lock() {
                            m.insert(
                                st.clone(),
                                ProxyExchangeSession {
                                    code_verifier: cv.clone(),
                                    redirect_uri: ru.clone(),
                                    status: "pending".to_string(),
                                    connection_id: None,
                                    email: None,
                                    error: None,
                                    created_at: Instant::now(),
                                },
                            );
                            server_side = true;
                        }
                    }
                }
                let mut out = result;
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("serverSide".to_string(), json!(server_side));
                }
                Json(out).into_response()
            }
            _ => json_error(
                StatusCode::BAD_REQUEST,
                "Proxy only supported for codex/xai/trae/windsurf/zed",
            ),
        },
        "poll-status" => {
            let Some(st) = query.get("state").filter(|s| !s.is_empty()) else {
                return json_error(StatusCode::BAD_REQUEST, "Missing state");
            };
            match provider.as_str() {
                "xiaomi-mimo" => {
                    let sess = runtime()
                        .xiaomi_sessions
                        .lock()
                        .ok()
                        .and_then(|mut m| {
                            let ttl = Duration::from_secs(PROXY_IDLE_TIMEOUT_SECS);
                            m.retain(|_, v| v.created_at.elapsed() < ttl);
                            m.get(st).cloned()
                        });
                    let Some(s) = sess else {
                        return Json(json!({ "status": "unknown" })).into_response();
                    };
                    if s.status == "done" || s.status == "error" {
                        let result_val = if s.access_token.is_some() {
                            json!({
                                "uid": s.uid,
                                "accessToken": s.access_token,
                                "baseUrl": s.base_url.unwrap_or_else(|| "https://api.xiaomimimo.com/v1".to_string()),
                            })
                        } else {
                            Value::Null
                        };
                        let payload = json!({
                            "status": s.status,
                            "result": result_val,
                            "error": s.error,
                        });
                        if s.status == "error" {
                            if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                                m.remove(st);
                            }
                            stop_proxy_slot(&runtime().xiaomi_proxy);
                        }
                        return Json(payload).into_response();
                    }
                    Json(json!({ "status": s.status })).into_response()
                }
                "trae" | "windsurf" | "zed" => {
                    let lock = match provider.as_str() {
                        "trae" => &runtime().trae_session,
                        "windsurf" => &runtime().windsurf_session,
                        _ => &runtime().zed_session,
                    };
                    let sess = lock.lock().ok().and_then(|mut g| {
                        if let Some(s) = g.as_ref() {
                            if s.created_at.elapsed() >= Duration::from_secs(LONG_PROXY_IDLE_TIMEOUT_SECS) {
                                *g = None;
                                return None;
                            }
                            if s.state == *st {
                                return Some(s.clone());
                            }
                        }
                        None
                    });
                    let Some(s) = sess else {
                        return Json(json!({ "status": "unknown" })).into_response();
                    };
                    if s.status == "done" || s.status == "error" {
                        if let Ok(mut g) = lock.lock() {
                            if g.as_ref().is_some_and(|x| x.state == *st) {
                                *g = None;
                            }
                        }
                        return Json(json!({
                            "status": s.status,
                            "connectionId": s.connection_id,
                            "email": s.email,
                            "error": s.error,
                        }))
                        .into_response();
                    }
                    Json(json!({ "status": s.status })).into_response()
                }
                "codex" | "xai" => {
                    let lock = if provider == "xai" {
                        &runtime().xai_sessions
                    } else {
                        &runtime().codex_sessions
                    };
                    let sess = lock.lock().ok().and_then(|mut m| {
                        let ttl = Duration::from_secs(PROXY_IDLE_TIMEOUT_SECS);
                        m.retain(|_, v| v.created_at.elapsed() < ttl);
                        m.get(st).cloned()
                    });
                    let Some(s) = sess else {
                        return Json(json!({ "status": "unknown" })).into_response();
                    };
                    if s.status == "done" || s.status == "error" {
                        if let Ok(mut m) = lock.lock() {
                            m.remove(st);
                        }
                        return Json(json!({
                            "status": s.status,
                            "connectionId": s.connection_id,
                            "email": s.email,
                            "error": s.error,
                        }))
                        .into_response();
                    }
                    Json(json!({ "status": s.status })).into_response()
                }
                _ => json_error(
                    StatusCode::BAD_REQUEST,
                    "Poll only supported for codex/xai/trae/windsurf/zed/xiaomi-mimo",
                ),
            }
        }
        "stop-proxy" => {
            match provider.as_str() {
                "trae" => stop_proxy_slot(&runtime().trae_proxy),
                "windsurf" => stop_proxy_slot(&runtime().windsurf_proxy),
                "zed" => stop_proxy_slot(&runtime().zed_proxy),
                "xai" => stop_proxy_slot(&runtime().xai_proxy),
                "codex" => stop_proxy_slot(&runtime().codex_proxy),
                "xiaomi-mimo" => {
                    stop_proxy_slot(&runtime().xiaomi_proxy);
                    if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                        m.clear();
                    }
                }
                _ => {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        "Proxy only supported for codex/xai/trae/windsurf/zed/xiaomi-mimo",
                    )
                }
            }
            Json(json!({ "success": true })).into_response()
        }
        "ide-status" => {
            if provider != "trae" && provider != "windsurf" {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "ide-status only supported for trae/windsurf",
                );
            }
            let (installed, path) = detect_ide_installed(&provider).await;
            Json(json!({ "installed": installed, "path": path })).into_response()
        }
        "device-code" => {
            let flow = match provider_flow_type(&provider) {
                Ok(f) => f,
                Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            };
            if flow != "device_code" {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "Provider does not support device code flow",
                );
            }
            let meta = HashMap::new();
            let auth_data =
                match generate_auth_data(state.proxy.client(), &provider, None, &meta).await {
                    Ok(d) => d,
                    Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                };
            let code_verifier_default = auth_data
                .get("codeVerifier")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let code_challenge = auth_data.get("codeChallenge").and_then(|v| v.as_str());

            let device_options = if provider == "kiro" {
                let mut opts = Map::new();
                if let Some(su) = query.get("start_url").filter(|s| !s.is_empty()) {
                    opts.insert("startUrl".to_string(), json!(su));
                }
                if let Some(reg) = query.get("region").filter(|s| !s.is_empty()) {
                    opts.insert("region".to_string(), json!(reg));
                }
                if let Some(am) = query.get("auth_method").filter(|s| !s.is_empty()) {
                    opts.insert("authMethod".to_string(), json!(am));
                }
                Value::Object(opts)
            } else {
                json!({})
            };

            let no_pkce = matches!(
                provider.as_str(),
                "github"
                    | "kiro"
                    | "kimi"
                    | "kimi-coding"
                    | "kilocode"
                    | "codebuddy-cn"
                    | "codebuddy-intl"
                    | "qoder"
                    | "qoder-cn"
                    | "grok-cli"
                    | "muse"
                    | "glm"
                    | "nous"
            );
            let challenge_arg = if no_pkce { None } else { code_challenge };

            let mut device_data = match request_device_code(
                state.proxy.client(),
                &provider,
                challenge_arg,
                &device_options,
            )
            .await
            {
                Ok(d) => d,
                Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            };
            if let Some(obj) = device_data.as_object_mut() {
                if !obj.contains_key("codeVerifier") {
                    obj.insert("codeVerifier".to_string(), json!(code_verifier_default));
                }
            }
            Json(device_data).into_response()
        }
        "lease-status" => {
            if provider != "minimax-code" && provider != "minimax-code-global" {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "lease-status only supported for minimax-code/minimax-code-global",
                );
            }
            let data_dir = resolve_mcode_data_dir(query.get("data_dir").map(|s| s.as_str()));
            let req = json!({
                "requestId": format!("9r-status-{}", chrono::Utc::now().timestamp_millis()),
                "method": "status",
            });
            match send_mcode_lease_request(&data_dir, req, 5000).await {
                Ok(res) => Json(json!({
                    "found": true,
                    "dataDir": data_dir.display().to_string(),
                    "status": res.get("status"),
                    "generation": res.get("generation"),
                    "expiresAtMs": res.get("expiresAtMs"),
                }))
                .into_response(),
                Err((code, msg)) => Json(json!({
                    "found": false,
                    "dataDir": data_dir.display().to_string(),
                    "error": code,
                    "message": msg,
                }))
                .into_response(),
            }
        }
        _ => json_error(StatusCode::BAD_REQUEST, "Unknown action"),
    }
}

// 21b. POST /api/oauth/{provider}/{action}
pub async fn oauth_dynamic_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((provider, action)): AxumPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    raw_body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let body: Value = match serde_json::from_slice(&raw_body) {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid or empty request body"),
    };

    match action.as_str() {
        "lease-connect" => {
            if provider != "minimax-code" && provider != "minimax-code-global" {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "lease-connect only supported for minimax-code/minimax-code-global",
                );
            }
            let data_dir = resolve_mcode_data_dir(body.get("dataDir").and_then(|v| v.as_str()));
            let req = json!({
                "requestId": format!("9r-lease-{}", chrono::Utc::now().timestamp_millis()),
                "method": "lease",
                "minValidityMs": MCODE_MAX_MIN_VALIDITY_MS,
            });
            if let Err((code, msg)) = send_mcode_lease_request(&data_dir, req, 10000).await {
                return (StatusCode::CONFLICT, Json(json!({ "error": msg, "code": code })))
                    .into_response();
            }

            let data_dir_str = data_dir.display().to_string();
            let all = state.db.load_provider_connections().unwrap_or_default();
            if let Some(existing) = all.into_iter().find(|c| {
                if c.provider != provider || c.auth_type != "mcode-lease" {
                    return false;
                }
                let raw_val: Value = serde_json::from_str(&c.raw_data).unwrap_or(Value::Null);
                let dir = raw_val
                    .get("providerSpecificData")
                    .and_then(|p| p.get("mcodeDataDir"))
                    .and_then(|v| v.as_str())
                    .map(|s| resolve_mcode_data_dir(Some(s)).display().to_string())
                    .unwrap_or_else(|| resolve_mcode_data_dir(None).display().to_string());
                dir == data_dir_str
            }) {
                return Json(json!({
                    "success": true,
                    "connection": { "id": existing.id, "provider": existing.provider },
                    "reused": true,
                }))
                .into_response();
            }

            let mut psd = Map::new();
            psd.insert("mcodeDataDir".to_string(), json!(data_dir_str));
            let name = if provider == "minimax-code-global" {
                "MiniMax Desktop (Lease, Global)"
            } else {
                "MiniMax Desktop (Lease)"
            };
            match create_provider_connection(
                &state.db,
                ConnectionCreateInput {
                    provider: provider.clone(),
                    auth_type: "mcode-lease".to_string(),
                    name: Some(name.to_string()),
                    test_status: Some("active".to_string()),
                    provider_specific_data: Some(psd),
                    ..Default::default()
                },
            ) {
                Ok(conn) => Json(json!({
                    "success": true,
                    "connection": { "id": conn.id, "provider": conn.provider },
                }))
                .into_response(),
                Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        "register-session" => {
            let st = query
                .get("state")
                .map(|s| s.as_str())
                .or_else(|| body.get("state").and_then(|v| v.as_str()))
                .filter(|s| !s.is_empty());
            let Some(st) = st else {
                return json_error(StatusCode::BAD_REQUEST, "Missing state");
            };
            let ok = match provider.as_str() {
                "trae" => {
                    if let Ok(mut g) = runtime().trae_session.lock() {
                        *g = Some(SingletonSession {
                            state: st.to_string(),
                            code_verifier: None,
                            system_id: None,
                            status: "pending".to_string(),
                            connection_id: None,
                            email: None,
                            error: None,
                            created_at: Instant::now(),
                        });
                        true
                    } else {
                        false
                    }
                }
                "windsurf" => {
                    if let Ok(mut g) = runtime().windsurf_session.lock() {
                        *g = Some(SingletonSession {
                            state: st.to_string(),
                            code_verifier: None,
                            system_id: None,
                            status: "pending".to_string(),
                            connection_id: None,
                            email: None,
                            error: None,
                            created_at: Instant::now(),
                        });
                        true
                    } else {
                        false
                    }
                }
                "zed" => {
                    let cv = body
                        .get("codeVerifier")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());
                    if let Some(cv) = cv {
                        if let Ok(mut g) = runtime().zed_session.lock() {
                            *g = Some(SingletonSession {
                                state: st.to_string(),
                                code_verifier: Some(cv.to_string()),
                                system_id: body
                                    .get("systemId")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                status: "pending".to_string(),
                                connection_id: None,
                                email: None,
                                error: None,
                                created_at: Instant::now(),
                            });
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                _ => {
                    return json_error(
                        StatusCode::BAD_REQUEST,
                        "register-session only supported for trae/windsurf/zed",
                    )
                }
            };
            Json(json!({ "success": ok })).into_response()
        }
        "exchange" => {
            let code = body.get("code").and_then(|v| v.as_str());
            let redirect_uri = body.get("redirectUri").and_then(|v| v.as_str());
            let code_verifier = body.get("codeVerifier").and_then(|v| v.as_str());
            let st = body.get("state").and_then(|v| v.as_str());
            let system_id = body.get("systemId").and_then(|v| v.as_str());
            let meta = body.get("meta").cloned().unwrap_or_else(|| json!({}));

            if provider == "xiaomi-mimo" {
                let Some(st) = st.filter(|s| !s.is_empty()) else {
                    return json_error(StatusCode::BAD_REQUEST, "Missing state");
                };
                let sess = runtime()
                    .xiaomi_sessions
                    .lock()
                    .ok()
                    .and_then(|m| m.get(st).cloned());
                let Some(sess) = sess.as_ref().filter(|s| s.status == "done" && s.access_token.is_some()).cloned()
                else {
                    let msg = sess
                        .and_then(|s| s.error)
                        .unwrap_or_else(|| {
                            "OAuth session not completed. Please restart the login flow."
                                .to_string()
                        });
                    return json_error(StatusCode::BAD_REQUEST, &msg);
                };
                let uid = sess.uid;
                let access_token = sess.access_token.unwrap();
                let base_url = sess
                    .base_url
                    .unwrap_or_else(|| "https://api.xiaomimimo.com/v1".to_string());
                let pass_token = read_desktop_pass_token_from_home(&resolve_home_dir());

                let email = uid.as_ref().map(|u| format!("{}@xiaomi", u));
                let display_name = uid
                    .as_ref()
                    .map(|u| format!("Xiaomi {}", u))
                    .unwrap_or_else(|| "Xiaomi MiMo".to_string());
                let mut psd = Map::new();
                psd.insert("uid".to_string(), uid.map(Value::String).unwrap_or(Value::Null));
                psd.insert("baseUrl".to_string(), json!(base_url));
                psd.insert("authMethod".to_string(), json!("oauth"));
                psd.insert(
                    "mimoPassToken".to_string(),
                    pass_token
                        .as_ref()
                        .map(|(pt, _, _)| json!(pt))
                        .unwrap_or(Value::Null),
                );
                psd.insert(
                    "mimoUserId".to_string(),
                    pass_token
                        .as_ref()
                        .and_then(|(_, u, _)| u.as_ref())
                        .map(|u| json!(u))
                        .unwrap_or(Value::Null),
                );
                psd.insert(
                    "mimoCUserId".to_string(),
                    pass_token
                        .as_ref()
                        .and_then(|(_, _, cu)| cu.as_ref())
                        .map(|cu| json!(cu))
                        .unwrap_or(Value::Null),
                );

                let expires_at = (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339();
                let res = create_provider_connection(
                    &state.db,
                    ConnectionCreateInput {
                        provider: "xiaomi-mimo".to_string(),
                        auth_type: "oauth".to_string(),
                        access_token: Some(access_token),
                        expires_at: Some(expires_at),
                        email,
                        display_name: Some(display_name.clone()),
                        test_status: Some("active".to_string()),
                        provider_specific_data: Some(psd),
                        ..Default::default()
                    },
                );
                if let Ok(mut m) = runtime().xiaomi_sessions.lock() {
                    m.remove(st);
                }
                stop_proxy_slot(&runtime().xiaomi_proxy);
                return match res {
                    Ok(conn) => Json(json!({
                        "success": true,
                        "connection": {
                            "id": conn.id,
                            "provider": conn.provider,
                            "email": conn.email,
                            "displayName": display_name,
                        }
                    }))
                    .into_response(),
                    Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                };
            }

            if provider == "trae" || provider == "windsurf" {
                let token = code.map(|s| s.trim()).unwrap_or("");
                if token.is_empty() {
                    return json_error(StatusCode::BAD_REQUEST, "Missing token or callback URL");
                }
                let token_data = match exchange_tokens(
                    state.proxy.client(),
                    &provider,
                    token,
                    None,
                    None,
                    st,
                    &json!({}),
                )
                .await
                {
                    Ok(d) => d,
                    Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                };
                let auth_type = if provider == "windsurf" {
                    "api_key"
                } else {
                    "oauth"
                };
                let input = input_from_mapped_tokens(&provider, auth_type, &token_data);
                return match create_provider_connection(&state.db, input) {
                    Ok(conn) => {
                        let dn = serde_json::from_str::<Value>(&conn.raw_data)
                            .ok()
                            .and_then(|v| {
                                v.get("displayName")
                                    .and_then(|d| d.as_str())
                                    .map(|s| s.to_string())
                            });
                        Json(json!({
                            "success": true,
                            "connection": {
                                "id": conn.id,
                                "provider": conn.provider,
                                "email": conn.email,
                                "displayName": dn,
                            }
                        }))
                        .into_response()
                    }
                    Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                };
            }

            if let Some(raw_code) = code {
                if raw_code.starts_with("eyJ") && raw_code.contains('.') {
                    let info = extract_codex_account_info(raw_code);
                    let payload = decode_jwt_payload(raw_code);
                    let account_id = info.chatgpt_account_id.or_else(|| {
                        payload
                            .as_ref()
                            .and_then(|p| p.get("account_id"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    let plan_type = info.chatgpt_plan_type.or_else(|| {
                        payload
                            .as_ref()
                            .and_then(|p| p.get("plan_type"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    let email = info.email.or_else(|| {
                        payload
                            .as_ref()
                            .and_then(|p| p.get("email"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    let mut psd = Map::new();
                    psd.insert("authMethod".to_string(), json!("access_token"));
                    if let Some(aid) = account_id {
                        psd.insert("chatgptAccountId".to_string(), json!(aid));
                    }
                    if let Some(pt) = plan_type {
                        psd.insert("chatgptPlanType".to_string(), json!(pt));
                    }
                    return match create_provider_connection(
                        &state.db,
                        ConnectionCreateInput {
                            provider: provider.clone(),
                            auth_type: "access_token".to_string(),
                            access_token: Some(raw_code.to_string()),
                            email,
                            test_status: Some("active".to_string()),
                            provider_specific_data: Some(psd),
                            ..Default::default()
                        },
                    ) {
                        Ok(conn) => Json(json!({
                            "success": true,
                            "connection": {
                                "id": conn.id,
                                "provider": conn.provider,
                                "email": conn.email,
                                "displayName": Value::Null,
                            }
                        }))
                        .into_response(),
                        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                    };
                }
            }

            let no_pkce = matches!(provider.as_str(), "cline" | "clinepass" | "kimchi");
            if code.is_none_or(|s| s.is_empty())
                || redirect_uri.is_none_or(|s| s.is_empty())
                || (!no_pkce && code_verifier.is_none_or(|s| s.is_empty()))
            {
                return json_error(StatusCode::BAD_REQUEST, "Missing required fields");
            }

            let mut merged_meta = meta.as_object().cloned().unwrap_or_default();
            if let Some(sid) = system_id {
                if !sid.is_empty() {
                    merged_meta.insert("systemId".to_string(), json!(sid));
                }
            }

            let token_data = match exchange_tokens(
                state.proxy.client(),
                &provider,
                code.unwrap(),
                redirect_uri,
                code_verifier,
                st,
                &Value::Object(merged_meta),
            )
            .await
            {
                Ok(d) => d,
                Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            };
            if let Some(s) = st {
                if let Ok(mut store) = runtime().pkce_store.lock() {
                    store.remove(s);
                }
            }
            let input = input_from_mapped_tokens(&provider, "oauth", &token_data);
            match create_provider_connection(&state.db, input) {
                Ok(conn) => {
                    let dn = serde_json::from_str::<Value>(&conn.raw_data)
                        .ok()
                        .and_then(|v| {
                            v.get("displayName")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string())
                        });
                    Json(json!({
                        "success": true,
                        "connection": {
                            "id": conn.id,
                            "provider": conn.provider,
                            "email": conn.email,
                            "displayName": dn,
                        }
                    }))
                    .into_response()
                }
                Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        "poll" => {
            let Some(device_code) = body
                .get("deviceCode")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                return json_error(StatusCode::BAD_REQUEST, "Missing device code");
            };
            let code_verifier = body.get("codeVerifier").and_then(|v| v.as_str());
            let extra_data = body.get("extraData");

            let no_pkce = matches!(
                provider.as_str(),
                "github"
                    | "kimi"
                    | "kimi-coding"
                    | "kilocode"
                    | "codebuddy-cn"
                    | "codebuddy-intl"
                    | "glm"
                    | "nous"
            );
            if !no_pkce && provider != "kiro" && code_verifier.is_none_or(|s| s.is_empty()) {
                return json_error(StatusCode::BAD_REQUEST, "Missing code verifier");
            }

            let result = match poll_for_token(
                state.proxy.client(),
                &provider,
                device_code,
                code_verifier,
                extra_data,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            };

            if result.get("success").and_then(|v| v.as_bool()) == Some(true) {
                let provider_id = canonical_provider(&provider);
                let tokens = result.get("tokens").cloned().unwrap_or(json!({}));
                let input = input_from_mapped_tokens(provider_id, "oauth", &tokens);
                return match create_provider_connection(&state.db, input) {
                    Ok(conn) => Json(json!({
                        "success": true,
                        "connection": {
                            "id": conn.id,
                            "provider": conn.provider,
                        }
                    }))
                    .into_response(),
                    Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                };
            }

            let err = result.get("error").and_then(|v| v.as_str());
            let is_pending = result
                .get("pending")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || err == Some("authorization_pending")
                || err == Some("slow_down");
            let mut out = json!({
                "success": false,
                "error": result.get("error"),
                "errorDescription": result.get("errorDescription"),
                "pending": is_pending,
            });
            if result.get("fatal").and_then(|v| v.as_bool()) == Some(true) {
                out.as_object_mut()
                    .unwrap()
                    .insert("fatal".to_string(), json!(true));
            }
            Json(out).into_response()
        }
        "manual-code" => {
            if provider != "xai" {
                return json_error(StatusCode::BAD_REQUEST, "Manual code only supported for xai");
            }
            let code = body
                .get("code")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .unwrap_or("");
            let st = body
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .unwrap_or("");
            let sess = if st.is_empty() {
                None
            } else {
                runtime()
                    .xai_sessions
                    .lock()
                    .ok()
                    .and_then(|m| m.get(st).cloned())
            };
            let Some(sess) = sess else {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "xAI OAuth session not found; restart the login flow and paste the code again",
                );
            };
            if code.is_empty() {
                if let Ok(mut m) = runtime().xai_sessions.lock() {
                    m.remove(st);
                }
                stop_proxy_slot(&runtime().xai_proxy);
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Missing xAI authorization code");
            }

            let res = async {
                let token_data = exchange_tokens(
                    state.proxy.client(),
                    "xai",
                    code,
                    Some(&sess.redirect_uri),
                    Some(&sess.code_verifier),
                    Some(st),
                    &json!({}),
                )
                .await?;
                let input = input_from_mapped_tokens("xai", "oauth", &token_data);
                create_provider_connection(&state.db, input)
            }
            .await;

            if let Ok(mut m) = runtime().xai_sessions.lock() {
                m.remove(st);
            }
            stop_proxy_slot(&runtime().xai_proxy);

            match res {
                Ok(conn) => Json(json!({
                    "success": true,
                    "connection": {
                        "id": conn.id,
                        "provider": conn.provider,
                        "email": conn.email,
                        "displayName": Value::Null,
                    }
                }))
                .into_response(),
                Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        _ => json_error(StatusCode::BAD_REQUEST, "Unknown action"),
    }
}

// ============================================================================
// Public Router Composition
// ============================================================================

pub fn oauth_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/oauth/codex/bulk-import",
            post(codex_bulk_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/codex/import-token",
            post(codex_import_token_handler).options(options_handler),
        )
        .route(
            "/api/oauth/commandcode/auto-import",
            get(commandcode_auto_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/cursor/auto-import",
            get(cursor_auto_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/cursor/import",
            get(cursor_import_get_handler)
                .post(cursor_import_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/oauth/gitlab/pat",
            post(gitlab_pat_handler).options(options_handler),
        )
        .route(
            "/api/oauth/grok-cli/bulk-import",
            post(grok_cli_bulk_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/iflow/cookie",
            post(iflow_cookie_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/api-key",
            post(kiro_api_key_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/auto-import",
            get(kiro_auto_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/import-cli-proxy",
            post(kiro_import_cli_proxy_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/import",
            post(kiro_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/social-authorize",
            get(kiro_social_authorize_handler).options(options_handler),
        )
        .route(
            "/api/oauth/kiro/social-exchange",
            post(kiro_social_exchange_handler).options(options_handler),
        )
        .route(
            "/api/oauth/xiaomi-mimo/api-key",
            post(xiaomi_mimo_api_key_handler).options(options_handler),
        )
        .route(
            "/api/oauth/xiaomi-mimo/auto-import",
            get(xiaomi_mimo_auto_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/xiaomi-mimo/login/start",
            post(xiaomi_mimo_login_start_handler).options(options_handler),
        )
        .route(
            "/api/oauth/xiaomi-mimo/login/status",
            get(xiaomi_mimo_login_status_handler).options(options_handler),
        )
        .route(
            "/api/oauth/zed/auto-import",
            get(zed_auto_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/zed/import",
            post(zed_import_handler).options(options_handler),
        )
        .route(
            "/api/oauth/{provider}/{action}",
            get(oauth_dynamic_get_handler)
                .post(oauth_dynamic_post_handler)
                .options(options_handler),
        )
}

// ============================================================================
// Unit & Router Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use axum::body::Body;
    use axum::http::Request;
    use ring::agreement;
    use ring::aead::UnboundKey;
    use tower::ServiceExt;

    fn make_test_state() -> AppState {
        let db = Database::open_in_memory().unwrap();
        let config = Arc::new(crate::config::Config {
            data_dir: PathBuf::from("/tmp"),
            db_path: PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "test-secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let console_buffer = crate::routes::ConsoleLogBuffer::new();
        let rate_limiter = Arc::new(crate::ratelimit::RateLimiter::from_config(&config));
        AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer,
            rate_limiter,
        }
    }

    fn make_fake_jwt(payload: Value) -> String {
        let header = b64url_encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = b64url_encode(payload.to_string().as_bytes());
        format!("{}.{}.sig", header, body)
    }

    #[test]
    fn test_pkce_generation_and_ttl_cleanup() {
        let (v, c, s) = generate_pkce(32);
        assert!(v.len() >= 43);
        assert!(!c.is_empty());
        assert!(!s.is_empty());
        let expected_c = b64url_encode(digest::digest(&digest::SHA256, v.as_bytes()).as_ref());
        assert_eq!(c, expected_c);

        let (v96, _, _) = generate_pkce(XAI_PKCE_VERIFIER_BYTES);
        assert_eq!(v96.len(), 128);

        let mut store = PkceStore::default();
        store.insert(
            PkceEntry {
                provider: "codex".to_string(),
                state: "expired".to_string(),
                code_verifier: "cv1".to_string(),
                code_challenge: "cc1".to_string(),
                redirect_uri: None,
                system_id: None,
                created_at: Instant::now() - Duration::from_secs(100),
            },
            Duration::from_secs(60),
        );
        store.insert(
            PkceEntry {
                provider: "codex".to_string(),
                state: "fresh".to_string(),
                code_verifier: "cv2".to_string(),
                code_challenge: "cc2".to_string(),
                redirect_uri: None,
                system_id: None,
                created_at: Instant::now(),
            },
            Duration::from_secs(60),
        );
        assert!(store.get("expired", Duration::from_secs(60)).is_none());
        assert!(store.get("fresh", Duration::from_secs(60)).is_some());
    }

    #[test]
    fn test_x25519_matches_ring_and_decrypts_xiaomi_callback() {
        let (pub_spki_b64, priv_key) = generate_xiaomi_x25519_keypair();
        let spki = B64_STANDARD.decode(&pub_spki_b64).unwrap();
        assert_eq!(spki.len(), 44);
        let client_pub_raw = &spki[12..44];

        // Use ring's X25519 ephemeral key to encrypt a callback payload to our public key
        let rng = SystemRandom::new();
        let eph_priv = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).unwrap();
        let eph_pub = eph_priv.compute_public_key().unwrap();
        let peer_pub = agreement::UnparsedPublicKey::new(&agreement::X25519, client_pub_raw);

        let shared_from_ring = agreement::agree_ephemeral(eph_priv, &peer_pub, |key_material| {
            key_material.to_vec()
        })
        .unwrap();

        let mut eph_pub_arr = [0u8; 32];
        eph_pub_arr.copy_from_slice(eph_pub.as_ref());
        let shared_from_ours = x25519_scalarmult(&priv_key, &eph_pub_arr);
        assert_eq!(shared_from_ring, shared_from_ours);

        let derived = digest::digest(&digest::SHA256, &shared_from_ring);
        let unbound = UnboundKey::new(&AES_256_GCM, derived.as_ref()).unwrap();
        let aead = LessSafeKey::new(unbound);
        let nonce_bytes = [7u8; 12];
        let mut buf = br#"{"uid":"12345","sk":"sk-mimo-secret","url":"https://api.xiaomimimo.com/v1"}"#.to_vec();
        let tag = aead
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::empty(),
                &mut buf,
            )
            .unwrap();

        let mut wire = Vec::new();
        wire.extend_from_slice(&nonce_bytes);
        wire.extend_from_slice(eph_pub.as_ref());
        wire.extend_from_slice(&buf);
        wire.extend_from_slice(tag.as_ref());
        let u_b64 = B64_STANDARD.encode(&wire);

        let decrypted = decrypt_xiaomi_callback(&priv_key, &u_b64).unwrap();
        assert_eq!(decrypted.uid.as_deref(), Some("12345"));
        assert_eq!(decrypted.sk.as_deref(), Some("sk-mimo-secret"));
        assert_eq!(decrypted.url, "https://api.xiaomimimo.com/v1");
    }

    #[test]
    fn test_zed_callback_parser_and_rsa_roundtrip() {
        let (uid, tok) =
            parse_zed_callback_payload("/callback?user_id=42&access_token=enc123").unwrap();
        assert_eq!(uid, "42");
        assert_eq!(tok, "enc123");

        let (uid2, tok2) =
            parse_zed_callback_payload(r#"{"userId":"99","token":"enc456"}"#).unwrap();
        assert_eq!(uid2, "99");
        assert_eq!(tok2, "enc456");

        let auth = create_zed_native_auth_data(None, Some(58443), Some("sys-1")).unwrap();
        assert!(auth["authUrl"]
            .as_str()
            .unwrap()
            .starts_with("https://zed.dev/native_app_signin"));
        let verifier = auth["privateKeyVerifier"].as_str().unwrap();
        assert!(verifier.starts_with(ZED_PRIVATE_KEY_PREFIX));
    }

    #[test]
    fn test_aws_region_and_external_idp_validation() {
        assert!(is_valid_aws_region("us-east-1"));
        assert!(is_valid_aws_region("eu-west-2"));
        assert!(!is_valid_aws_region("us-east-1.evil.com"));
        assert!(!is_valid_aws_region("http://127.0.0.1"));

        let jwt = make_fake_jwt(json!({ "email": "corp@example.com", "exp": 2000000000 }));
        let normalized = normalize_kiro_external_idp_auth(&json!({
            "auth_method": "external_idp",
            "access_token": jwt,
            "refresh_token": "rt-ms",
            "client_id": "cid",
            "token_endpoint": "https://login.microsoftonline.com/tenant/oauth2/v2.0/token",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123:profile/abc",
            "scopes": ["openid", "offline_access"],
        }))
        .unwrap();
        assert_eq!(normalized["email"], "corp@example.com");
        assert_eq!(
            normalized["providerSpecificData"]["scope"],
            "openid offline_access"
        );
        assert!(normalize_kiro_external_idp_auth(&json!({
            "access_token": "a",
            "refresh_token": "r",
            "client_id": "c",
            "token_endpoint": "https://evil.example.com/token",
            "profile_arn": "arn",
            "scopes": "openid"
        }))
        .is_err());
    }

    #[test]
    fn test_local_auto_imports_in_temp_dirs() {
        let tmp = std::env::temp_dir().join(format!("9r-oauth-test-{}", uuid_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 1. commandcode
        std::fs::create_dir_all(tmp.join(".commandcode")).unwrap();
        std::fs::write(
            tmp.join(".commandcode/auth.json"),
            r#"{"apiKey":"cc-key-123","userId":"u1","userName":"alice"}"#,
        )
        .unwrap();
        let cc = commandcode_auto_import_from_home(&tmp, None);
        assert_eq!(cc["found"], true);
        assert_eq!(cc["apiKey"], "cc-key-123");

        // 2. cursor sqlite
        let cursor_dir = tmp.join(".config/Cursor/User/globalStorage");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let cursor_db = cursor_dir.join("state.vscdb");
        {
            let conn = SqliteConnection::open(&cursor_db).unwrap();
            conn.execute("CREATE TABLE itemTable (key TEXT PRIMARY KEY, value TEXT)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO itemTable VALUES ('cursorAuth/accessToken', '\"tok-cursor-xyz\"')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO itemTable VALUES ('storage.serviceMachineId', '11111111-2222-3333-4444-555555555555')",
                [],
            )
            .unwrap();
        }
        let (st, cur) = cursor_auto_import_from_home(&tmp);
        assert_eq!(st, StatusCode::OK);
        assert_eq!(cur["found"], true);
        assert_eq!(cur["accessToken"], "tok-cursor-xyz");

        // 3. kiro SSO cache + profile.json
        let sso_dir = tmp.join(".aws/sso/cache");
        std::fs::create_dir_all(&sso_dir).unwrap();
        std::fs::write(
            sso_dir.join("kiro-auth-token.json"),
            r#"{"refreshToken":"aorAAAAAG_test_rt","clientIdHash":"hash1","region":"eu-central-1","authMethod":"idc"}"#,
        )
        .unwrap();
        std::fs::write(
            sso_dir.join("hash1.json"),
            r#"{"clientId":"cid-1","clientSecret":"csec-1"}"#,
        )
        .unwrap();
        let kiro_prof_dir = tmp.join(".config/Kiro/User/globalStorage/kiro.kiroagent");
        std::fs::create_dir_all(&kiro_prof_dir).unwrap();
        std::fs::write(
            kiro_prof_dir.join("profile.json"),
            r#"{"arn":"arn:aws:codewhisperer:eu-central-1:999:profile/p1"}"#,
        )
        .unwrap();
        let kiro = kiro_auto_import_from_home(&tmp, None);
        assert_eq!(kiro["found"], true);
        assert_eq!(kiro["clientId"], "cid-1");
        assert_eq!(
            kiro["profileArn"],
            "arn:aws:codewhisperer:us-east-1:999:profile/p1"
        );

        // 4. xiaomi-mimo auth.json + Cookies sqlite
        let mimo_auth_dir = tmp.join(".local/share/mimocode");
        std::fs::create_dir_all(&mimo_auth_dir).unwrap();
        std::fs::write(
            mimo_auth_dir.join("auth.json"),
            r#"{"xiaomi":{"type":"api","key":"sk-mimo-999","metadata":{"uid":"777","base_url":"https://api.xiaomimimo.com/v1"}}}"#,
        )
        .unwrap();
        let mimo_cookie_dir = tmp.join(".config/Xiaomi MiMo/Partitions/xiaomi-account/Network");
        std::fs::create_dir_all(&mimo_cookie_dir).unwrap();
        {
            let conn = SqliteConnection::open(mimo_cookie_dir.join("Cookies")).unwrap();
            conn.execute(
                "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cookies VALUES ('.account.xiaomi.com', 'passToken', 'pt-secret')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cookies VALUES ('.account.xiaomi.com', 'userId', '777')",
                [],
            )
            .unwrap();
        }
        let xm = xiaomi_mimo_auto_import_from_home(&tmp, None);
        assert_eq!(xm["found"], true);
        assert_eq!(xm["apiKey"], "sk-mimo-999");
        assert_eq!(xm["mimoPassToken"], "pt-secret");
        assert_eq!(xm["mimoUserId"], "777");

        // 5. zed system_id kv_store
        let zed_db_dir = tmp.join(".local/share/zed/db/0-global");
        std::fs::create_dir_all(&zed_db_dir).unwrap();
        {
            let conn = SqliteConnection::open(zed_db_dir.join("db.sqlite")).unwrap();
            conn.execute("CREATE TABLE kv_store (key TEXT PRIMARY KEY, value TEXT)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO kv_store VALUES ('system_id', 'zed-sys-123')",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            read_zed_system_id_from_home(&tmp).as_deref(),
            Some("zed-sys-123")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_oauth_router_codex_grok_cursor_kiro_mimo_and_authorize() {
        let state = make_test_state();
        let app = oauth_router().with_state(state.clone());

        // 1. Codex bulk-import (with JWT backfill)
        let jwt = make_fake_jwt(json!({
            "email": "dev@openai.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_123",
                "chatgpt_plan_type": "plus"
            }
        }));
        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/codex/bulk-import")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "accounts": [{ "accessToken": jwt }] }).to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["success"], 1);
        assert_eq!(body["failed"], 0);

        // 2. Codex import-token
        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/codex/import-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "accessToken": jwt }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["connection"]["email"], "dev@openai.com");
        assert_eq!(body["connection"]["workspace"], "acct_123");

        // 3. Grok CLI bulk-import
        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/grok-cli/bulk-import")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!([{ "access_token": "xai-tok", "email": "grok@x.ai" }]).to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["total"], 1);
        assert_eq!(body["success"], 1);

        // 4. Cursor import GET & POST
        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/cursor/import")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let long_tok = "a".repeat(60);
        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/cursor/import")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "accessToken": long_tok,
                    "machineId": "11111111-2222-3333-4444-555555555555"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 5. Kiro social-authorize & import-cli-proxy
        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/kiro/social-authorize?provider=google")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(body["authUrl"].as_str().unwrap().contains("idp=Google"));

        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/kiro/import-cli-proxy")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "access_token": "at",
                    "refresh_token": "rt",
                    "client_id": "cid",
                    "token_endpoint": "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                    "profile_arn": "arn:aws:codewhisperer:us-east-1:1:profile/1",
                    "scopes": ["openid"],
                    "email": "ms@example.com"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 6. Xiaomi MiMo api-key (session-only + dedup update) & login/status
        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/xiaomi-mimo/api-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "mimoPassToken": "pt-1",
                    "mimoUserId": "u-9",
                    "uid": "u-9",
                    "region": "sgp"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut sess = MimoLoginSession {
            state: "st-mimo-1".to_string(),
            region: "cn".to_string(),
            proxy_url: None,
            jar: HashMap::new(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            upstream_base: resolve_mimo_region_base("cn").to_string(),
        };
        sess.jar.insert(
            "passToken|account.xiaomi.com|/".to_string(),
            MimoCookieItem {
                name: "passToken".to_string(),
                value: "pt-xyz".to_string(),
                domain: "account.xiaomi.com".to_string(),
                path: "/".to_string(),
            },
        );
        let cookie_hdr = format!("{}={}", MIMO_SESSION_COOKIE, encode_mimo_session_cookie(&sess));
        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/xiaomi-mimo/login/status?state=st-mimo-1")
            .header(header::COOKIE, cookie_hdr)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["status"], "done");
        assert_eq!(body["passToken"], "pt-xyz");

        // 7. Dynamic authorize for claude / codex / gitlab / zed
        for uri in [
            "/api/oauth/claude/authorize?redirect_uri=http://localhost:8080/callback",
            "/api/oauth/codex/authorize?redirect_uri=http://localhost:1455/auth/callback",
            "/api/oauth/gitlab/authorize?redirect_uri=http://localhost:8080/callback&clientId=gl-cid",
            "/api/oauth/zed/authorize?redirect_uri=http://127.0.0.1:58443/",
        ] {
            let req = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "failed for {}", uri);
        }

        // Verify persisted connections in SQLite DB
        let conns = state.db.load_provider_connections().unwrap();
        assert!(conns.len() >= 5);
    }

    #[tokio::test]
    async fn test_mock_upstream_gitlab_pat_and_oauth_proxy_and_lease() {
        // Mock HTTP server for GitLab /api/v4/user
        let mock_app = Router::new().route(
            "/api/v4/user",
            get(|| async {
                Json(json!({
                    "username": "gluser",
                    "email": "gluser@gitlab.local",
                    "name": "GitLab User"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, mock_app).await;
        });

        let state = make_test_state();
        let app = oauth_router().with_state(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/gitlab/pat")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "token": "glpat-123",
                    "baseUrl": format!("http://{}", addr),
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Test start-proxy + register-session + poll-status + stop-proxy for windsurf
        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/windsurf/start-proxy")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["success"], true);

        let req = Request::builder()
            .method("POST")
            .uri("/api/oauth/windsurf/register-session")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "state": "ws-st-1" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/windsurf/poll-status?state=ws-st-1")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["status"], "pending");

        let req = Request::builder()
            .method("GET")
            .uri("/api/oauth/windsurf/stop-proxy")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Test MiniMax Code lease-status & lease-connect over mock Unix socket
        #[cfg(unix)]
        {
            let tmp_dir = std::env::temp_dir().join(format!("9r-mcode-{}", uuid_v4()));
            let run_dir = tmp_dir.join("run");
            std::fs::create_dir_all(&run_dir).unwrap();
            std::fs::write(
                run_dir.join("mcode-auth-lease-v1.cap"),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .unwrap();
            let sock_path = run_dir.join("mcode-auth-lease-v1.sock");
            let u_listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
            tokio::spawn(async move {
                for _ in 0..2 {
                    if let Ok((mut stream, _)) = u_listener.accept().await {
                        let mut len_buf = [0u8; 4];
                        if stream.read_exact(&mut len_buf).await.is_ok() {
                            let len = u32::from_be_bytes(len_buf) as usize;
                            let mut buf = vec![0u8; len];
                            let _ = stream.read_exact(&mut buf).await;
                            let req_json: Value = serde_json::from_slice(&buf).unwrap();
                            let method = req_json["method"].as_str().unwrap_or("");
                            let resp_json = if method == "status" {
                                json!({
                                    "ok": true,
                                    "result": { "status": "ready", "generation": 1, "expiresAtMs": 9999999999999i64 }
                                })
                            } else {
                                json!({
                                    "ok": true,
                                    "result": { "accessToken": "leased-tok", "generation": 1, "expiresAtMs": 9999999999999i64 }
                                })
                            };
                            let out = serde_json::to_vec(&resp_json).unwrap();
                            let mut frame = Vec::with_capacity(4 + out.len());
                            frame.extend_from_slice(&(out.len() as u32).to_be_bytes());
                            frame.extend_from_slice(&out);
                            let _ = stream.write_all(&frame).await;
                        }
                    }
                }
            });

            let req = Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/oauth/minimax-code/lease-status?data_dir={}",
                    encode_uri_component(&tmp_dir.display().to_string())
                ))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["found"], true);
            assert_eq!(body["status"], "ready");

            let req = Request::builder()
                .method("POST")
                .uri("/api/oauth/minimax-code/lease-connect")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "dataDir": tmp_dir.display().to_string() }).to_string(),
                ))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["success"], true);

            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
    }
}
