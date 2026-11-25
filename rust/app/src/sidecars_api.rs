use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Local, TimeZone, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::process::{ChildStdin, Command as TokioCommand};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::auth::verify_internal_auth;
use crate::cli::daemon::DaemonManager;
use crate::proxy_pools::generate_uuid;
use crate::routes::{options_handler, AppState};

// ============================================================================
// Shared Constants & Helpers
// ============================================================================

const DEFAULT_HEADROOM_URL_FALLBACK: &str = "http://localhost:8787";
const HEADROOM_DEFAULT_PORT: u16 = 8787;
const HEADROOM_STARTUP_TIMEOUT_MS: u64 = 8000;
const HEADROOM_HEALTH_TIMEOUT_MS: u64 = 1500;
const HEADROOM_PIP_TIMEOUT_MS: u64 = 8000;
const HEADROOM_COMPRESSION_EXTRAS: [&str; 2] = ["code", "ml"];
const HEADROOM_DASHBOARD_PREFIX: &str = "/api/headroom/proxy";

const PXPIPE_PACKAGE: &str = "pxpipe-proxy";
const PXPIPE_INSTALL_TIMEOUT_SECS: u64 = 300;
const PXPIPE_MAX_EVENTS_BYTES: u64 = 5 * 1024 * 1024;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const HOP_BY_HOP_HEADERS: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn default_headroom_url() -> String {
    std::env::var("HEADROOM_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HEADROOM_URL_FALLBACK.to_string())
}

fn extended_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            for py in ["Python313", "Python312", "Python311", "Python310"] {
                dirs.push(PathBuf::from(&local).join("Programs").join("Python").join(py).join("Scripts"));
            }
        }
        if let Ok(app) = std::env::var("APPDATA") {
            dirs.push(PathBuf::from(&app).join("Python").join("Python313").join("Scripts"));
            dirs.push(PathBuf::from(&app).join("npm"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            dirs.push(PathBuf::from(&pf).join("nodejs"));
        }
    }
    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        for ver in ["3.13", "3.12", "3.11", "3.10"] {
            dirs.push(PathBuf::from(format!(
                "/Library/Frameworks/Python.framework/Versions/{}/bin",
                ver
            )));
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                dirs.push(PathBuf::from(home).join(".local").join("bin"));
            }
        }
        dirs.push(PathBuf::from("/usr/bin"));
        dirs.push(PathBuf::from("/bin"));
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for p in std::env::split_paths(&path_var) {
            if !p.as_os_str().is_empty() && !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    dirs
}

fn extended_path_env() -> std::ffi::OsString {
    std::env::join_paths(extended_bin_dirs()).unwrap_or_default()
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_binary_in_dirs(names: &[&str], dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        for name in names {
            let candidate = dir.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn json_error(status: StatusCode, message: impl Into<String>, code: Option<&str>) -> Response {
    let body = match code {
        Some(c) => json!({ "error": message.into(), "code": c }),
        None => json!({ "error": message.into(), "code": Value::Null }),
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(body),
    )
        .into_response()
}

fn json_error_simple(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

fn read_log_tail(path: &Path, max_lines: usize) -> String {
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn run_sync_with_timeout(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", extended_path_env());

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("command timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn kill_pid_graceful(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-15", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn kill_pid_force(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

// ============================================================================
// Headroom Sidecar Management (/api/headroom/*)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomExtrasFlags {
    pub code: bool,
    pub ml: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadroomExtrasStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub extras: HeadroomExtrasFlags,
}

impl Default for HeadroomExtrasStatus {
    fn default() -> Self {
        Self {
            installed: false,
            version: None,
            extras: HeadroomExtrasFlags::default(),
        }
    }
}

#[derive(Debug)]
struct SidecarError {
    status: StatusCode,
    code: Option<&'static str>,
    message: String,
}

impl SidecarError {
    fn new(status: StatusCode, code: Option<&'static str>, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn into_response(self) -> Response {
        json_error(self.status, self.message, self.code)
    }
}

fn headroom_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("headroom")
}

fn headroom_pid_file(dir: &Path) -> PathBuf {
    dir.join("proxy.pid")
}

fn headroom_log_file(dir: &Path) -> PathBuf {
    dir.join("proxy.log")
}

fn headroom_install_log_file(dir: &Path) -> PathBuf {
    dir.join("install.log")
}

fn read_managed_pid(dir: &Path) -> Option<u32> {
    let content = fs::read_to_string(headroom_pid_file(dir)).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    if pid > 0 && DaemonManager::is_pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

fn write_managed_pid(dir: &Path, pid: u32) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(headroom_pid_file(dir), pid.to_string())
}

fn clear_managed_pid(dir: &Path) {
    let _ = fs::remove_file(headroom_pid_file(dir));
}

fn parse_port_from_url(raw: &str) -> Option<u16> {
    url::Url::parse(raw).ok()?.port()
}

pub fn is_loopback_headroom_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let clean = host.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    matches!(clean.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

fn find_headroom_binary() -> Option<String> {
    #[cfg(windows)]
    let names = ["headroom.exe", "headroom.cmd", "headroom"];
    #[cfg(not(windows))]
    let names = ["headroom"];

    find_binary_in_dirs(&names, &extended_bin_dirs())
        .map(|p| p.to_string_lossy().to_string())
}

fn parse_python_version(output: &str) -> Option<(u32, u32)> {
    for token in output.split_whitespace() {
        let mut parts = token.split('.');
        if let (Some(maj_s), Some(min_s)) = (parts.next(), parts.next()) {
            if let (Ok(maj), Ok(min)) = (maj_s.parse::<u32>(), min_s.parse::<u32>()) {
                return Some((maj, min));
            }
        }
    }
    None
}

fn find_python310_blocking() -> Option<String> {
    let dirs = extended_bin_dirs();
    let py_names: &[&str] = &[
        "python3.13",
        "python3.12",
        "python3.11",
        "python3.10",
        "python3",
        "python",
    ];
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    if let Some(bin) = find_headroom_binary() {
        if let Some(parent) = Path::new(&bin).parent() {
            #[cfg(windows)]
            let local_names = ["python.exe", "python3.exe"];
            #[cfg(not(windows))]
            let local_names = ["python3", "python3.13", "python"];
            for name in local_names {
                let p = parent.join(name);
                if is_executable_file(&p) && seen.insert(p.clone()) {
                    candidates.push(p);
                }
            }
        }
    }

    for dir in &dirs {
        for name in py_names {
            #[cfg(windows)]
            let p = dir.join(format!("{}.exe", name));
            #[cfg(not(windows))]
            let p = dir.join(name);
            if is_executable_file(&p) && seen.insert(p.clone()) {
                candidates.push(p);
            }
        }
    }

    let mut fallback: Option<String> = None;
    for candidate in candidates {
        let Ok(ver_out) = run_sync_with_timeout(&candidate, &["--version"], Duration::from_secs(2))
        else {
            continue;
        };
        if !ver_out.status.success() {
            continue;
        }
        let ver_text = format!(
            "{} {}",
            String::from_utf8_lossy(&ver_out.stdout),
            String::from_utf8_lossy(&ver_out.stderr)
        );
        let Some((major, minor)) = parse_python_version(&ver_text) else {
            continue;
        };
        if !(major > 3 || (major == 3 && minor >= 10)) {
            continue;
        }
        let cand_str = candidate.to_string_lossy().to_string();
        if fallback.is_none() {
            fallback = Some(cand_str.clone());
        }
        if let Ok(pip_out) = run_sync_with_timeout(
            &candidate,
            &["-m", "pip", "show", "headroom-ai"],
            Duration::from_millis(HEADROOM_PIP_TIMEOUT_MS),
        ) {
            if pip_out.status.success() {
                return Some(cand_str);
            }
        }
    }

    fallback
}

fn parse_pip_list_extras(pip_list_json: &str) -> HeadroomExtrasStatus {
    let Ok(packages) = serde_json::from_str::<Vec<Value>>(pip_list_json) else {
        return HeadroomExtrasStatus::default();
    };
    let mut names: HashSet<String> = HashSet::new();
    let mut version: Option<String> = None;
    for pkg in &packages {
        if let Some(name) = pkg.get("name").and_then(|v| v.as_str()) {
            let lower = name.to_ascii_lowercase();
            if lower == "headroom-ai" {
                version = pkg
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            names.insert(lower);
        }
    }
    if !names.contains("headroom-ai") {
        return HeadroomExtrasStatus::default();
    }
    let code = ["tree-sitter", "tree-sitter-language-pack"]
        .iter()
        .any(|m| names.contains(*m));
    let ml = ["torch", "huggingface-hub"]
        .iter()
        .any(|m| names.contains(*m));

    HeadroomExtrasStatus {
        installed: true,
        version,
        extras: HeadroomExtrasFlags { code, ml },
    }
}

fn get_installed_headroom_extras_blocking(python: Option<&str>) -> HeadroomExtrasStatus {
    let resolved;
    let py = match python {
        Some(p) => p,
        None => {
            resolved = find_python310_blocking();
            match resolved.as_deref() {
                Some(p) => p,
                None => return HeadroomExtrasStatus::default(),
            }
        }
    };
    let Ok(out) = run_sync_with_timeout(
        Path::new(py),
        &["-m", "pip", "list", "--format=json", "--disable-pip-version-check"],
        Duration::from_millis(HEADROOM_PIP_TIMEOUT_MS),
    ) else {
        return HeadroomExtrasStatus::default();
    };
    if !out.status.success() {
        return HeadroomExtrasStatus::default();
    }
    parse_pip_list_extras(&String::from_utf8_lossy(&out.stdout))
}

async fn probe_proxy_running(url: &str) -> bool {
    let base = url.trim().trim_end_matches('/');
    if base.is_empty() {
        return false;
    }
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(HEADROOM_HEALTH_TIMEOUT_MS))
        .build()
    else {
        return false;
    };
    match client.get(format!("{}/health", base)).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

fn headroom_proxy_args(port: u16, code_aware: bool, kompress: bool) -> Vec<String> {
    let mut args = vec!["proxy".to_string(), "--port".to_string(), port.to_string()];
    if code_aware {
        args.push("--code-aware".to_string());
    }
    if !kompress {
        args.push("--disable-kompress".to_string());
    }
    args
}

async fn start_headroom_proxy_with_binary(
    dir: &Path,
    binary: Option<String>,
    port: u16,
    code_aware: bool,
    kompress: bool,
) -> Result<Value, SidecarError> {
    let binary = binary.ok_or_else(|| {
        SidecarError::new(
            StatusCode::BAD_REQUEST,
            Some("NOT_INSTALLED"),
            "Headroom CLI not installed",
        )
    })?;

    if let Some(existing) = read_managed_pid(dir) {
        return Ok(json!({
            "success": true,
            "pid": existing,
            "alreadyRunning": true
        }));
    }

    fs::create_dir_all(dir).map_err(|e| {
        SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("MKDIR_FAILED"),
            format!("Failed to create headroom directory: {}", e),
        )
    })?;

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(headroom_log_file(dir))
        .map_err(|e| {
            SidecarError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("LOG_OPEN_FAILED"),
                format!("Failed to open headroom proxy.log: {}", e),
            )
        })?;
    let err_file = log_file.try_clone().map_err(|e| {
        SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("LOG_OPEN_FAILED"),
            format!("Failed to clone headroom log fd: {}", e),
        )
    })?;

    let safe_port = if port > 0 { port } else { HEADROOM_DEFAULT_PORT };
    let args = headroom_proxy_args(safe_port, code_aware, kompress);

    let mut cmd = TokioCommand::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file))
        .env("PATH", extended_path_env());

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            // Detach into its own session so SIGHUP on terminal does not kill sidecar.
            // Uses libc setsid via std or ignored if unavailable; standard Command doesn't expose setsid directly without libc,
            // so we keep kill_on_drop(false).
            Ok(())
        });
    }
    cmd.kill_on_drop(false);

    let mut child = cmd.spawn().map_err(|e| {
        SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("SPAWN_FAILED"),
            format!("Failed to spawn headroom proxy: {}", e),
        )
    })?;

    let pid = child.id().ok_or_else(|| {
        SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("SPAWN_FAILED"),
            "Failed to spawn headroom proxy",
        )
    })?;

    if let Err(e) = write_managed_pid(dir, pid) {
        let _ = child.start_kill();
        return Err(SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("PID_WRITE_FAILED"),
            format!("Failed to write headroom PID file: {}", e),
        ));
    }

    let sleep = tokio::time::sleep(Duration::from_millis(HEADROOM_STARTUP_TIMEOUT_MS));
    tokio::pin!(sleep);

    tokio::select! {
        exit_res = child.wait() => {
            clear_managed_pid(dir);
            let code_str = match exit_res {
                Ok(st) => st.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string()),
                Err(_) => "unknown".to_string(),
            };
            return Err(SidecarError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("EARLY_EXIT"),
                format!("headroom proxy exited early (code={}) — see proxy.log", code_str),
            ));
        }
        _ = &mut sleep => {
            // Still alive after startup window; reap asynchronously when it later exits.
            let dir_owned = dir.to_path_buf();
            tokio::spawn(async move {
                let _ = child.wait().await;
                clear_managed_pid(&dir_owned);
            });
            if DaemonManager::is_pid_alive(pid) {
                Ok(json!({
                    "success": true,
                    "pid": pid,
                    "alreadyRunning": false
                }))
            } else {
                clear_managed_pid(dir);
                Err(SidecarError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Some("EARLY_EXIT"),
                    "headroom proxy exited during startup — see proxy.log",
                ))
            }
        }
    }
}

fn stop_headroom_proxy_in_dir(dir: &Path) -> Result<(StatusCode, Value), SidecarError> {
    let Some(pid) = read_managed_pid(dir) else {
        return Ok((
            StatusCode::CONFLICT,
            json!({ "stopped": false, "reason": "not_running" }),
        ));
    };
    kill_pid_graceful(pid);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000)).await;
        if DaemonManager::is_pid_alive(pid) {
            kill_pid_force(pid);
        }
    });
    clear_managed_pid(dir);
    Ok((StatusCode::OK, json!({ "stopped": true, "pid": pid })))
}

async fn restart_headroom_proxy_in_dir(
    dir: &Path,
    binary: Option<String>,
    port: u16,
    code_aware: bool,
    kompress: bool,
) -> Result<Value, SidecarError> {
    if let Some(pid) = read_managed_pid(dir) {
        kill_pid_graceful(pid);
        for _ in 0..30 {
            if !DaemonManager::is_pid_alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if DaemonManager::is_pid_alive(pid) {
            kill_pid_force(pid);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        clear_managed_pid(dir);
    }
    start_headroom_proxy_with_binary(dir, binary, port, code_aware, kompress).await
}

pub async fn headroom_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or_default();
    let url = settings
        .get("headroomUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_headroom_url);

    let probe_fut = probe_proxy_running(&url);
    let detect_fut = tokio::task::spawn_blocking(move || {
        let bin = find_headroom_binary();
        let py = find_python310_blocking();
        let installed = bin.is_some();
        let extras_status = if installed {
            get_installed_headroom_extras_blocking(py.as_deref())
        } else {
            HeadroomExtrasStatus::default()
        };
        (bin, py, installed, extras_status)
    });

    let (running, detect_res) = tokio::join!(probe_fut, detect_fut);
    let (bin, py, installed, extras_status) = match detect_res {
        Ok(v) => v,
        Err(e) => {
            return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
    };

    let local_url = is_loopback_headroom_url(&url);
    let can_start = installed && local_url;
    let managed_pid = read_managed_pid(&headroom_dir(&state.config.data_dir));

    Json(json!({
        "installed": installed,
        "path": bin,
        "running": running,
        "python": py,
        "localUrl": local_url,
        "canStart": can_start,
        "version": extras_status.version,
        "extras": extras_status.extras,
        "url": url,
        "managedPid": managed_pid,
    }))
    .into_response()
}

pub async fn headroom_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or_default();
    let url = settings
        .get("headroomUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_headroom_url);

    if !is_loopback_headroom_url(&url) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "External Headroom proxies must be started outside 9Router",
            Some("EXTERNAL_PROXY"),
        );
    }

    let port = parse_port_from_url(&url).unwrap_or(HEADROOM_DEFAULT_PORT);
    let code_aware = settings.get("headroomCodeAware").and_then(|v| v.as_bool()) == Some(true);
    let kompress = settings.get("headroomKompress").and_then(|v| v.as_bool()) != Some(false);
    let dir = headroom_dir(&state.config.data_dir);
    let binary = tokio::task::spawn_blocking(find_headroom_binary)
        .await
        .ok()
        .flatten();

    match start_headroom_proxy_with_binary(&dir, binary, port, code_aware, kompress).await {
        Ok(val) => Json(val).into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn headroom_stop_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = headroom_dir(&state.config.data_dir);
    match stop_headroom_proxy_in_dir(&dir) {
        Ok((status, val)) => (status, Json(val)).into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn headroom_restart_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or_default();
    let url = settings
        .get("headroomUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_headroom_url);

    if !is_loopback_headroom_url(&url) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "External Headroom proxies must be started outside 9Router",
            Some("EXTERNAL_PROXY"),
        );
    }

    let port = parse_port_from_url(&url).unwrap_or(HEADROOM_DEFAULT_PORT);
    let code_aware = settings.get("headroomCodeAware").and_then(|v| v.as_bool()) == Some(true);
    let kompress = settings.get("headroomKompress").and_then(|v| v.as_bool()) != Some(false);
    let dir = headroom_dir(&state.config.data_dir);
    let binary = tokio::task::spawn_blocking(find_headroom_binary)
        .await
        .ok()
        .flatten();

    match restart_headroom_proxy_in_dir(&dir, binary, port, code_aware, kompress).await {
        Ok(val) => Json(val).into_response(),
        Err(err) => err.into_response(),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct HeadroomExtrasQuery {
    pub log: Option<String>,
}

pub async fn headroom_extras_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HeadroomExtrasQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = headroom_dir(&state.config.data_dir);
    if query.log.as_deref() == Some("1") {
        let log_tail = read_log_tail(&headroom_install_log_file(&dir), 15);
        return Json(json!({ "log": log_tail })).into_response();
    }

    match tokio::task::spawn_blocking(move || {
        let py = find_python310_blocking();
        get_installed_headroom_extras_blocking(py.as_deref())
    })
    .await
    {
        Ok(status) => Json(json!({
            "available": HEADROOM_COMPRESSION_EXTRAS,
            "installed": status.installed,
            "version": status.version,
            "extras": status.extras,
        }))
        .into_response(),
        Err(e) => json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn filter_headroom_extras(body: &Value) -> Vec<String> {
    body.get("extras")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str())
                .filter(|s| HEADROOM_COMPRESSION_EXTRAS.contains(s))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub async fn headroom_extras_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| json!({}));
    let requested = filter_headroom_extras(&body);
    let dir = headroom_dir(&state.config.data_dir);

    let (py, has_bin) = match tokio::task::spawn_blocking(|| {
        (find_python310_blocking(), find_headroom_binary().is_some())
    })
    .await
    {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };

    let Some(py) = py else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "Python >= 3.10 not found",
            Some("NO_PYTHON"),
        );
    };
    if !has_bin {
        return json_error(
            StatusCode::BAD_REQUEST,
            "headroom-ai not installed (run `pip install headroom-ai[proxy]` first)",
            Some("NOT_INSTALLED"),
        );
    }

    let mut extras_list = vec!["proxy".to_string()];
    extras_list.extend(requested.iter().cloned());
    let spec = format!("headroom-ai[{}]", extras_list.join(","));

    if let Err(e) = fs::create_dir_all(&dir) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None);
    }
    let log_file = match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(headroom_install_log_file(&dir))
    {
        Ok(f) => f,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };
    let err_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };

    let status_res = TokioCommand::new(&py)
        .args(["-m", "pip", "install", "--upgrade", &spec])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file))
        .env("PATH", extended_path_env())
        .status()
        .await;

    match status_res {
        Ok(st) if st.success() => {
            let py_clone = py.clone();
            let installed_status = tokio::task::spawn_blocking(move || {
                get_installed_headroom_extras_blocking(Some(&py_clone))
            })
            .await
            .unwrap_or_default();
            Json(json!({
                "success": true,
                "code": 0,
                "spec": spec,
                "installed": installed_status.installed,
                "version": installed_status.version,
                "extras": installed_status.extras,
            }))
            .into_response()
        }
        Ok(st) => {
            let code_val = st.code().unwrap_or(-1);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("pip install exited with code={} — see headroom/install.log", code_val),
                Some("INSTALL_FAILED"),
            )
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    }
}

pub async fn headroom_extras_delete_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| json!({}));
    let requested = filter_headroom_extras(&body);
    let dir = headroom_dir(&state.config.data_dir);

    let py = match tokio::task::spawn_blocking(find_python310_blocking).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "Python >= 3.10 not found",
                Some("NO_PYTHON"),
            );
        }
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };

    let mut pkgs: Vec<&'static str> = Vec::new();
    for extra in &requested {
        let markers: &[&str] = match extra.as_str() {
            "code" => &["tree-sitter", "tree-sitter-language-pack"],
            "ml" => &["torch", "huggingface-hub"],
            _ => &[],
        };
        for m in markers {
            if !pkgs.contains(m) {
                pkgs.push(m);
            }
        }
    }

    if pkgs.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "No valid extras to remove",
            Some("INVALID_EXTRAS"),
        );
    }

    if let Err(e) = fs::create_dir_all(&dir) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None);
    }
    let log_file = match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(headroom_install_log_file(&dir))
    {
        Ok(f) => f,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };
    let err_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    };

    let mut args = vec!["-m", "pip", "uninstall", "-y"];
    args.extend(pkgs.iter().copied());

    let status_res = TokioCommand::new(&py)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file))
        .env("PATH", extended_path_env())
        .status()
        .await;

    match status_res {
        Ok(st) if st.success() => {
            let py_clone = py.clone();
            let installed_status = tokio::task::spawn_blocking(move || {
                get_installed_headroom_extras_blocking(Some(&py_clone))
            })
            .await
            .unwrap_or_default();
            Json(json!({
                "success": true,
                "code": 0,
                "removed": pkgs,
                "installed": installed_status.installed,
                "version": installed_status.version,
                "extras": installed_status.extras,
            }))
            .into_response()
        }
        Ok(st) => {
            let code_val = st.code().unwrap_or(-1);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("pip uninstall exited with code={} — see headroom/install.log", code_val),
                Some("UNINSTALL_FAILED"),
            )
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None),
    }
}

fn is_hop_by_hop(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

fn rewrite_dashboard_html(html: &str) -> String {
    const TARGETS: &[&str] = &[
        "/stats-history",
        "/stats",
        "/health",
        "/transformations/feed",
    ];
    let mut out = String::with_capacity(html.len() + 128);
    let mut rest = html;
    while let Some(idx) = rest.find("fetch('") {
        out.push_str(&rest[..idx + 7]);
        rest = &rest[idx + 7..];
        if TARGETS.iter().any(|t| rest.starts_with(t)) {
            out.push_str(HEADROOM_DASHBOARD_PREFIX);
        }
    }
    out.push_str(rest);
    out
}

fn build_headroom_proxy_target(
    base_url: &str,
    subpath: &str,
    query: Option<&str>,
) -> Result<url::Url, String> {
    let mut target = url::Url::parse(base_url).map_err(|e| e.to_string())?;
    if !matches!(target.scheme(), "http" | "https") {
        return Err("Headroom URL must use http or https".to_string());
    }
    let clean_sub = subpath.trim_start_matches('/');
    target.set_path(&format!("/{}", clean_sub));
    target.set_query(query);
    Ok(target)
}

async fn headroom_proxy_inner(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    subpath: String,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or_default();
    let base_url = settings
        .get("headroomUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_headroom_url);

    let target = match build_headroom_proxy_target(&base_url, &subpath, uri.query()) {
        Ok(u) => u,
        Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    let target_is_loopback = target
        .host_str()
        .map(|h| {
            let clean = h.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
            matches!(clean.as_str(), "localhost" | "127.0.0.1" | "::1")
        })
        .unwrap_or(false);

    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut req_builder = client.request(req_method, target.clone());

    let mut fwd_headers = reqwest::header::HeaderMap::new();
    for (name, value) in &headers {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) || lower == "host" {
            continue;
        }
        if !target_is_loopback && (lower == "cookie" || lower == "authorization") {
            continue;
        }
        if let (Ok(rname), Ok(rval)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            fwd_headers.append(rname, rval);
        }
    }
    req_builder = req_builder.headers(fwd_headers);

    if method != Method::GET && method != Method::HEAD && !body_bytes.is_empty() {
        req_builder = req_builder.body(body_bytes.to_vec());
    }

    let upstream = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);

    let mut resp_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) {
            continue;
        }
        if let (Ok(aname), Ok(aval)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            resp_headers.append(aname, aval);
        }
    }

    let is_dashboard = subpath.trim_matches('/') == "dashboard";
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if is_dashboard && content_type.contains("text/html") {
        resp_headers.remove(header::CONTENT_LENGTH);
        let text = match upstream.text().await {
            Ok(t) => t,
            Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let rewritten = rewrite_dashboard_html(&text);
        return (status, resp_headers, rewritten).into_response();
    }

    let stream = upstream.bytes_stream().map(|res| {
        res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    });
    (status, resp_headers, Body::from_stream(stream)).into_response()
}

pub async fn headroom_proxy_handler(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    headroom_proxy_inner(state, method, uri, headers, path, body_bytes).await
}

pub async fn headroom_proxy_root_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    headroom_proxy_inner(state, method, uri, headers, String::new(), body_bytes).await
}

// ============================================================================
// PXPIPE Sidecar Management (/api/pxpipe/*)
// ============================================================================

#[derive(Debug, Clone)]
struct PxpipeLoadedInfo {
    loaded_at_ms: i64,
}

#[derive(Debug, Default)]
struct PxpipeDirRuntime {
    loaded: Option<PxpipeLoadedInfo>,
    installing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PxpipeInstallInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

static PXPIPE_RUNTIME: LazyLock<Mutex<HashMap<PathBuf, PxpipeDirRuntime>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PXPIPE_INSTALL_MUTEX: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

fn pxpipe_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("pxpipe")
}

fn pxpipe_package_root(dir: &Path) -> PathBuf {
    dir.join("node_modules").join(PXPIPE_PACKAGE)
}

fn pxpipe_library_entry(dir: &Path) -> PathBuf {
    pxpipe_package_root(dir)
        .join("dist")
        .join("core")
        .join("library.js")
}

fn pxpipe_install_log(dir: &Path) -> PathBuf {
    dir.join("install.log")
}

fn pxpipe_events_file(dir: &Path) -> PathBuf {
    dir.join("events.jsonl")
}

fn pxpipe_rotated_events_file(dir: &Path) -> PathBuf {
    dir.join("events.jsonl.1")
}

fn find_npm_binary() -> Option<PathBuf> {
    #[cfg(windows)]
    let names = ["npm.cmd", "npm.exe", "npm"];
    #[cfg(not(windows))]
    let names = ["npm"];
    find_binary_in_dirs(&names, &extended_bin_dirs())
}

fn find_node_binary() -> Option<PathBuf> {
    #[cfg(windows)]
    let names = ["node.exe", "node"];
    #[cfg(not(windows))]
    let names = ["node"];
    find_binary_in_dirs(&names, &extended_bin_dirs())
}

pub fn get_pxpipe_install_info(dir: &Path) -> PxpipeInstallInfo {
    let pkg_root = pxpipe_package_root(dir);
    let pkg_json = pkg_root.join("package.json");
    let lib_entry = pxpipe_library_entry(dir);

    if !pkg_json.is_file() || !lib_entry.is_file() {
        return PxpipeInstallInfo {
            installed: false,
            version: None,
            path: None,
        };
    }

    let version = fs::read_to_string(&pkg_json)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(|s| s.to_string()));

    PxpipeInstallInfo {
        installed: true,
        version,
        path: Some(pkg_root.to_string_lossy().to_string()),
    }
}

fn get_pxpipe_loaded_info(dir: &Path) -> Option<PxpipeLoadedInfo> {
    let guard = PXPIPE_RUNTIME.lock().ok()?;
    guard.get(dir).and_then(|r| r.loaded.clone())
}

fn is_pxpipe_installing(dir: &Path) -> bool {
    PXPIPE_RUNTIME
        .lock()
        .ok()
        .and_then(|g| g.get(dir).map(|r| r.installing))
        .unwrap_or(false)
}

fn set_pxpipe_installing(dir: &Path, val: bool) {
    if let Ok(mut g) = PXPIPE_RUNTIME.lock() {
        g.entry(dir.to_path_buf()).or_default().installing = val;
    }
}

fn set_pxpipe_loaded(dir: &Path, info: PxpipeLoadedInfo) {
    if let Ok(mut g) = PXPIPE_RUNTIME.lock() {
        g.entry(dir.to_path_buf()).or_default().loaded = Some(info);
    }
}

fn unload_pxpipe_in_dir(dir: &Path) -> bool {
    let Ok(mut g) = PXPIPE_RUNTIME.lock() else {
        return false;
    };
    let entry = g.entry(dir.to_path_buf()).or_default();
    let was = entry.loaded.is_some();
    entry.loaded = None;
    was
}

fn get_pxpipe_status_json(dir: &Path) -> Value {
    let install = get_pxpipe_install_info(dir);
    let loaded = get_pxpipe_loaded_info(dir);
    let now_ms = Utc::now().timestamp_millis();
    let (running, loaded_at, uptime_ms) = match loaded {
        Some(info) => (
            true,
            Value::Number(info.loaded_at_ms.into()),
            (now_ms - info.loaded_at_ms).max(0),
        ),
        None => (false, Value::Null, 0),
    };
    json!({
        "installed": install.installed,
        "installing": is_pxpipe_installing(dir),
        "version": install.version,
        "path": install.path,
        "running": running,
        "loadedAt": loaded_at,
        "uptimeMs": uptime_ms,
        "npmAvailable": find_npm_binary().is_some(),
        "mode": "library",
    })
}

async fn verify_pxpipe_module_loads(dir: &Path) -> Result<PxpipeLoadedInfo, SidecarError> {
    if let Some(existing) = get_pxpipe_loaded_info(dir) {
        return Ok(existing);
    }
    let info = get_pxpipe_install_info(dir);
    if !info.installed {
        return Err(SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("NOT_INSTALLED"),
            "PXPIPE is not installed",
        ));
    }

    let entry_path = pxpipe_library_entry(dir);
    if let Some(node_bin) = find_node_binary() {
        let script = r#"
import { pathToFileURL } from "url";
const mod = await import(pathToFileURL(process.argv[1]).href);
if (typeof mod.transformAnthropicMessages !== "function") {
  console.error("installed pxpipe package does not export transformAnthropicMessages");
  process.exit(2);
}
"#;
        let out = TokioCommand::new(&node_bin)
            .args([
                "--input-type=module",
                "-e",
                script,
                &entry_path.to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PATH", extended_path_env())
            .output()
            .await
            .map_err(|e| {
                SidecarError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                    format!("Failed to verify pxpipe module: {}", e),
                )
            })?;

        if !out.status.success() {
            let err_msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(SidecarError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
                if err_msg.is_empty() {
                    "installed pxpipe package does not export transformAnthropicMessages".to_string()
                } else {
                    err_msg
                },
            ));
        }
    }

    let loaded = PxpipeLoadedInfo {
        loaded_at_ms: Utc::now().timestamp_millis(),
    };
    set_pxpipe_loaded(dir, loaded.clone());
    Ok(loaded)
}

async fn run_pxpipe_self_test(dir: &Path) -> Result<(String, u64), String> {
    let started = Instant::now();
    let entry_path = pxpipe_library_entry(dir);
    let node_bin = find_node_binary().ok_or_else(|| "node binary not found on PATH".to_string())?;
    let script = r#"
import { pathToFileURL } from "url";
const mod = await import(pathToFileURL(process.argv[1]).href);
const body = new TextEncoder().encode(JSON.stringify({
  model: "claude-fable-5",
  max_tokens: 16,
  messages: [{ role: "user", content: "ping" }]
}));
const result = await mod.transformAnthropicMessages({ body, model: "claude-fable-5" });
if (!result || typeof result.applied !== "boolean" || !(result.body instanceof Uint8Array)) {
  throw new Error("transform returned an unexpected shape");
}
console.log(JSON.stringify({ ok: true, reason: result.reason || "ok" }));
"#;
    let out = TokioCommand::new(&node_bin)
        .args([
            "--input-type=module",
            "-e",
            script,
            &entry_path.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", extended_path_env())
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("self-test exited with {}", out.status)
        } else {
            stderr
        });
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let reason = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "ok".to_string());

    Ok((reason, started.elapsed().as_millis() as u64))
}

async fn install_pxpipe_in_dir(dir: &Path) -> Result<PxpipeInstallInfo, SidecarError> {
    let _lock = PXPIPE_INSTALL_MUTEX.lock().await;
    let Some(npm_bin) = find_npm_binary() else {
        return Err(SidecarError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("NPM_NOT_FOUND"),
            "npm not found on PATH — Node.js/npm is required to install PXPIPE",
        ));
    };

    fs::create_dir_all(dir).map_err(|e| {
        SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
    })?;
    let pkg_json = dir.join("package.json");
    if !pkg_json.exists() {
        let init_pkg = serde_json::to_string_pretty(&json!({
            "name": "9router-pxpipe-host",
            "private": true
        }))
        .unwrap_or_else(|_| "{}".to_string());
        fs::write(&pkg_json, init_pkg).map_err(|e| {
            SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
        })?;
    }

    set_pxpipe_installing(dir, true);
    let res = async {
        let mut log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(pxpipe_install_log(dir))
            .map_err(|e| SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string()))?;
        let _ = writeln!(
            log_file,
            "\n[{}] npm install {}@latest",
            Utc::now().to_rfc3339(),
            PXPIPE_PACKAGE
        );
        let out_fd = log_file.try_clone().map_err(|e| {
            SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
        })?;
        let err_fd = log_file.try_clone().map_err(|e| {
            SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
        })?;

        let pkg_spec = format!("{}@latest", PXPIPE_PACKAGE);
        let mut child = TokioCommand::new(&npm_bin)
            .args(["install", &pkg_spec, "--no-audit", "--no-fund", "--omit=dev"])
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_fd))
            .stderr(Stdio::from(err_fd))
            .env("PATH", extended_path_env())
            .spawn()
            .map_err(|e| {
                SidecarError::new(StatusCode::INTERNAL_SERVER_ERROR, None, e.to_string())
            })?;

        let wait_res = tokio::time::timeout(
            Duration::from_secs(PXPIPE_INSTALL_TIMEOUT_SECS),
            child.wait(),
        )
        .await;

        match wait_res {
            Err(_) => {
                let _ = child.kill().await;
                return Err(SidecarError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                    "npm install timed out after 5 minutes — see install.log",
                ));
            }
            Ok(Err(e)) => {
                return Err(SidecarError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                    e.to_string(),
                ));
            }
            Ok(Ok(st)) if !st.success() => {
                let code = st.code().unwrap_or(-1);
                return Err(SidecarError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                    format!("npm install exited with code {} — see install.log", code),
                ));
            }
            Ok(Ok(_)) => {}
        }

        let info = get_pxpipe_install_info(dir);
        if !info.installed {
            return Err(SidecarError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
                "install finished but package is missing — see install.log",
            ));
        }
        Ok(info)
    }
    .await;

    set_pxpipe_installing(dir, false);
    res
}

async fn run_pxpipe_health_check(dir: &Path) -> Value {
    let mut checks = Vec::new();
    let install = get_pxpipe_install_info(dir);
    checks.push(json!({
        "id": "installed",
        "label": "PXPIPE installed",
        "ok": install.installed,
        "detail": install.version.as_ref().map(|v| format!("v{}", v)),
    }));
    if !install.installed {
        return json!({
            "healthy": false,
            "checks": checks,
            "error": "pxpipe not installed"
        });
    }

    match verify_pxpipe_module_loads(dir).await {
        Ok(_) => {
            checks.push(json!({
                "id": "module",
                "label": "Transform module loads",
                "ok": true,
                "detail": install.version.as_ref().map(|v| format!("v{}", v)),
            }));
        }
        Err(e) => {
            checks.push(json!({
                "id": "module",
                "label": "Transform module loads",
                "ok": false,
                "detail": e.message,
            }));
            return json!({
                "healthy": false,
                "checks": checks,
                "error": format!("Cannot load module: {}", e.message)
            });
        }
    }

    match run_pxpipe_self_test(dir).await {
        Ok((reason, duration_ms)) => {
            checks.push(json!({
                "id": "transform",
                "label": "Test request transforms",
                "ok": true,
                "detail": format!("{}ms ({})", duration_ms, reason),
            }));
            json!({
                "healthy": true,
                "checks": checks,
                "error": Value::Null
            })
        }
        Err(msg) => {
            checks.push(json!({
                "id": "transform",
                "label": "Test request transforms",
                "ok": false,
                "detail": msg,
            }));
            json!({
                "healthy": false,
                "checks": checks,
                "error": format!("Self-test failed: {}", msg)
            })
        }
    }
}

pub fn append_pxpipe_event(dir: &Path, event: &Value) {
    let _ = fs::create_dir_all(dir);
    let events_path = pxpipe_events_file(dir);
    if let Ok(meta) = fs::metadata(&events_path) {
        if meta.len() > PXPIPE_MAX_EVENTS_BYTES {
            let _ = fs::rename(&events_path, pxpipe_rotated_events_file(dir));
        }
    }
    let mut obj = match event.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    obj.entry("ts".to_string())
        .or_insert_with(|| json!(Utc::now().timestamp_millis()));
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)
    {
        if let Ok(line) = serde_json::to_string(&Value::Object(obj)) {
            let _ = writeln!(file, "{}", line);
        }
    }
}

fn read_pxpipe_events(dir: &Path, since_ms: Option<i64>, limit: Option<usize>) -> Vec<Value> {
    let mut events = Vec::new();
    for file in [pxpipe_rotated_events_file(dir), pxpipe_events_file(dir)] {
        let Ok(f) = fs::File::open(&file) else {
            continue;
        };
        let reader = BufReader::new(f);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let ts = ev.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(min_ts) = since_ms {
                if ts < min_ts {
                    continue;
                }
            }
            events.push(ev);
        }
    }
    events.sort_by_key(|ev| ev.get("ts").and_then(|v| v.as_i64()).unwrap_or(0));
    match limit {
        Some(lim) if events.len() > lim => events.split_off(events.len() - lim),
        _ => events,
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PxpipeWindowTotals {
    requests: u64,
    compressed: u64,
    bypassed: u64,
    errors: u64,
    tokens_before_est: u64,
    tokens_after_est: u64,
    tokens_saved_est: u64,
    saved_pct: f64,
    images_generated: u64,
    compression_time_ms: u64,
    avg_compression_ms: u64,
}

impl PxpipeWindowTotals {
    fn accumulate(&mut self, ev: &Value) {
        self.requests += 1;
        let applied = ev.get("applied").and_then(|v| v.as_bool()).unwrap_or(false);
        let reason = ev.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        if applied {
            self.compressed += 1;
            self.tokens_before_est += ev.get("tokensBeforeEst").and_then(|v| v.as_u64()).unwrap_or(0);
            self.tokens_after_est += ev.get("tokensAfterEst").and_then(|v| v.as_u64()).unwrap_or(0);
            self.tokens_saved_est += ev.get("tokensSavedEst").and_then(|v| v.as_u64()).unwrap_or(0);
            self.images_generated += ev.get("imageCount").and_then(|v| v.as_u64()).unwrap_or(0);
            self.compression_time_ms += ev.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0);
        } else if reason == "transform_error" || reason == "timeout" {
            self.errors += 1;
        } else {
            self.bypassed += 1;
        }
    }

    fn finalize(&mut self) {
        self.saved_pct = if self.tokens_before_est > 0 {
            ((self.tokens_saved_est as f64 / self.tokens_before_est as f64) * 10000.0).round()
                / 100.0
        } else {
            0.0
        };
        self.avg_compression_ms = if self.compressed > 0 {
            ((self.compression_time_ms as f64) / (self.compressed as f64)).round() as u64
        } else {
            0
        };
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PxpipeTimelineBucket {
    date: String,
    tokens_saved_est: u64,
    compressed: u64,
    requests: u64,
}

fn utc_date_key(ts_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ts_ms)
        .unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH)
        .format("%Y-%m-%d")
        .to_string()
}

fn local_start_of_today_ms(now_ms: i64) -> i64 {
    let dt = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_else(Local::now);
    dt.date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).single())
        .map(|local_dt| local_dt.timestamp_millis())
        .unwrap_or(now_ms)
}

fn compute_pxpipe_stats(dir: &Path, timeline_days: usize, recent_limit: usize) -> Value {
    let events = read_pxpipe_events(dir, None, None);
    let now = Utc::now().timestamp_millis();
    let start_of_today = local_start_of_today_ms(now);

    let mut all = PxpipeWindowTotals::default();
    let mut today = PxpipeWindowTotals::default();
    let mut yesterday = PxpipeWindowTotals::default();
    let mut last7d = PxpipeWindowTotals::default();
    let mut last30d = PxpipeWindowTotals::default();

    let mut timeline_keys: Vec<String> = Vec::with_capacity(timeline_days);
    let mut timeline_map: HashMap<String, PxpipeTimelineBucket> = HashMap::new();

    for i in (0..timeline_days).rev() {
        let day_ms = start_of_today - (i as i64) * DAY_MS;
        let key = utc_date_key(day_ms);
        timeline_keys.push(key.clone());
        timeline_map.insert(
            key.clone(),
            PxpipeTimelineBucket {
                date: key,
                tokens_saved_est: 0,
                compressed: 0,
                requests: 0,
            },
        );
    }

    for ev in &events {
        let ts = ev.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        all.accumulate(ev);
        if ts >= start_of_today {
            today.accumulate(ev);
        } else if ts >= start_of_today - DAY_MS {
            yesterday.accumulate(ev);
        }
        if ts >= now - 7 * DAY_MS {
            last7d.accumulate(ev);
        }
        if ts >= now - 30 * DAY_MS {
            last30d.accumulate(ev);
        }

        let key = utc_date_key(ts);
        if let Some(bucket) = timeline_map.get_mut(&key) {
            bucket.requests += 1;
            if ev.get("applied").and_then(|v| v.as_bool()).unwrap_or(false) {
                bucket.compressed += 1;
                bucket.tokens_saved_est +=
                    ev.get("tokensSavedEst").and_then(|v| v.as_u64()).unwrap_or(0);
            }
        }
    }

    all.finalize();
    today.finalize();
    yesterday.finalize();
    last7d.finalize();
    last30d.finalize();

    let timeline: Vec<PxpipeTimelineBucket> = timeline_keys
        .into_iter()
        .filter_map(|k| timeline_map.remove(&k))
        .collect();

    let mut recent = if events.len() > recent_limit {
        events[events.len() - recent_limit..].to_vec()
    } else {
        events
    };
    recent.reverse();

    json!({
        "windows": {
            "all": all,
            "today": today,
            "yesterday": yesterday,
            "last7d": last7d,
            "last30d": last30d,
        },
        "timeline": timeline,
        "recent": recent,
    })
}

pub async fn pxpipe_install_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = pxpipe_dir(&state.config.data_dir);
    let info = match install_pxpipe_in_dir(&dir).await {
        Ok(i) => i,
        Err(err) => return err.into_response(),
    };
    unload_pxpipe_in_dir(&dir);
    let health = run_pxpipe_health_check(&dir).await;

    Json(json!({
        "installed": info.installed,
        "version": info.version,
        "path": info.path,
        "health": health,
    }))
    .into_response()
}

pub async fn pxpipe_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = pxpipe_dir(&state.config.data_dir);
    if !get_pxpipe_install_info(&dir).installed {
        let settings = state.db.load_settings().unwrap_or_default();
        let auto_install = settings
            .get("pxpipeAutoInstall")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !auto_install {
            return json_error(
                StatusCode::CONFLICT,
                "PXPIPE is not installed",
                Some("NOT_INSTALLED"),
            );
        }
        if let Err(err) = install_pxpipe_in_dir(&dir).await {
            return err.into_response();
        }
    }

    if let Err(err) = verify_pxpipe_module_loads(&dir).await {
        return err.into_response();
    }

    Json(get_pxpipe_status_json(&dir)).into_response()
}

pub async fn pxpipe_stop_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = pxpipe_dir(&state.config.data_dir);
    let was_loaded = unload_pxpipe_in_dir(&dir);
    let mut status = get_pxpipe_status_json(&dir);
    if let Some(obj) = status.as_object_mut() {
        obj.insert("stopped".to_string(), json!(was_loaded));
    }
    Json(status).into_response()
}

pub async fn pxpipe_restart_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = pxpipe_dir(&state.config.data_dir);
    unload_pxpipe_in_dir(&dir);
    if let Err(err) = verify_pxpipe_module_loads(&dir).await {
        return err.into_response();
    }
    Json(get_pxpipe_status_json(&dir)).into_response()
}

pub async fn pxpipe_health_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let dir = pxpipe_dir(&state.config.data_dir);
    Json(run_pxpipe_health_check(&dir).await).into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct PxpipeLimitQuery {
    pub limit: Option<usize>,
}

pub async fn pxpipe_logs_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PxpipeLimitQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let limit = query.limit.filter(|&l| l > 0).unwrap_or(100).min(500);
    let dir = pxpipe_dir(&state.config.data_dir);
    let install_log = read_log_tail(&pxpipe_install_log(&dir), 200);
    let mut events = read_pxpipe_events(&dir, None, Some(limit));
    events.reverse();

    Json(json!({
        "installLog": install_log,
        "events": events,
    }))
    .into_response()
}

pub async fn pxpipe_stats_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PxpipeLimitQuery>,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let recent_limit = query.limit.filter(|&l| l > 0).unwrap_or(100).min(500);
    let dir = pxpipe_dir(&state.config.data_dir);
    Json(compute_pxpipe_stats(&dir, 30, recent_limit)).into_response()
}

pub async fn pxpipe_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or_default();
    let dir = pxpipe_dir(&state.config.data_dir);
    let mut status = get_pxpipe_status_json(&dir);

    let enabled = settings
        .get("pxpipeEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auto_install = settings
        .get("pxpipeAutoInstall")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let min_chars = settings
        .get("pxpipeMinChars")
        .cloned()
        .unwrap_or_else(|| json!(25000));
    let timeout_ms = settings
        .get("pxpipeTimeoutMs")
        .cloned()
        .unwrap_or_else(|| json!(15000));

    if let Some(obj) = status.as_object_mut() {
        obj.insert("enabled".to_string(), json!(enabled));
        obj.insert("autoInstall".to_string(), json!(auto_install));
        obj.insert("minChars".to_string(), min_chars);
        obj.insert("timeoutMs".to_string(), timeout_ms);
    }
    Json(status).into_response()
}

// ============================================================================
// MCP Stdio <-> SSE Bridge (/api/mcp/{plugin}/sse, /api/mcp/{plugin}/message)
// ============================================================================

const MCP_MAX_TEXT_CHARS: usize = 50_000;
const MCP_COLLAPSE_THRESHOLD: usize = 30;
const MCP_COLLAPSE_KEEP_HEAD: usize = 10;
const MCP_COLLAPSE_KEEP_TAIL: usize = 5;

#[derive(Debug, Clone)]
pub struct LocalStdioPlugin {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub tool_names: &'static [&'static str],
}

pub const LOCAL_STDIO_PLUGINS: &[LocalStdioPlugin] = &[LocalStdioPlugin {
    name: "browsermcp",
    command: "npx",
    args: &["-y", "@browsermcp/mcp@latest"],
    tool_names: &[
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_type",
        "browser_screenshot",
        "browser_get_console_logs",
        "browser_wait",
        "browser_press_key",
        "browser_go_back",
        "browser_go_forward",
    ],
}];

fn match_role_line(line: &str) -> Option<(&str, &str)> {
    let trimmed_start = line.trim_start_matches([' ', '\t']);
    let indent_len = line.len() - trimmed_start.len();
    let indent = &line[..indent_len];

    let after_dash = trimmed_start.strip_prefix('-')?;
    let after_ws = after_dash.trim_start_matches([' ', '\t']);
    let role_len = after_ws
        .bytes()
        .take_while(|b| b.is_ascii_alphabetic())
        .count();
    if role_len == 0 {
        return None;
    }
    let rest = &after_ws[role_len..];
    if let Some(first_after) = rest.bytes().next() {
        if first_after.is_ascii_alphanumeric() || first_after == b'_' {
            return None;
        }
    }
    Some((indent, &after_ws[..role_len]))
}

fn is_noise_generic_line(line: &str) -> bool {
    let t = line.trim();
    if let Some(after) = t.strip_prefix('-') {
        let rest = after.trim();
        return rest == "generic" || rest == "generic:" || rest == "text: \"\"";
    }
    false
}

fn find_nth_sibling_end(lines: &[&str], start: usize, indent: &str, role: &str, n: usize) -> usize {
    let mut count = 0;
    for (k, line) in lines.iter().enumerate().skip(start) {
        if let Some((li, lr)) = match_role_line(line) {
            if li == indent && lr == role {
                count += 1;
                if count > n {
                    return k;
                }
            }
        }
    }
    lines.len()
}

fn find_last_n_sibling_start(
    lines: &[&str],
    start: usize,
    end: usize,
    indent: &str,
    role: &str,
    n: usize,
) -> usize {
    let mut positions = Vec::new();
    for (k, line) in lines.iter().enumerate().take(end).skip(start) {
        if let Some((li, lr)) = match_role_line(line) {
            if li == indent && lr == role {
                positions.push(k);
            }
        }
    }
    if positions.len() > n {
        positions[positions.len() - n]
    } else {
        end
    }
}

fn collapse_repeated(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let Some((indent, role)) = match_role_line(line) else {
            out.push(line.to_string());
            i += 1;
            continue;
        };

        let indent_space = format!("{} ", indent);
        let indent_tab = format!("{}\t", indent);
        let mut j = i;
        while j < lines.len() {
            let ln = lines[j];
            if let Some((li, lr)) = match_role_line(ln) {
                if li == indent && lr == role {
                    j += 1;
                    continue;
                }
            }
            if ln.starts_with(&indent_space) || ln.starts_with(&indent_tab) {
                j += 1;
                continue;
            }
            break;
        }

        let group_len = j - i;
        if group_len >= MCP_COLLAPSE_THRESHOLD {
            let head_end = find_nth_sibling_end(&lines, i, indent, role, MCP_COLLAPSE_KEEP_HEAD);
            let tail_start =
                find_last_n_sibling_start(&lines, i, j, indent, role, MCP_COLLAPSE_KEEP_TAIL);
            for k in i..head_end.min(j) {
                out.push(lines[k].to_string());
            }
            out.push(format!(
                "{}... [{} similar \"{}\" items omitted by 9router bridge]",
                indent,
                group_len - MCP_COLLAPSE_KEEP_HEAD - MCP_COLLAPSE_KEEP_TAIL,
                role
            ));
            for k in tail_start..j {
                out.push(lines[k].to_string());
            }
        } else {
            for k in i..j {
                out.push(lines[k].to_string());
            }
        }
        i = j;
    }

    out.join("\n")
}

pub fn smart_filter_text(text: &str) -> String {
    let orig_chars = text.chars().count();
    if orig_chars < 2000 {
        return text.to_string();
    }
    let stripped: Vec<&str> = text
        .split('\n')
        .map(|l| if is_noise_generic_line(l) { "" } else { l })
        .collect();
    let mut out = collapse_repeated(&stripped.join("\n"));
    let out_chars = out.chars().count();
    if out_chars > MCP_MAX_TEXT_CHARS {
        let keep = MCP_MAX_TEXT_CHARS - 300;
        let head: String = out.chars().take(keep).collect();
        out = format!(
            "{}\n\n... [truncated {} chars by 9router bridge. Page is large; ask user to scroll/navigate to a specific section, or click an element with the refs shown above]",
            head,
            orig_chars.saturating_sub(keep)
        );
    }
    out
}

pub fn filter_mcp_frame(line: &str) -> String {
    let Ok(mut msg) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let Some(content) = msg
        .get_mut("result")
        .and_then(|r| r.get_mut("content"))
        .and_then(|c| c.as_array_mut())
    else {
        return line.to_string();
    };

    let mut mutated = false;
    for item in content.iter_mut() {
        let is_text = item.get("type").and_then(|t| t.as_str()) == Some("text");
        if is_text {
            if let Some(orig) = item.get("text").and_then(|t| t.as_str()) {
                let filtered = smart_filter_text(orig);
                if filtered != orig {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("text".to_string(), Value::String(filtered));
                        mutated = true;
                    }
                }
            }
        }
    }

    if mutated {
        serde_json::to_string(&msg).unwrap_or_else(|_| line.to_string())
    } else {
        line.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct McpPluginSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

impl From<&LocalStdioPlugin> for McpPluginSpec {
    fn from(p: &LocalStdioPlugin) -> Self {
        Self {
            name: p.name.to_string(),
            command: p.command.to_string(),
            args: p.args.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

struct McpBridgeEntry {
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    sessions: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>>,
    child_kill: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

pub struct McpBridgeRegistry {
    plugins: Vec<McpPluginSpec>,
    entries: Arc<Mutex<HashMap<String, Arc<McpBridgeEntry>>>>,
}

impl McpBridgeRegistry {
    pub fn new(plugins: Vec<McpPluginSpec>) -> Self {
        Self {
            plugins,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn default_registry() -> Self {
        Self::new(LOCAL_STDIO_PLUGINS.iter().map(McpPluginSpec::from).collect())
    }

    pub fn find_plugin(&self, name: &str) -> Option<McpPluginSpec> {
        self.plugins.iter().find(|p| p.name == name).cloned()
    }

    fn get_or_spawn(&self, name: &str) -> Result<Arc<McpBridgeEntry>, String> {
        let mut guard = self.entries.lock().map_err(|_| "bridge lock poisoned".to_string())?;
        if let Some(existing) = guard.get(name) {
            if existing.alive.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(Arc::clone(existing));
            }
            guard.remove(name);
        }

        let spec = self
            .find_plugin(name)
            .ok_or_else(|| format!("Unknown local plugin: {}", name))?;

        let mut cmd = TokioCommand::new(&spec.command);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PATH", extended_path_env())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", spec.command, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to capture MCP child stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture MCP child stdout".to_string())?;
        let stderr = child.stderr.take();

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
        let sessions: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<String>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let entry = Arc::new(McpBridgeEntry {
            stdin: Arc::new(AsyncMutex::new(Some(stdin))),
            sessions: Arc::clone(&sessions),
            child_kill: Arc::new(Mutex::new(Some(kill_tx))),
            alive: Arc::clone(&alive),
        });
        guard.insert(name.to_string(), Arc::clone(&entry));

        // Stdout reader -> filter JSON-RPC frame -> broadcast to active sessions
        let sessions_out = Arc::clone(&sessions);
        tokio::spawn(async move {
            let mut lines = TokioBufReader::new(stdout).lines();
            while let Ok(Some(raw)) = lines.next_line().await {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let filtered = filter_mcp_frame(trimmed);
                if let Ok(mut map) = sessions_out.lock() {
                    map.retain(|_, tx| tx.send(filtered.clone()).is_ok());
                }
            }
        });

        // Stderr drain so child never blocks on stderr pipe
        if let Some(err_stream) = stderr {
            let plugin_name = name.to_string();
            tokio::spawn(async move {
                let mut lines = TokioBufReader::new(err_stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let t = line.trim();
                    if !t.is_empty() {
                        tracing::debug!(plugin = %plugin_name, "[mcp:{}] {}", plugin_name, t);
                    }
                }
            });
        }

        // Lifecycle reaper: waits for child exit OR explicit kill signal
        let entries_ref = Arc::clone(&self.entries);
        let alive_ref = Arc::clone(&alive);
        let plugin_name = name.to_string();
        tokio::spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                _ = kill_rx => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
            alive_ref.store(false, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut map) = entries_ref.lock() {
                if let Some(cur) = map.get(&plugin_name) {
                    if !cur.alive.load(std::sync::atomic::Ordering::SeqCst) {
                        map.remove(&plugin_name);
                    }
                }
            }
        });

        Ok(entry)
    }

    pub fn register_session(
        &self,
        name: &str,
    ) -> Result<(String, mpsc::UnboundedReceiver<String>), String> {
        let entry = self.get_or_spawn(name)?;
        let sid = generate_uuid();
        let (tx, rx) = mpsc::unbounded_channel();
        let mut sessions = entry
            .sessions
            .lock()
            .map_err(|_| "session lock poisoned".to_string())?;
        sessions.insert(sid.clone(), tx);
        Ok((sid, rx))
    }

    pub fn unregister_session(&self, name: &str, sid: &str) {
        let entry_opt = self.entries.lock().ok().and_then(|g| g.get(name).cloned());
        let Some(entry) = entry_opt else {
            return;
        };
        let empty = if let Ok(mut sessions) = entry.sessions.lock() {
            sessions.remove(sid);
            sessions.is_empty()
        } else {
            false
        };
        if empty {
            entry.alive.store(false, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut k) = entry.child_kill.lock() {
                if let Some(tx) = k.take() {
                    let _ = tx.send(());
                }
            }
            if let Ok(mut g) = self.entries.lock() {
                g.remove(name);
            }
        }
    }

    pub async fn send_to_child(&self, name: &str, json_rpc: &Value) -> Result<(), String> {
        let entry = self
            .entries
            .lock()
            .ok()
            .and_then(|g| g.get(name).cloned())
            .ok_or_else(|| format!("Bridge not running: {}", name))?;

        if !entry.alive.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(format!("Bridge not running: {}", name));
        }

        let payload = format!(
            "{}\n",
            serde_json::to_string(json_rpc).map_err(|e| e.to_string())?
        );
        let mut stdin_guard = entry.stdin.lock().await;
        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| format!("Bridge not running: {}", name))?;
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("Bridge stdin write failed: {}", e))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("Bridge stdin flush failed: {}", e))?;
        Ok(())
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.entries
            .lock()
            .ok()
            .and_then(|g| {
                g.get(name)
                    .map(|e| e.alive.load(std::sync::atomic::Ordering::SeqCst))
            })
            .unwrap_or(false)
    }
}

static GLOBAL_MCP_BRIDGES: LazyLock<Arc<McpBridgeRegistry>> =
    LazyLock::new(|| Arc::new(McpBridgeRegistry::default_registry()));

struct SessionDropGuard {
    registry: Arc<McpBridgeRegistry>,
    plugin: String,
    sid: String,
}

impl Drop for SessionDropGuard {
    fn drop(&mut self) {
        self.registry.unregister_session(&self.plugin, &self.sid);
    }
}

async fn mcp_sse_with_registry(
    registry: Arc<McpBridgeRegistry>,
    plugin: String,
    initial_body: Option<axum::body::Bytes>,
) -> Response {
    if registry.find_plugin(&plugin).is_none() {
        return (StatusCode::NOT_FOUND, format!("Unknown plugin: {}", plugin)).into_response();
    }

    let (sid, mut rx) = match registry.register_session(&plugin) {
        Ok(v) => v,
        Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e),
    };

    if let Some(bytes) = initial_body {
        if !bytes.trim_ascii().is_empty() {
            if let Ok(json_rpc) = serde_json::from_slice::<Value>(&bytes) {
                let _ = registry.send_to_child(&plugin, &json_rpc).await;
            }
        }
    }

    let guard = SessionDropGuard {
        registry,
        plugin: plugin.clone(),
        sid: sid.clone(),
    };

    let endpoint_data = format!("/api/mcp/{}/message?sessionId={}", plugin, sid);
    let stream = async_stream::stream! {
        let _keep_alive_guard = guard;
        yield Ok::<Event, Infallible>(Event::default().event("endpoint").data(endpoint_data));
        while let Some(msg) = rx.recv().await {
            yield Ok::<Event, Infallible>(Event::default().event("message").data(msg));
        }
    };

    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp
}

async fn mcp_message_with_registry(
    registry: Arc<McpBridgeRegistry>,
    plugin: String,
    body_bytes: axum::body::Bytes,
) -> Response {
    if registry.find_plugin(&plugin).is_none() {
        return json_error_simple(
            StatusCode::NOT_FOUND,
            format!("Unknown plugin: {}", plugin),
        );
    }

    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => return json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    match registry.send_to_child(&plugin, &body).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e) => json_error_simple(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn mcp_sse_get_handler(
    State(state): State<AppState>,
    AxumPath(plugin): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    mcp_sse_with_registry(Arc::clone(&GLOBAL_MCP_BRIDGES), plugin, None).await
}

pub async fn mcp_sse_post_handler(
    State(state): State<AppState>,
    AxumPath(plugin): AxumPath<String>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    mcp_sse_with_registry(Arc::clone(&GLOBAL_MCP_BRIDGES), plugin, Some(body_bytes)).await
}

pub async fn mcp_message_handler(
    State(state): State<AppState>,
    AxumPath(plugin): AxumPath<String>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    mcp_message_with_registry(Arc::clone(&GLOBAL_MCP_BRIDGES), plugin, body_bytes).await
}

// ============================================================================
// Public Axum Router Export
// ============================================================================

pub fn sidecars_router() -> Router<AppState> {
    Router::new()
        // Headroom (6 routes)
        .route(
            "/api/headroom/start",
            post(headroom_start_handler).options(options_handler),
        )
        .route(
            "/api/headroom/stop",
            post(headroom_stop_handler).options(options_handler),
        )
        .route(
            "/api/headroom/restart",
            post(headroom_restart_handler).options(options_handler),
        )
        .route(
            "/api/headroom/status",
            get(headroom_status_handler).options(options_handler),
        )
        .route(
            "/api/headroom/extras",
            get(headroom_extras_get_handler)
                .post(headroom_extras_post_handler)
                .delete(headroom_extras_delete_handler)
                .options(options_handler),
        )
        .route(
            "/api/headroom/proxy",
            get(headroom_proxy_root_handler)
                .post(headroom_proxy_root_handler)
                .put(headroom_proxy_root_handler)
                .patch(headroom_proxy_root_handler)
                .delete(headroom_proxy_root_handler)
                .options(headroom_proxy_root_handler),
        )
        .route(
            "/api/headroom/proxy/{*path}",
            get(headroom_proxy_handler)
                .post(headroom_proxy_handler)
                .put(headroom_proxy_handler)
                .patch(headroom_proxy_handler)
                .delete(headroom_proxy_handler)
                .options(headroom_proxy_handler),
        )
        // PXPIPE (8 routes)
        .route(
            "/api/pxpipe/install",
            post(pxpipe_install_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/start",
            post(pxpipe_start_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/stop",
            post(pxpipe_stop_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/restart",
            post(pxpipe_restart_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/health",
            get(pxpipe_health_handler)
                .post(pxpipe_health_handler)
                .options(options_handler),
        )
        .route(
            "/api/pxpipe/logs",
            get(pxpipe_logs_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/stats",
            get(pxpipe_stats_handler).options(options_handler),
        )
        .route(
            "/api/pxpipe/status",
            get(pxpipe_status_handler).options(options_handler),
        )
        // MCP Bridge (2 routes)
        .route(
            "/api/mcp/{plugin}/sse",
            get(mcp_sse_get_handler)
                .post(mcp_sse_post_handler)
                .options(options_handler),
        )
        .route(
            "/api/mcp/{plugin}/message",
            post(mcp_message_handler).options(options_handler),
        )
}

// ============================================================================
// Unit & Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    struct TempTestDir(PathBuf);

    impl TempTestDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{}-{}", prefix, generate_uuid()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn build_test_state(data_dir: PathBuf) -> AppState {
        let db = crate::db::Database::open_in_memory().unwrap();
        let config = Arc::new(crate::config::Config {
            data_dir: data_dir.clone(),
            db_path: data_dir.join("test.sqlite"),
            port: 20127,
            jwt_secret: "test-jwt-secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let rate_limiter = Arc::new(crate::ratelimit::RateLimiter::from_config(&config));
        AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: crate::routes::ConsoleLogBuffer::new(),
            rate_limiter,
        }
    }

    #[test]
    fn test_headroom_helpers_and_html_rewrite() {
        assert!(is_loopback_headroom_url("http://localhost:8787"));
        assert!(is_loopback_headroom_url("http://127.0.0.1:8787"));
        assert!(is_loopback_headroom_url("http://[::1]:8787"));
        assert!(!is_loopback_headroom_url("https://headroom.example.com:8787"));
        assert!(!is_loopback_headroom_url("not-a-url"));

        assert_eq!(parse_port_from_url("http://localhost:9123"), Some(9123));
        assert_eq!(parse_port_from_url("http://localhost"), None);

        let pip_json = r#"[
            {"name": "headroom-ai", "version": "1.2.3"},
            {"name": "tree-sitter", "version": "0.21.0"}
        ]"#;
        let parsed = parse_pip_list_extras(pip_json);
        assert!(parsed.installed);
        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
        assert!(parsed.extras.code);
        assert!(!parsed.extras.ml);

        let html = "<script>fetch('/stats');fetch('/health');fetch('/stats-history');fetch('/transformations/feed');fetch('/other');</script>";
        let rewritten = rewrite_dashboard_html(html);
        assert!(rewritten.contains("fetch('/api/headroom/proxy/stats')"));
        assert!(rewritten.contains("fetch('/api/headroom/proxy/health')"));
        assert!(rewritten.contains("fetch('/api/headroom/proxy/stats-history')"));
        assert!(rewritten.contains("fetch('/api/headroom/proxy/transformations/feed')"));
        assert!(rewritten.contains("fetch('/other')"));
    }

    #[test]
    fn test_mcp_smart_filter_text_and_collapse_repeated() {
        let mut lines = Vec::new();
        lines.push("  - generic:".to_string());
        lines.push("  - text: \"\"".to_string());
        for i in 0..35 {
            lines.push(format!("  - button \"Item {}\" [ref=e{}]", i, i));
        }
        // Pad > 2000 chars to engage smart_filter_text
        while lines.join("\n").len() < 2100 {
            lines.push("  - paragraph: filler content to cross threshold".to_string());
        }
        let filtered = smart_filter_text(&lines.join("\n"));
        assert!(!filtered.contains("- generic:"));
        assert!(!filtered.contains("- text: \"\""));
        assert!(filtered.contains("20 similar \"button\" items omitted by 9router bridge"));
        assert!(filtered.contains("[ref=e0]"));
        assert!(filtered.contains("[ref=e34]"));

        let rpc = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "content": [{ "type": "text", "text": "short" }] }
        })
        .to_string();
        assert_eq!(filter_mcp_frame(&rpc), rpc);
    }

    #[tokio::test]
    async fn test_headroom_status_start_stop_and_errors() {
        let tmp = TempTestDir::new("9r-headroom-test");
        let state = build_test_state(tmp.path().to_path_buf());
        let app = sidecars_router().with_state(state.clone());

        // 1. GET /api/headroom/status
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/headroom/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let status_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status_json["url"], "http://localhost:8787");
        assert!(status_json["managedPid"].is_null());
        assert_eq!(status_json["localUrl"], true);

        // 2. POST /api/headroom/stop when not running -> 409
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/headroom/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let stop_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stop_json["stopped"], false);
        assert_eq!(stop_json["reason"], "not_running");

        // 3. POST /api/headroom/start with external headroomUrl -> 400 EXTERNAL_PROXY
        state
            .db
            .save_settings(&json!({ "headroomUrl": "https://remote-headroom.example.com:8787" }))
            .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/headroom/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let ext_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(ext_json["code"], "EXTERNAL_PROXY");

        // 4. POST /api/headroom/restart with external headroomUrl -> 400 EXTERNAL_PROXY
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/headroom/restart")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 5. GET /api/headroom/extras?log=1 returns log tail
        let h_dir = headroom_dir(tmp.path());
        fs::create_dir_all(&h_dir).unwrap();
        fs::write(headroom_install_log_file(&h_dir), "line1\nline2\n").unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/headroom/extras?log=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let log_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(log_json["log"], "line1\nline2");
    }

    #[tokio::test]
    async fn test_headroom_proxy_forwarding_rewrite_and_error_handling() {
        let tmp = TempTestDir::new("9r-headroom-proxy");
        let state = build_test_state(tmp.path().to_path_buf());
        let app = sidecars_router().with_state(state.clone());

        // 1. Invalid scheme -> 500 JSON error
        state
            .db
            .save_settings(&json!({ "headroomUrl": "ftp://localhost:8787" }))
            .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/headroom/proxy/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // 2. Spin up mock upstream Headroom server
        let mock_app = Router::new()
            .route(
                "/dashboard",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        "<html><script>fetch('/stats');fetch('/health');</script></html>",
                    )
                }),
            )
            .route(
                "/echo",
                post(|headers: HeaderMap, body: String| async move {
                    let has_auth = headers.contains_key(header::AUTHORIZATION);
                    Json(json!({ "echo": body, "hasAuth": has_auth }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv_handle = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_app).await;
        });

        state
            .db
            .save_settings(&json!({ "headroomUrl": format!("http://127.0.0.1:{}", addr.port()) }))
            .unwrap();

        // 3. Dashboard HTML rewrite
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/headroom/proxy/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("fetch('/api/headroom/proxy/stats')"));
        assert!(html.contains("fetch('/api/headroom/proxy/health')"));

        // 4. POST forwarding preserves body and loopback auth header
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/headroom/proxy/echo?foo=bar")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .body(Body::from("hello-headroom"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let echo_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(echo_json["echo"], "hello-headroom");
        assert_eq!(echo_json["hasAuth"], true);

        srv_handle.abort();

        // 5. Connection refused after upstream shutdown -> 500 JSON error
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/headroom/proxy/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let err_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(err_json["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_pxpipe_status_logs_stats_health_and_lifecycle() {
        let tmp = TempTestDir::new("9r-pxpipe-test");
        let state = build_test_state(tmp.path().to_path_buf());
        let app = sidecars_router().with_state(state.clone());

        // 1. Status when not installed
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/pxpipe/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let status_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(status_json["installed"], false);
        assert_eq!(status_json["running"], false);
        assert_eq!(status_json["mode"], "library");
        assert_eq!(status_json["autoInstall"], true);
        assert_eq!(status_json["minChars"], 25000);
        assert_eq!(status_json["timeoutMs"], 15000);

        // 2. Start when not installed & pxpipeAutoInstall=false -> 409 NOT_INSTALLED
        state
            .db
            .save_settings(&json!({ "pxpipeAutoInstall": false }))
            .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let err_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(err_json["code"], "NOT_INSTALLED");

        // 3. Health check when not installed -> 200 healthy=false
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/pxpipe/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let health_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(health_json["healthy"], false);
        assert_eq!(health_json["error"], "pxpipe not installed");

        // 4. Write events + install.log and verify /api/pxpipe/logs and /api/pxpipe/stats
        let p_dir = pxpipe_dir(tmp.path());
        fs::create_dir_all(&p_dir).unwrap();
        fs::write(pxpipe_install_log(&p_dir), "npm line 1\nnpm line 2\n").unwrap();

        let now_ms = Utc::now().timestamp_millis();
        append_pxpipe_event(
            &p_dir,
            &json!({
                "ts": now_ms - 5000,
                "applied": true,
                "tokensBeforeEst": 1000,
                "tokensAfterEst": 400,
                "tokensSavedEst": 600,
                "imageCount": 2,
                "durationMs": 120
            }),
        );
        append_pxpipe_event(
            &p_dir,
            &json!({
                "ts": now_ms - 1000,
                "applied": false,
                "reason": "timeout"
            }),
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/pxpipe/logs?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let logs_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(logs_json["installLog"], "npm line 1\nnpm line 2");
        assert_eq!(logs_json["events"].as_array().unwrap().len(), 2);
        // Newest first
        assert_eq!(logs_json["events"][0]["reason"], "timeout");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/pxpipe/stats?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let stats_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(stats_json["windows"]["all"]["requests"], 2);
        assert_eq!(stats_json["windows"]["all"]["compressed"], 1);
        assert_eq!(stats_json["windows"]["all"]["errors"], 1);
        assert_eq!(stats_json["windows"]["all"]["tokensSavedEst"], 600);
        assert_eq!(stats_json["windows"]["all"]["savedPct"], 60.0);
        assert_eq!(stats_json["timeline"].as_array().unwrap().len(), 30);

        // 5. Stop when not loaded -> stopped=false, 200
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let stop_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(stop_json["stopped"], false);

        // 6. Install a local fake pxpipe-proxy module and verify start/health/restart/stop
        let pkg_root = pxpipe_package_root(&p_dir);
        let core_dir = pkg_root.join("dist").join("core");
        fs::create_dir_all(&core_dir).unwrap();
        fs::write(
            pkg_root.join("package.json"),
            r#"{"name":"pxpipe-proxy","version":"0.9.9"}"#,
        )
        .unwrap();
        fs::write(
            core_dir.join("library.js"),
            r#"export async function transformAnthropicMessages({ body }) {
                return { applied: false, reason: "below_threshold", body };
            }"#,
        )
        .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let started_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(started_json["installed"], true);
        assert_eq!(started_json["running"], true);
        assert_eq!(started_json["version"], "0.9.9");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let health_ok: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(health_ok["healthy"], true);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/restart")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pxpipe/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let stopped_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(stopped_json["stopped"], true);
        assert_eq!(stopped_json["running"], false);
    }

    #[tokio::test]
    async fn test_mcp_unknown_plugin_and_sse_message_roundtrip() {
        let tmp = TempTestDir::new("9r-mcp-test");
        let state = build_test_state(tmp.path().to_path_buf());
        let app = sidecars_router().with_state(state);

        // 1. Unknown plugin -> 404 on /sse and /message
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/mcp/unknown-plugin/sse")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mcp/unknown-plugin/message")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // 2. Known plugin `browsermcp` when bridge not running -> 500 Bridge not running
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mcp/browsermcp/message")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let err_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert!(err_json["error"]
            .as_str()
            .unwrap_or("")
            .contains("Bridge not running: browsermcp"));

        // 3. Full Axum SSE + POST /message roundtrip using an isolated stdio echo plugin
        let registry = Arc::new(McpBridgeRegistry::new(vec![McpPluginSpec {
            name: "echo-mcp".to_string(),
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "while IFS= read -r line; do printf '%s\\n' \"$line\"; done".to_string(),
            ],
        }]));

        let reg_sse_get = Arc::clone(&registry);
        let reg_sse_post = Arc::clone(&registry);
        let reg_msg = Arc::clone(&registry);
        let test_router = Router::new()
            .route(
                "/api/mcp/{plugin}/sse",
                get(move |AxumPath(plugin): AxumPath<String>| {
                    let r = Arc::clone(&reg_sse_get);
                    async move { mcp_sse_with_registry(r, plugin, None).await }
                })
                .post(move |AxumPath(plugin): AxumPath<String>, body: axum::body::Bytes| {
                    let r = Arc::clone(&reg_sse_post);
                    async move { mcp_sse_with_registry(r, plugin, Some(body)).await }
                }),
            )
            .route(
                "/api/mcp/{plugin}/message",
                post(move |AxumPath(plugin): AxumPath<String>, body: axum::body::Bytes| {
                    let r = Arc::clone(&reg_msg);
                    async move { mcp_message_with_registry(r, plugin, body).await }
                }),
            );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let _ = axum::serve(listener, test_router).await;
        });

        let client = reqwest::Client::new();
        let sse_resp = client
            .get(format!("http://127.0.0.1:{}/api/mcp/echo-mcp/sse", addr.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(sse_resp.status(), reqwest::StatusCode::OK);
        assert_eq!(
            sse_resp
                .headers()
                .get("x-accel-buffering")
                .and_then(|v| v.to_str().ok()),
            Some("no")
        );

        let mut stream = sse_resp.bytes_stream();
        // Read initial endpoint event
        let first_chunk = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let first_text = String::from_utf8_lossy(&first_chunk);
        assert!(first_text.contains("event: endpoint"));
        assert!(first_text.contains("/api/mcp/echo-mcp/message?sessionId="));
        assert!(registry.is_running("echo-mcp"));

        // Extract session endpoint path and POST a JSON-RPC message
        let endpoint_line = first_text
            .lines()
            .find(|l| l.starts_with("data: "))
            .unwrap()
            .trim_start_matches("data: ")
            .trim();
        let rpc_req = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/list",
            "params": {}
        });
        let msg_resp = client
            .post(format!("http://127.0.0.1:{}{}", addr.port(), endpoint_line))
            .json(&rpc_req)
            .send()
            .await
            .unwrap();
        assert_eq!(msg_resp.status(), reqwest::StatusCode::ACCEPTED);

        // Verify echoed JSON-RPC message arrives over SSE stream
        let mut collected = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(Ok(chunk))) =
                tokio::time::timeout(Duration::from_millis(500), stream.next()).await
            {
                collected.push_str(&String::from_utf8_lossy(&chunk));
                if collected.contains("\"id\":42") {
                    break;
                }
            }
        }
        assert!(collected.contains("event: message"), "got: {}", collected);
        assert!(collected.contains("\"id\":42"), "got: {}", collected);
        assert!(collected.contains("\"tools/list\""), "got: {}", collected);

        // Drop SSE stream and confirm child reaper cleans up session
        drop(stream);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!registry.is_running("echo-mcp"));

        // 4. POST /api/mcp/echo-mcp/sse with inline JSON-RPC body
        let post_sse_resp = client
            .post(format!("http://127.0.0.1:{}/api/mcp/echo-mcp/sse", addr.port()))
            .json(&json!({ "jsonrpc": "2.0", "id": 77, "method": "ping" }))
            .send()
            .await
            .unwrap();
        assert_eq!(post_sse_resp.status(), reqwest::StatusCode::OK);
        let mut post_stream = post_sse_resp.bytes_stream();
        let mut post_collected = String::new();
        let post_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < post_deadline {
            if let Ok(Some(Ok(chunk))) =
                tokio::time::timeout(Duration::from_millis(500), post_stream.next()).await
            {
                post_collected.push_str(&String::from_utf8_lossy(&chunk));
                if post_collected.contains("\"id\":77") {
                    break;
                }
            }
        }
        assert!(post_collected.contains("event: endpoint"));
        assert!(post_collected.contains("\"id\":77"), "got: {}", post_collected);
        drop(post_stream);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!registry.is_running("echo-mcp"));

        srv.abort();
    }

    #[tokio::test]
    async fn test_sidecars_auth_and_headroom_process_lifecycle() {
        let tmp = TempTestDir::new("9r-sidecars-auth");
        let state = build_test_state(tmp.path().to_path_buf());
        state
            .db
            .save_settings(&json!({ "requireLogin": true }))
            .unwrap();
        let app = sidecars_router().with_state(state.clone());

        for (method, uri) in [
            ("GET", "/api/headroom/status"),
            ("POST", "/api/headroom/start"),
            ("GET", "/api/pxpipe/status"),
            ("GET", "/api/pxpipe/logs"),
            ("GET", "/api/pxpipe/stats"),
            ("GET", "/api/mcp/browsermcp/sse"),
            ("POST", "/api/mcp/browsermcp/message"),
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
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "expected 401 for {} {}",
                method,
                uri
            );
        }

        // Headroom process start helper: None binary -> NOT_INSTALLED
        let h_dir = headroom_dir(tmp.path());
        let err = start_headroom_proxy_with_binary(&h_dir, None, 8787, false, true)
            .await
            .unwrap_err();
        assert_eq!(err.code, Some("NOT_INSTALLED"));
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        // Immediate-exiting binary -> EARLY_EXIT and cleared PID
        let exit_script = tmp.path().join("exit_fast.sh");
        fs::write(&exit_script, "#!/bin/sh\nexit 2\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exit_script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = start_headroom_proxy_with_binary(
            &h_dir,
            Some(exit_script.to_string_lossy().to_string()),
            8787,
            true,
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Some("EARLY_EXIT"));
        assert!(read_managed_pid(&h_dir).is_none());

        // Spawn a real sleeping child, record its PID, verify alreadyRunning & stop
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let spid = sleeper.id();
        write_managed_pid(&h_dir, spid).unwrap();

        let already = start_headroom_proxy_with_binary(
            &h_dir,
            Some(exit_script.to_string_lossy().to_string()),
            8787,
            false,
            true,
        )
        .await
        .unwrap();
        assert_eq!(already["alreadyRunning"], true);
        assert_eq!(already["pid"], spid);

        state
            .db
            .save_settings(&json!({ "requireLogin": false }))
            .unwrap();
        let stop_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/headroom/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop_resp.status(), StatusCode::OK);
        let stop_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(stop_resp.into_body(), usize::MAX).await.unwrap(),
        )
        .unwrap();
        assert_eq!(stop_json["stopped"], true);
        assert_eq!(stop_json["pid"], spid);
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }
}
