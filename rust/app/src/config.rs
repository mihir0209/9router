use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Helpers for Parsing Environment Variables
// ---------------------------------------------------------------------------

fn parse_bool_val(val: &str, default: bool) -> bool {
    match val.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn parse_bool_from<F: Fn(&str) -> Option<String>>(lookup: &F, key: &str, default: bool) -> bool {
    lookup(key)
        .map(|v| parse_bool_val(&v, default))
        .unwrap_or(default)
}

fn parse_bool_aliases_from<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    keys: &[&str],
    default: bool,
) -> bool {
    for &key in keys {
        if let Some(v) = lookup(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return parse_bool_val(trimmed, default);
            }
        }
    }
    default
}

pub fn parse_u16_from<F: Fn(&str) -> Option<String>>(lookup: &F, key: &str, default: u16) -> u16 {
    lookup(key)
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

fn parse_u32_from<F: Fn(&str) -> Option<String>>(lookup: &F, key: &str, default: u32) -> u32 {
    lookup(key)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn parse_u64_from<F: Fn(&str) -> Option<String>>(lookup: &F, key: &str, default: u64) -> u64 {
    lookup(key)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_u64_aliases_from<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    keys: &[&str],
    default: u64,
) -> u64 {
    for &key in keys {
        if let Some(v) = lookup(key) {
            if let Ok(num) = v.trim().parse::<u64>() {
                return num;
            }
        }
    }
    default
}

fn parse_usize_from<F: Fn(&str) -> Option<String>>(lookup: &F, key: &str, default: usize) -> usize {
    lookup(key)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_string_from<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    key: &str,
    default: &str,
) -> String {
    lookup(key)
        .map(|v| v.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn parse_opt_string_from<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    key: &str,
) -> Option<String> {
    lookup(key)
        .map(|v| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_opt_string_aliases_from<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    keys: &[&str],
) -> Option<String> {
    for &key in keys {
        if let Some(v) = lookup(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Constant-time string equality check to prevent timing attacks on tokens.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut diff = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// 1. ServerConfig (Tier 1: Core Server & Runtime Mode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server bind host (default: "0.0.0.0").
    pub host: String,
    /// HTTP server port (default: 20127 or $PORT).
    pub port: u16,
    /// Base URL for internal callbacks / OAuth (default: "http://127.0.0.1:{port}").
    pub base_url: String,
    /// Base path for reverse proxy subpath routing (default: "").
    pub base_path: String,
    /// Decouple from Node.js runtime if true (STANDALONE=1 or NO_NODE=1).
    pub standalone: bool,
    /// Alias for standalone.
    pub no_node: bool,
    /// Serve embedded Next.js static UI assets if true.
    pub embed_ui: bool,
    /// Environment mode ("production", "development").
    pub node_env: String,
    /// Instance identifier name for cluster/dashboard (default: "9router").
    pub instance_name: String,
    /// Maximum body limit in megabytes (default: 20MB).
    pub max_body_mb: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 20127,
            base_url: "http://127.0.0.1:20127".to_string(),
            base_path: String::new(),
            standalone: false,
            no_node: false,
            embed_ui: false,
            node_env: "production".to_string(),
            instance_name: "9router".to_string(),
            max_body_mb: 20,
        }
    }
}

impl ServerConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let port = resolve_port_from(&lookup);
        let host = parse_string_from(&lookup, "HOST", "0.0.0.0");
        let default_base_url = format!("http://127.0.0.1:{}", port);
        let base_url = parse_opt_string_aliases_from(&lookup, &["BASE_URL", "NEXT_PUBLIC_BASE_URL"])
            .unwrap_or(default_base_url);
        let base_path = parse_string_from(&lookup, "NEXT_PUBLIC_BASE_PATH", "");
        let standalone = parse_bool_aliases_from(&lookup, &["STANDALONE", "NO_NODE"], false);
        let no_node = standalone;
        let embed_ui = parse_bool_from(&lookup, "EMBED_UI", false);
        let node_env = parse_string_from(&lookup, "NODE_ENV", "production");
        let instance_name = parse_string_from(&lookup, "INSTANCE_NAME", "9router");
        let max_body_mb = parse_usize_from(&lookup, "NINEROUTER_MAX_BODY_MB", 20);

        Self {
            host,
            port,
            base_url,
            base_path,
            standalone,
            no_node,
            embed_ui,
            node_env,
            instance_name,
            max_body_mb,
        }
    }
}

// ---------------------------------------------------------------------------
// 2. StorageConfig (Tier 1 & 6: Data Directory, Paths & Database Persistence)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Directory for application data storage.
    pub data_dir: PathBuf,
    /// Path to SQLite database file.
    pub db_path: PathBuf,
    /// Optional external PostgreSQL or SQLite connection URL.
    pub database_url: Option<String>,
    /// Connection pool size for external database (default: 5).
    pub db_pool_max: u32,
    /// Whether TLS/SSL is required for external database connection.
    pub db_ssl: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        let data_dir = PathBuf::from(".9router");
        let db_path = data_dir.join("db").join("data.sqlite");
        Self {
            data_dir,
            db_path,
            database_url: None,
            db_pool_max: 5,
            db_ssl: false,
        }
    }
}

impl StorageConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let data_dir = resolve_data_dir_from(&lookup);
        let db_path = data_dir.join("db").join("data.sqlite");
        let database_url = parse_opt_string_aliases_from(&lookup, &["DATABASE_URL", "POSTGRES_URL"]);
        let db_pool_max = parse_u32_from(&lookup, "DB_POOL_MAX", 5);
        let db_ssl = parse_bool_from(&lookup, "DB_SSL", false);

        Self {
            data_dir,
            db_path,
            database_url,
            db_pool_max,
            db_ssl,
        }
    }
}

// ---------------------------------------------------------------------------
// 3. AuthConfig (Tier 2: Security, Authentication & Cryptography)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// High-entropy JWT secret for signing dashboard session tokens.
    pub jwt_secret: String,
    /// Initial onboarding admin password (default: "change-me").
    pub initial_password: Option<String>,
    /// Dedicated admin bearer API token.
    pub admin_api_token: Option<String>,
    /// Inter-service router authentication token.
    pub internal_api_token: Option<String>,
    /// Local MITM proxy and CLI administrative API key.
    pub router_api_key: Option<String>,
    /// Alternative CLI admin key alias (NINEROUTER_API_KEY / NINE_ROUTER_API_KEY).
    pub ninerouter_api_key: Option<String>,
    /// Whether session cookies must have Secure flag (default: false for localhost).
    pub auth_cookie_secure: bool,
    /// Trust X-Forwarded-For client IP headers when behind reverse proxy.
    pub trust_proxy: bool,
    /// Environment override forcing API key requirement on /v1/* endpoints.
    pub require_api_key: bool,
    /// Legacy HMAC signature secret for API keys.
    pub api_key_secret: String,
    /// Salt for stable client machine fingerprinting.
    pub machine_id_salt: String,
    /// Master encryption key for BYOK credential storage.
    pub byok_enc_key: Option<String>,
    /// Trusted peer token for clustering.
    pub peer_token: Option<String>,
    /// Secret required for /api/cron/* endpoints if set.
    pub cron_secret: Option<String>,
    /// Secret required for /api/shutdown endpoint if set.
    pub shutdown_secret: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: "default-secret-key-change-in-production-00000000".to_string(),
            initial_password: Some("change-me".to_string()),
            admin_api_token: None,
            internal_api_token: None,
            router_api_key: None,
            ninerouter_api_key: None,
            auth_cookie_secure: false,
            trust_proxy: false,
            require_api_key: false,
            api_key_secret: "endpoint-proxy-api-key-secret".to_string(),
            machine_id_salt: "endpoint-proxy-salt".to_string(),
            byok_enc_key: None,
            peer_token: None,
            cron_secret: None,
            shutdown_secret: None,
        }
    }
}

impl AuthConfig {
    pub fn load(data_dir: &Path) -> Self {
        Self::load_from(data_dir, |k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(data_dir: &Path, lookup: F) -> Self {
        let jwt_secret = resolve_jwt_secret_from(data_dir, &lookup);
        let initial_password = parse_opt_string_from(&lookup, "INITIAL_PASSWORD")
            .or_else(|| Some("change-me".to_string()));
        let admin_api_token = parse_opt_string_aliases_from(&lookup, &["ADMIN_API_TOKEN", "ADMIN_KEY"]);
        let internal_api_token = parse_opt_string_from(&lookup, "INTERNAL_API_TOKEN");
        let router_api_key = parse_opt_string_from(&lookup, "ROUTER_API_KEY");
        let ninerouter_api_key = parse_opt_string_aliases_from(&lookup, &["NINEROUTER_API_KEY", "NINE_ROUTER_API_KEY"]);
        let auth_cookie_secure = parse_bool_from(&lookup, "AUTH_COOKIE_SECURE", false);
        let trust_proxy = parse_bool_from(&lookup, "TRUST_PROXY", false);
        let require_api_key = parse_bool_from(&lookup, "REQUIRE_API_KEY", false);
        let api_key_secret = parse_string_from(&lookup, "API_KEY_SECRET", "endpoint-proxy-api-key-secret");
        let machine_id_salt = parse_string_from(&lookup, "MACHINE_ID_SALT", "endpoint-proxy-salt");
        let byok_enc_key = parse_opt_string_from(&lookup, "BYOK_ENC_KEY");
        let peer_token = parse_opt_string_from(&lookup, "NINEROUTER_PEER_TOKEN");
        let cron_secret = parse_opt_string_from(&lookup, "CRON_SECRET");
        let shutdown_secret = parse_opt_string_from(&lookup, "SHUTDOWN_SECRET");

        Self {
            jwt_secret,
            initial_password,
            admin_api_token,
            internal_api_token,
            router_api_key,
            ninerouter_api_key,
            auth_cookie_secure,
            trust_proxy,
            require_api_key,
            api_key_secret,
            machine_id_salt,
            byok_enc_key,
            peer_token,
            cron_secret,
            shutdown_secret,
        }
    }

    /// Verifies if a given token matches any configured administrative token.
    pub fn is_valid_admin_token(&self, candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let candidates = [
            self.admin_api_token.as_deref(),
            self.ninerouter_api_key.as_deref(),
            self.router_api_key.as_deref(),
            self.internal_api_token.as_deref(),
        ];
        for opt in candidates {
            if let Some(token) = opt {
                if constant_time_eq(token, candidate) {
                    return true;
                }
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// 4. NetworkConfig (Tier 3: Outbound Egress & Enterprise Proxies)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Outbound HTTP proxy URL (HTTP_PROXY / http_proxy).
    pub http_proxy: Option<String>,
    /// Outbound HTTPS proxy URL (HTTPS_PROXY / https_proxy).
    pub https_proxy: Option<String>,
    /// Outbound general SOCKS5/HTTP proxy URL (ALL_PROXY / all_proxy).
    pub all_proxy: Option<String>,
    /// Proxy exclusion list (NO_PROXY / no_proxy, default: "localhost,127.0.0.1").
    pub no_proxy: Option<String>,
    /// Dedicated high-priority proxy URL overriding system proxy settings.
    pub nine_router_proxy_url: Option<String>,
    /// Dedicated exclusion list for nine_router_proxy_url.
    pub nine_router_no_proxy: Option<String>,
    /// Feature flag for managed outbound proxy agent.
    pub nine_router_proxy_managed: bool,
    /// Whether TLS verification is strictly enforced (default: true).
    pub strict_ssl: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            http_proxy: None,
            https_proxy: None,
            all_proxy: None,
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            nine_router_proxy_url: None,
            nine_router_no_proxy: None,
            nine_router_proxy_managed: false,
            strict_ssl: true,
        }
    }
}

impl NetworkConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let http_proxy = parse_opt_string_aliases_from(&lookup, &["HTTP_PROXY", "http_proxy"]);
        let https_proxy = parse_opt_string_aliases_from(&lookup, &["HTTPS_PROXY", "https_proxy"]);
        let all_proxy = parse_opt_string_aliases_from(&lookup, &["ALL_PROXY", "all_proxy"]);
        let no_proxy = parse_opt_string_aliases_from(&lookup, &["NO_PROXY", "no_proxy"])
            .or_else(|| Some("localhost,127.0.0.1".to_string()));
        let nine_router_proxy_url = parse_opt_string_from(&lookup, "NINE_ROUTER_PROXY_URL");
        let nine_router_no_proxy = parse_opt_string_from(&lookup, "NINE_ROUTER_NO_PROXY");
        let nine_router_proxy_managed = parse_bool_from(&lookup, "NINE_ROUTER_PROXY_MANAGED", false);
        let strict_ssl = parse_bool_from(&lookup, "STRICT_SSL", true);

        Self {
            http_proxy,
            https_proxy,
            all_proxy,
            no_proxy,
            nine_router_proxy_url,
            nine_router_no_proxy,
            nine_router_proxy_managed,
            strict_ssl,
        }
    }

    /// Resolves the effective HTTP proxy URL, prioritizing NINE_ROUTER_PROXY_URL.
    pub fn effective_http_proxy(&self) -> Option<&str> {
        self.nine_router_proxy_url
            .as_deref()
            .or(self.http_proxy.as_deref())
            .or(self.all_proxy.as_deref())
    }

    /// Resolves the effective HTTPS proxy URL, prioritizing NINE_ROUTER_PROXY_URL.
    pub fn effective_https_proxy(&self) -> Option<&str> {
        self.nine_router_proxy_url
            .as_deref()
            .or(self.https_proxy.as_deref())
            .or(self.all_proxy.as_deref())
    }

    /// Resolves the effective proxy exclusion list.
    pub fn effective_no_proxy(&self) -> Option<&str> {
        self.nine_router_no_proxy
            .as_deref()
            .or(self.no_proxy.as_deref())
    }
}

// ---------------------------------------------------------------------------
// 5. RoutingConfig (Tier 4: Routing Engine, Resilience, Circuit Breaker & Hedging)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Consecutive failures to trip circuit breaker (default: 5).
    pub circuit_breaker_failure_threshold: u32,
    /// Cooldown seconds before probing tripped circuit breaker (default: 30s).
    pub circuit_breaker_cooldown_secs: u64,
    /// Rolling window seconds for circuit breaker failure rate (default: 60s).
    pub circuit_breaker_rolling_window_secs: u64,
    /// Milliseconds before dispatching speculative hedged secondary request (default: 0 = disabled).
    pub hedged_delay_ms: u64,
    /// Whether bidirectional protocol translation is enabled (default: true).
    pub enable_translator: bool,
    /// Model catalog source: "live" or "cached" (default: "live").
    pub models_list_source: String,
    /// Structured output transformation mode (default: "auto").
    pub structured_output_mode: String,
    /// Buffer token count added to completion quotas (default: 2000).
    pub usage_buffer_tokens: u64,
    /// Single-flight request coalescing window in milliseconds (default: 0).
    pub flight_wait_ms: u64,
    /// Maximum outbound concurrent requests per provider (default: 0 = unlimited).
    pub outbound_concurrency: usize,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            circuit_breaker_failure_threshold: 5,
            circuit_breaker_cooldown_secs: 30,
            circuit_breaker_rolling_window_secs: 60,
            hedged_delay_ms: 0,
            enable_translator: true,
            models_list_source: "live".to_string(),
            structured_output_mode: "auto".to_string(),
            usage_buffer_tokens: 2000,
            flight_wait_ms: 0,
            outbound_concurrency: 0,
        }
    }
}

impl RoutingConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let circuit_breaker_failure_threshold = parse_u32_from(&lookup, "CIRCUIT_BREAKER_FAILURE_THRESHOLD", 5);
        let circuit_breaker_cooldown_secs = parse_u64_from(&lookup, "CIRCUIT_BREAKER_COOLDOWN_SECS", 30);
        let circuit_breaker_rolling_window_secs = parse_u64_from(&lookup, "CIRCUIT_BREAKER_ROLLING_WINDOW_SECS", 60);
        let hedged_delay_ms = parse_u64_aliases_from(&lookup, &["HEDGED_DELAY_MS", "HEDGED_DELAY"], 0);
        let enable_translator = parse_bool_from(&lookup, "ENABLE_TRANSLATOR", true);
        let models_list_source = parse_string_from(&lookup, "MODELS_LIST_SOURCE", "live");
        let structured_output_mode = parse_string_from(&lookup, "STRUCTURED_OUTPUT_MODE", "auto");
        let usage_buffer_tokens = parse_u64_from(&lookup, "USAGE_BUFFER_TOKENS", 2000);
        let flight_wait_ms = parse_u64_from(&lookup, "NINEROUTER_FLIGHT_WAIT_MS", 0);
        let outbound_concurrency = parse_usize_from(&lookup, "NINEROUTER_OUTBOUND_CONCURRENCY", 0);

        Self {
            circuit_breaker_failure_threshold,
            circuit_breaker_cooldown_secs,
            circuit_breaker_rolling_window_secs,
            hedged_delay_ms,
            enable_translator,
            models_list_source,
            structured_output_mode,
            usage_buffer_tokens,
            flight_wait_ms,
            outbound_concurrency,
        }
    }
}

// ---------------------------------------------------------------------------
// 6. StreamingConfig (Tier 5: Streaming, Buffers, Timeouts & Protocol Limits)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingConfig {
    /// SSE stream chunk stall timeout in milliseconds (default: 360000 = 360s).
    pub stream_stall_timeout_ms: u64,
    /// Time to first token (TTFT) timeout in milliseconds (default: 200000 = 200s).
    pub stream_first_chunk_timeout_ms: u64,
    /// Deadline for peek evaluation in combo racing (default: 45000 = 45s).
    pub empty_peek_deadline_ms: u64,
    /// HTTP client connect timeout in milliseconds (default: 60000 = 60s).
    pub fetch_connect_timeout_ms: u64,
    /// TCP keepalive probe timeout in milliseconds (default: 4000 = 4s).
    pub fetch_keepalive_timeout_ms: u64,
    /// Retry immediately with fresh socket on broken pipe / reset (default: true).
    pub fetch_fresh_retry: bool,
    /// Maximum standard request body in megabytes (default: 20MB).
    pub max_body_mb: usize,
    /// Maximum multipart audio/video upload in megabytes (default: 32MB).
    pub max_upload_mb: usize,
    /// Maximum buffered response size in megabytes (default: 32MB).
    pub max_response_mb: usize,
    /// Maximum byte slice captured for request detail logging (default: 65536 = 64KB).
    pub stream_capture_max_bytes: usize,
    /// Maximum buffer size for combo empty peek detection (default: 262144 = 256KB).
    pub empty_peek_max_bytes: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            stream_stall_timeout_ms: 360_000,
            stream_first_chunk_timeout_ms: 200_000,
            empty_peek_deadline_ms: 45_000,
            fetch_connect_timeout_ms: 60_000,
            fetch_keepalive_timeout_ms: 4_000,
            fetch_fresh_retry: true,
            max_body_mb: 20,
            max_upload_mb: 32,
            max_response_mb: 32,
            stream_capture_max_bytes: 65_536,
            empty_peek_max_bytes: 262_144,
        }
    }
}

impl StreamingConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let stream_stall_timeout_ms = parse_u64_from(&lookup, "STREAM_STALL_TIMEOUT_MS", 360_000);
        let stream_first_chunk_timeout_ms = parse_u64_from(&lookup, "STREAM_FIRST_CHUNK_TIMEOUT_MS", 200_000);
        let empty_peek_deadline_ms = parse_u64_from(&lookup, "EMPTY_PEEK_DEADLINE_MS", 45_000);
        let fetch_connect_timeout_ms = parse_u64_from(&lookup, "FETCH_CONNECT_TIMEOUT_MS", 60_000);
        let fetch_keepalive_timeout_ms = parse_u64_from(&lookup, "FETCH_KEEPALIVE_TIMEOUT_MS", 4_000);
        let fetch_fresh_retry = parse_bool_from(&lookup, "FETCH_FRESH_RETRY", true);
        let max_body_mb = parse_usize_from(&lookup, "NINEROUTER_MAX_BODY_MB", 20);
        let max_upload_mb = parse_usize_from(&lookup, "NINEROUTER_MAX_UPLOAD_MB", 32);
        let max_response_mb = parse_usize_from(&lookup, "NINEROUTER_MAX_RESPONSE_MB", 32);
        let stream_capture_max_bytes = parse_usize_from(&lookup, "NINEROUTER_STREAM_CAPTURE_MAX_BYTES", 65_536);
        let empty_peek_max_bytes = parse_usize_from(&lookup, "NINEROUTER_EMPTY_PEEK_MAX_BYTES", 262_144);

        Self {
            stream_stall_timeout_ms,
            stream_first_chunk_timeout_ms,
            empty_peek_deadline_ms,
            fetch_connect_timeout_ms,
            fetch_keepalive_timeout_ms,
            fetch_fresh_retry,
            max_body_mb,
            max_upload_mb,
            max_response_mb,
            stream_capture_max_bytes,
            empty_peek_max_bytes,
        }
    }

    pub fn max_body_bytes(&self) -> usize {
        self.max_body_mb.saturating_mul(1024 * 1024)
    }

    pub fn max_upload_bytes(&self) -> usize {
        self.max_upload_mb.saturating_mul(1024 * 1024)
    }

    pub fn max_response_bytes(&self) -> usize {
        self.max_response_mb.saturating_mul(1024 * 1024)
    }
}

// ---------------------------------------------------------------------------
// 7. ObservabilityConfig (Tier 7: Metrics, Logging & Telemetry)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Enable async request logging into SQLite request_details table (default: true).
    pub enable_request_logs: bool,
    /// System log level ("debug", "info", "warn", "error", default: "info").
    pub log_level: String,
    /// Master toggle for internal telemetry and performance tracking (default: true).
    pub observability_enabled: bool,
    /// Flush batch size for database logging worker (default: 50).
    pub batch_size: usize,
    /// Flush interval timer in milliseconds for metrics writer (default: 5000 = 5s).
    pub flush_interval_ms: u64,
    /// Retention pruning threshold for request_details rows (default: 10000).
    pub max_records: usize,
    /// Maximum JSON payload captured per logged request (default: 1048576 = 1MB).
    pub max_json_size: usize,
    /// Emits detailed per-token usage logs in translator (default: false).
    pub log_usage_verbose: bool,
    /// RPC client timeout for cluster coordinator requests (default: 10000 = 10s).
    pub coordinator_timeout_ms: u64,
    /// Direct console log output capture into ConsoleLogBuffer (default: true).
    pub console_capture: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enable_request_logs: true,
            log_level: "info".to_string(),
            observability_enabled: true,
            batch_size: 50,
            flush_interval_ms: 5000,
            max_records: 10000,
            max_json_size: 1048576,
            log_usage_verbose: false,
            coordinator_timeout_ms: 10000,
            console_capture: true,
        }
    }
}

impl ObservabilityConfig {
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let enable_request_logs = parse_bool_from(&lookup, "ENABLE_REQUEST_LOGS", true);
        let log_level = parse_string_from(&lookup, "LOG_LEVEL", "info");
        let observability_enabled = parse_bool_from(&lookup, "OBSERVABILITY_ENABLED", true);
        let batch_size = parse_usize_from(&lookup, "OBSERVABILITY_BATCH_SIZE", 50);
        let flush_interval_ms = parse_u64_from(&lookup, "OBSERVABILITY_FLUSH_INTERVAL_MS", 5000);
        let max_records = parse_usize_from(&lookup, "OBSERVABILITY_MAX_RECORDS", 10000);
        let max_json_size = parse_usize_from(&lookup, "OBSERVABILITY_MAX_JSON_SIZE", 1048576);
        let log_usage_verbose = parse_bool_from(&lookup, "LOG_USAGE_VERBOSE", false);
        let coordinator_timeout_ms = parse_u64_from(&lookup, "COORDINATOR_TIMEOUT_MS", 10000);
        let console_capture = parse_bool_from(&lookup, "NINEROUTER_CONSOLE_CAPTURE", true);

        Self {
            enable_request_logs,
            log_level,
            observability_enabled,
            batch_size,
            flush_interval_ms,
            max_records,
            max_json_size,
            log_usage_verbose,
            coordinator_timeout_ms,
            console_capture,
        }
    }
}

// ---------------------------------------------------------------------------
// Root Config Struct
// ---------------------------------------------------------------------------

/// Unified Application Configuration for 9Router.
///
/// Combines modular sub-configs with backward-compatible direct field access
/// for seamless integration across all server subsystems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // Backward-compatible direct fields
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub port: u16,
    pub jwt_secret: String,

    // Modular sub-configs
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub auth: AuthConfig,
    pub network: NetworkConfig,
    pub routing: RoutingConfig,
    pub streaming: StreamingConfig,
    pub observability: ObservabilityConfig,
}

impl Default for Config {
    fn default() -> Self {
        let server = ServerConfig::default();
        let storage = StorageConfig::default();
        let auth = AuthConfig::default();
        let network = NetworkConfig::default();
        let routing = RoutingConfig::default();
        let streaming = StreamingConfig::default();
        let observability = ObservabilityConfig::default();

        Self {
            data_dir: storage.data_dir.clone(),
            db_path: storage.db_path.clone(),
            port: server.port,
            jwt_secret: auth.jwt_secret.clone(),
            server,
            storage,
            auth,
            network,
            routing,
            streaming,
            observability,
        }
    }
}

impl Config {
    /// Load configuration by resolving environment variables and local files.
    pub fn load() -> Self {
        Self::load_from(|k| env::var(k).ok())
    }

    /// Load configuration using a custom environment lookup closure.
    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let storage = StorageConfig::load_from(&lookup);
        let server = ServerConfig::load_from(&lookup);
        let auth = AuthConfig::load_from(&storage.data_dir, &lookup);
        let network = NetworkConfig::load_from(&lookup);
        let routing = RoutingConfig::load_from(&lookup);
        let streaming = StreamingConfig::load_from(&lookup);
        let observability = ObservabilityConfig::load_from(&lookup);

        Self {
            data_dir: storage.data_dir.clone(),
            db_path: storage.db_path.clone(),
            port: server.port,
            jwt_secret: auth.jwt_secret.clone(),
            server,
            storage,
            auth,
            network,
            routing,
            streaming,
            observability,
        }
    }

    /// Ensure that data directory and database directory exist on disk.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(self.data_dir.join("db"))?;
        Ok(())
    }

    /// Check if running in standalone mode (no Node.js dependency).
    pub fn is_standalone(&self) -> bool {
        self.server.standalone
    }

    /// Check if embedded static UI serving is activated.
    pub fn embed_ui(&self) -> bool {
        self.server.embed_ui
    }

    /// Validate candidate admin token against all configured keys.
    pub fn is_valid_admin_token(&self, candidate: &str) -> bool {
        self.auth.is_valid_admin_token(candidate)
    }

    /// Resolve effective outbound HTTP proxy URL.
    pub fn effective_http_proxy(&self) -> Option<&str> {
        self.network.effective_http_proxy()
    }

    /// Resolve effective outbound HTTPS proxy URL.
    pub fn effective_https_proxy(&self) -> Option<&str> {
        self.network.effective_https_proxy()
    }

    /// Resolve effective proxy exclusion list.
    pub fn effective_no_proxy(&self) -> Option<&str> {
        self.network.effective_no_proxy()
    }
}

// ---------------------------------------------------------------------------
// Standalone Public Resolver Functions (Preserved for Backward Compatibility)
// ---------------------------------------------------------------------------

/// Resolves the base data directory:
/// 1. $DATA_DIR env variable if present.
/// 2. ~/.9router (or %APPDATA%/9router on Windows).
pub fn resolve_data_dir() -> PathBuf {
    resolve_data_dir_from(|k| env::var(k).ok())
}

pub fn resolve_data_dir_from<F: Fn(&str) -> Option<String>>(lookup: F) -> PathBuf {
    if let Some(dir) = lookup("DATA_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return expand_tilde_from(trimmed, &lookup);
        }
    }

    if cfg!(windows) {
        if let Some(app_data) = lookup("APPDATA") {
            let trimmed = app_data.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("9router");
            }
        }
    }

    if let Some(home) = lookup("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".9router");
        }
    }

    if let Some(user_profile) = lookup("USERPROFILE") {
        let trimmed = user_profile.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".9router");
        }
    }

    PathBuf::from(".9router")
}

/// Expands a leading tilde (~/) to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_from(path, &|k| env::var(k).ok())
}

fn expand_tilde_from<F: Fn(&str) -> Option<String>>(path: &str, lookup: &F) -> PathBuf {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        let home = lookup("HOME")
            .or_else(|| lookup("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            if path == "~" {
                return PathBuf::from(home);
            }
            return PathBuf::from(home).join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

/// Resolves the HTTP server port:
/// Reads $PORT (e.g. 20127), defaults to 20127.
pub fn resolve_port() -> u16 {
    resolve_port_from(|k| env::var(k).ok())
}

pub fn resolve_port_from<F: Fn(&str) -> Option<String>>(lookup: F) -> u16 {
    if let Some(port_str) = lookup("PORT") {
        if let Ok(port) = port_str.trim().parse::<u16>() {
            if port > 0 {
                return port;
            }
        }
    }
    20127
}

/// Resolves the JWT secret:
/// 1. $JWT_SECRET env variable if set and non-empty.
/// 2. File `<data_dir>/jwt-secret` if present and non-empty.
/// 3. Generates 32 random bytes (64 hex characters), writes to `<data_dir>/jwt-secret`, and returns it.
pub fn resolve_jwt_secret(data_dir: &Path) -> String {
    resolve_jwt_secret_from(data_dir, |k| env::var(k).ok())
}

pub fn resolve_jwt_secret_from<F: Fn(&str) -> Option<String>>(data_dir: &Path, lookup: F) -> String {
    if let Some(secret) = lookup("JWT_SECRET") {
        let trimmed = secret.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let secret_file = data_dir.join("jwt-secret");
    if let Ok(content) = fs::read_to_string(&secret_file) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Generate fresh secret
    let generated = generate_random_hex(32);
    let _ = fs::create_dir_all(data_dir);
    let _ = fs::write(&secret_file, &generated);
    generated
}

/// Generates random hex string of given byte length.
fn generate_random_hex(bytes_count: usize) -> String {
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = vec![0u8; bytes_count];
        if file.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{:02x}", b)).collect();
        }
    }

    // High entropy fallback using system time, pid, and PCG-style bit mixing
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut state = seed ^ (std::process::id() as u128) << 32;
    let mut out = String::with_capacity(bytes_count * 2);
    for _ in 0..bytes_count {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let byte = (state >> 56) as u8;
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::env;

    // Helper mock lookup closure builder
    fn mock_env(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |k| map.get(k).map(|s| s.to_string())
    }

    // --- Legacy / Core Resolver Tests ---

    #[test]
    fn test_resolve_port_default() {
        let mock = mock_env(HashMap::new());
        assert_eq!(resolve_port_from(mock), 20127);
    }

    #[test]
    fn test_resolve_port_custom() {
        let mut map = HashMap::new();
        map.insert("PORT", "3000");
        assert_eq!(resolve_port_from(mock_env(map)), 3000);
    }

    #[test]
    fn test_resolve_port_invalid_fallback() {
        let mut map = HashMap::new();
        map.insert("PORT", "invalid-port");
        assert_eq!(resolve_port_from(mock_env(map)), 20127);
    }

    #[test]
    fn test_resolve_port_zero_fallback() {
        let mut map = HashMap::new();
        map.insert("PORT", "0");
        assert_eq!(resolve_port_from(mock_env(map)), 20127);
    }

    #[test]
    fn test_resolve_data_dir_env() {
        let mut map = HashMap::new();
        map.insert("DATA_DIR", "/custom/data/dir");
        assert_eq!(resolve_data_dir_from(mock_env(map)), PathBuf::from("/custom/data/dir"));
    }

    #[test]
    fn test_resolve_data_dir_tilde() {
        let mut map = HashMap::new();
        map.insert("HOME", "/home/developer");
        map.insert("DATA_DIR", "~/custom-9r");
        assert_eq!(
            resolve_data_dir_from(mock_env(map)),
            PathBuf::from("/home/developer/custom-9r")
        );
    }

    #[test]
    fn test_jwt_secret_generation_and_persistence() {
        let temp_dir = env::temp_dir().join(format!("9r-test-jwt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        let secret1 = resolve_jwt_secret_from(&temp_dir, |_| None);
        assert_eq!(secret1.len(), 64);

        // Read back from persisted file
        let secret2 = resolve_jwt_secret_from(&temp_dir, |_| None);
        assert_eq!(secret1, secret2);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_jwt_secret_env_override() {
        let temp_dir = env::temp_dir().join(format!("9r-test-jwt-env-{}", std::process::id()));
        let mut map = HashMap::new();
        map.insert("JWT_SECRET", "super-secret-jwt-token-from-env");

        let secret = resolve_jwt_secret_from(&temp_dir, mock_env(map));
        assert_eq!(secret, "super-secret-jwt-token-from-env");
        let _ = fs::remove_dir_all(&temp_dir);
    }

    // --- Tier 1: ServerConfig Tests ---

    #[test]
    fn test_server_config_defaults() {
        let cfg = ServerConfig::load_from(mock_env(HashMap::new()));
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 20127);
        assert_eq!(cfg.base_url, "http://127.0.0.1:20127");
        assert_eq!(cfg.base_path, "");
        assert!(!cfg.standalone);
        assert!(!cfg.no_node);
        assert!(!cfg.embed_ui);
        assert_eq!(cfg.node_env, "production");
        assert_eq!(cfg.instance_name, "9router");
        assert_eq!(cfg.max_body_mb, 20);
    }

    #[test]
    fn test_server_config_custom_and_aliases() {
        let mut map = HashMap::new();
        map.insert("HOST", "127.0.0.1");
        map.insert("PORT", "8080");
        map.insert("BASE_URL", "https://gateway.example.com");
        map.insert("NEXT_PUBLIC_BASE_PATH", "/subpath");
        map.insert("STANDALONE", "1");
        map.insert("EMBED_UI", "true");
        map.insert("NODE_ENV", "development");
        map.insert("INSTANCE_NAME", "cluster-node-01");
        map.insert("NINEROUTER_MAX_BODY_MB", "50");

        let cfg = ServerConfig::load_from(mock_env(map));
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.base_url, "https://gateway.example.com");
        assert_eq!(cfg.base_path, "/subpath");
        assert!(cfg.standalone);
        assert!(cfg.no_node);
        assert!(cfg.embed_ui);
        assert_eq!(cfg.node_env, "development");
        assert_eq!(cfg.instance_name, "cluster-node-01");
        assert_eq!(cfg.max_body_mb, 50);
    }

    #[test]
    fn test_server_config_no_node_alias() {
        let mut map = HashMap::new();
        map.insert("NO_NODE", "true");
        let cfg = ServerConfig::load_from(mock_env(map));
        assert!(cfg.standalone);
        assert!(cfg.no_node);
    }

    #[test]
    fn test_server_config_next_public_base_url_fallback() {
        let mut map = HashMap::new();
        map.insert("NEXT_PUBLIC_BASE_URL", "https://public.example.com");
        let cfg = ServerConfig::load_from(mock_env(map));
        assert_eq!(cfg.base_url, "https://public.example.com");
    }

    // --- Tier 1 & 6: StorageConfig Tests ---

    #[test]
    fn test_storage_config_defaults() {
        let mut map = HashMap::new();
        map.insert("HOME", "/home/testuser");
        let cfg = StorageConfig::load_from(mock_env(map));
        assert_eq!(cfg.data_dir, PathBuf::from("/home/testuser/.9router"));
        assert_eq!(cfg.db_path, PathBuf::from("/home/testuser/.9router/db/data.sqlite"));
        assert_eq!(cfg.database_url, None);
        assert_eq!(cfg.db_pool_max, 5);
        assert!(!cfg.db_ssl);
    }

    #[test]
    fn test_storage_config_postgres_fallback() {
        let mut map = HashMap::new();
        map.insert("POSTGRES_URL", "postgres://user:pass@localhost:5432/9router");
        map.insert("DB_POOL_MAX", "10");
        map.insert("DB_SSL", "true");

        let cfg = StorageConfig::load_from(mock_env(map));
        assert_eq!(
            cfg.database_url,
            Some("postgres://user:pass@localhost:5432/9router".to_string())
        );
        assert_eq!(cfg.db_pool_max, 10);
        assert!(cfg.db_ssl);
    }

    // --- Tier 2: AuthConfig Tests ---

    #[test]
    fn test_auth_config_defaults() {
        let temp_dir = env::temp_dir().join(format!("9r-test-auth-def-{}", std::process::id()));
        let cfg = AuthConfig::load_from(&temp_dir, mock_env(HashMap::new()));

        assert_eq!(cfg.initial_password, Some("change-me".to_string()));
        assert_eq!(cfg.admin_api_token, None);
        assert_eq!(cfg.internal_api_token, None);
        assert_eq!(cfg.router_api_key, None);
        assert_eq!(cfg.ninerouter_api_key, None);
        assert!(!cfg.auth_cookie_secure);
        assert!(!cfg.trust_proxy);
        assert!(!cfg.require_api_key);
        assert_eq!(cfg.api_key_secret, "endpoint-proxy-api-key-secret");
        assert_eq!(cfg.machine_id_salt, "endpoint-proxy-salt");
        assert_eq!(cfg.byok_enc_key, None);
        assert_eq!(cfg.peer_token, None);
        assert_eq!(cfg.cron_secret, None);
        assert_eq!(cfg.shutdown_secret, None);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_auth_config_admin_aliases_and_validation() {
        let temp_dir = env::temp_dir().join(format!("9r-test-auth-alias-{}", std::process::id()));
        let mut map = HashMap::new();
        map.insert("ADMIN_KEY", "admin-key-alias-value");
        map.insert("NINEROUTER_API_KEY", "cli-ninerouter-key-value");
        map.insert("ROUTER_API_KEY", "cli-router-key-value");
        map.insert("INTERNAL_API_TOKEN", "inter-service-token-value");
        map.insert("AUTH_COOKIE_SECURE", "1");
        map.insert("TRUST_PROXY", "true");
        map.insert("REQUIRE_API_KEY", "true");

        let cfg = AuthConfig::load_from(&temp_dir, mock_env(map));
        assert_eq!(cfg.admin_api_token, Some("admin-key-alias-value".to_string()));
        assert_eq!(cfg.ninerouter_api_key, Some("cli-ninerouter-key-value".to_string()));
        assert_eq!(cfg.router_api_key, Some("cli-router-key-value".to_string()));
        assert_eq!(cfg.internal_api_token, Some("inter-service-token-value".to_string()));
        assert!(cfg.auth_cookie_secure);
        assert!(cfg.trust_proxy);
        assert!(cfg.require_api_key);

        // Verify constant-time validation works for all candidate keys
        assert!(cfg.is_valid_admin_token("admin-key-alias-value"));
        assert!(cfg.is_valid_admin_token("cli-ninerouter-key-value"));
        assert!(cfg.is_valid_admin_token("cli-router-key-value"));
        assert!(cfg.is_valid_admin_token("inter-service-token-value"));
        assert!(!cfg.is_valid_admin_token("invalid-random-token"));
        assert!(!cfg.is_valid_admin_token(""));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_auth_config_nine_router_key_underscore_alias() {
        let temp_dir = env::temp_dir().join(format!("9r-test-auth-nine-{}", std::process::id()));
        let mut map = HashMap::new();
        map.insert("NINE_ROUTER_API_KEY", "nine-router-with-underscore");

        let cfg = AuthConfig::load_from(&temp_dir, mock_env(map));
        assert_eq!(cfg.ninerouter_api_key, Some("nine-router-with-underscore".to_string()));
        assert!(cfg.is_valid_admin_token("nine-router-with-underscore"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    // --- Tier 3: NetworkConfig Tests ---

    #[test]
    fn test_network_config_defaults() {
        let cfg = NetworkConfig::load_from(mock_env(HashMap::new()));
        assert_eq!(cfg.http_proxy, None);
        assert_eq!(cfg.https_proxy, None);
        assert_eq!(cfg.all_proxy, None);
        assert_eq!(cfg.no_proxy, Some("localhost,127.0.0.1".to_string()));
        assert_eq!(cfg.nine_router_proxy_url, None);
        assert_eq!(cfg.nine_router_no_proxy, None);
        assert!(!cfg.nine_router_proxy_managed);
        assert!(cfg.strict_ssl);
        assert_eq!(cfg.effective_http_proxy(), None);
        assert_eq!(cfg.effective_https_proxy(), None);
        assert_eq!(cfg.effective_no_proxy(), Some("localhost,127.0.0.1"));
    }

    #[test]
    fn test_network_config_case_aliases() {
        let mut map = HashMap::new();
        map.insert("http_proxy", "http://corporate.proxy:8080");
        map.insert("https_proxy", "https://corporate.proxy:8443");
        map.insert("all_proxy", "socks5://corporate.proxy:1080");
        map.insert("no_proxy", "localhost,127.0.0.1,.internal.net");
        map.insert("STRICT_SSL", "false");

        let cfg = NetworkConfig::load_from(mock_env(map));
        assert_eq!(cfg.http_proxy, Some("http://corporate.proxy:8080".to_string()));
        assert_eq!(cfg.https_proxy, Some("https://corporate.proxy:8443".to_string()));
        assert_eq!(cfg.all_proxy, Some("socks5://corporate.proxy:1080".to_string()));
        assert_eq!(cfg.no_proxy, Some("localhost,127.0.0.1,.internal.net".to_string()));
        assert!(!cfg.strict_ssl);
    }

    #[test]
    fn test_network_config_nine_router_override_precedence() {
        let mut map = HashMap::new();
        map.insert("HTTP_PROXY", "http://system.proxy:8080");
        map.insert("HTTPS_PROXY", "https://system.proxy:8443");
        map.insert("NINE_ROUTER_PROXY_URL", "http://dedicated.proxy:9999");
        map.insert("NINE_ROUTER_NO_PROXY", "local.override");

        let cfg = NetworkConfig::load_from(mock_env(map));
        assert_eq!(cfg.effective_http_proxy(), Some("http://dedicated.proxy:9999"));
        assert_eq!(cfg.effective_https_proxy(), Some("http://dedicated.proxy:9999"));
        assert_eq!(cfg.effective_no_proxy(), Some("local.override"));
    }

    // --- Tier 4: RoutingConfig Tests ---

    #[test]
    fn test_routing_config_defaults() {
        let cfg = RoutingConfig::load_from(mock_env(HashMap::new()));
        assert_eq!(cfg.circuit_breaker_failure_threshold, 5);
        assert_eq!(cfg.circuit_breaker_cooldown_secs, 30);
        assert_eq!(cfg.circuit_breaker_rolling_window_secs, 60);
        assert_eq!(cfg.hedged_delay_ms, 0);
        assert!(cfg.enable_translator);
        assert_eq!(cfg.models_list_source, "live");
        assert_eq!(cfg.structured_output_mode, "auto");
        assert_eq!(cfg.usage_buffer_tokens, 2000);
        assert_eq!(cfg.flight_wait_ms, 0);
        assert_eq!(cfg.outbound_concurrency, 0);
    }

    #[test]
    fn test_routing_config_custom_and_hedged_alias() {
        let mut map = HashMap::new();
        map.insert("CIRCUIT_BREAKER_FAILURE_THRESHOLD", "10");
        map.insert("CIRCUIT_BREAKER_COOLDOWN_SECS", "45");
        map.insert("CIRCUIT_BREAKER_ROLLING_WINDOW_SECS", "120");
        map.insert("HEDGED_DELAY", "500");
        map.insert("ENABLE_TRANSLATOR", "false");
        map.insert("MODELS_LIST_SOURCE", "cached");
        map.insert("STRUCTURED_OUTPUT_MODE", "json_schema");
        map.insert("USAGE_BUFFER_TOKENS", "4000");
        map.insert("NINEROUTER_FLIGHT_WAIT_MS", "50");
        map.insert("NINEROUTER_OUTBOUND_CONCURRENCY", "16");

        let cfg = RoutingConfig::load_from(mock_env(map));
        assert_eq!(cfg.circuit_breaker_failure_threshold, 10);
        assert_eq!(cfg.circuit_breaker_cooldown_secs, 45);
        assert_eq!(cfg.circuit_breaker_rolling_window_secs, 120);
        assert_eq!(cfg.hedged_delay_ms, 500);
        assert!(!cfg.enable_translator);
        assert_eq!(cfg.models_list_source, "cached");
        assert_eq!(cfg.structured_output_mode, "json_schema");
        assert_eq!(cfg.usage_buffer_tokens, 4000);
        assert_eq!(cfg.flight_wait_ms, 50);
        assert_eq!(cfg.outbound_concurrency, 16);
    }

    // --- Tier 5: StreamingConfig Tests ---

    #[test]
    fn test_streaming_config_defaults() {
        let cfg = StreamingConfig::load_from(mock_env(HashMap::new()));
        assert_eq!(cfg.stream_stall_timeout_ms, 360_000);
        assert_eq!(cfg.stream_first_chunk_timeout_ms, 200_000);
        assert_eq!(cfg.empty_peek_deadline_ms, 45_000);
        assert_eq!(cfg.fetch_connect_timeout_ms, 60_000);
        assert_eq!(cfg.fetch_keepalive_timeout_ms, 4_000);
        assert!(cfg.fetch_fresh_retry);
        assert_eq!(cfg.max_body_mb, 20);
        assert_eq!(cfg.max_upload_mb, 32);
        assert_eq!(cfg.max_response_mb, 32);
        assert_eq!(cfg.stream_capture_max_bytes, 65_536);
        assert_eq!(cfg.empty_peek_max_bytes, 262_144);
        assert_eq!(cfg.max_body_bytes(), 20 * 1024 * 1024);
        assert_eq!(cfg.max_upload_bytes(), 32 * 1024 * 1024);
        assert_eq!(cfg.max_response_bytes(), 32 * 1024 * 1024);
    }

    #[test]
    fn test_streaming_config_custom_fetch_and_limits() {
        let mut map = HashMap::new();
        map.insert("STREAM_STALL_TIMEOUT_MS", "180000");
        map.insert("STREAM_FIRST_CHUNK_TIMEOUT_MS", "100000");
        map.insert("EMPTY_PEEK_DEADLINE_MS", "30000");
        map.insert("FETCH_CONNECT_TIMEOUT_MS", "15000");
        map.insert("FETCH_KEEPALIVE_TIMEOUT_MS", "2000");
        map.insert("FETCH_FRESH_RETRY", "off");
        map.insert("NINEROUTER_MAX_BODY_MB", "10");
        map.insert("NINEROUTER_MAX_UPLOAD_MB", "64");
        map.insert("NINEROUTER_MAX_RESPONSE_MB", "48");
        map.insert("NINEROUTER_STREAM_CAPTURE_MAX_BYTES", "131072");
        map.insert("NINEROUTER_EMPTY_PEEK_MAX_BYTES", "524288");

        let cfg = StreamingConfig::load_from(mock_env(map));
        assert_eq!(cfg.stream_stall_timeout_ms, 180_000);
        assert_eq!(cfg.stream_first_chunk_timeout_ms, 100_000);
        assert_eq!(cfg.empty_peek_deadline_ms, 30_000);
        assert_eq!(cfg.fetch_connect_timeout_ms, 15_000);
        assert_eq!(cfg.fetch_keepalive_timeout_ms, 2_000);
        assert!(!cfg.fetch_fresh_retry);
        assert_eq!(cfg.max_body_mb, 10);
        assert_eq!(cfg.max_upload_mb, 64);
        assert_eq!(cfg.max_response_mb, 48);
        assert_eq!(cfg.stream_capture_max_bytes, 131_072);
        assert_eq!(cfg.empty_peek_max_bytes, 524_288);
    }

    // --- Tier 7: ObservabilityConfig Tests ---

    #[test]
    fn test_observability_config_defaults() {
        let cfg = ObservabilityConfig::load_from(mock_env(HashMap::new()));
        assert!(cfg.enable_request_logs);
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.observability_enabled);
        assert_eq!(cfg.batch_size, 50);
        assert_eq!(cfg.flush_interval_ms, 5000);
        assert_eq!(cfg.max_records, 10000);
        assert_eq!(cfg.max_json_size, 1048576);
        assert!(!cfg.log_usage_verbose);
        assert_eq!(cfg.coordinator_timeout_ms, 10000);
        assert!(cfg.console_capture);
    }

    #[test]
    fn test_observability_config_custom() {
        let mut map = HashMap::new();
        map.insert("ENABLE_REQUEST_LOGS", "false");
        map.insert("LOG_LEVEL", "debug");
        map.insert("OBSERVABILITY_ENABLED", "0");
        map.insert("OBSERVABILITY_BATCH_SIZE", "100");
        map.insert("OBSERVABILITY_FLUSH_INTERVAL_MS", "2500");
        map.insert("OBSERVABILITY_MAX_RECORDS", "50000");
        map.insert("OBSERVABILITY_MAX_JSON_SIZE", "2097152");
        map.insert("LOG_USAGE_VERBOSE", "1");
        map.insert("COORDINATOR_TIMEOUT_MS", "20000");
        map.insert("NINEROUTER_CONSOLE_CAPTURE", "false");

        let cfg = ObservabilityConfig::load_from(mock_env(map));
        assert!(!cfg.enable_request_logs);
        assert_eq!(cfg.log_level, "debug");
        assert!(!cfg.observability_enabled);
        assert_eq!(cfg.batch_size, 100);
        assert_eq!(cfg.flush_interval_ms, 2500);
        assert_eq!(cfg.max_records, 50000);
        assert_eq!(cfg.max_json_size, 2097152);
        assert!(cfg.log_usage_verbose);
        assert_eq!(cfg.coordinator_timeout_ms, 20000);
        assert!(!cfg.console_capture);
    }

    // --- Root Config Integration & Serde Tests ---

    #[test]
    fn test_config_load_integration() {
        let mut map = HashMap::new();
        map.insert("PORT", "20128");
        map.insert("DATA_DIR", "/var/lib/9router");
        map.insert("ADMIN_API_TOKEN", "test-root-token-12345");
        map.insert("HTTP_PROXY", "http://proxy.local:8080");

        let cfg = Config::load_from(mock_env(map));

        // Direct field access (backward compatibility)
        assert_eq!(cfg.port, 20128);
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/9router"));
        assert_eq!(cfg.db_path, PathBuf::from("/var/lib/9router/db/data.sqlite"));

        // Modular sub-configs
        assert_eq!(cfg.server.port, 20128);
        assert_eq!(cfg.storage.data_dir, PathBuf::from("/var/lib/9router"));
        assert_eq!(cfg.auth.admin_api_token, Some("test-root-token-12345".to_string()));
        assert_eq!(cfg.network.http_proxy, Some("http://proxy.local:8080".to_string()));

        // Method forwarders
        assert!(cfg.is_valid_admin_token("test-root-token-12345"));
        assert_eq!(cfg.effective_http_proxy(), Some("http://proxy.local:8080"));
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let cfg = Config::default();
        let serialized = serde_json::to_string(&cfg).expect("Serialization failed");
        let deserialized: Config = serde_json::from_str(&serialized).expect("Deserialization failed");

        assert_eq!(cfg.port, deserialized.port);
        assert_eq!(cfg.server.port, deserialized.server.port);
        assert_eq!(cfg.storage.data_dir, deserialized.storage.data_dir);
        assert_eq!(cfg.routing.circuit_breaker_failure_threshold, deserialized.routing.circuit_breaker_failure_threshold);
        assert_eq!(cfg.streaming.stream_stall_timeout_ms, deserialized.streaming.stream_stall_timeout_ms);
        assert_eq!(cfg.observability.log_level, deserialized.observability.log_level);
    }
}
