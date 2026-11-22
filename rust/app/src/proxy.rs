use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::config::NetworkConfig;
use crate::translator::{ChatCompletionRequest, ClaudeMessageRequest};

// ============================================================================
// HTTP Client Initialization
// ============================================================================

/// Initializes a high-performance reqwest::Client configured with Keep-Alive
/// connection pooling, sensible connection/request timeouts, rustls-tls,
/// and optional proxy / SSL configuration resolved from environment variables.
pub fn create_proxy_client() -> reqwest::Client {
    create_proxy_client_with_config(None)
}

/// Initializes a reqwest::Client with optional explicit NetworkConfig
/// or falling back to environment variables.
pub fn create_proxy_client_with_config(config: Option<&NetworkConfig>) -> reqwest::Client {
    create_proxy_client_builder(config)
        .build()
        .expect("Failed to initialize reqwest client with rustls-tls")
}

/// Builds and configures a reqwest::ClientBuilder with pooling, timeouts,
/// proxy dispatcher (HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, NO_PROXY), and
/// SSL validation (STRICT_SSL).
pub fn create_proxy_client_builder(config: Option<&NetworkConfig>) -> reqwest::ClientBuilder {
    let resolved_config: NetworkConfig;
    let net = match config {
        Some(c) => c,
        None => {
            resolved_config = NetworkConfig::load();
            &resolved_config
        }
    };

    let connect_timeout = std::env::var("FETCH_CONNECT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(10));

    let keepalive_timeout = std::env::var("FETCH_KEEPALIVE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(60));

    let mut builder = reqwest::Client::builder()
        .tcp_keepalive(Some(keepalive_timeout))
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        .pool_max_idle_per_host(32)
        .connect_timeout(connect_timeout)
        .timeout(Duration::from_secs(180));

    // Custom SSL validation: when STRICT_SSL is false, accept invalid/self-signed certificates
    if !net.strict_ssl {
        builder = builder.danger_accept_invalid_certs(true);
    }

    let no_proxy_val = net.effective_no_proxy();

    // 1. High-priority dedicated proxy: NINE_ROUTER_PROXY_URL
    if let Some(dedicated_url) = &net.nine_router_proxy_url {
        let trimmed = dedicated_url.trim();
        if !trimmed.is_empty() {
            if let Ok(mut p) = reqwest::Proxy::all(trimmed) {
                if let Some(np) = no_proxy_val {
                    let np_trimmed = np.trim();
                    if !np_trimmed.is_empty() {
                        p = p.no_proxy(reqwest::NoProxy::from_string(np_trimmed));
                    }
                }
                builder = builder.proxy(p);
                return builder;
            }
        }
    }

    // 2. HTTP proxy (HTTP_PROXY / http_proxy)
    if let Some(http_url) = &net.http_proxy {
        let trimmed = http_url.trim();
        if !trimmed.is_empty() {
            if let Ok(mut p) = reqwest::Proxy::http(trimmed) {
                if let Some(np) = no_proxy_val {
                    let np_trimmed = np.trim();
                    if !np_trimmed.is_empty() {
                        p = p.no_proxy(reqwest::NoProxy::from_string(np_trimmed));
                    }
                }
                builder = builder.proxy(p);
            }
        }
    }

    // 3. HTTPS proxy (HTTPS_PROXY / https_proxy)
    if let Some(https_url) = &net.https_proxy {
        let trimmed = https_url.trim();
        if !trimmed.is_empty() {
            if let Ok(mut p) = reqwest::Proxy::https(trimmed) {
                if let Some(np) = no_proxy_val {
                    let np_trimmed = np.trim();
                    if !np_trimmed.is_empty() {
                        p = p.no_proxy(reqwest::NoProxy::from_string(np_trimmed));
                    }
                }
                builder = builder.proxy(p);
            }
        }
    }

    // 4. General ALL proxy (ALL_PROXY / all_proxy)
    if let Some(all_url) = &net.all_proxy {
        let trimmed = all_url.trim();
        if !trimmed.is_empty() {
            if let Ok(mut p) = reqwest::Proxy::all(trimmed) {
                if let Some(np) = no_proxy_val {
                    let np_trimmed = np.trim();
                    if !np_trimmed.is_empty() {
                        p = p.no_proxy(reqwest::NoProxy::from_string(np_trimmed));
                    }
                }
                builder = builder.proxy(p);
            }
        }
    }

    builder
}

// ============================================================================
// Provider Node Prefix Routing
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTarget {
    pub provider: Option<String>,
    pub model: String,
}

/// Parses provider node prefix routing.
/// E.g. "vertex/gemini-4-carbon" -> provider: Some("vertex"), model: "gemini-4-carbon"
/// "openai/gpt-4o" -> provider: Some("openai"), model: "gpt-4o"
/// "gpt-4o" -> provider: None, model: "gpt-4o"
pub fn parse_provider_and_model(model_str: &str) -> ParsedTarget {
    if let Some((prefix, model)) = model_str.split_once('/') {
        ParsedTarget {
            provider: Some(prefix.to_string()),
            model: model.to_string(),
        }
    } else {
        ParsedTarget {
            provider: None,
            model: model_str.to_string(),
        }
    }
}

/// Strips provider prefix if present, returning (Option<provider>, model).
pub fn strip_provider_prefix(model_str: &str) -> (Option<String>, String) {
    let parsed = parse_provider_and_model(model_str);
    (parsed.provider, parsed.model)
}

// ============================================================================
// Vertex AI Location & Fallback Resolver
// ============================================================================

/// Resolves the Vertex AI location according to 9Router fallback rules:
/// - Explicit per-model override in connection configuration takes precedence.
/// - If connection has a regional location (e.g. "asia-southeast1"):
///   - "gemini-4-carbon" only exists on global/us/eu, so it safely falls back to "global".
///   - "gemini-4-argon" preserves the configured regional location (e.g. "asia-southeast1").
/// - When no location is configured, defaults to "global".
pub fn resolve_vertex_location(
    model: &str,
    configured_location: Option<&str>,
    model_override: Option<&str>,
) -> String {
    // 1. Per-model override (e.g. providerSpecificData.modelLocations)
    if let Some(m_override) = model_override {
        let trimmed = m_override.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let base_model = model.split('(').next().unwrap_or(model).trim();

    // 2. Connection-level configured location
    if let Some(loc) = configured_location {
        let loc = loc.trim();
        if !loc.is_empty() {
            let is_carbon = base_model.to_ascii_lowercase().contains("carbon");
            let loc_lower = loc.to_ascii_lowercase();
            let is_regional = loc_lower.starts_with("asia-")
                || loc_lower.starts_with("australia-")
                || loc_lower.starts_with("southamerica-")
                || loc_lower.starts_with("europe-west");

            if is_carbon && is_regional {
                return "global".to_string();
            }
            return loc.to_string();
        }
    }

    // 3. Global fallback
    "global".to_string()
}

/// Constructs the Vertex AI REST endpoint URL.
pub fn build_vertex_url(
    project_id: Option<&str>,
    location: &str,
    model: &str,
    stream: bool,
    api_key: Option<&str>,
) -> String {
    let action = if stream {
        "streamGenerateContent"
    } else {
        "generateContent"
    };

    let mut url = match project_id {
        Some(pid) if !pid.trim().is_empty() => format!(
            "https://aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:{}",
            pid.trim(),
            location,
            model,
            action
        ),
        _ => format!(
            "https://aiplatform.googleapis.com/v1/publishers/google/models/{}:{}",
            model, action
        ),
    };

    let mut query = Vec::new();
    if stream {
        query.push("alt=sse".to_string());
    }
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            query.push(format!("key={}", key.trim()));
        }
    }

    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }

    url
}

// ============================================================================
// Request-Scoped Retries
// ============================================================================

/// Determines if an HTTP failure is "requestScoped" (the request itself was rejected,
/// e.g. context overflow, bad parameters, unsupported model) vs credential/quota scoped
/// (401, 403, 429) or server error (5xx).
pub fn is_request_scoped_error(status: Option<u16>, error_msg: &str) -> bool {
    if let Some(s) = status {
        if (400..500).contains(&s) && s != 401 && s != 402 && s != 403 && s != 429 {
            return true;
        }
    }

    let lower = error_msg.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("maximum context length")
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long")
        || lower.contains("model not found")
        || lower.contains("unsupported model")
        || lower.contains("invalid request")
        || lower.contains("bad request")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAttempt {
    pub attempt_number: usize,
    pub provider: Option<String>,
    pub model: String,
    pub status: Option<u16>,
    pub error: String,
    pub is_request_scoped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    Retry {
        backoff_ms: u64,
        is_request_scoped: bool,
    },
    DoNotRetry {
        is_request_scoped: bool,
        reason: String,
    },
}

/// Tracks retry attempts for a single incoming client request.
/// Prevents cross-request pollution and infinite loops while maintaining isolation.
#[derive(Debug, Clone)]
pub struct RequestRetryTracker {
    pub max_retries: usize,
    pub attempts: usize,
    pub history: Vec<RetryAttempt>,
}

impl RequestRetryTracker {
    pub fn new(max_retries: usize) -> Self {
        Self {
            max_retries,
            attempts: 0,
            history: Vec::new(),
        }
    }

    pub fn record_failure(
        &mut self,
        provider: Option<&str>,
        model: &str,
        status: Option<u16>,
        error_msg: &str,
    ) -> RetryDecision {
        self.attempts += 1;
        let is_req_scoped = is_request_scoped_error(status, error_msg);

        self.history.push(RetryAttempt {
            attempt_number: self.attempts,
            provider: provider.map(ToString::to_string),
            model: model.to_string(),
            status,
            error: error_msg.to_string(),
            is_request_scoped: is_req_scoped,
        });

        if self.attempts > self.max_retries {
            return RetryDecision::DoNotRetry {
                is_request_scoped: is_req_scoped,
                reason: format!("Max retries ({}) reached for request", self.max_retries),
            };
        }

        let backoff_ms = if is_req_scoped {
            50 // Immediate fallback for request-scoped alternative member
        } else {
            100 * (1 << (self.attempts - 1).min(4))
        };

        RetryDecision::Retry {
            backoff_ms,
            is_request_scoped: is_req_scoped,
        }
    }
}

// ============================================================================
// Upstream Request Forwarding
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct UpstreamConnection {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    pub project_id: Option<String>,
    pub location: Option<String>,
    pub custom_headers: Option<HeaderMap>,
    pub schema_mapping: Option<crate::custom_adapter::CustomSchemaMapping>,
}

/// Constructs the chat completions upstream URL based on baseUrl conventions:
/// - Already ends with /chat/completions -> use as is
/// - Cloudflare AI run URL (contains /ai/run) -> append /{model}
/// - Ends with /v1, /compat, or /v2 -> append /chat/completions
/// - Otherwise -> append /v1/chat/completions
pub fn build_chat_completions_url(base_url: &str, model: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        return base.to_string();
    }
    if base.contains("/ai/run") {
        return format!("{}/{}", base, model);
    }
    if base.ends_with("/v1") || base.ends_with("/compat") || base.ends_with("/v2") {
        return format!("{}/chat/completions", base);
    }
    format!("{}/v1/chat/completions", base)
}

#[derive(Clone)]
pub struct ProxyService {
    client: reqwest::Client,
    config: Option<NetworkConfig>,
}

impl Default for ProxyService {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyService {
    /// Initializes ProxyService resolving proxy configuration from NetworkConfig or environment variables
    /// (HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, NO_PROXY, STRICT_SSL).
    pub fn new() -> Self {
        Self::with_network_config(None)
    }

    /// Initializes ProxyService with an optional explicit NetworkConfig.
    /// If config is None, falls back to environment variables.
    pub fn with_network_config(config: Option<&NetworkConfig>) -> Self {
        let net = match config {
            Some(c) => c.clone(),
            None => NetworkConfig::load(),
        };
        let client = create_proxy_client_with_config(Some(&net));
        Self {
            client,
            config: Some(net),
        }
    }

    /// Initializes ProxyService directly from a NetworkConfig reference.
    pub fn from_config(config: &NetworkConfig) -> Self {
        Self::with_network_config(Some(config))
    }

    /// Alias for with_network_config.
    pub fn new_with_config(config: Option<&NetworkConfig>) -> Self {
        Self::with_network_config(config)
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn config(&self) -> Option<&NetworkConfig> {
        self.config.as_ref()
    }

    /// Forwards an OpenAI chat completions request to upstream provider.
    pub async fn forward_chat_completions(
        &self,
        req: &ChatCompletionRequest,
        conn: &UpstreamConnection,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let default_base = match conn.provider.to_ascii_lowercase().as_str() {
            "deepseek" => "https://api.deepseek.com",
            "groq" => "https://api.groq.com/openai",
            "openrouter" => "https://openrouter.ai/api",
            "mistral" => "https://api.mistral.ai",
            "together" => "https://api.together.xyz",
            "perplexity" => "https://api.perplexity.ai",
            _ => "https://api.openai.com",
        };
        let base = conn
            .base_url
            .as_deref()
            .unwrap_or(default_base);
        let url = build_chat_completions_url(base, &req.model);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let provider_lower = conn.provider.to_ascii_lowercase();
        if provider_lower == "azure" {
            if let Some(key) = &conn.api_key {
                if let Ok(val) = HeaderValue::from_str(key) {
                    headers.insert(reqwest::header::HeaderName::from_static("api-key"), val);
                }
            }
        } else if provider_lower == "gemini" || provider_lower == "google" {
            if let Some(key) = &conn.api_key {
                if let Ok(val) = HeaderValue::from_str(key) {
                    headers.insert(reqwest::header::HeaderName::from_static("x-goog-api-key"), val);
                }
            }
        } else if let Some(token) = &conn.access_token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        } else if let Some(key) = &conn.api_key {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", key)) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        if let Some(custom) = &conn.custom_headers {
            headers.extend(custom.clone());
        }

        self.client
            .post(&url)
            .headers(headers)
            .json(req)
            .send()
            .await
    }

    /// Forwards a Claude messages request to Anthropic upstream.
    pub async fn forward_claude_messages(
        &self,
        req: &ClaudeMessageRequest,
        conn: &UpstreamConnection,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let base = conn
            .base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com");
        let url = format!("{}/v1/messages", base.trim_end_matches('/'));

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        if let Some(key) = &conn.api_key {
            if let Ok(val) = HeaderValue::from_str(key) {
                headers.insert(reqwest::header::HeaderName::from_static("x-api-key"), val);
            }
        } else if let Some(token) = &conn.access_token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        if let Some(custom) = &conn.custom_headers {
            headers.extend(custom.clone());
        }

        self.client
            .post(&url)
            .headers(headers)
            .json(req)
            .send()
            .await
    }

    /// Forwards an arbitrary payload to a custom provider endpoint.
    pub async fn forward_custom_request(
        &self,
        url: &str,
        body: &serde_json::Value,
        conn: &UpstreamConnection,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(key) = &conn.api_key {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", key)) {
                headers.insert(AUTHORIZATION, val);
            }
        }
        if let Some(custom) = &conn.custom_headers {
            headers.extend(custom.clone());
        }

        self.client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_prefix_stripping() {
        // vertex prefix
        let parsed = parse_provider_and_model("vertex/gemini-4-carbon");
        assert_eq!(parsed.provider.as_deref(), Some("vertex"));
        assert_eq!(parsed.model, "gemini-4-carbon");

        // openai prefix
        let (prov, model) = strip_provider_prefix("openai/gpt-4o");
        assert_eq!(prov.as_deref(), Some("openai"));
        assert_eq!(model, "gpt-4o");

        // nested prefix
        let (prov, model) = strip_provider_prefix("cf/@cf/meta/llama-3-8b-instruct");
        assert_eq!(prov.as_deref(), Some("cf"));
        assert_eq!(model, "@cf/meta/llama-3-8b-instruct");

        // no prefix
        let (prov, model) = strip_provider_prefix("gpt-4o");
        assert_eq!(prov, None);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn test_vertex_carbon_global_endpoint_override() {
        // gemini-4-carbon with regional location asia-southeast1 -> falls back to global
        let loc = resolve_vertex_location("gemini-4-carbon", Some("asia-southeast1"), None);
        assert_eq!(loc, "global");

        // gemini-4-argon with regional location asia-southeast1 -> preserved
        let loc_argon = resolve_vertex_location("gemini-4-argon", Some("asia-southeast1"), None);
        assert_eq!(loc_argon, "asia-southeast1");

        // gemini-4-carbon with per-model override -> honors override
        let loc_override =
            resolve_vertex_location("gemini-4-carbon", Some("asia-southeast1"), Some("us"));
        assert_eq!(loc_override, "us");

        // default location when none configured is global
        let loc_default = resolve_vertex_location("gemini-4-carbon", None, None);
        assert_eq!(loc_default, "global");

        // URL building test
        let url = build_vertex_url(
            Some("698194328976"),
            &loc,
            "gemini-4-carbon",
            false,
            Some("test-key"),
        );
        assert_eq!(
            url,
            "https://aiplatform.googleapis.com/v1/projects/698194328976/locations/global/publishers/google/models/gemini-4-carbon:generateContent?key=test-key"
        );

        let stream_url = build_vertex_url(
            Some("698194328976"),
            &loc,
            "gemini-4-carbon",
            true,
            Some("test-key"),
        );
        assert_eq!(
            stream_url,
            "https://aiplatform.googleapis.com/v1/projects/698194328976/locations/global/publishers/google/models/gemini-4-carbon:streamGenerateContent?alt=sse&key=test-key"
        );
    }

    #[test]
    fn test_request_scoped_retries() {
        let mut tracker = RequestRetryTracker::new(2);

        // 400 Bad Request error is request-scoped
        let dec1 = tracker.record_failure(
            Some("vertex"),
            "gemini-4-carbon",
            Some(400),
            "context_length_exceeded: prompt exceeds max tokens",
        );
        match dec1 {
            RetryDecision::Retry {
                is_request_scoped, ..
            } => {
                assert!(is_request_scoped);
            }
            _ => panic!("Expected Retry"),
        }
        assert_eq!(tracker.attempts, 1);

        // 429 Rate limit is not request-scoped (it's quota/credential scoped)
        let dec2 = tracker.record_failure(
            Some("vertex"),
            "gemini-4-carbon",
            Some(429),
            "Too many requests",
        );
        match dec2 {
            RetryDecision::Retry {
                is_request_scoped, ..
            } => {
                assert!(!is_request_scoped);
            }
            _ => panic!("Expected Retry"),
        }
        assert_eq!(tracker.attempts, 2);

        // Third attempt exceeds max_retries (2)
        let dec3 = tracker.record_failure(
            Some("vertex"),
            "gemini-4-carbon",
            Some(500),
            "Internal server error",
        );
        match dec3 {
            RetryDecision::DoNotRetry { .. } => {}
            _ => panic!("Expected DoNotRetry"),
        }
    }

    static PROXY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_proxy_builder_defaults() {
        let _lock = PROXY_ENV_LOCK.lock().unwrap();
        let builder = create_proxy_client_builder(None);
        assert!(builder.build().is_ok());

        let service = ProxyService::new();
        assert!(service.client().get("https://example.com").build().is_ok());
    }

    #[test]
    fn test_proxy_builder_with_network_config() {
        let cfg = NetworkConfig {
            http_proxy: Some("http://proxy.internal:8080".to_string()),
            https_proxy: Some("http://proxy.internal:8443".to_string()),
            all_proxy: None,
            no_proxy: Some("localhost,127.0.0.1,internal.domain".to_string()),
            nine_router_proxy_url: None,
            nine_router_no_proxy: None,
            nine_router_proxy_managed: false,
            strict_ssl: true,
        };

        let builder = create_proxy_client_builder(Some(&cfg));
        let client = builder.build().expect("Build client with HTTP/HTTPS proxies");
        assert!(client.get("https://example.com").build().is_ok());

        let service = ProxyService::from_config(&cfg);
        assert_eq!(
            service.config().and_then(|c| c.http_proxy.as_deref()),
            Some("http://proxy.internal:8080")
        );
    }

    #[test]
    fn test_proxy_builder_all_proxy_and_socks() {
        let cfg = NetworkConfig {
            http_proxy: None,
            https_proxy: None,
            all_proxy: Some("socks5://127.0.0.1:1080".to_string()),
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            nine_router_proxy_url: None,
            nine_router_no_proxy: None,
            nine_router_proxy_managed: false,
            strict_ssl: true,
        };

        let builder = create_proxy_client_builder(Some(&cfg));
        assert!(builder.build().is_ok());
    }

    #[test]
    fn test_proxy_builder_nine_router_dedicated_proxy() {
        let cfg = NetworkConfig {
            http_proxy: Some("http://fallback.proxy:8080".to_string()),
            https_proxy: None,
            all_proxy: None,
            no_proxy: Some("localhost".to_string()),
            nine_router_proxy_url: Some("http://high-priority.proxy:3128".to_string()),
            nine_router_no_proxy: Some("127.0.0.1".to_string()),
            nine_router_proxy_managed: true,
            strict_ssl: true,
        };

        let builder = create_proxy_client_builder(Some(&cfg));
        let client = builder.build().expect("Build client with dedicated proxy");
        assert!(client.get("https://example.com").build().is_ok());
    }

    #[test]
    fn test_proxy_builder_strict_ssl_disabled() {
        let cfg = NetworkConfig {
            http_proxy: None,
            https_proxy: None,
            all_proxy: None,
            no_proxy: None,
            nine_router_proxy_url: None,
            nine_router_no_proxy: None,
            nine_router_proxy_managed: false,
            strict_ssl: false,
        };

        let builder = create_proxy_client_builder(Some(&cfg));
        let client = builder.build().expect("Build client with dangerous invalid certs");
        assert!(client.get("https://self-signed.local").build().is_ok());
    }

    #[test]
    fn test_proxy_service_env_resolution() {
        let _lock = PROXY_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("HTTP_PROXY", "http://env-proxy.local:8080");
            std::env::set_var("HTTPS_PROXY", "http://env-proxy.local:8443");
            std::env::set_var("NO_PROXY", "localhost,127.0.0.1,cluster.local");
            std::env::set_var("STRICT_SSL", "0");
        }

        let service = ProxyService::new();
        let cfg = service.config().expect("Config should be populated");
        assert_eq!(cfg.http_proxy.as_deref(), Some("http://env-proxy.local:8080"));
        assert_eq!(cfg.https_proxy.as_deref(), Some("http://env-proxy.local:8443"));
        assert!(!cfg.strict_ssl);

        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("STRICT_SSL");
        }
    }
}
