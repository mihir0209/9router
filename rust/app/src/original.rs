use std::ffi::OsString;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const RUN: &[&str] = &["node", "custom-server.js"];

fn resolve(word: &str, port: u16) -> OsString {
    let word = word.replace("{port}", &port.to_string());
    if word.starts_with('-') {
        return word.into();
    }
    let path = Path::new(ROOT).join(&word);
    let looks_like_path = word.contains('/') || word.contains('\\') || path.is_file();
    if looks_like_path && path.exists() {
        return std::path::absolute(&path).unwrap_or(path).into_os_string();
    }
    word.into()
}

/// The original Next.js server, started on a free port of its own.
#[derive(Clone)]
pub struct Original {
    pub base: String,
    pub client: reqwest::Client,
    pub child: Arc<Mutex<Option<Child>>>,
}

impl Original {
    pub fn empty() -> Self {
        Original {
            base: String::new(),
            client: reqwest::Client::new(),
            child: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_running(&self) -> bool {
        !self.base.is_empty()
    }

    pub async fn start() -> Self {
        let disabled = std::env::var("STANDALONE").map(|v| v == "1" || v == "true").unwrap_or(false)
            || std::env::var("NO_NODE").map(|v| v == "1" || v == "true").unwrap_or(false);

        if disabled {
            tracing::info!("Running in standalone single-binary mode (embedded UI active, Node.js disabled)");
            return Self::empty();
        }

        let root_path = Path::new(ROOT);
        let server_script = root_path.join("custom-server.js");
        if !server_script.exists() {
            tracing::info!("custom-server.js not found at {}; running in standalone embedded UI mode", server_script.display());
            return Self::empty();
        }

        let port = match TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()) {
            Ok(a) => a.port(),
            Err(e) => {
                tracing::warn!("Failed to bind free port for original server ({e}); using embedded UI fallback");
                return Self::empty();
            }
        };

        let (program, rest) = RUN.split_first().expect("the original's command");
        let spawn_res = Command::new(resolve(program, port))
            .args(rest.iter().map(|w| resolve(w, port)))
            .args(std::env::args_os().skip(1))
            .current_dir(ROOT)
            .env("PORT", port.to_string())
            .stdin(Stdio::null())
            .spawn();

        let mut child = match spawn_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to spawn child process ({e}); running in standalone embedded UI mode");
                return Self::empty();
            }
        };

        let deadline = Instant::now() + Duration::from_secs(45);
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            if let Ok(Some(status)) = child.try_wait() {
                tracing::warn!("Child process exited ({status}) before listening; using embedded UI mode");
                return Self::empty();
            }
            if Instant::now() >= deadline {
                tracing::warn!("Child process did not listen within timeout; using embedded UI mode");
                let _ = child.kill();
                return Self::empty();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Original {
            base: format!("http://127.0.0.1:{port}"),
            client,
            child: Arc::new(Mutex::new(Some(child))),
        }
    }

    pub fn stop(&self) {
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Original {
    fn drop(&mut self) {
        // Only kill if this is the last reference
        if Arc::strong_count(&self.child) == 1 {
            self.stop();
        }
    }
}

fn sanitize_location(loc: &str, base: &str) -> String {
    if let Some(stripped) = loc.strip_prefix(base) {
        if stripped.is_empty() {
            "/".to_string()
        } else {
            stripped.to_string()
        }
    } else if let Ok(url) = reqwest::Url::parse(loc) {
        if (url.host_str() == Some("127.0.0.1") || url.host_str() == Some("localhost"))
            && url.port() == reqwest::Url::parse(base).ok().and_then(|u| u.port())
        {
            let mut rel = url.path().to_string();
            if let Some(query) = url.query() {
                rel.push('?');
                rel.push_str(query);
            }
            if rel.is_empty() {
                "/".to_string()
            } else {
                rel
            }
        } else {
            loc.to_string()
        }
    } else {
        loc.to_string()
    }
}

/// Reverse proxies incoming requests to the original Next.js instance with location sanitization.
/// When running in standalone mode or when original is inactive, serves embedded assets directly.
pub async fn proxy(State(state): State<crate::routes::AppState>, req: Request) -> Response {
    let original = &state.original;
    let (parts, body) = req.into_parts();

    let prefer_embedded = original.base.is_empty()
        || std::env::var("EMBED_UI").map(|v| v == "1" || v == "true").unwrap_or(false);

    let raw_path = parts.uri.path();
    if (prefer_embedded && !raw_path.starts_with("/api/"))
        || raw_path.starts_with("/assets/")
        || raw_path.starts_with("/icons/")
        || raw_path == "/favicon.ico"
        || raw_path == "/favicon.svg"
        || raw_path == "/manifest.webmanifest"
    {
        return crate::embed::static_handler(parts.uri).await;
    }

    if original.base.is_empty() {
        return crate::embed::static_handler(parts.uri).await;
    }

    let target = parts
        .uri
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_string();

    let Ok(body) = axum::body::to_bytes(body, 128 * 1024 * 1024).await else {
        return (
            StatusCode::BAD_REQUEST,
            "front door: unreadable request body",
        )
            .into_response();
    };

    let mut request = original
        .client
        .request(parts.method, format!("{}{target}", original.base))
        .body(body);

    for (name, value) in &parts.headers {
        if name != header::HOST {
            request = request.header(name, value);
        }
    }
    if let Some(host) = parts.headers.get(header::HOST) {
        request = request.header("x-forwarded-host", host);
    }
    request = request.header("x-forwarded-proto", "http");

    match request.send().await {
        Ok(answer) => {
            let mut response = Response::builder().status(answer.status());
            for (name, value) in answer.headers() {
                if name != header::TRANSFER_ENCODING && name != header::CONNECTION {
                    if name == header::LOCATION {
                        let loc_str = value.to_str().unwrap_or("");
                        let clean = sanitize_location(loc_str, &original.base);
                        if let Ok(val) = HeaderValue::from_str(&clean) {
                            response = response.header(header::LOCATION, val);
                        }
                    } else {
                        response = response.header(name, value);
                    }
                }
            }
            let body = Body::from_stream(answer.bytes_stream());
            response
                .body(body)
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("front door: the original did not answer: {e}"),
        )
            .into_response(),
    }
}
