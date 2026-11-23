use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use reqwest::Client as HttpClient;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;

use super::ca::{create_mitm_server_config, CertificateAuthority, COMMON_TARGET_HOSTS};

pub type ResponseBody = BoxBody<Bytes, anyhow::Error>;

pub fn empty_body() -> ResponseBody {
    Full::new(Bytes::new())
        .map_err(|e| match e {})
        .boxed()
}

pub fn bytes_body(bytes: Bytes) -> ResponseBody {
    Full::new(bytes)
        .map_err(|e| match e {})
        .boxed()
}

pub fn json_response(status: StatusCode, value: serde_json::Value) -> Response<ResponseBody> {
    let bytes = Bytes::from(serde_json::to_vec(&value).unwrap_or_default());
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(bytes_body(bytes))
        .unwrap()
}

#[derive(Debug, Clone)]
pub struct MitmConfig {
    pub port: u16,
    pub router_base: String,
    pub router_api_key: Option<String>,
    pub mitm_dir: PathBuf,
}

impl Default for MitmConfig {
    fn default() -> Self {
        let port = std::env::var("MITM_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8443);
        let default_router_port = std::env::var("PORT").unwrap_or_else(|_| "20127".to_string());
        let router_base = std::env::var("MITM_ROUTER_BASE")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{}", default_router_port));
        let router_api_key = std::env::var("ROUTER_API_KEY").ok();
        let data_dir = crate::config::resolve_data_dir();
        let mitm_dir = data_dir.join("mitm");

        Self {
            port,
            router_base,
            router_api_key,
            mitm_dir,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetTool {
    OpenAI,
    Anthropic,
    Cursor,
    Copilot,
    Antigravity,
    Kiro,
    Cloudflare,
    Other(String),
}

impl TargetTool {
    pub fn from_host(host: &str) -> Option<Self> {
        let h = host.split(':').next().unwrap_or(host).to_lowercase();
        if h == "api.openai.com" {
            Some(TargetTool::OpenAI)
        } else if h == "api.anthropic.com" {
            Some(TargetTool::Anthropic)
        } else if h == "api2.cursor.sh" || h == "cursor.com" {
            Some(TargetTool::Cursor)
        } else if h == "api.individual.githubcopilot.com" {
            Some(TargetTool::Copilot)
        } else if h == "cloudcode-pa.googleapis.com" || h == "daily-cloudcode-pa.googleapis.com" {
            Some(TargetTool::Antigravity)
        } else if h == "runtime.us-east-1.kiro.dev"
            || h == "q.us-east-1.amazonaws.com"
            || h == "codewhisperer.us-east-1.amazonaws.com"
        {
            Some(TargetTool::Kiro)
        } else if h == "gateway.ai.cloudflare.com" {
            Some(TargetTool::Cloudflare)
        } else {
            None
        }
    }
}

/// Extracted routing decision for an incoming interceptable request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Forward to 9router at the target path
    ForwardToRouter {
        target_path: String,
        is_chat: bool,
    },
    /// Direct upstream passthrough (e.g. Antigravity tab autocomplete, non-chat paths)
    PassthroughUpstream {
        host: String,
        path_and_query: String,
    },
    /// Health check probe
    HealthCheck,
}

/// Extract model identifier from URL path or JSON request payload.
pub fn extract_model_from_request(url: &str, body: &[u8]) -> Option<String> {
    // 1. Try URL path: e.g. /models/gemini-pro:streamGenerateContent
    if let Some(pos) = url.find("/models/") {
        let sub = &url[pos + 8..];
        let end = sub.find(|c: char| c == '/' || c == ':' || c == '?').unwrap_or(sub.len());
        let m = &sub[..end];
        if !m.is_empty() {
            return Some(m.to_string());
        }
    }

    // 2. Try JSON body
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            return Some(m.to_string());
        }
        if let Some(m) = v
            .get("conversationState")
            .and_then(|c| c.get("currentMessage"))
            .and_then(|m| m.get("userInputMessage"))
            .and_then(|u| u.get("modelId"))
            .and_then(|s| s.as_str())
        {
            return Some(m.to_string());
        }
    }
    None
}

/// Checks if an Antigravity request is a latency-critical tab autocomplete request
/// that should NEVER be re-routed.
pub fn is_antigravity_tab_autocomplete(model: Option<&str>, path: &str) -> bool {
    if let Some(m) = model {
        let lower = m.to_lowercase();
        if lower.starts_with("tab_") || lower.starts_with("tab-") {
            return true;
        }
    }
    path.contains("tab_jump") || path.contains("tab_flash")
}

/// Determine whether a request should be forwarded to 9router or passed through to upstream.
pub fn classify_mitm_request(
    host: &str,
    path_and_query: &str,
    _method: &Method,
    body: &[u8],
) -> RouteDecision {
    if path_and_query == "/_mitm_health" {
        return RouteDecision::HealthCheck;
    }

    let tool = TargetTool::from_host(host);

    match tool {
        Some(TargetTool::OpenAI) => {
            // Forward standard OpenAI paths directly to local 9router
            RouteDecision::ForwardToRouter {
                target_path: path_and_query.to_string(),
                is_chat: path_and_query.starts_with("/v1/chat/completions"),
            }
        }
        Some(TargetTool::Anthropic) => {
            RouteDecision::ForwardToRouter {
                target_path: path_and_query.to_string(),
                is_chat: path_and_query.starts_with("/v1/messages"),
            }
        }
        Some(TargetTool::Cursor) => {
            // Cursor IDE requests
            if path_and_query.contains("/Run") || path_and_query.contains("/RunSSE") || path_and_query.contains("/BidiAppend") {
                RouteDecision::ForwardToRouter {
                    target_path: "/v1/chat/completions".to_string(),
                    is_chat: true,
                }
            } else {
                RouteDecision::PassthroughUpstream {
                    host: host.to_string(),
                    path_and_query: path_and_query.to_string(),
                }
            }
        }
        Some(TargetTool::Copilot) => {
            if path_and_query.starts_with("/chat/completions")
                || path_and_query.starts_with("/v1/messages")
                || path_and_query.starts_with("/responses")
            {
                let target = if path_and_query.starts_with("/v1/messages") {
                    "/v1/messages".to_string()
                } else {
                    "/v1/chat/completions".to_string()
                };
                RouteDecision::ForwardToRouter {
                    target_path: target,
                    is_chat: true,
                }
            } else {
                RouteDecision::PassthroughUpstream {
                    host: host.to_string(),
                    path_and_query: path_and_query.to_string(),
                }
            }
        }
        Some(TargetTool::Antigravity) => {
            let is_chat = path_and_query.contains(":streamGenerateContent")
                || path_and_query.contains(":generateContent");
            if is_chat {
                let model = extract_model_from_request(path_and_query, body);
                if is_antigravity_tab_autocomplete(model.as_deref(), path_and_query) {
                    RouteDecision::PassthroughUpstream {
                        host: host.to_string(),
                        path_and_query: path_and_query.to_string(),
                    }
                } else {
                    RouteDecision::ForwardToRouter {
                        target_path: "/v1/chat/completions".to_string(),
                        is_chat: true,
                    }
                }
            } else {
                RouteDecision::PassthroughUpstream {
                    host: host.to_string(),
                    path_and_query: path_and_query.to_string(),
                }
            }
        }
        Some(TargetTool::Kiro) => {
            if path_and_query.contains("/generateAssistantResponse") || path_and_query == "/" {
                RouteDecision::ForwardToRouter {
                    target_path: "/v1/chat/completions".to_string(),
                    is_chat: true,
                }
            } else {
                RouteDecision::PassthroughUpstream {
                    host: host.to_string(),
                    path_and_query: path_and_query.to_string(),
                }
            }
        }
        Some(TargetTool::Cloudflare) => {
            RouteDecision::ForwardToRouter {
                target_path: "/v1/chat/completions".to_string(),
                is_chat: true,
            }
        }
        Some(TargetTool::Other(_)) | None => {
            if path_and_query.starts_with("/v1/") {
                RouteDecision::ForwardToRouter {
                    target_path: path_and_query.to_string(),
                    is_chat: path_and_query.starts_with("/v1/chat/completions"),
                }
            } else {
                RouteDecision::PassthroughUpstream {
                    host: host.to_string(),
                    path_and_query: path_and_query.to_string(),
                }
            }
        }
    }
}

pub struct MitmServer {
    pub config: MitmConfig,
    pub ca: Arc<CertificateAuthority>,
    pub http_client: HttpClient,
}

impl MitmServer {
    pub fn new(config: MitmConfig, ca: Arc<CertificateAuthority>) -> Self {
        let http_client = HttpClient::builder()
            .danger_accept_invalid_certs(true)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .unwrap_or_default();

        // Preload leaf certs for common domains
        ca.preload_target_hosts(COMMON_TARGET_HOSTS);

        Self {
            config,
            ca,
            http_client,
        }
    }

    /// Run transparent TLS MITM server on configured port with graceful shutdown.
    pub async fn run<F>(self: Arc<Self>, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let port = self.config.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = TcpListener::bind(addr).await
            .with_context(|| format!("Failed to bind MITM interceptor on {}", addr))?;

        let server_config = create_mitm_server_config(Arc::clone(&self.ca))?;
        let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));

        tracing::info!("🚀 9Router Native MITM TLS interceptor listening on https://0.0.0.0:{}", port);

        let (shutdown_tx, _) = broadcast::channel(1);
        let mut shutdown_rx = shutdown_tx.subscribe();

        let server_clone = Arc::clone(&self);
        let listen_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((tcp_stream, peer_addr)) => {
                                let tls_acceptor = tls_acceptor.clone();
                                let server = Arc::clone(&server_clone);

                                tokio::spawn(async move {
                                    match tls_acceptor.accept(tcp_stream).await {
                                        Ok(tls_stream) => {
                                            let io = TokioIo::new(tls_stream);
                                            let service = service_fn(move |req| {
                                                let server = Arc::clone(&server);
                                                async move {
                                                    server.handle_http_request(req).await
                                                }
                                            });

                                            let builder = ConnBuilder::new(TokioExecutor::new());
                                            if let Err(e) = builder.serve_connection_with_upgrades(io, service).await {
                                                tracing::debug!("MITM connection error from {}: {}", peer_addr, e);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::debug!("TLS handshake failed from {}: {}", peer_addr, e);
                                        }
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::error!("MITM listener accept error: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::info!("MITM server shutdown signal received");
                        break;
                    }
                }
            }
        });

        shutdown.await;
        let _ = shutdown_tx.send(());
        let _ = listen_task.await;
        tracing::info!("MITM TLS interceptor stopped gracefully");
        Ok(())
    }

    /// Handles a decrypted incoming HTTP request.
    pub async fn handle_http_request(
        &self,
        req: Request<Incoming>,
    ) -> Result<Response<ResponseBody>> {
        let (parts, incoming_body) = req.into_parts();
        let body_bytes = incoming_body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        self.handle_request_parts(parts, body_bytes).await
    }

    /// Handles request parts and raw body bytes.
    pub async fn handle_request_parts(
        &self,
        parts: hyper::http::request::Parts,
        body_bytes: Bytes,
    ) -> Result<Response<ResponseBody>> {
        let path_and_query = parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/").to_string();

        // 1. Health check probe
        if path_and_query == "/_mitm_health" {
            let info = serde_json::json!({
                "ok": true,
                "engine": "rust",
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "port": self.config.port,
            });
            return Ok(json_response(StatusCode::OK, info));
        }

        let host = parts
            .headers
            .get(hyper::header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost")
            .to_string();

        // 2. Anti-loop protection: if request came from local 9router, passthrough
        let is_internal = parts.headers.contains_key("x-request-source")
            || parts.headers.contains_key("x-9router-internal");
        if is_internal {
            return self.passthrough_upstream(&host, &path_and_query, parts.method, parts.headers, body_bytes).await;
        }

        // 3. Classify route
        let decision = classify_mitm_request(&host, &path_and_query, &parts.method, &body_bytes);

        match decision {
            RouteDecision::HealthCheck => {
                let info = serde_json::json!({ "ok": true, "engine": "rust" });
                Ok(json_response(StatusCode::OK, info))
            }
            RouteDecision::ForwardToRouter { target_path, is_chat: _ } => {
                self.forward_to_9router(&target_path, parts.method, parts.headers, body_bytes).await
            }
            RouteDecision::PassthroughUpstream { host: target_host, path_and_query: target_pq } => {
                self.passthrough_upstream(&target_host, &target_pq, parts.method, parts.headers, body_bytes).await
            }
        }
    }

    /// Forward request directly to 9router on local port.
    pub async fn forward_to_9router(
        &self,
        target_path: &str,
        method: Method,
        headers: hyper::HeaderMap,
        body_bytes: Bytes,
    ) -> Result<Response<ResponseBody>> {
        let router_url = format!("{}{}", self.config.router_base.trim_end_matches('/'), target_path);

        let mut req_builder = self.http_client.request(method, &router_url);

        // Forward headers, stripping hop-by-hop and host headers
        for (name, val) in headers.iter() {
            let key = name.as_str().to_lowercase();
            if key == "host"
                || key == "connection"
                || key == "keep-alive"
                || key == "transfer-encoding"
                || key == "content-length"
            {
                continue;
            }
            req_builder = req_builder.header(name.as_str(), val.as_bytes());
        }

        // Add 9router authorization header if configured and not present
        if let Some(ref key) = self.config.router_api_key {
            if !headers.contains_key(hyper::header::AUTHORIZATION) {
                req_builder = req_builder.header("authorization", format!("Bearer {}", key));
            }
        }

        // Add internal loop prevention header
        req_builder = req_builder.header("x-request-source", "local");
        req_builder = req_builder.body(body_bytes);

        let res = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to forward request to 9router at {}: {}", router_url, e);
                return Ok(json_response(
                    StatusCode::BAD_GATEWAY,
                    serde_json::json!({
                        "error": {
                            "message": format!("9Router MITM backend unavailable: {}", e),
                            "type": "mitm_bad_gateway"
                        }
                    }),
                ));
            }
        };

        let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::OK);
        let mut resp_builder = Response::builder().status(status);

        for (name, val) in res.headers() {
            let key = name.as_str().to_lowercase();
            if key == "connection" || key == "transfer-encoding" || key == "content-length" {
                continue;
            }
            resp_builder = resp_builder.header(name.as_str(), val.as_bytes());
        }

        let stream = res.bytes_stream().map_ok(Frame::data).map_err(|e| anyhow::anyhow!(e));
        let boxed_stream: ResponseBody = StreamBody::new(stream).boxed();
        Ok(resp_builder.body(boxed_stream)?)
    }

    /// Passthrough request directly to real upstream.
    pub async fn passthrough_upstream(
        &self,
        host: &str,
        path_and_query: &str,
        method: Method,
        headers: hyper::HeaderMap,
        body_bytes: Bytes,
    ) -> Result<Response<ResponseBody>> {
        let clean_host = host.split(':').next().unwrap_or(host);
        let upstream_url = format!("https://{}{}", clean_host, path_and_query);

        let mut req_builder = self.http_client.request(method, &upstream_url);

        for (name, val) in headers.iter() {
            let key = name.as_str().to_lowercase();
            if key == "host" || key == "connection" || key == "transfer-encoding" || key == "content-length" {
                continue;
            }
            req_builder = req_builder.header(name.as_str(), val.as_bytes());
        }
        req_builder = req_builder.header("host", clean_host);
        req_builder = req_builder.body(body_bytes);

        let res = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Upstream passthrough error for {}: {}", upstream_url, e);
                return Ok(json_response(
                    StatusCode::BAD_GATEWAY,
                    serde_json::json!({ "error": { "message": e.to_string(), "type": "passthrough_error" } }),
                ));
            }
        };

        let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::OK);
        let mut resp_builder = Response::builder().status(status);

        for (name, val) in res.headers() {
            let key = name.as_str().to_lowercase();
            if key == "connection" || key == "transfer-encoding" || key == "content-length" {
                continue;
            }
            resp_builder = resp_builder.header(name.as_str(), val.as_bytes());
        }

        let stream = res.bytes_stream().map_ok(Frame::data).map_err(|e| anyhow::anyhow!(e));
        let boxed_stream: ResponseBody = StreamBody::new(stream).boxed();
        Ok(resp_builder.body(boxed_stream)?)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_tool_from_host() {
        assert_eq!(TargetTool::from_host("api.openai.com"), Some(TargetTool::OpenAI));
        assert_eq!(TargetTool::from_host("api.openai.com:443"), Some(TargetTool::OpenAI));
        assert_eq!(TargetTool::from_host("api.anthropic.com"), Some(TargetTool::Anthropic));
        assert_eq!(TargetTool::from_host("api2.cursor.sh"), Some(TargetTool::Cursor));
        assert_eq!(TargetTool::from_host("cloudcode-pa.googleapis.com"), Some(TargetTool::Antigravity));
        assert_eq!(TargetTool::from_host("daily-cloudcode-pa.googleapis.com"), Some(TargetTool::Antigravity));
        assert_eq!(TargetTool::from_host("runtime.us-east-1.kiro.dev"), Some(TargetTool::Kiro));
        assert_eq!(TargetTool::from_host("gateway.ai.cloudflare.com"), Some(TargetTool::Cloudflare));
        assert_eq!(TargetTool::from_host("unknown.domain.org"), None);
    }

    #[test]
    fn test_extract_model_from_request() {
        // Path extraction
        let path = "/v1/models/gemini-2.5-pro:streamGenerateContent";
        assert_eq!(
            extract_model_from_request(path, b""),
            Some("gemini-2.5-pro".to_string())
        );

        // JSON body extraction
        let json_body = br#"{"model":"claude-3-5-sonnet","messages":[]}"#;
        assert_eq!(
            extract_model_from_request("/v1/chat/completions", json_body),
            Some("claude-3-5-sonnet".to_string())
        );

        // Kiro nested conversation state
        let kiro_body = br#"{"conversationState":{"currentMessage":{"userInputMessage":{"modelId":"claude-3-7-sonnet"}}}}"#;
        assert_eq!(
            extract_model_from_request("/", kiro_body),
            Some("claude-3-7-sonnet".to_string())
        );
    }

    #[test]
    fn test_is_antigravity_tab_autocomplete() {
        assert!(is_antigravity_tab_autocomplete(Some("tab_flash_lite_preview"), "/streamGenerateContent"));
        assert!(is_antigravity_tab_autocomplete(Some("tab-jump-model"), "/streamGenerateContent"));
        assert!(is_antigravity_tab_autocomplete(None, "/v1/tab_jump:streamGenerateContent"));
        assert!(!is_antigravity_tab_autocomplete(Some("gemini-3.8-flash-high"), "/streamGenerateContent"));
    }

    #[test]
    fn test_classify_mitm_request_routing() {
        // Health check
        assert_eq!(
            classify_mitm_request("localhost", "/_mitm_health", &Method::GET, b""),
            RouteDecision::HealthCheck
        );

        // OpenAI chat
        assert_eq!(
            classify_mitm_request("api.openai.com", "/v1/chat/completions", &Method::POST, b"{}"),
            RouteDecision::ForwardToRouter {
                target_path: "/v1/chat/completions".to_string(),
                is_chat: true,
            }
        );

        // Anthropic messages
        assert_eq!(
            classify_mitm_request("api.anthropic.com", "/v1/messages", &Method::POST, b"{}"),
            RouteDecision::ForwardToRouter {
                target_path: "/v1/messages".to_string(),
                is_chat: true,
            }
        );

        // Antigravity chat vs tab-autocomplete
        assert_eq!(
            classify_mitm_request(
                "cloudcode-pa.googleapis.com",
                "/v1/models/gemini-3.8-flash:streamGenerateContent",
                &Method::POST,
                b"{}"
            ),
            RouteDecision::ForwardToRouter {
                target_path: "/v1/chat/completions".to_string(),
                is_chat: true,
            }
        );

        assert_eq!(
            classify_mitm_request(
                "cloudcode-pa.googleapis.com",
                "/v1/models/tab_flash_lite:streamGenerateContent",
                &Method::POST,
                b"{}"
            ),
            RouteDecision::PassthroughUpstream {
                host: "cloudcode-pa.googleapis.com".to_string(),
                path_and_query: "/v1/models/tab_flash_lite:streamGenerateContent".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn test_handle_request_parts_health() {
        let mitm_dir = std::env::temp_dir().join(format!("mitm-srv-{}", std::process::id()));
        let ca = CertificateAuthority::generate(&mitm_dir).unwrap();
        let config = MitmConfig {
            port: 8443,
            router_base: "http://127.0.0.1:20127".to_string(),
            router_api_key: None,
            mitm_dir: mitm_dir.clone(),
        };
        let server = MitmServer::new(config, Arc::new(ca));

        let req = Request::builder()
            .uri("/_mitm_health")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();

        let resp = server.handle_request_parts(parts, Bytes::new()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let _ = std::fs::remove_dir_all(&mitm_dir);
    }
}
