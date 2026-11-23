use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::routes::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareTunnelState {
    pub enabled: bool,
    pub running: bool,
    pub tunnel_url: String,
    pub short_id: String,
    pub public_url: String,
    pub reconnecting: bool,
    pub manual_disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleTunnelState {
    pub enabled: bool,
    pub running: bool,
    pub tunnel_url: String,
    pub logged_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelFullStatus {
    pub tunnel: CloudflareTunnelState,
    pub tailscale: TailscaleTunnelState,
    pub download: serde_json::Value,
}

pub struct TunnelManager {
    child: Arc<Mutex<Option<Child>>>,
    state: Arc<Mutex<CloudflareTunnelState>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(CloudflareTunnelState {
                enabled: false,
                running: false,
                tunnel_url: String::new(),
                short_id: String::new(),
                public_url: String::new(),
                reconnecting: false,
                manual_disabled: false,
            })),
        }
    }

    pub async fn get_status(&self) -> CloudflareTunnelState {
        let mut s = self.state.lock().await.clone();
        let mut ch = self.child.lock().await;
        if let Some(child) = ch.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *ch = None;
                    s.running = false;
                    s.enabled = false;
                }
                Ok(None) => {
                    s.running = true;
                }
                Err(_) => {
                    s.running = false;
                }
            }
        } else {
            s.running = false;
        }
        s
    }

    pub async fn enable(&self, port: u16) -> Result<CloudflareTunnelState, String> {
        let mut ch = self.child.lock().await;
        if let Some(child) = ch.as_mut() {
            if let Ok(None) = child.try_wait() {
                let st = self.state.lock().await.clone();
                return Ok(st);
            }
        }

        let mut cmd = Command::new("cloudflared");
        cmd.arg("tunnel")
            .arg("--url")
            .arg(format!("http://127.0.0.1:{}", port))
            .arg("--no-autoupdate")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("Failed to spawn cloudflared: {}. Ensure cloudflared is installed.", e));
            }
        };

        let stderr = child.stderr.take().ok_or("Failed to capture cloudflared stderr")?;
        let mut reader = BufReader::new(stderr).lines();

        let mut found_url = None;
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(15));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                line_res = reader.next_line() => {
                    match line_res {
                        Ok(Some(line)) => {
                            if line.contains("trycloudflare.com") {
                                for word in line.split_whitespace() {
                                    if word.starts_with("https://") && word.contains(".trycloudflare.com") {
                                        found_url = Some(word.trim().to_string());
                                        break;
                                    }
                                }
                                if found_url.is_some() {
                                    break;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                _ = &mut timeout => {
                    break;
                }
            }
        }

        let tunnel_url = match found_url {
            Some(u) => u,
            None => {
                let _ = child.kill().await;
                return Err("Failed to obtain trycloudflare.com tunnel URL within timeout".to_string());
            }
        };

        // Extract short id or hash
        let short_id = tunnel_url
            .strip_prefix("https://")
            .and_then(|u| u.split('.').next())
            .unwrap_or("cf")
            .to_string();

        let public_url = tunnel_url.clone();

        let new_state = CloudflareTunnelState {
            enabled: true,
            running: true,
            tunnel_url: tunnel_url.clone(),
            short_id: short_id.clone(),
            public_url: public_url.clone(),
            reconnecting: false,
            manual_disabled: false,
        };

        *ch = Some(child);
        let mut st = self.state.lock().await;
        *st = new_state.clone();

        Ok(new_state)
    }

    pub async fn disable(&self) -> Result<(), String> {
        let mut ch = self.child.lock().await;
        if let Some(mut child) = ch.take() {
            let _ = child.kill().await;
        }

        let mut st = self.state.lock().await;
        st.enabled = false;
        st.running = false;
        st.tunnel_url.clear();
        st.manual_disabled = true;

        Ok(())
    }
}

// Global tunnel manager lazy instance
static TUNNEL_MGR: std::sync::LazyLock<TunnelManager> = std::sync::LazyLock::new(TunnelManager::new);

pub async fn tunnel_status_handler(State(_state): State<AppState>) -> Response {
    let cf_status = TUNNEL_MGR.get_status().await;

    // Check tailscale status via CLI probe
    let tailscale_installed = which_bin("tailscale").is_some();
    let tailscale_status = TailscaleTunnelState {
        enabled: false,
        running: false,
        tunnel_url: String::new(),
        logged_in: false,
    };

    let full = json!({
        "tunnel": cf_status,
        "tailscale": tailscale_status,
        "download": {
            "active": false,
            "installed": tailscale_installed
        }
    });

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(full),
    ).into_response()
}

pub async fn tunnel_enable_handler(State(state): State<AppState>) -> Response {
    let port = state.config.server.port;
    match TUNNEL_MGR.enable(port).await {
        Ok(st) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({
                "success": true,
                "tunnelUrl": st.tunnel_url,
                "shortId": st.short_id,
                "publicUrl": st.public_url
            })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({ "error": e })),
        ).into_response(),
    }
}

pub async fn tunnel_disable_handler(State(_state): State<AppState>) -> Response {
    match TUNNEL_MGR.disable().await {
        Ok(_) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({ "success": true })),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({ "error": e })),
        ).into_response(),
    }
}

pub async fn tailscale_check_handler(State(_state): State<AppState>) -> Response {
    let installed = which_bin("tailscale").is_some();
    let resp = json!({
        "installed": installed,
        "loggedIn": false,
        "platform": "linux",
        "brewAvailable": false,
        "daemonRunning": false,
        "customDaemonRunning": false,
        "systemDaemonRunning": false,
        "hasCachedPassword": false
    });

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(resp),
    ).into_response()
}

pub async fn tailscale_enable_handler(State(_state): State<AppState>) -> Response {
    let installed = which_bin("tailscale").is_some();
    if !installed {
        return (
            StatusCode::BAD_REQUEST,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            Json(json!({ "error": "Tailscale is not installed on the system." })),
        ).into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "success": false, "needsLogin": true, "authUrl": "" })),
    ).into_response()
}

pub async fn tailscale_disable_handler(State(_state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "success": true })),
    ).into_response()
}

pub async fn tailscale_install_handler(State(_state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({ "status": "info", "message": "Run curl -fsSL https://tailscale.com/install.sh | sh to install Tailscale." })),
    ).into_response()
}

fn which_bin(name: &str) -> Option<std::path::PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let p = std::path::Path::new(dir).join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloudflare_tunnel_state_serialization() {
        let state = CloudflareTunnelState {
            enabled: true,
            running: true,
            tunnel_url: "https://test-subdomain.trycloudflare.com".to_string(),
            short_id: "test-subdomain".to_string(),
            public_url: "https://rtest-subdomain.abc-tunnel.us".to_string(),
            reconnecting: false,
            manual_disabled: false,
        };

        let val = serde_json::to_value(&state).expect("serialize tunnel state");
        assert_eq!(val["enabled"], true);
        assert_eq!(val["running"], true);
        assert_eq!(val["tunnelUrl"], "https://test-subdomain.trycloudflare.com");
        assert_eq!(val["shortId"], "test-subdomain");
        assert_eq!(val["publicUrl"], "https://rtest-subdomain.abc-tunnel.us");
    }

    #[test]
    fn test_which_bin_finds_standard_binary() {
        let sh = which_bin("sh");
        assert!(sh.is_some(), "should locate /bin/sh or similar in PATH");
    }
}
