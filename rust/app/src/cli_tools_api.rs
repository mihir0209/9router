use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Map, Value};

use crate::cli::mitm::{MitmDaemonManager, MitmPidRecord};
use crate::cli::tools::{
    home_dir, read_jsonc_value, rewrite_file, strip_jsonc_trailing_commas, v1_url,
    write_file_with_backup,
};
use crate::db::Database;
use crate::mitm::ca::{check_ca_installed, install_ca_to_system, CertificateAuthority};
use crate::mitm::dns::{
    check_tool_dns_status, clean_all_mitm_dns, disable_dns_for_hosts, enable_dns_for_hosts,
    get_hosts_file_path, TOOL_HOSTS,
};
use crate::routes::{options_handler, AppState};

// ============================================================================
// Router
// ============================================================================

pub fn cli_tools_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/cli-tools/claude-settings",
            get(claude_get)
                .post(claude_post)
                .delete(claude_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/cline-settings",
            get(cline_get)
                .post(cline_post)
                .delete(cline_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/codewhale-settings",
            get(codewhale_get)
                .post(codewhale_post)
                .delete(codewhale_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/codex-profiles",
            get(codex_profiles_get)
                .post(codex_profiles_post)
                .delete(codex_profiles_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/codex-settings",
            get(codex_get)
                .post(codex_post)
                .delete(codex_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/copilot-settings",
            get(copilot_get)
                .post(copilot_post)
                .delete(copilot_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/cowork-mcp-registry",
            get(cowork_mcp_registry_get).options(options_handler),
        )
        .route(
            "/api/cli-tools/cowork-mcp-tools",
            post(cowork_mcp_tools_post).options(options_handler),
        )
        .route(
            "/api/cli-tools/cowork-settings",
            get(cowork_get)
                .post(cowork_post)
                .delete(cowork_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/crush-settings",
            get(crush_get)
                .post(crush_post)
                .delete(crush_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/deepseek-tui-settings",
            get(deepseek_tui_get)
                .post(deepseek_tui_post)
                .delete(deepseek_tui_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/devin-settings",
            get(devin_get).options(options_handler),
        )
        .route(
            "/api/cli-tools/droid-settings",
            get(droid_get)
                .post(droid_post)
                .delete(droid_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/forge-settings",
            get(forge_get)
                .post(forge_post)
                .delete(forge_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/grok-build-settings",
            get(grok_build_get)
                .post(grok_build_post)
                .delete(grok_build_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/hermes-profiles",
            get(hermes_profiles_get).options(options_handler),
        )
        .route(
            "/api/cli-tools/hermes-settings",
            get(hermes_get)
                .post(hermes_post)
                .delete(hermes_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/jcode-settings",
            get(jcode_get)
                .post(jcode_post)
                .delete(jcode_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/kilo-settings",
            get(kilo_get)
                .post(kilo_post)
                .delete(kilo_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/omp-settings",
            get(omp_get)
                .post(omp_post)
                .delete(omp_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/openclaw-settings",
            get(openclaw_get)
                .post(openclaw_post)
                .delete(openclaw_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/opencode-settings",
            get(opencode_get)
                .post(opencode_post)
                .patch(opencode_patch)
                .delete(opencode_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/pi-settings",
            get(pi_get)
                .post(pi_post)
                .delete(pi_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/smelt-settings",
            get(smelt_get)
                .post(smelt_post)
                .delete(smelt_delete)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/antigravity-mitm",
            get(mitm_get)
                .post(mitm_post)
                .delete(mitm_delete)
                .patch(mitm_patch)
                .options(options_handler),
        )
        .route(
            "/api/cli-tools/antigravity-mitm/alias",
            get(mitm_alias_get)
                .put(mitm_alias_put)
                .options(options_handler),
        )
}

// ============================================================================
// Common Helpers
// ============================================================================

fn json_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn json_err_obj(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": { "message": msg.into() } }))).into_response()
}

fn parse_json_body(bytes: &[u8]) -> Result<Value, ()> {
    if bytes.is_empty() {
        return Err(());
    }
    serde_json::from_slice::<Value>(bytes).map_err(|_| ())
}

fn read_json_or_empty_obj(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let stripped = strip_jsonc_trailing_commas(&content);
            let val: Value = serde_json::from_str(&stripped).map_err(|e| e.to_string())?;
            Ok(val.as_object().cloned().unwrap_or_default())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn read_json_obj_optional(path: &Path) -> Option<Map<String, Value>> {
    read_jsonc_value(path).and_then(|v| v.as_object().cloned())
}

fn write_json_pretty_with_backup(path: &Path, val: &Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(val).map_err(|e| e.to_string())?;
    write_file_with_backup(path, &content).map_err(|e| e.to_string())
}

fn write_json_pretty_no_backup(path: &Path, val: &Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(val).map_err(|e| e.to_string())?;
    rewrite_file(path, &content).map_err(|e| e.to_string())
}

fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    #[allow(unused_mut)]
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            dirs.insert(0, PathBuf::from(appdata).join("npm"));
        }
    }

    for dir in dirs {
        let candidate = dir.join(bin);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let with_ext = dir.join(format!("{}.{}", bin, ext));
                if is_executable_file(&with_ext) {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn is_tool_installed(bin_name: &str, fallback_paths: &[PathBuf]) -> bool {
    if find_in_path(bin_name).is_some() {
        return true;
    }
    fallback_paths.iter().any(|p| p.exists())
}

/// Resolves the API key to write into a CLI tool config.
/// Order: caller key (unless empty or "sk_9router") -> first active key in DB -> "".
pub fn resolve_cli_api_key(db: &Database, caller_key: Option<&str>) -> String {
    if let Some(k) = caller_key {
        let trimmed = k.trim();
        if !trimmed.is_empty() && trimmed != "sk_9router" {
            return trimmed.to_string();
        }
    }
    if let Ok(keys) = db.list_api_keys() {
        if let Some(active) = keys.into_iter().find(|k| k.is_active) {
            return active.key;
        }
    }
    String::new()
}

fn strip_v1_suffix(base: &str) -> String {
    let trimmed = base.trim();
    if let Some(stripped) = trimmed.strip_suffix("/v1") {
        stripped.to_string()
    } else {
        trimmed.to_string()
    }
}

fn get_str_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn truthy_value(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true" || s == "1",
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

// ============================================================================
// Simple TOML Parser / Serializer (no Cargo.toml changes)
// ============================================================================

fn unquote_toml_str(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
        let inner = &t[1..t.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2 {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn strip_toml_inline_comment(s: &str) -> &str {
    let mut in_dq = false;
    let mut in_sq = false;
    let mut escaped = false;
    let mut bracket_depth: usize = 0;
    for (idx, ch) in s.char_indices() {
        if in_dq {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_dq = false;
            }
            continue;
        }
        if in_sq {
            if ch == '\'' {
                in_sq = false;
            }
            continue;
        }
        match ch {
            '"' => in_dq = true,
            '\'' => in_sq = true,
            '[' | '{' => bracket_depth += 1,
            ']' | '}' => bracket_depth = bracket_depth.saturating_sub(1),
            '#' if bracket_depth == 0 => return s[..idx].trim_end(),
            _ => {}
        }
    }
    s.trim_end()
}

fn split_toml_key_path(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_dq = false;
    let mut in_sq = false;
    for ch in raw.chars() {
        if in_dq {
            if ch == '"' {
                in_dq = false;
            } else {
                cur.push(ch);
            }
            continue;
        }
        if in_sq {
            if ch == '\'' {
                in_sq = false;
            } else {
                cur.push(ch);
            }
            continue;
        }
        match ch {
            '"' => in_dq = true,
            '\'' => in_sq = true,
            '.' => {
                let t = cur.trim();
                if !t.is_empty() {
                    parts.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        parts.push(t.to_string());
    }
    parts
}

fn split_toml_eq(line: &str) -> Option<(&str, &str)> {
    let mut in_dq = false;
    let mut in_sq = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if in_dq {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_dq = false;
            }
            continue;
        }
        if in_sq {
            if ch == '\'' {
                in_sq = false;
            }
            continue;
        }
        match ch {
            '"' => in_dq = true,
            '\'' => in_sq = true,
            '=' => return Some((line[..idx].trim(), line[idx + 1..].trim())),
            _ => {}
        }
    }
    None
}

fn split_comma_respecting_quotes(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_dq = false;
    let mut in_sq = false;
    let mut escaped = false;
    let mut depth: usize = 0;
    for ch in inner.chars() {
        if in_dq {
            cur.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_dq = false;
            }
            continue;
        }
        if in_sq {
            cur.push(ch);
            if ch == '\'' {
                in_sq = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_dq = true;
                cur.push(ch);
            }
            '\'' => {
                in_sq = true;
                cur.push(ch);
            }
            '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            ']' | '}' => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
            }
            ',' if depth == 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

fn parse_toml_value(raw: &str) -> Value {
    let t = raw.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        return Value::String(unquote_toml_str(t));
    }
    if t == "true" {
        return Value::Bool(true);
    }
    if t == "false" {
        return Value::Bool(false);
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = t.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    if t.starts_with('[') && t.ends_with(']') {
        let inner = &t[1..t.len() - 1];
        let items = split_comma_respecting_quotes(inner)
            .into_iter()
            .map(|item| parse_toml_value(&item))
            .collect();
        return Value::Array(items);
    }
    if t.starts_with('{') && t.ends_with('}') {
        let inner = &t[1..t.len() - 1];
        let mut map = Map::new();
        for item in split_comma_respecting_quotes(inner) {
            if let Some((k, v)) = split_toml_eq(&item) {
                let path = split_toml_key_path(k);
                set_nested_value(&mut map, &path, parse_toml_value(v));
            }
        }
        return Value::Object(map);
    }
    Value::String(t.to_string())
}

fn set_nested_value(root: &mut Map<String, Value>, path: &[String], val: Value) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        root.insert(path[0].clone(), val);
        return;
    }
    let entry = root
        .entry(path[0].clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Some(obj) = entry.as_object_mut() {
        set_nested_value(obj, &path[1..], val);
    }
}

fn delete_nested_value(root: &mut Map<String, Value>, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        root.remove(path[0]);
        return;
    }
    if let Some(obj) = root.get_mut(path[0]).and_then(|v| v.as_object_mut()) {
        delete_nested_value(obj, &path[1..]);
    }
}

pub fn parse_toml_to_map(content: &str) -> Map<String, Value> {
    let mut root = Map::new();
    let mut current_section: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        let line = strip_toml_inline_comment(raw_line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let inner = line.trim_start_matches('[').trim_end_matches(']').trim();
            current_section = split_toml_key_path(inner);
            // Ensure section exists as an object even if empty
            if !current_section.is_empty() {
                let mut cur = &mut root;
                for seg in &current_section {
                    let entry = cur
                        .entry(seg.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                    if !entry.is_object() {
                        *entry = Value::Object(Map::new());
                    }
                    cur = entry.as_object_mut().unwrap();
                }
            }
            continue;
        }
        if let Some((k_raw, v_raw)) = split_toml_eq(line) {
            let mut full_path = current_section.clone();
            full_path.extend(split_toml_key_path(k_raw));
            set_nested_value(&mut root, &full_path, parse_toml_value(v_raw));
        }
    }

    root
}

fn format_toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_else(|_| format!("\"{}\"", k))
    }
}

fn format_toml_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "\"\"".to_string(),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_toml_scalar).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(obj) => {
            let pairs: Vec<String> = obj
                .iter()
                .map(|(k, val)| format!("{} = {}", format_toml_key(k), format_toml_scalar(val)))
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
    }
}

fn write_toml_table(out: &mut String, prefix: &[String], map: &Map<String, Value>) {
    let mut scalars = Vec::new();
    let mut tables = Vec::new();
    for (k, v) in map {
        if let Some(sub) = v.as_object() {
            tables.push((k, sub));
        } else {
            scalars.push((k, v));
        }
    }

    if !prefix.is_empty() && (!scalars.is_empty() || tables.is_empty()) {
        if !out.is_empty() && !out.ends_with("\n\n") {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        let header: Vec<String> = prefix.iter().map(|s| format_toml_key(s)).collect();
        out.push_str(&format!("[{}]\n", header.join(".")));
    }

    for (k, v) in scalars {
        out.push_str(&format!("{} = {}\n", format_toml_key(k), format_toml_scalar(v)));
    }

    for (k, sub) in tables {
        let mut next = prefix.to_vec();
        next.push(k.clone());
        write_toml_table(out, &next, sub);
    }
}

pub fn serialize_toml_map(map: &Map<String, Value>) -> String {
    let mut out = String::new();
    write_toml_table(&mut out, &[], map);
    out
}

// ============================================================================
// 1. /api/cli-tools/claude-settings
// ============================================================================

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";
const CLAUDE_RESET_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "API_TIMEOUT_MS",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
];

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn claude_json_path() -> PathBuf {
    home_dir().join(".claude.json")
}

fn write_claude_json_mcp(exa_entry: Option<Value>) -> Result<(), String> {
    let path = claude_json_path();
    let mut data = read_json_or_empty_obj(&path)?;
    if let Some(entry) = exa_entry {
        let mut mcp = data
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        mcp.insert("exa".to_string(), entry);
        data.insert("mcpServers".to_string(), Value::Object(mcp));
    } else if let Some(mcp) = data.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        mcp.remove("exa");
        if mcp.is_empty() {
            data.remove("mcpServers");
        }
    }
    write_json_pretty_no_backup(&path, &Value::Object(data))
}

async fn claude_get() -> Response {
    let settings_path = claude_settings_path();
    if !is_tool_installed("claude", &[settings_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Claude CLI is not installed"
        }))
        .into_response();
    }

    let settings = read_jsonc_value(&settings_path);
    let has_9router = settings
        .as_ref()
        .and_then(|s| s.get("env"))
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|u| u.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let claude_json = read_jsonc_value(&claude_json_path());
    let exa_mcp_enabled = claude_json
        .as_ref()
        .and_then(|c| c.get("mcpServers"))
        .and_then(|m| m.get("exa"))
        .map(|v| !v.is_null())
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "settings": settings,
        "has9Router": has_9router,
        "exaMcpEnabled": exa_mcp_enabled,
        "settingsPath": settings_path.to_string_lossy(),
    }))
    .into_response()
}

async fn claude_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update claude settings");
    };
    let Some(env_val) = raw.get("env").and_then(|v| v.as_object()) else {
        return json_err(StatusCode::BAD_REQUEST, "Invalid env object");
    };
    let mut env_obj = env_val.clone();

    let settings_path = claude_settings_path();
    let current = match read_json_or_empty_obj(&settings_path) {
        Ok(obj) => obj,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to update claude settings",
            )
        }
    };

    if let Some(base) = env_obj
        .get("ANTHROPIC_BASE_URL")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| v1_url(s))
    {
        env_obj.insert("ANTHROPIC_BASE_URL".to_string(), Value::String(base));
    }

    let has_existing_token = current
        .get("env")
        .and_then(|v| v.as_object())
        .and_then(|e| get_str_field(e, "ANTHROPIC_AUTH_TOKEN"))
        .is_some();
    if has_existing_token {
        env_obj.remove("ANTHROPIC_AUTH_TOKEN");
    }

    let mut merged_env = current
        .get("env")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (k, v) in env_obj {
        merged_env.insert(k, v);
    }

    match raw.get("autoCompactWindow") {
        Some(Value::String(s)) if !s.is_empty() => {
            merged_env.insert(
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
                Value::String(s.clone()),
            );
        }
        Some(Value::Number(n)) if n.as_i64().unwrap_or(0) != 0 => {
            merged_env.insert(
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
                Value::String(n.to_string()),
            );
        }
        _ => {
            merged_env.remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
        }
    }

    let mut new_settings = current;
    new_settings.insert("hasCompletedOnboarding".to_string(), Value::Bool(true));
    new_settings.insert("env".to_string(), Value::Object(merged_env));

    if write_json_pretty_with_backup(&settings_path, &Value::Object(new_settings)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update claude settings",
        );
    }

    let exa_enabled = truthy_value(raw.get("exaMcpEnabled"));
    let exa_entry = if exa_enabled {
        Some(json!({ "type": "http", "url": EXA_MCP_URL }))
    } else {
        None
    };
    if write_claude_json_mcp(exa_entry).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update claude settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Settings updated successfully"
    }))
    .into_response()
}

async fn claude_delete() -> Response {
    let settings_path = claude_settings_path();
    if !settings_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No settings file to reset"
        }))
        .into_response();
    }

    let mut current = match read_json_or_empty_obj(&settings_path) {
        Ok(obj) => obj,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset claude settings",
            )
        }
    };

    if let Some(env_obj) = current.get_mut("env").and_then(|v| v.as_object_mut()) {
        for key in CLAUDE_RESET_ENV_KEYS {
            env_obj.remove(*key);
        }
        if env_obj.is_empty() {
            current.remove("env");
        }
    }

    let _ = write_claude_json_mcp(None);

    if write_json_pretty_no_backup(&settings_path, &Value::Object(current)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset claude settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Settings reset successfully"
    }))
    .into_response()
}

// ============================================================================
// 2. /api/cli-tools/cline-settings
// ============================================================================

fn cline_data_dir() -> PathBuf {
    home_dir().join(".cline").join("data")
}

fn cline_global_state_path() -> PathBuf {
    cline_data_dir().join("globalState.json")
}

fn cline_secrets_path() -> PathBuf {
    cline_data_dir().join("secrets.json")
}

fn cline_has_9router(gs: Option<&Map<String, Value>>) -> bool {
    let Some(gs) = gs else {
        return false;
    };
    let is_openai = get_str_field(gs, "actModeApiProvider") == Some("openai")
        || get_str_field(gs, "planModeApiProvider") == Some("openai");
    let base_url = get_str_field(gs, "openAiBaseUrl").unwrap_or("");
    is_openai
        && (base_url.contains("localhost")
            || base_url.contains("127.0.0.1")
            || base_url.contains("9router"))
}

async fn cline_get() -> Response {
    let gs_path = cline_global_state_path();
    if !is_tool_installed("cline", &[gs_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Cline CLI is not installed"
        }))
        .into_response();
    }

    let gs = read_json_obj_optional(&gs_path);
    let has_9router = cline_has_9router(gs.as_ref());
    let settings = json!({
        "actModeApiProvider": gs.as_ref().and_then(|g| g.get("actModeApiProvider")).cloned().unwrap_or(Value::Null),
        "planModeApiProvider": gs.as_ref().and_then(|g| g.get("planModeApiProvider")).cloned().unwrap_or(Value::Null),
        "openAiBaseUrl": gs.as_ref().and_then(|g| g.get("openAiBaseUrl")).cloned().unwrap_or(Value::Null),
        "openAiModelId": gs.as_ref().and_then(|g| g.get("openAiModelId")).cloned().unwrap_or(Value::Null),
    });

    Json(json!({
        "installed": true,
        "settings": settings,
        "has9Router": has_9router,
        "globalStatePath": gs_path.to_string_lossy(),
    }))
    .into_response()
}

async fn cline_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update cline settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");

    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "baseUrl, apiKey and model are required",
        );
    }

    let gs_path = cline_global_state_path();
    let secrets_path = cline_secrets_path();
    let normalized_base = strip_v1_suffix(base_url);

    let mut gs = read_json_obj_optional(&gs_path).unwrap_or_default();
    gs.insert(
        "actModeApiProvider".to_string(),
        Value::String("openai".to_string()),
    );
    gs.insert(
        "planModeApiProvider".to_string(),
        Value::String("openai".to_string()),
    );
    gs.insert("openAiBaseUrl".to_string(), Value::String(normalized_base));
    gs.insert(
        "openAiModelId".to_string(),
        Value::String(model.to_string()),
    );
    gs.insert(
        "planModeOpenAiModelId".to_string(),
        Value::String(model.to_string()),
    );
    if write_json_pretty_with_backup(&gs_path, &Value::Object(gs)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update cline settings",
        );
    }

    let mut secrets = read_json_obj_optional(&secrets_path).unwrap_or_default();
    secrets.insert(
        "openAiApiKey".to_string(),
        Value::String(api_key.to_string()),
    );
    if write_json_pretty_with_backup(&secrets_path, &Value::Object(secrets)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update cline settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Cline settings applied successfully!",
        "globalStatePath": gs_path.to_string_lossy(),
    }))
    .into_response()
}

async fn cline_delete() -> Response {
    let gs_path = cline_global_state_path();
    let Some(mut gs) = read_json_obj_optional(&gs_path) else {
        return Json(json!({
            "success": true,
            "message": "No settings file to reset"
        }))
        .into_response();
    };

    if get_str_field(&gs, "actModeApiProvider") == Some("openai") {
        gs.remove("openAiBaseUrl");
        gs.remove("openAiModelId");
        gs.remove("planModeOpenAiModelId");
        gs.insert(
            "actModeApiProvider".to_string(),
            Value::String("cline".to_string()),
        );
        gs.insert(
            "planModeApiProvider".to_string(),
            Value::String("cline".to_string()),
        );
    }
    if write_json_pretty_no_backup(&gs_path, &Value::Object(gs)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset cline settings",
        );
    }

    let secrets_path = cline_secrets_path();
    let mut secrets = read_json_obj_optional(&secrets_path).unwrap_or_default();
    secrets.remove("openAiApiKey");
    if write_json_pretty_no_backup(&secrets_path, &Value::Object(secrets)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset cline settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9Router settings removed from Cline"
    }))
    .into_response()
}

// ============================================================================
// 3. /api/cli-tools/codewhale-settings
// ============================================================================

fn codewhale_config_path() -> PathBuf {
    home_dir().join(".codewhale").join("config.toml")
}

async fn codewhale_get() -> Response {
    let config_path = codewhale_config_path();
    if !is_tool_installed("codewhale", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "CodeWhale CLI is not installed"
        }))
        .into_response();
    }

    let content = std::fs::read_to_string(&config_path).ok();
    let config = content
        .as_deref()
        .map(|c| Value::Object(parse_toml_to_map(c)));
    let has_9router = content
        .as_deref()
        .map(|c| c.contains("managed by 9Router") || c.contains("localhost:20128"))
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn codewhale_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }
    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("provider/model-id");

    let config_path = codewhale_config_path();
    let mut existing = std::fs::read_to_string(&config_path)
        .ok()
        .map(|c| parse_toml_to_map(&c))
        .unwrap_or_default();

    let mut openai = Map::new();
    openai.insert("base_url".to_string(), Value::String(v1_url(base_url)));
    openai.insert("api_key".to_string(), Value::String(api_key));
    openai.insert("model".to_string(), Value::String(model.to_string()));
    existing.insert("openai".to_string(), Value::Object(openai));

    let content = format!(
        "# CodeWhale config — managed by 9Router\n\n{}",
        serialize_toml_map(&existing)
    );
    if let Err(e) = write_file_with_backup(&config_path, &content) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    Json(json!({
        "success": true,
        "message": "CodeWhale settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn codewhale_delete() -> Response {
    let config_path = codewhale_config_path();
    let Ok(raw) = std::fs::read_to_string(&config_path) else {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    };
    let mut existing = parse_toml_to_map(&raw);
    existing.remove("openai");

    if existing.is_empty() {
        let _ = std::fs::remove_file(&config_path);
    } else if let Err(e) = rewrite_file(&config_path, &serialize_toml_map(&existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from CodeWhale"
    }))
    .into_response()
}

// ============================================================================
// 4. /api/cli-tools/codex-profiles
// ============================================================================

fn codex_dir() -> PathBuf {
    home_dir().join(".codex")
}

fn is_valid_codex_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.to_lowercase() != "config"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_valid_codex_model(model: &str) -> bool {
    let trimmed = model.trim();
    !trimmed.is_empty()
        && model.len() <= 256
        && !model.contains('\r')
        && !model.contains('\n')
        && !model.contains('"')
}

fn extract_toml_model_line(content: &str) -> String {
    for line in content.lines() {
        let t = line.trim();
        if let Some((k, v)) = split_toml_eq(t) {
            if k == "model" {
                return unquote_toml_str(v);
            }
        }
    }
    String::new()
}

async fn codex_profiles_get() -> Response {
    let dir = codex_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({ "profiles": [] })).into_response();
        }
        Err(_) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read profiles"),
    };

    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_str = file_name.to_string_lossy();
        if file_str.ends_with(".config.toml") && file_str != "config.toml" {
            let name = file_str.trim_end_matches(".config.toml").to_string();
            let model = std::fs::read_to_string(entry.path())
                .map(|c| extract_toml_model_line(&c))
                .unwrap_or_default();
            profiles.push((
                name.clone(),
                json!({
                    "name": name,
                    "model": model,
                    "command": format!("codex -p {}", name),
                }),
            ));
        }
    }
    profiles.sort_by(|a, b| a.0.cmp(&b.0));
    let out: Vec<Value> = profiles.into_iter().map(|(_, v)| v).collect();
    Json(json!({ "profiles": out })).into_response()
}

async fn codex_profiles_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save profile");
    };
    let clean_name = raw
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if !is_valid_codex_profile_name(&clean_name) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "Profile name can only contain alphanumeric characters, dashes, underscores and cannot be 'config'",
        );
    }
    let clean_model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !is_valid_codex_model(&clean_model) {
        return json_err(StatusCode::BAD_REQUEST, "Invalid model name");
    }

    let file_path = codex_dir().join(format!("{}.config.toml", clean_name));
    let content = format!(
        "# codex -p {}\nmodel = \"{}\"\nmodel_provider = \"9router\"\n",
        clean_name, clean_model
    );
    if rewrite_file(&file_path, &content).is_err() {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save profile");
    }

    Json(json!({
        "success": true,
        "profile": {
            "name": clean_name,
            "model": clean_model,
            "command": format!("codex -p {}", clean_name),
        }
    }))
    .into_response()
}

async fn codex_profiles_delete(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete profile");
    };
    let clean_name = raw
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if !is_valid_codex_profile_name(&clean_name) {
        return json_err(StatusCode::BAD_REQUEST, "Invalid profile name");
    }

    let file_path = codex_dir().join(format!("{}.config.toml", clean_name));
    match std::fs::remove_file(&file_path) {
        Ok(()) => Json(json!({
            "success": true,
            "message": format!("Profile {} deleted", clean_name)
        }))
        .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json_err(StatusCode::NOT_FOUND, "Profile not found")
        }
        Err(_) => json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete profile"),
    }
}

// ============================================================================
// 5. /api/cli-tools/codex-settings
// ============================================================================

fn codex_config_path() -> PathBuf {
    codex_dir().join("config.toml")
}

fn codex_auth_path() -> PathBuf {
    codex_dir().join("auth.json")
}

async fn codex_get() -> Response {
    let config_path = codex_config_path();
    if !is_tool_installed("codex", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Codex CLI is not installed"
        }))
        .into_response();
    }

    let config = match std::fs::read_to_string(&config_path) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to check codex settings",
            )
        }
    };
    let has_9router = config
        .as_deref()
        .map(|c| c.contains("model_provider = \"9router\"") || c.contains("[model_providers.9router]"))
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn codex_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update codex settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let subagent_model = raw
        .get("subagentModel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(model);

    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "baseUrl, apiKey and model are required",
        );
    }

    let config_path = codex_config_path();
    let mut parsed = std::fs::read_to_string(&config_path)
        .ok()
        .map(|c| parse_toml_to_map(&c))
        .unwrap_or_default();

    parsed.insert("model".to_string(), Value::String(model.to_string()));
    parsed.insert(
        "model_provider".to_string(),
        Value::String("9router".to_string()),
    );

    let provider_obj = json!({
        "name": "9Router",
        "base_url": v1_url(base_url),
        "wire_api": "responses",
        "http_headers": {
            "Authorization": format!("Bearer {}", api_key),
        }
    });
    set_nested_value(
        &mut parsed,
        &["model_providers".to_string(), "9router".to_string()],
        provider_obj,
    );
    delete_nested_value(&mut parsed, &["agents", "subagent"]);
    set_nested_value(
        &mut parsed,
        &[
            "agents".to_string(),
            "default_subagent_model".to_string(),
        ],
        Value::String(subagent_model.to_string()),
    );

    if write_file_with_backup(&config_path, &serialize_toml_map(&parsed)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update codex settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Codex settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn codex_delete() -> Response {
    let config_path = codex_config_path();
    let raw = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({
                "success": true,
                "message": "No config file to reset"
            }))
            .into_response();
        }
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset codex settings",
            )
        }
    };

    let mut parsed = parse_toml_to_map(&raw);
    if get_str_field(&parsed, "model_provider") == Some("9router") {
        parsed.remove("model");
        parsed.remove("model_provider");
    }
    delete_nested_value(&mut parsed, &["model_providers", "9router"]);
    if parsed
        .get("model_providers")
        .and_then(|v| v.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false)
    {
        parsed.remove("model_providers");
    }
    delete_nested_value(&mut parsed, &["agents", "default_subagent_model"]);
    delete_nested_value(&mut parsed, &["agents", "subagent"]);
    if parsed
        .get("agents")
        .and_then(|v| v.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false)
    {
        parsed.remove("agents");
    }

    if rewrite_file(&config_path, &serialize_toml_map(&parsed)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset codex settings",
        );
    }

    let auth_path = codex_auth_path();
    if let Some(mut auth_obj) = read_json_obj_optional(&auth_path) {
        auth_obj.remove("OPENAI_API_KEY");
        auth_obj.remove("auth_mode");
        if auth_obj.is_empty() {
            let _ = std::fs::remove_file(&auth_path);
        } else {
            let _ = write_json_pretty_no_backup(&auth_path, &Value::Object(auth_obj));
        }
    }

    Json(json!({
        "success": true,
        "message": "9Router settings removed successfully"
    }))
    .into_response()
}

// ============================================================================
// 6. /api/cli-tools/copilot-settings
// ============================================================================

fn copilot_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(home_dir);
        appdata
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        home_dir()
            .join(".config")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    }
}

async fn copilot_get() -> Response {
    let config_path = copilot_config_path();
    let config = read_jsonc_value(&config_path);
    let entry = config
        .as_ref()
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|e| e.get("name").and_then(|n| n.as_str()) == Some("9Router"))
        });

    let current_model = entry
        .and_then(|e| e.get("models"))
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let current_url = entry
        .and_then(|e| e.get("models"))
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("url"))
        .cloned()
        .unwrap_or(Value::Null);

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": entry.is_some(),
        "configPath": config_path.to_string_lossy(),
        "currentModel": current_model,
        "currentUrl": current_url,
    }))
    .into_response()
}

async fn copilot_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update copilot settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let models = raw
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if base_url.is_empty() || models.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and models are required");
    }

    let config_path = copilot_config_path();
    let mut list = read_jsonc_value(&config_path)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();

    let endpoint_url = format!("{}/chat/completions#models.ai.azure.com", base_url);
    let key_to_use = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));

    let mapped_models: Vec<Value> = models
        .into_iter()
        .filter_map(|m| m.as_str().map(|s| s.to_string()))
        .map(|id| {
            json!({
                "id": id,
                "name": id,
                "url": endpoint_url,
                "toolCalling": true,
                "vision": false,
                "maxInputTokens": 128000,
                "maxOutputTokens": 16000,
            })
        })
        .collect();

    let new_entry = json!({
        "name": "9Router",
        "vendor": "azure",
        "apiKey": key_to_use,
        "models": mapped_models,
    });

    if let Some(pos) = list
        .iter()
        .position(|e| e.get("name").and_then(|n| n.as_str()) == Some("9Router"))
    {
        list[pos] = new_entry;
    } else {
        list.push(new_entry);
    }

    if write_json_pretty_with_backup(&config_path, &Value::Array(list)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update copilot settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Copilot settings applied! Reload VS Code to take effect.",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn copilot_delete() -> Response {
    let config_path = copilot_config_path();
    if !config_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    }

    let mut list = match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            let stripped = strip_jsonc_trailing_commas(&content);
            match serde_json::from_str::<Value>(&stripped) {
                Ok(Value::Array(arr)) => arr,
                Ok(_) => Vec::new(),
                Err(_) => {
                    return json_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to reset copilot settings",
                    )
                }
            }
        }
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset copilot settings",
            )
        }
    };

    list.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some("9Router"));
    if write_json_pretty_no_backup(&config_path, &Value::Array(list)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset copilot settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from Copilot config"
    }))
    .into_response()
}

// ============================================================================
// 7. /api/cli-tools/cowork-mcp-registry
// ============================================================================

const MCP_REGISTRY_URL: &str = "https://api.anthropic.com/mcp-registry/v0/servers";
const MCP_REGISTRY_VISIBILITY: &str = "commercial,gsuite,gsuite-google";
const MCP_REGISTRY_CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct RegistryCache {
    fetched_at: Instant,
    servers: Vec<Value>,
}

static REGISTRY_CACHE: OnceLock<tokio::sync::RwLock<Option<RegistryCache>>> = OnceLock::new();

fn registry_cache() -> &'static tokio::sync::RwLock<Option<RegistryCache>> {
    REGISTRY_CACHE.get_or_init(|| tokio::sync::RwLock::new(None))
}

fn is_direct_connect_mcp_url(url_str: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url_str) else {
        return false;
    };
    if !parsed.scheme().eq_ignore_ascii_case("https") {
        return false;
    }
    if url_str.contains('<') || url_str.contains('{') {
        return false;
    }
    if let Some(host) = parsed.host_str() {
        let lower = host.to_ascii_lowercase();
        if lower == "mcp.claude.com" || lower.ends_with(".mcp.claude.com") {
            return false;
        }
        if lower == "api.anthropic.com" && parsed.path().starts_with("/mcp") {
            return false;
        }
    }
    true
}

fn parse_mcp_registry_page(j: &Value, out: &mut Vec<Value>) -> Option<String> {
    if let Some(items) = j.get("servers").and_then(|v| v.as_array()) {
        for item in items {
            let s = item.get("server").and_then(|v| v.as_object());
            let Some(s) = s else { continue };
            let meta = item
                .get("_meta")
                .and_then(|m| m.get("com.anthropic.api/mcp-registry"))
                .and_then(|v| v.as_object());
            let remote = s
                .get("remotes")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_object());
            let Some(remote) = remote else { continue };
            let Some(remote_url) = remote.get("url").and_then(|v| v.as_str()) else {
                continue;
            };
            if !is_direct_connect_mcp_url(remote_url) {
                continue;
            }
            let has_req_fields = meta
                .and_then(|m| m.get("requiredFields"))
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if has_req_fields {
                continue;
            }
            let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let slug = meta
                .and_then(|m| m.get("slug"))
                .and_then(|v| v.as_str())
                .unwrap_or(name);
            let title = s
                .get("title")
                .and_then(|v| v.as_str())
                .or_else(|| meta.and_then(|m| m.get("displayName")).and_then(|v| v.as_str()))
                .unwrap_or(name);
            let description = s
                .get("description")
                .and_then(|v| v.as_str())
                .or_else(|| meta.and_then(|m| m.get("oneLiner")).and_then(|v| v.as_str()))
                .unwrap_or("");
            let transport = if remote.get("type").and_then(|v| v.as_str()) == Some("sse") {
                "sse"
            } else {
                "http"
            };
            let is_authless = meta
                .and_then(|m| m.get("isAuthless"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let tool_names: Vec<Value> = meta
                .and_then(|m| m.get("toolNames"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let tool_count = tool_names.len();
            let icon_url = meta
                .and_then(|m| m.get("iconUrl"))
                .cloned()
                .unwrap_or(Value::Null);

            out.push(json!({
                "name": name,
                "slug": slug,
                "title": title,
                "description": description,
                "url": remote_url,
                "transport": transport,
                "oauth": !is_authless,
                "toolNames": tool_names,
                "toolCount": tool_count,
                "iconUrl": icon_url,
            }));
        }
    }
    j.get("metadata")
        .and_then(|m| m.get("nextCursor"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

async fn fetch_all_mcp_registry(client: &reqwest::Client) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _ in 0..20 {
        let mut req_url = format!(
            "{}?limit=500&visibility={}",
            MCP_REGISTRY_URL, MCP_REGISTRY_VISIBILITY
        );
        if !cursor.is_empty() {
            let encoded: String =
                url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect();
            req_url.push_str(&format!("&cursor={}", encoded));
        }
        let resp = client
            .get(&req_url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            break;
        }
        let j: Value = resp.json().await.map_err(|e| e.to_string())?;
        match parse_mcp_registry_page(&j, &mut out) {
            Some(next) => cursor = next,
            None => break,
        }
    }
    let mut seen = HashSet::new();
    out.retain(|item| {
        item.get("url")
            .and_then(|u| u.as_str())
            .map(|u| seen.insert(u.to_string()))
            .unwrap_or(false)
    });
    Ok(out)
}

async fn cowork_mcp_registry_get(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let force = params.get("refresh").map(|s| s == "1").unwrap_or(false);
    if !force {
        let guard = registry_cache().read().await;
        if let Some(ref c) = *guard {
            if c.fetched_at.elapsed() < MCP_REGISTRY_CACHE_TTL {
                return Json(json!({
                    "cached": true,
                    "servers": c.servers,
                    "total": c.servers.len(),
                }))
                .into_response();
            }
        }
    }

    let client = state.proxy.client();
    match fetch_all_mcp_registry(client).await {
        Ok(servers) => {
            let total = servers.len();
            let mut guard = registry_cache().write().await;
            *guard = Some(RegistryCache {
                fetched_at: Instant::now(),
                servers: servers.clone(),
            });
            Json(json!({
                "cached": false,
                "servers": servers,
                "total": total,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e, "servers": [], "total": 0 })),
        )
            .into_response(),
    }
}

// ============================================================================
// 8. /api/cli-tools/cowork-mcp-tools
// ============================================================================

fn is_loopback_host_str(raw: &str) -> bool {
    let mut name = raw.trim().to_ascii_lowercase();
    if name.starts_with('[') {
        if let Some(end) = name.find(']') {
            name = name[1..end].to_string();
        } else {
            return false;
        }
    } else if let Some(first_colon) = name.find(':') {
        if name.rfind(':') == Some(first_colon) {
            name = name[..first_colon].to_string();
        }
    }
    if let Some(v4) = name.strip_prefix("::ffff:") {
        name = v4.to_string();
    }
    matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn is_local_request(headers: &HeaderMap) -> bool {
    if headers.contains_key("x-9r-via-proxy") {
        return false;
    }
    let peer_ok = if let Some(real_ip) = headers.get("x-9r-real-ip").and_then(|v| v.to_str().ok())
    {
        is_loopback_host_str(real_ip)
    } else if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        is_loopback_host_str(host)
    } else {
        // Direct in-process calls (e.g., unit tests without Host header)
        true
    };
    if !peer_ok {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let Ok(u) = url::Url::parse(origin) else {
            return false;
        };
        let Some(h) = u.host_str() else {
            return false;
        };
        if !is_loopback_host_str(h) {
            return false;
        }
    }
    true
}

fn is_public_literal_url(raw_url: &str) -> Result<(), &'static str> {
    let parsed = url::Url::parse(raw_url).map_err(|_| "Invalid URL")?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("URL not allowed");
    }
    let host = parsed
        .host_str()
        .ok_or("URL not allowed")?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if matches!(
        host.as_str(),
        "localhost" | "ip6-localhost" | "ip6-loopback"
    ) || host.ends_with(".internal")
        || host.ends_with(".local")
        || host.ends_with(".localhost")
    {
        return Err("Blocked URL: internal host");
    }
    let bracketless = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bracketless.parse::<IpAddr>() {
        if is_blocked_ip_addr(ip) {
            return Err("Blocked URL: internal host");
        }
    }
    Ok(())
}

fn is_blocked_ip_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let oct = v4.octets();
            let u = u32::from(v4);
            oct[0] == 0
                || oct[0] == 10
                || oct[0] == 127
                || (oct[0] == 100 && (oct[1] & 0b1100_0000) == 64)
                || (oct[0] == 169 && oct[1] == 254)
                || (oct[0] == 172 && (16..=31).contains(&oct[1]))
                || (oct[0] == 192 && oct[1] == 168)
                || u == 0
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip_addr(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            (seg[0] & 0xffc0) == 0xfe80 || (seg[0] & 0xfe00) == 0xfc00
        }
    }
}

fn parse_mcp_tools_from_response(ct: &str, text: &str) -> Vec<Value> {
    let parsed: Option<Value> = if ct.contains("text/event-stream") {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .find_map(|data| {
                let obj: Value = serde_json::from_str(data.trim()).ok()?;
                if obj.get("id").and_then(|v| v.as_i64()) == Some(2) && obj.get("result").is_some()
                {
                    Some(obj)
                } else {
                    None
                }
            })
    } else {
        serde_json::from_str(text).ok()
    };

    parsed
        .and_then(|p| p.get("result")?.get("tools")?.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(|v| v.as_str())?;
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(json!({ "name": name, "description": description }))
        })
        .collect()
}

async fn probe_mcp(client: &reqwest::Client, url: &str) -> Value {
    let init_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "9router", "version": "1" }
        }
    });

    let init_res = match client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(Duration::from_secs(8))
        .json(&init_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                "timeout".to_string()
            } else {
                e.to_string()
            };
            return json!({ "error": msg, "tools": [] });
        }
    };

    let status = init_res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return json!({ "requiresAuth": true, "tools": [] });
    }
    if !status.is_success() {
        return json!({ "error": format!("init {}", status.as_u16()), "tools": [] });
    }

    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let _ = init_res.text().await;

    let mut notif_req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(Duration::from_secs(8))
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }));
    if !session_id.is_empty() {
        notif_req = notif_req.header("mcp-session-id", &session_id);
    }
    let _ = notif_req.send().await;

    let mut list_req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(Duration::from_secs(8))
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    if !session_id.is_empty() {
        list_req = list_req.header("mcp-session-id", &session_id);
    }

    let list_res = match list_req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                "timeout".to_string()
            } else {
                e.to_string()
            };
            return json!({ "error": msg, "tools": [] });
        }
    };

    if list_res.status() == reqwest::StatusCode::UNAUTHORIZED
        || list_res.status() == reqwest::StatusCode::FORBIDDEN
    {
        return json!({ "requiresAuth": true, "tools": [] });
    }
    let ct = list_res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = list_res.text().await.unwrap_or_default();
    let tools = parse_mcp_tools_from_response(&ct, &text);
    json!({ "tools": tools })
}

async fn cowork_mcp_tools_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Invalid JSON body", "tools": [] })),
        )
            .into_response();
    };
    let Some(target_url) = raw.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
        return json_err(StatusCode::BAD_REQUEST, "url required");
    };

    if !is_local_request(&headers) && is_public_literal_url(target_url).is_err() {
        return json_err(StatusCode::BAD_REQUEST, "URL not allowed");
    }

    let client = state.proxy.client();
    Json(probe_mcp(client, target_url).await).into_response()
}

// ============================================================================
// 9. /api/cli-tools/cowork-settings
// ============================================================================

const CLI_TOKEN_HEADER: &str = "x-9r-cli-token";
const CLI_TOKEN_SALT: &str = "9r-cli-auth";

fn default_cowork_plugins() -> Vec<Value> {
    vec![
        json!({
            "name": "exa",
            "title": "Exa",
            "description": "Real-time web search and code documentation",
            "url": "https://mcp.exa.ai/mcp",
            "transport": "http",
            "oauth": false,
            "toolNames": ["web_search_exa", "web_fetch_exa"],
        }),
        json!({
            "name": "tavily",
            "title": "Tavily",
            "description": "Real-time web search optimized for LLM agents",
            "url": "https://mcp.tavily.com/mcp",
            "transport": "http",
            "oauth": true,
            "toolNames": ["tavily_search", "tavily_extract", "tavily_crawl", "tavily_map"],
        }),
    ]
}

fn local_stdio_plugins() -> Vec<Value> {
    vec![json!({
        "name": "browsermcp",
        "title": "Browser MCP",
        "description": "Control your running Chrome (requires Chrome extension)",
        "extensionUrl": "https://chromewebstore.google.com/detail/browser-mcp-automate-your/bjfgambnhccakkhmkepdoekmckoijdlc",
        "command": "npx",
        "args": ["-y", "@browsermcp/mcp@latest"],
        "toolNames": [
            "browser_navigate", "browser_snapshot", "browser_click", "browser_type",
            "browser_screenshot", "browser_get_console_logs", "browser_wait",
            "browser_press_key", "browser_go_back", "browser_go_forward"
        ],
    })]
}

fn random_uuid_v4() -> String {
    let rng = SystemRandom::new();
    let mut b = [0u8; 16];
    let _ = rng.fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn load_or_create_raw_machine_id(data_dir: &Path) -> String {
    let file = data_dir.join("machine-id");
    if let Ok(s) = std::fs::read_to_string(&file) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let id = random_uuid_v4();
    let _ = rewrite_file(&file, &id);
    id
}

fn load_or_create_cli_secret(data_dir: &Path) -> String {
    let file = data_dir.join("auth").join("cli-secret");
    if let Ok(s) = std::fs::read_to_string(&file) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let rng = SystemRandom::new();
    let mut b = [0u8; 32];
    let _ = rng.fill(&mut b);
    let hex: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
    let _ = rewrite_file(&file, &hex);
    hex
}

fn compute_cli_token(data_dir: &Path) -> String {
    let raw = load_or_create_raw_machine_id(data_dir);
    let extra = load_or_create_cli_secret(data_dir);
    let combined = format!("{}{}{}", raw, CLI_TOKEN_SALT, extra);
    let d = digest::digest(&digest::SHA256, combined.as_bytes());
    let hex: String = d.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    hex.chars().take(16).collect()
}

fn cowork_candidate_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let base = home_dir().join("Library").join("Application Support");
        vec![base.join("Claude-3p"), base.join("Claude")]
    }
    #[cfg(target_os = "windows")]
    {
        let local_app = std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
        let roaming = std::env::var("APPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"));
        vec![
            local_app.join("Claude-3p"),
            roaming.join("Claude-3p"),
            local_app.join("Claude"),
            roaming.join("Claude"),
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        vec![
            home_dir().join(".config").join("Claude-3p"),
            home_dir().join(".config").join("Claude"),
        ]
    }
}

fn cowork_app_install_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Applications/Claude.app"),
            home_dir().join("Applications").join("Claude.app"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        let local_app = std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
        let prog = std::env::var("ProgramFiles")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        vec![
            local_app.join("AnthropicClaude"),
            prog.join("Claude"),
            prog.join("AnthropicClaude"),
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

fn cowork_1p_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Claude")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("Claude")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home_dir().join(".config").join("Claude")
    }
}

fn cowork_resolve_read_root() -> PathBuf {
    let candidates = cowork_candidate_roots();
    for dir in &candidates {
        if dir.join("configLibrary").exists() {
            return dir.clone();
        }
    }
    candidates.into_iter().next().unwrap()
}

fn cowork_write_root() -> PathBuf {
    cowork_candidate_roots().into_iter().next().unwrap()
}

fn cowork_installed() -> bool {
    cowork_candidate_roots()
        .into_iter()
        .chain(cowork_app_install_paths())
        .any(|p| p.exists())
}

fn build_managed_mcp_servers(plugins: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for p in plugins {
        let Some(name) = p.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(url) = p.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        if !seen.insert(name.to_string()) {
            continue;
        }
        let transport = p
            .get("transport")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if url.contains("/sse") {
                    "sse"
                } else {
                    "http"
                }
            });
        let mut entry = Map::new();
        entry.insert("name".to_string(), Value::String(name.to_string()));
        entry.insert("url".to_string(), Value::String(url.to_string()));
        entry.insert("transport".to_string(), Value::String(transport.to_string()));
        if p.get("oauth").and_then(|v| v.as_bool()) == Some(true) {
            entry.insert("oauth".to_string(), Value::Bool(true));
        }
        if let Some(tool_names) = p.get("toolNames").and_then(|v| v.as_array()) {
            if !tool_names.is_empty() {
                let prefix = format!("{}-", name);
                let mut bare = Vec::new();
                let mut bare_seen = HashSet::new();
                for raw in tool_names {
                    let Some(s) = raw.as_str().filter(|x| !x.is_empty()) else {
                        continue;
                    };
                    let mut t = s;
                    while let Some(rest) = t.strip_prefix(&prefix) {
                        t = rest;
                    }
                    if bare_seen.insert(t.to_string()) {
                        bare.push(t.to_string());
                    }
                }
                let mut policy = Map::new();
                for t in bare {
                    policy.insert(t.clone(), Value::String("allow".to_string()));
                    policy.insert(
                        format!("{}{}", prefix, t),
                        Value::String("allow".to_string()),
                    );
                }
                entry.insert("toolPolicy".to_string(), Value::Object(policy));
            }
        }
        out.push(Value::Object(entry));
    }
    out
}

fn build_local_bridge_entries(names: &[Value], app_port: u16, token: &str) -> Vec<Value> {
    let presets = local_stdio_plugins();
    let mut out = Vec::new();
    for n_val in names {
        let Some(name) = n_val.as_str() else {
            continue;
        };
        let Some(def) = presets
            .iter()
            .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(name))
        else {
            continue;
        };
        let url = format!("http://localhost:{}/api/mcp/{}/sse", app_port, name);
        let mut entry = Map::new();
        entry.insert("name".to_string(), Value::String(name.to_string()));
        entry.insert("url".to_string(), Value::String(url));
        entry.insert("transport".to_string(), Value::String("sse".to_string()));
        entry.insert(
            "headers".to_string(),
            json!({ CLI_TOKEN_HEADER: token }),
        );
        if let Some(tool_names) = def.get("toolNames").and_then(|v| v.as_array()) {
            if !tool_names.is_empty() {
                let prefix = format!("{}-", name);
                let mut policy = Map::new();
                for t_val in tool_names {
                    if let Some(t) = t_val.as_str() {
                        policy.insert(t.to_string(), Value::String("allow".to_string()));
                        policy.insert(
                            format!("{}{}", prefix, t),
                            Value::String("allow".to_string()),
                        );
                    }
                }
                entry.insert("toolPolicy".to_string(), Value::Object(policy));
            }
        }
        out.push(Value::Object(entry));
    }
    out
}

fn build_custom_entries(custom_plugins: &[Value], app_port: u16, token: &str) -> Vec<Value> {
    let local_prefix = format!("http://localhost:{}/api/mcp/", app_port);
    let mut out = Vec::new();
    for p in custom_plugins {
        let Some(name) = p.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(url) = p.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let transport = p
            .get("transport")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("sse");
        let mut entry = Map::new();
        entry.insert("name".to_string(), Value::String(name.to_string()));
        entry.insert("url".to_string(), Value::String(url.to_string()));
        entry.insert("transport".to_string(), Value::String(transport.to_string()));
        entry.insert("custom".to_string(), Value::Bool(true));
        if url.starts_with(&local_prefix) {
            entry.insert(
                "headers".to_string(),
                json!({ CLI_TOKEN_HEADER: token }),
            );
        }
        out.push(Value::Object(entry));
    }
    out
}

fn cleanup_1p_legacy() {
    let cfg_path = cowork_1p_root().join("claude_desktop_config.json");
    let mut cfg = read_json_obj_optional(&cfg_path).unwrap_or_default();
    if let Some(mcp) = cfg.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        for p in local_stdio_plugins() {
            if let Some(name) = p.get("name").and_then(|v| v.as_str()) {
                mcp.remove(name);
            }
        }
        if mcp.is_empty() {
            cfg.remove("mcpServers");
        }
        let _ = write_json_pretty_no_backup(&cfg_path, &Value::Object(cfg));
    }
}

fn write_skip_approvals(managed_servers: &[Value]) -> Result<usize, String> {
    let cfg_path = cowork_write_root().join("config.json");
    let mut cfg = read_json_or_empty_obj(&cfg_path)?;
    let mut skip = Map::new();
    for srv in managed_servers {
        if let Some(name) = srv.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            skip.insert(name.to_string(), Value::Bool(true));
        }
    }
    let count = skip.len();
    cfg.insert("operonSkipMcpApprovals".to_string(), Value::Object(skip));
    write_json_pretty_no_backup(&cfg_path, &Value::Object(cfg))?;
    Ok(count)
}

async fn cowork_get() -> Response {
    if !cowork_installed() {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Claude Desktop (Cowork mode) not detected"
        }))
        .into_response();
    }

    let config_dir = cowork_resolve_read_root().join("configLibrary");
    let meta = read_json_obj_optional(&config_dir.join("_meta.json"));
    let applied_id = meta
        .as_ref()
        .and_then(|m| m.get("appliedId"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let config_path = applied_id
        .as_ref()
        .map(|id| config_dir.join(format!("{}.json", id)));
    let config = config_path.as_ref().and_then(|p| read_jsonc_value(p));

    let base_url = config
        .as_ref()
        .and_then(|c| c.get("inferenceGatewayBaseUrl"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let models: Vec<String> = config
        .as_ref()
        .and_then(|c| c.get("inferenceModels"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.as_str()
                        .or_else(|| m.get("name").and_then(|n| n.as_str()))
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    let managed_mcp = config
        .as_ref()
        .and_then(|c| c.get("managedMcpServers"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let has_9router = config
        .as_ref()
        .and_then(|c| c.get("inferenceProvider"))
        .and_then(|v| v.as_str())
        == Some("gateway")
        && base_url.as_ref().map(|s| !s.is_empty()).unwrap_or(false);

    let stdio_defs = local_stdio_plugins();
    let stdio_names: HashSet<&str> = stdio_defs
        .iter()
        .filter_map(|p| p.get("name").and_then(|v| v.as_str()))
        .collect();
    let defaults = default_cowork_plugins();

    let mut active_local = Vec::new();
    let mut active_custom = Vec::new();
    let mut plugins = Vec::new();

    for m in &managed_mcp {
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = m.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let is_bridge = url.contains("/api/mcp/");
        let is_stdio = stdio_names.contains(name) && is_bridge;
        let is_custom = m.get("custom").and_then(|v| v.as_bool()) == Some(true)
            || (!stdio_names.contains(name) && is_bridge);

        if is_stdio {
            active_local.push(Value::String(name.to_string()));
        } else if is_custom {
            active_custom.push(json!({
                "name": name,
                "url": url,
                "transport": m.get("transport").cloned().unwrap_or(Value::Null),
                "custom": true,
            }));
        } else {
            let prefix = format!("{}-", name);
            let mut bare = Vec::new();
            let mut bare_seen = HashSet::new();
            if let Some(policy) = m.get("toolPolicy").and_then(|v| v.as_object()) {
                for k in policy.keys() {
                    let mut t = k.as_str();
                    while let Some(rest) = t.strip_prefix(&prefix) {
                        t = rest;
                    }
                    if bare_seen.insert(t.to_string()) {
                        bare.push(Value::String(t.to_string()));
                    }
                }
            }
            let def_tools = defaults
                .iter()
                .find(|d| d.get("name").and_then(|v| v.as_str()) == Some(name))
                .and_then(|d| d.get("toolNames"))
                .and_then(|v| v.as_array())
                .cloned();
            let tool_names = def_tools.unwrap_or(bare);
            plugins.push(json!({
                "name": name,
                "url": url,
                "transport": m.get("transport").cloned().unwrap_or(Value::Null),
                "oauth": m.get("oauth").and_then(|v| v.as_bool()).unwrap_or(false),
                "toolNames": tool_names,
            }));
        }
    }

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.map(|p| p.to_string_lossy().to_string()),
        "cowork": {
            "appliedId": applied_id,
            "baseUrl": base_url,
            "models": models,
            "provider": config.as_ref().and_then(|c| c.get("inferenceProvider")).cloned().unwrap_or(Value::Null),
            "plugins": plugins,
            "localPlugins": active_local,
            "customPlugins": active_custom,
        },
        "defaultPlugins": defaults,
        "localStdioPlugins": stdio_defs,
    }))
    .into_response()
}

async fn cowork_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to apply cowork settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() || api_key.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and apiKey are required");
    }

    let models_array: Vec<String> = raw
        .get("models")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.as_str().map(str::trim).filter(|s| !s.is_empty()))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if models_array.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "At least one model is required");
    }

    let plugins_array = match raw.get("plugins").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => default_cowork_plugins(),
    };
    let local_plugins = raw
        .get("localPlugins")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let custom_plugins: Vec<Value> = raw
        .get("customPlugins")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|p| {
                    p.get("url")
                        .and_then(|u| u.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let token = compute_cli_token(&state.config.data_dir);
    let app_port = state.config.port;
    let bridge_entries = build_local_bridge_entries(&local_plugins, app_port, &token);
    let custom_entries = build_custom_entries(&custom_plugins, app_port, &token);
    let mut managed_mcp_servers = build_managed_mcp_servers(&plugins_array);
    managed_mcp_servers.extend(bridge_entries);
    managed_mcp_servers.extend(custom_entries);

    // Bootstrap deploymentMode = "3p"
    let cfg_1p_path = cowork_1p_root().join("claude_desktop_config.json");
    let mut cfg_1p = read_json_obj_optional(&cfg_1p_path).unwrap_or_default();
    let bootstrapped = cfg_1p.get("deploymentMode").and_then(|v| v.as_str()) != Some("3p");
    if bootstrapped {
        cfg_1p.insert(
            "deploymentMode".to_string(),
            Value::String("3p".to_string()),
        );
        if write_json_pretty_no_backup(&cfg_1p_path, &Value::Object(cfg_1p)).is_err() {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to apply cowork settings",
            );
        }
    }

    // Ensure _meta.json
    let write_cfg_dir = cowork_write_root().join("configLibrary");
    let write_meta_path = write_cfg_dir.join("_meta.json");
    let read_meta_path = cowork_resolve_read_root()
        .join("configLibrary")
        .join("_meta.json");
    let mut meta = read_json_obj_optional(&write_meta_path)
        .or_else(|| read_json_obj_optional(&read_meta_path))
        .unwrap_or_default();
    let applied_id = match meta
        .get("appliedId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(id) => id.to_string(),
        None => {
            let new_id = random_uuid_v4();
            meta.insert("appliedId".to_string(), Value::String(new_id.clone()));
            meta.insert(
                "entries".to_string(),
                json!([{ "id": new_id, "name": "Default" }]),
            );
            new_id
        }
    };
    if write_json_pretty_no_backup(&write_meta_path, &Value::Object(meta)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to apply cowork settings",
        );
    }

    let config_path = write_cfg_dir.join(format!("{}.json", applied_id));
    let mut new_cfg = serde_json::json!({
        "coworkEgressAllowedHosts": ["*"],
        "disabledBuiltinTools": [],
        "isLocalDevMcpEnabled": true,
        "isDesktopExtensionEnabled": true,
        "isDesktopExtensionDirectoryEnabled": true,
        "isDesktopExtensionSignatureRequired": false,
        "isClaudeCodeForDesktopEnabled": true,
        "disableEssentialTelemetry": true,
        "disableNonessentialTelemetry": true,
        "disableNonessentialServices": true,
        "inferenceProvider": "gateway",
        "inferenceGatewayBaseUrl": base_url,
        "inferenceGatewayApiKey": api_key,
        "inferenceModels": models_array.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
    });
    if !managed_mcp_servers.is_empty() {
        new_cfg
            .as_object_mut()
            .unwrap()
            .insert("managedMcpServers".to_string(), Value::Array(managed_mcp_servers.clone()));
    }
    if write_json_pretty_with_backup(&config_path, &new_cfg).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to apply cowork settings",
        );
    }

    let skip_result = match write_skip_approvals(&managed_mcp_servers) {
        Ok(written) => json!({ "written": written }),
        Err(e) => json!({ "error": e }),
    };
    cleanup_1p_legacy();

    let local_names: Vec<Value> = local_plugins
        .iter()
        .filter(|v| v.is_string())
        .cloned()
        .collect();

    Json(json!({
        "success": true,
        "bootstrapped": bootstrapped,
        "message": if bootstrapped {
            "Cowork enabled (3p mode set). Quit & reopen Claude Desktop."
        } else {
            "Cowork settings applied. Quit & reopen Claude Desktop."
        },
        "configPath": config_path.to_string_lossy(),
        "skipApprovals": skip_result,
        "localMcp": { "applied": local_names, "via": "3p-sse-bridge" },
    }))
    .into_response()
}

async fn cowork_delete() -> Response {
    let config_dir = cowork_resolve_read_root().join("configLibrary");
    let meta = read_json_obj_optional(&config_dir.join("_meta.json"));
    let Some(applied_id) = meta
        .as_ref()
        .and_then(|m| m.get("appliedId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Json(json!({
            "success": true,
            "message": "No active config to reset"
        }))
        .into_response();
    };

    let config_path = config_dir.join(format!("{}.json", applied_id));
    if config_path.exists() {
        if write_json_pretty_no_backup(&config_path, &json!({})).is_err() {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset cowork settings",
            );
        }
    }
    let _ = write_skip_approvals(&[]);
    cleanup_1p_legacy();

    Json(json!({
        "success": true,
        "message": "Cowork config reset"
    }))
    .into_response()
}

// ============================================================================
// 10. /api/cli-tools/crush-settings
// ============================================================================

fn crush_config_path() -> PathBuf {
    xdg_config_home().join("crush").join("crush.json")
}

fn crush_has_9router(cfg: Option<&Map<String, Value>>) -> bool {
    let Some(providers) = cfg.and_then(|c| c.get("providers")).and_then(|v| v.as_object()) else {
        return false;
    };
    if providers
        .get("9router")
        .and_then(|p| p.get("base_url"))
        .and_then(|b| b.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    providers.values().any(|prov| {
        prov.get("base_url")
            .and_then(|b| b.as_str())
            .map(|s| s.contains("20128"))
            .unwrap_or(false)
    })
}

async fn crush_get() -> Response {
    let config_path = crush_config_path();
    if !is_tool_installed("crush", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Crush CLI is not installed"
        }))
        .into_response();
    }

    let config = read_jsonc_value(&config_path);
    let has_9router = crush_has_9router(config.as_ref().and_then(|v| v.as_object()));
    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn crush_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }

    let config_path = crush_config_path();
    let mut existing = read_json_obj_optional(&config_path).unwrap_or_default();
    let mut providers = existing
        .get("providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let model_id = raw
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("provider/model-id");
    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));

    providers.insert(
        "9router".to_string(),
        json!({
            "type": "openai-compat",
            "base_url": v1_url(base_url),
            "api_key": api_key,
            "models": [{
                "id": model_id,
                "name": model_id,
                "context_window": 128000,
            }]
        }),
    );
    existing.insert("providers".to_string(), Value::Object(providers));

    if let Err(e) = write_json_pretty_with_backup(&config_path, &Value::Object(existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "Crush settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn crush_delete() -> Response {
    let config_path = crush_config_path();
    if !config_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    }

    let Some(mut existing) = read_json_obj_optional(&config_path) else {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    };

    if let Some(providers) = existing.get_mut("providers").and_then(|v| v.as_object_mut()) {
        if providers.remove("9router").is_some() {
            if providers.is_empty() {
                existing.remove("providers");
            }
            if let Err(e) = write_json_pretty_no_backup(&config_path, &Value::Object(existing)) {
                return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
            }
        }
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from Crush"
    }))
    .into_response()
}

// ============================================================================
// 11. /api/cli-tools/deepseek-tui-settings
// ============================================================================

fn deepseek_config_path() -> PathBuf {
    home_dir().join(".deepseek").join("config.toml")
}

/// Flat-section TOML parser matching JS `deepseek-tui-settings/route.js` (e.g. key `"providers.openai"`).
fn parse_deepseek_toml(content: &str) -> Map<String, Value> {
    let mut root = Map::new();
    let mut current_section: Option<String> = None;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section_name = trimmed[1..trimmed.len() - 1].trim().to_string();
            if !root.contains_key(&section_name) {
                root.insert(section_name.clone(), Value::Object(Map::new()));
            }
            current_section = Some(section_name);
            continue;
        }
        if let Some((k, v)) = split_toml_eq(trimmed) {
            let val = Value::String(unquote_toml_str(v));
            if let Some(ref sec) = current_section {
                if let Some(obj) = root.get_mut(sec).and_then(|s| s.as_object_mut()) {
                    obj.insert(k.to_string(), val);
                }
            } else {
                root.insert(k.to_string(), val);
            }
        }
    }
    root
}

fn deepseek_has_9router(cfg: &Map<String, Value>) -> bool {
    if get_str_field(cfg, "provider") != Some("openai") {
        return false;
    }
    let base_url = cfg
        .get("providers.openai")
        .and_then(|v| v.as_object())
        .and_then(|o| get_str_field(o, "base_url"))
        .unwrap_or("");
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("0.0.0.0")
}

async fn deepseek_tui_get() -> Response {
    let config_path = deepseek_config_path();
    if !is_tool_installed("deepseek", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "DeepSeek TUI is not installed"
        }))
        .into_response();
    }

    let toml_text = std::fs::read_to_string(&config_path).unwrap_or_default();
    let config = parse_deepseek_toml(&toml_text);
    let has_9router = deepseek_has_9router(&config);
    Json(json!({
        "installed": true,
        "settings": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn deepseek_tui_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update deepseek-tui settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() || model.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and model are required");
    }

    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let content = format!(
        "provider = \"openai\"\n\n[providers.openai]\nbase_url = \"{}\"\napi_key = \"{}\"\nmodel = \"{}\"\n",
        v1_url(base_url),
        api_key,
        model
    );
    let config_path = deepseek_config_path();
    if write_file_with_backup(&config_path, &content).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update deepseek-tui settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "DeepSeek TUI settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn deepseek_tui_delete() -> Response {
    let config_path = deepseek_config_path();
    if !config_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    }

    if rewrite_file(&config_path, "provider = \"deepseek\"\n").is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset deepseek-tui settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9router config reset to DeepSeek defaults"
    }))
    .into_response()
}

// ============================================================================
// 12. /api/cli-tools/devin-settings
// ============================================================================

fn candidate_devin_paths() -> Vec<PathBuf> {
    let home = home_dir();
    #[cfg(target_os = "windows")]
    {
        let local_app = std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"));
        vec![
            local_app.join("devin").join("cli").join("bin").join("devin.exe"),
            home.join(".local").join("bin").join("devin.exe"),
            home.join("scoop").join("shims").join("devin.exe"),
            local_app.join("Programs").join("devin").join("devin.exe"),
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        vec![
            home.join(".local").join("share").join("devin").join("bin").join("devin"),
            home.join(".devin").join("bin").join("devin"),
            home.join(".local").join("bin").join("devin"),
            PathBuf::from("/opt/homebrew/bin/devin"),
            PathBuf::from("/usr/local/bin/devin"),
            PathBuf::from("/usr/bin/devin"),
        ]
    }
}

async fn devin_get() -> Response {
    let mut found: Option<(String, PathBuf)> = None;
    if let Some(p) = find_in_path("devin") {
        found = Some(("path".to_string(), p));
    } else {
        for cand in candidate_devin_paths() {
            if cand.exists() {
                found = Some((cand.to_string_lossy().to_string(), cand));
                break;
            }
        }
    }

    let Some((source, bin_path)) = found else {
        return Json(json!({
            "installed": false,
            "message": "Devin CLI is not installed. Install it from https://cli.devin.ai and run `devin auth login`.",
            "installUrl": "https://cli.devin.ai",
        }))
        .into_response();
    };

    let version = Command::new(&bin_path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    Json(json!({
        "installed": true,
        "source": source,
        "version": version,
        "message": "Devin CLI detected. Make sure `devin auth login` has been run.",
    }))
    .into_response()
}

// ============================================================================
// 13. /api/cli-tools/droid-settings
// ============================================================================

fn droid_settings_path() -> PathBuf {
    home_dir().join(".factory").join("settings.json")
}

fn droid_has_9router(settings: Option<&Map<String, Value>>) -> bool {
    settings
        .and_then(|s| s.get("customModels"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().any(|m| {
                m.get("id")
                    .and_then(|id| id.as_str())
                    .map(|s| s.starts_with("custom:9Router"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

async fn droid_get() -> Response {
    let settings_path = droid_settings_path();
    if !is_tool_installed("droid", &[settings_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Factory Droid CLI is not installed"
        }))
        .into_response();
    }

    let settings = read_jsonc_value(&settings_path);
    let has_9router = droid_has_9router(settings.as_ref().and_then(|v| v.as_object()));
    Json(json!({
        "installed": true,
        "settings": settings,
        "has9Router": has_9router,
        "settingsPath": settings_path.to_string_lossy(),
    }))
    .into_response()
}

async fn droid_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update droid settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let models_array: Vec<String> = match raw.get("models").and_then(|v| v.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect(),
        None => raw
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };

    if base_url.is_empty() || models_array.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "baseUrl and at least one model are required",
        );
    }

    let settings_path = droid_settings_path();
    let mut settings = read_json_obj_optional(&settings_path).unwrap_or_default();
    let mut custom_models: Vec<Value> = settings
        .get("customModels")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| {
            !m.get("id")
                .and_then(|id| id.as_str())
                .map(|s| s.starts_with("custom:9Router"))
                .unwrap_or(false)
        })
        .collect();

    let normalized_base = v1_url(base_url);
    let key_to_use = raw
        .get("apiKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("your_api_key");

    let mut default_idx: isize = 0;
    if let Some(active_model) = raw.get("activeModel").and_then(|v| v.as_str()) {
        if active_model.is_empty() {
            default_idx = -1;
        } else if let Some(pos) = models_array.iter().position(|m| m == active_model) {
            default_idx = pos as isize;
        }
    }

    let insert_start = custom_models.len();
    for (i, m) in models_array.iter().enumerate() {
        if m.is_empty() {
            continue;
        }
        custom_models.push(json!({
            "model": m,
            "id": format!("custom:9Router-{}", i),
            "index": i,
            "baseUrl": normalized_base,
            "apiKey": key_to_use,
            "displayName": m,
            "maxOutputTokens": 131072,
            "noImageSupport": false,
            "provider": "openai",
        }));
    }

    if default_idx >= 0 {
        let target = insert_start + (default_idx as usize);
        if target < custom_models.len() {
            let chosen = custom_models.remove(target);
            custom_models.insert(0, chosen);
            for (i, item) in custom_models.iter_mut().enumerate() {
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("index".to_string(), json!(i));
                }
            }
        }
    }

    settings.insert("customModels".to_string(), Value::Array(custom_models));
    if write_json_pretty_with_backup(&settings_path, &Value::Object(settings)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update droid settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Factory Droid settings applied successfully!",
        "settingsPath": settings_path.to_string_lossy(),
    }))
    .into_response()
}

async fn droid_delete() -> Response {
    let settings_path = droid_settings_path();
    if !settings_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No settings file to reset"
        }))
        .into_response();
    }

    let mut settings = match read_json_or_empty_obj(&settings_path) {
        Ok(obj) => obj,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset droid settings",
            )
        }
    };

    if let Some(arr) = settings
        .get_mut("customModels")
        .and_then(|v| v.as_array_mut())
    {
        arr.retain(|m| {
            !m.get("id")
                .and_then(|id| id.as_str())
                .map(|s| s.starts_with("custom:9Router"))
                .unwrap_or(false)
        });
        if arr.is_empty() {
            settings.remove("customModels");
        }
    }

    if write_json_pretty_no_backup(&settings_path, &Value::Object(settings)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset droid settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9Router settings removed successfully"
    }))
    .into_response()
}

// ============================================================================
// 14. /api/cli-tools/forge-settings
// ============================================================================

fn forge_config_path() -> PathBuf {
    home_dir().join(".forge").join("config.toml")
}

async fn forge_get() -> Response {
    let config_path = forge_config_path();
    if !is_tool_installed("forge", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "ForgeCode CLI is not installed"
        }))
        .into_response();
    }

    let content = std::fs::read_to_string(&config_path).ok();
    let config = content
        .as_deref()
        .map(|c| Value::Object(parse_toml_to_map(c)));
    let has_9router = content
        .as_deref()
        .map(|c| c.contains("managed by 9Router") || c.contains("localhost:20128"))
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn forge_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }
    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("provider/model-id");

    let config_path = forge_config_path();
    let mut existing = std::fs::read_to_string(&config_path)
        .ok()
        .map(|c| parse_toml_to_map(&c))
        .unwrap_or_default();

    let mut openai = Map::new();
    openai.insert("api_key".to_string(), Value::String(api_key));
    openai.insert("base_url".to_string(), Value::String(v1_url(base_url)));
    openai.insert("model".to_string(), Value::String(model.to_string()));
    existing.insert("openai".to_string(), Value::Object(openai));

    let content = format!(
        "# Forge config — managed by 9Router\n\n{}",
        serialize_toml_map(&existing)
    );
    if let Err(e) = write_file_with_backup(&config_path, &content) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    Json(json!({
        "success": true,
        "message": "ForgeCode settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn forge_delete() -> Response {
    let config_path = forge_config_path();
    let Ok(raw) = std::fs::read_to_string(&config_path) else {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    };
    let mut existing = parse_toml_to_map(&raw);
    existing.remove("openai");

    if existing.is_empty() {
        let _ = std::fs::remove_file(&config_path);
    } else if let Err(e) = rewrite_file(&config_path, &serialize_toml_map(&existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from ForgeCode"
    }))
    .into_response()
}

// ============================================================================
// 15. /api/cli-tools/grok-build-settings (+ model capability helpers)
// ============================================================================

const GROK_MAIN_MODEL_SLOT: &str = "9router";
const GROK_BUILTIN_DEFAULT: &str = "grok-build";
const GROK_SUBAGENT_TYPES: &[&str] = &["general-purpose", "explore", "plan"];
const GROK_UNSET_SENTINEL: &str = "__9router_unset__";

fn grok_config_path() -> PathBuf {
    home_dir().join(".grok").join("config.toml")
}

fn grok_bin_path() -> PathBuf {
    home_dir().join(".grok").join("bin").join("grok")
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Clone, Copy)]
struct ModelCaps {
    reasoning: bool,
    thinking_format: Option<&'static str>,
    thinking_can_disable: bool,
    context_window: u64,
}

impl Default for ModelCaps {
    fn default() -> Self {
        Self {
            reasoning: false,
            thinking_format: None,
            thinking_can_disable: true,
            context_window: 200_000,
        }
    }
}

fn model_caps_for(provider: Option<&str>, model: &str) -> ModelCaps {
    let base_model = model.rsplit('/').next().unwrap_or(model);
    if matches!(provider, Some("commandcode" | "cmc")) {
        return ModelCaps {
            reasoning: true,
            thinking_format: Some("commandcode"),
            thinking_can_disable: true,
            context_window: 1_000_000,
        };
    }

    let patterns: &[(&str, ModelCaps)] = &[
        ("*claude*opus-5*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*claude*sonnet-5*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*claude*opus-4.6*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*opus-4.7*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*opus-4.8*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*sonnet-4.6*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*sonnet-4.7*", ModelCaps { reasoning: true, thinking_format: Some("claude-adaptive"), thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*fable*", ModelCaps { reasoning: true, thinking_format: Some("claude-budget"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*claude*mythos*", ModelCaps { reasoning: true, thinking_format: Some("claude-budget"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*claude-3*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 200_000 }),
        ("*claude*", ModelCaps { reasoning: true, thinking_format: Some("claude-budget"), thinking_can_disable: true, context_window: 200_000 }),
        ("*gemini-4*", ModelCaps { reasoning: true, thinking_format: Some("gemini-level"), thinking_can_disable: false, context_window: 1_048_576 }),
        ("*gemini-3*", ModelCaps { reasoning: true, thinking_format: Some("gemini-level"), thinking_can_disable: false, context_window: 1_048_576 }),
        ("*gemini-2.5*", ModelCaps { reasoning: true, thinking_format: Some("gemini-budget"), thinking_can_disable: true, context_window: 1_048_576 }),
        ("*gemini*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 1_048_576 }),
        ("*gemma*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 128_000 }),
        ("*gpt-6*image*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 200_000 }),
        ("*gpt-6*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 1_050_000 }),
        ("*gpt-5*image*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 200_000 }),
        ("*gpt-5*codex*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 400_000 }),
        ("*gpt-5.4-mini*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 400_000 }),
        ("*gpt-5.4-nano*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 400_000 }),
        ("*gpt-5.4*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 1_050_000 }),
        ("*gpt-5.5*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 1_050_000 }),
        ("*gpt-5.6*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 1_050_000 }),
        ("*gpt-5*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 400_000 }),
        ("*gpt-4.1*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 1_000_000 }),
        ("*gpt-4*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 128_000 }),
        ("*gpt-3.5*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 16_385 }),
        ("*gpt-oss*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 128_000 }),
        ("*o1-mini*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 128_000 }),
        ("*o1*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 200_000 }),
        ("*o3*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 200_000 }),
        ("*o4*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 200_000 }),
        ("*grok-4.7*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 500_000 }),
        ("*grok-4.6*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 500_000 }),
        ("*grok-4.5*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 500_000 }),
        ("*grok-3*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 131_072 }),
        ("*grok*", ModelCaps { reasoning: true, thinking_format: Some("openai"), thinking_can_disable: true, context_window: 256_000 }),
        ("*qwen*coder*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*qwen*max*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*qwen3.5*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*qwen3.6*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*qwen3.7*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*qwq*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: false, context_window: 131_072 }),
        ("*qwen*", ModelCaps { reasoning: true, thinking_format: Some("qwen"), thinking_can_disable: true, context_window: 262_144 }),
        ("*kimi*k3*", ModelCaps { reasoning: true, thinking_format: Some("kimi"), thinking_can_disable: false, context_window: 1_048_576 }),
        ("*kimi*", ModelCaps { reasoning: true, thinking_format: Some("kimi"), thinking_can_disable: true, context_window: 262_144 }),
        ("*glm-5.3*", ModelCaps { reasoning: true, thinking_format: Some("zai"), thinking_can_disable: false, context_window: 1_000_000 }),
        ("*glm-5.2*", ModelCaps { reasoning: true, thinking_format: Some("zai"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*glm*", ModelCaps { reasoning: true, thinking_format: Some("zai"), thinking_can_disable: true, context_window: 200_000 }),
        ("*deepseek-v4*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*reasoner*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: false, context_window: 128_000 }),
        ("*deepseek-r*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: false, context_window: 128_000 }),
        ("*deepseek-chat*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 128_000 }),
        ("*deepseek*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: true, context_window: 128_000 }),
        ("*minimax-m3*", ModelCaps { reasoning: true, thinking_format: Some("minimax"), thinking_can_disable: true, context_window: 1_000_000 }),
        ("*minimax-m2.7*", ModelCaps { reasoning: true, thinking_format: Some("minimax"), thinking_can_disable: false, context_window: 204_800 }),
        ("*minimax-m2.5*", ModelCaps { reasoning: true, thinking_format: Some("minimax"), thinking_can_disable: false, context_window: 204_800 }),
        ("*minimax*", ModelCaps { reasoning: true, thinking_format: Some("minimax"), thinking_can_disable: false, context_window: 200_000 }),
        ("*mimo*v2.6*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: false, context_window: 1_048_576 }),
        ("*mimo*v2.5*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: false, context_window: 1_048_576 }),
        ("*mimo*", ModelCaps { reasoning: true, thinking_format: Some("deepseek"), thinking_can_disable: false, context_window: 262_144 }),
        ("*llama-4*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 1_000_000 }),
        ("*llama*", ModelCaps { reasoning: false, thinking_format: None, thinking_can_disable: true, context_window: 128_000 }),
        ("*hunyuan*", ModelCaps { reasoning: true, thinking_format: Some("hunyuan"), thinking_can_disable: true, context_window: 262_144 }),
        ("*step-*", ModelCaps { reasoning: true, thinking_format: Some("step"), thinking_can_disable: true, context_window: 128_000 }),
        ("*nemotron*", ModelCaps { reasoning: true, thinking_format: None, thinking_can_disable: true, context_window: 128_000 }),
    ];

    for (pat, caps) in patterns {
        if glob_match(pat, base_model) || glob_match(pat, model) {
            return *caps;
        }
    }
    ModelCaps::default()
}

fn normalize_grok_context_window(explicit: Option<&Value>, model: &str) -> u64 {
    if let Some(n) = explicit.and_then(|v| v.as_f64()) {
        if n.is_finite() && n > 0.0 {
            return n.floor() as u64;
        }
    }
    let (provider, model_id) = match model.find('/') {
        Some(idx) if idx > 0 => (Some(&model[..idx]), &model[idx + 1..]),
        _ => (None, model),
    };
    model_caps_for(provider, model_id).context_window
}

fn extract_toml_section_bounds(toml: &str, section: &str) -> Option<(usize, usize, String)> {
    let target_header = format!("[{}]", section);
    let mut offset = 0usize;
    let mut start: Option<usize> = None;
    let mut body_start = 0usize;

    for line in toml.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(s_pos) = start {
            if trimmed.starts_with('[') {
                return Some((s_pos, offset, toml[body_start..offset].to_string()));
            }
        } else if trimmed == target_header {
            start = Some(offset);
            body_start = offset + line.len();
        }
        offset += line.len();
    }

    start.map(|s_pos| (s_pos, toml.len(), toml[body_start..].to_string()))
}

fn grok_get_section_field(toml: &str, section: &str, key: &str) -> Option<String> {
    let (_, _, body) = extract_toml_section_bounds(toml, section)?;
    for line in body.lines() {
        let t = line.trim();
        if let Some((k, v)) = split_toml_eq(t) {
            if k == key && v.starts_with('"') {
                return Some(unquote_toml_str(v));
            }
        }
    }
    None
}

fn grok_get_section_number(toml: &str, section: &str, key: &str) -> Option<u64> {
    let (_, _, body) = extract_toml_section_bounds(toml, section)?;
    for line in body.lines() {
        let t = line.trim();
        if let Some((k, v)) = split_toml_eq(t) {
            if k == key {
                if let Ok(n) = v.trim().parse::<f64>() {
                    if n.is_finite() && n > 0.0 {
                        return Some(n.floor() as u64);
                    }
                }
            }
        }
    }
    None
}

fn grok_set_section_field(toml: &str, section: &str, key: &str, value: &str) -> String {
    let line_str = format!("{} = {}", key, serde_json::to_string(value).unwrap_or_default());
    let Some((s, e, body)) = extract_toml_section_bounds(toml, section) else {
        let prefix = if !toml.is_empty() && !toml.ends_with('\n') {
            format!("{}\n", toml)
        } else {
            toml.to_string()
        };
        return format!("{}\n[{}]\n{}\n", prefix, section, line_str);
    };

    let mut replaced = false;
    let mut new_lines = Vec::new();
    for line in body.lines() {
        if let Some((k, _)) = split_toml_eq(line.trim()) {
            if k == key && !replaced {
                new_lines.push(line_str.clone());
                replaced = true;
                continue;
            }
        }
        new_lines.push(line.to_string());
    }
    let next_body = if replaced {
        let mut s = new_lines.join("\n");
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s
    } else if body.is_empty() {
        format!("{}\n", line_str)
    } else {
        format!("{}\n{}", line_str, body)
    };
    format!("{}[{}]\n{}{}", &toml[..s], section, next_body, &toml[e..])
}

fn grok_delete_section_field(toml: &str, section: &str, key: &str) -> String {
    let Some((s, e, body)) = extract_toml_section_bounds(toml, section) else {
        return toml.to_string();
    };
    let mut kept_lines = Vec::new();
    for line in body.lines() {
        if let Some((k, _)) = split_toml_eq(line.trim()) {
            if k == key {
                continue;
            }
        }
        kept_lines.push(line);
    }
    let next_body = kept_lines.join("\n");
    if next_body.trim().is_empty() {
        let combined = format!("{}{}", &toml[..s], &toml[e..]);
        collapse_triple_newlines(&combined)
    } else {
        let mut b = next_body;
        if !b.ends_with('\n') {
            b.push('\n');
        }
        format!("{}[{}]\n{}{}", &toml[..s], section, b, &toml[e..])
    }
}

fn collapse_triple_newlines(s: &str) -> String {
    let mut out = s.to_string();
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out
}

fn grok_parse_model_section(toml: &str, slot: &str) -> Option<Value> {
    let section = format!("model.{}", slot);
    let (_, _, raw_body) = extract_toml_section_bounds(toml, &section)?;
    Some(json!({
        "model": grok_get_section_field(toml, &section, "model"),
        "base_url": grok_get_section_field(toml, &section, "base_url"),
        "name": grok_get_section_field(toml, &section, "name"),
        "api_key": grok_get_section_field(toml, &section, "api_key"),
        "api_backend": grok_get_section_field(toml, &section, "api_backend"),
        "context_window": grok_get_section_number(toml, &section, "context_window"),
        "raw": raw_body,
    }))
}

fn grok_build_model_section(
    slot: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
    context_window: u64,
    name: &str,
) -> String {
    let mut lines = vec![
        format!("[model.{}]", slot),
        format!("model = {}", serde_json::to_string(model).unwrap()),
        format!("base_url = {}", serde_json::to_string(base_url).unwrap()),
        format!("name = {}", serde_json::to_string(name).unwrap()),
        "description = \"Routed via 9Router gateway\"".to_string(),
        "api_backend = \"chat_completions\"".to_string(),
    ];
    if !api_key.is_empty() {
        lines.push(format!("api_key = {}", serde_json::to_string(api_key).unwrap()));
    }
    if context_window > 0 {
        lines.push(format!("context_window = {}", context_window));
    }
    format!("{}\n", lines.join("\n"))
}

fn grok_upsert_model_section(
    toml: &str,
    slot: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
    context_window: u64,
    name: &str,
) -> String {
    let section_name = format!("model.{}", slot);
    let built = grok_build_model_section(slot, model, base_url, api_key, context_window, name);
    if let Some((s, e, _)) = extract_toml_section_bounds(toml, &section_name) {
        format!("{}{}{}", &toml[..s], built, &toml[e..])
    } else if toml.is_empty() {
        format!("\n{}", built)
    } else if toml.ends_with('\n') {
        format!("{}\n{}", toml, built)
    } else {
        format!("{}\n\n{}", toml, built)
    }
}

fn grok_remove_model_section(toml: &str, slot: &str) -> String {
    let section_name = format!("model.{}", slot);
    if let Some((s, e, _)) = extract_toml_section_bounds(toml, &section_name) {
        let combined = format!("{}{}", &toml[..s], &toml[e..]);
        collapse_triple_newlines(&combined)
    } else {
        toml.to_string()
    }
}

fn grok_insert_marker(toml: &str, marker: &str) -> String {
    let main_sec = format!("model.{}", GROK_MAIN_MODEL_SLOT);
    if let Some((s, _, _)) = extract_toml_section_bounds(toml, &main_sec) {
        format!("{}{}{}", &toml[..s], marker, &toml[s..])
    } else if toml.is_empty() || toml.ends_with('\n') {
        format!("{}{}", toml, marker)
    } else {
        format!("{}\n{}", toml, marker)
    }
}

fn find_marker_value(toml: &str, prefix: &str) -> Option<(usize, usize, String)> {
    let mut offset = 0usize;
    for line in toml.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if let Some(val_part) = rest.strip_prefix(" = ") {
                let val = unquote_toml_str(val_part);
                return Some((offset, offset + line.len(), val));
            }
        }
        offset += line.len();
    }
    None
}

fn grok_remember_prev_default(toml: &str) -> String {
    let prefix = "# 9router-prev-default";
    if find_marker_value(toml, prefix).is_some() {
        return toml.to_string();
    }
    let Some(current) = grok_get_section_field(toml, "models", "default") else {
        return toml.to_string();
    };
    if current == GROK_MAIN_MODEL_SLOT {
        return toml.to_string();
    }
    grok_insert_marker(
        toml,
        &format!(
            "# 9router-prev-default = {}\n",
            serde_json::to_string(&current).unwrap()
        ),
    )
}

fn grok_restore_prev_default(toml: &str) -> String {
    let prefix = "# 9router-prev-default";
    let (next, prev) = if let Some((s, e, val)) = find_marker_value(toml, prefix) {
        let p = if val.is_empty() {
            GROK_BUILTIN_DEFAULT.to_string()
        } else {
            val
        };
        (format!("{}{}", &toml[..s], &toml[e..]), p)
    } else {
        (toml.to_string(), GROK_BUILTIN_DEFAULT.to_string())
    };
    if grok_get_section_field(&next, "models", "default").as_deref() == Some(GROK_MAIN_MODEL_SLOT) {
        grok_set_section_field(&next, "models", "default", &prev)
    } else {
        next
    }
}

fn grok_remember_prev_subagent(toml: &str, typ: &str) -> String {
    let prefix = format!("# 9router-prev-subagent-{}", typ);
    if find_marker_value(toml, &prefix).is_some() {
        return toml.to_string();
    }
    let current = grok_get_section_field(toml, "subagents.models", typ);
    let prev = current.unwrap_or_else(|| GROK_UNSET_SENTINEL.to_string());
    grok_insert_marker(
        toml,
        &format!("{} = {}\n", prefix, serde_json::to_string(&prev).unwrap()),
    )
}

fn grok_restore_prev_subagent(toml: &str, typ: &str) -> String {
    let prefix = format!("# 9router-prev-subagent-{}", typ);
    let slot = format!("{}-{}", GROK_MAIN_MODEL_SLOT, typ);
    let (next, prev) = if let Some((s, e, val)) = find_marker_value(toml, &prefix) {
        let p = if val.is_empty() {
            GROK_UNSET_SENTINEL.to_string()
        } else {
            val
        };
        (format!("{}{}", &toml[..s], &toml[e..]), p)
    } else {
        (toml.to_string(), GROK_UNSET_SENTINEL.to_string())
    };
    if grok_get_section_field(&next, "subagents.models", typ).as_deref() != Some(slot.as_str()) {
        return next;
    }
    if prev == GROK_UNSET_SENTINEL {
        grok_delete_section_field(&next, "subagents.models", typ)
    } else {
        grok_set_section_field(&next, "subagents.models", typ, &prev)
    }
}

pub fn parse_grok_build_config(toml: &str) -> Value {
    let mut subagent_models = Map::new();
    let mut subagent_mappings = Map::new();
    for typ in GROK_SUBAGENT_TYPES {
        let slot = format!("{}-{}", GROK_MAIN_MODEL_SLOT, typ);
        let mapping = grok_get_section_field(toml, "subagents.models", typ);
        subagent_mappings.insert(
            (*typ).to_string(),
            mapping
                .as_ref()
                .map(|m| Value::String(m.clone()))
                .unwrap_or(Value::Null),
        );
        let section_val = if mapping.as_deref() == Some(slot.as_str()) {
            grok_parse_model_section(toml, &slot).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        subagent_models.insert((*typ).to_string(), section_val);
    }

    json!({
        "model": grok_parse_model_section(toml, GROK_MAIN_MODEL_SLOT),
        "default": grok_get_section_field(toml, "models", "default"),
        "subagentModels": Value::Object(subagent_models),
        "subagentMappings": Value::Object(subagent_mappings),
    })
}

pub fn apply_grok_build_config(
    toml: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    context_window: u64,
    subagent_models: Option<&BTreeMap<String, (String, u64)>>,
) -> String {
    let mut next = grok_remember_prev_default(toml);
    next = grok_upsert_model_section(
        &next,
        GROK_MAIN_MODEL_SLOT,
        model,
        base_url,
        api_key,
        context_window,
        "9Router",
    );
    next = grok_set_section_field(&next, "models", "default", GROK_MAIN_MODEL_SLOT);

    if let Some(subs) = subagent_models {
        for typ in GROK_SUBAGENT_TYPES {
            let slot = format!("{}-{}", GROK_MAIN_MODEL_SLOT, typ);
            if let Some((sub_model, sub_ctx)) = subs.get(*typ) {
                next = grok_remember_prev_subagent(&next, typ);
                next = grok_upsert_model_section(
                    &next,
                    &slot,
                    sub_model,
                    base_url,
                    api_key,
                    *sub_ctx,
                    &format!("9Router {}", typ),
                );
                next = grok_set_section_field(&next, "subagents.models", typ, &slot);
            } else {
                next = grok_restore_prev_subagent(&next, typ);
                next = grok_remove_model_section(&next, &slot);
            }
        }
    }
    next
}

pub fn reset_grok_build_config(toml: &str) -> String {
    let mut next = toml.to_string();
    for typ in GROK_SUBAGENT_TYPES {
        next = grok_restore_prev_subagent(&next, typ);
        next = grok_remove_model_section(&next, &format!("{}-{}", GROK_MAIN_MODEL_SLOT, typ));
    }
    next = grok_remove_model_section(&next, GROK_MAIN_MODEL_SLOT);
    next = grok_restore_prev_default(&next);
    collapse_triple_newlines(&next)
}

async fn grok_build_get() -> Response {
    let cfg_path = grok_config_path();
    let bin_path = grok_bin_path();
    if !is_tool_installed("grok", &[bin_path, cfg_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Grok Build is not installed"
        }))
        .into_response();
    }

    let toml_text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let settings = parse_grok_build_config(&toml_text);
    let has_9router = settings
        .get("model")
        .and_then(|m| m.get("base_url"))
        .and_then(|u| u.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "settings": settings,
        "has9Router": has_9router,
        "configPath": cfg_path.to_string_lossy(),
    }))
    .into_response()
}

async fn grok_build_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update grok-build settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let selected_model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if base_url.is_empty() || selected_model.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and model are required");
    }

    let subagent_models: Option<HashMap<String, (String, u64)>> = match raw.get("subagentModels") {
        None | Some(Value::Null) => None,
        Some(Value::Object(obj)) => {
            let mut map = HashMap::new();
            for typ in GROK_SUBAGENT_TYPES {
                if let Some(entry) = obj.get(*typ) {
                    let (m_str, ctx_val) = if let Some(s) = entry.as_str() {
                        (s.trim().to_string(), None)
                    } else if let Some(eobj) = entry.as_object() {
                        let m = eobj
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .unwrap_or("")
                            .to_string();
                        (m, eobj.get("contextWindow"))
                    } else {
                        (String::new(), None)
                    };
                    if !m_str.is_empty() {
                        let ctx = normalize_grok_context_window(ctx_val, &m_str);
                        map.insert((*typ).to_string(), (m_str, ctx));
                    }
                }
            }
            Some(map)
        }
        Some(_) => Some(HashMap::new()),
    };

    let cfg_path = grok_config_path();
    let existing = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let ctx_win = normalize_grok_context_window(raw.get("contextWindow"), selected_model);

    let sub_ref = subagent_models.as_ref().map(|m| {
        let mut bm = BTreeMap::new();
        for (k, v) in m {
            bm.insert(k.clone(), v.clone());
        }
        bm
    });

    let toml_out = apply_grok_build_config(
        &existing,
        &v1_url(base_url),
        &api_key,
        selected_model,
        ctx_win,
        sub_ref.as_ref(),
    );

    if write_file_with_backup(&cfg_path, &toml_out).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update grok-build settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Grok Build settings applied successfully!",
        "configPath": cfg_path.to_string_lossy(),
        "modelSlot": "9router",
    }))
    .into_response()
}

async fn grok_build_delete() -> Response {
    let cfg_path = grok_config_path();
    let toml = match std::fs::read_to_string(&cfg_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({
                "success": true,
                "message": "No config file to reset"
            }))
            .into_response();
        }
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset grok-build settings",
            )
        }
    };

    if rewrite_file(&cfg_path, &reset_grok_build_config(&toml)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset grok-build settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9router model slots removed from Grok Build"
    }))
    .into_response()
}

// ============================================================================
// 16 & 17. /api/cli-tools/hermes-profiles & /api/cli-tools/hermes-settings
// ============================================================================

const HERMES_IDENTITY_FILES: &[&str] = &[
    "config.yaml",
    ".env",
    "SOUL.md",
    "profile.yaml",
    "auth.json",
    "state.db",
];
const HERMES_STT_API_KEY_REF: &str = "${OPENAI_API_KEY}";

#[derive(Clone)]
struct HermesHome {
    name: String,
    dir: PathBuf,
    config_path: PathBuf,
    env_path: PathBuf,
    is_default: bool,
}

fn hermes_root() -> PathBuf {
    home_dir().join(".hermes")
}

fn is_valid_hermes_profile_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name.eq_ignore_ascii_case("default") {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn resolve_hermes_home(profile: Option<&str>) -> Result<HermesHome, String> {
    let base = hermes_root();
    let name = profile.map(str::trim).unwrap_or("");
    if name.is_empty() || name == "default" {
        return Ok(HermesHome {
            name: "default".to_string(),
            config_path: base.join("config.yaml"),
            env_path: base.join(".env"),
            dir: base,
            is_default: true,
        });
    }
    if !is_valid_hermes_profile_name(name) {
        return Err(
            "Invalid Hermes profile name (letters, digits, - and _ only, max 64 chars)".to_string(),
        );
    }
    let profiles_root = base.join("profiles");
    let dir = profiles_root.join(name);
    Ok(HermesHome {
        name: name.to_string(),
        config_path: dir.join("config.yaml"),
        env_path: dir.join(".env"),
        dir,
        is_default: false,
    })
}

fn is_hermes_profile_home(dir: &Path) -> bool {
    HERMES_IDENTITY_FILES
        .iter()
        .any(|f| dir.join(f).exists())
}

fn extract_yaml_top_block(yaml: &str, key: &str) -> Option<(usize, usize, String)> {
    let prefix = format!("{}:", key);
    let mut offset = 0usize;
    let mut start: Option<usize> = None;
    let mut body_start = 0usize;

    for line in yaml.split_inclusive('\n') {
        if let Some(s) = start {
            let no_nl = line.trim_end_matches(['\r', '\n']);
            if !no_nl.is_empty() && !no_nl.starts_with(' ') && !no_nl.starts_with('\t') {
                return Some((s, offset, yaml[body_start..offset].to_string()));
            }
        } else {
            let no_nl = line.trim_end_matches(['\r', '\n']);
            if let Some(rest) = no_nl.strip_prefix(&prefix) {
                if rest.trim().is_empty() {
                    start = Some(offset);
                    body_start = offset + line.len();
                }
            }
        }
        offset += line.len();
    }
    start.map(|s| (s, yaml.len(), yaml[body_start..].to_string()))
}

fn extract_yaml_scalar_line(yaml: &str, key: &str) -> Option<(usize, usize)> {
    let prefix = format!("{}:", key);
    let mut offset = 0usize;
    for line in yaml.split_inclusive('\n') {
        let no_nl = line.trim_end_matches(['\r', '\n']);
        if no_nl.starts_with(&prefix) {
            return Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    None
}

fn strip_yaml_quotes(val: &str) -> String {
    let t = val.trim();
    if ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
        && t.len() >= 2
    {
        t[1..t.len() - 1].trim().to_string()
    } else {
        t.to_string()
    }
}

fn yaml_block_get_field(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{}:", key);
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(&prefix) {
            let val = strip_yaml_quotes(rest);
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn parse_hermes_model_block(yaml: &str) -> Option<Value> {
    let (_, _, body) = extract_yaml_top_block(yaml, "model")?;
    Some(json!({
        "default": yaml_block_get_field(&body, "default"),
        "provider": yaml_block_get_field(&body, "provider"),
        "base_url": yaml_block_get_field(&body, "base_url"),
        "api_key": yaml_block_get_field(&body, "api_key"),
    }))
}

fn parse_hermes_delegation_block(yaml: &str) -> Option<Value> {
    let (_, _, body) = extract_yaml_top_block(yaml, "delegation")?;
    Some(json!({
        "model": yaml_block_get_field(&body, "model"),
        "provider": yaml_block_get_field(&body, "provider"),
        "base_url": yaml_block_get_field(&body, "base_url"),
    }))
}

fn parse_hermes_aux_roles(yaml: &str) -> Map<String, Value> {
    let mut roles = Map::new();
    let Some((_, _, aux_body)) = extract_yaml_top_block(yaml, "auxiliary") else {
        return roles;
    };
    let mut current_role: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    for line in aux_body.lines() {
        if let Some(after_two) = line.strip_prefix("  ") {
            if !after_two.starts_with(' ') && !after_two.starts_with('\t') && after_two.contains(':') {
                if let Some(role) = current_role.take() {
                    let b = current_lines.join("\n");
                    roles.insert(
                        role,
                        json!({
                            "model": yaml_block_get_field(&b, "model"),
                            "provider": yaml_block_get_field(&b, "provider"),
                            "base_url": yaml_block_get_field(&b, "base_url"),
                        }),
                    );
                    current_lines.clear();
                }
                let role_name = after_two.split(':').next().unwrap_or("").trim();
                if !role_name.is_empty()
                    && role_name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    current_role = Some(role_name.to_string());
                }
                continue;
            }
        }
        if current_role.is_some() {
            current_lines.push(line.to_string());
        }
    }
    if let Some(role) = current_role {
        let b = current_lines.join("\n");
        roles.insert(
            role,
            json!({
                "model": yaml_block_get_field(&b, "model"),
                "provider": yaml_block_get_field(&b, "provider"),
                "base_url": yaml_block_get_field(&b, "base_url"),
            }),
        );
    }
    roles
}

fn parse_hermes_stt_block(yaml: &str) -> Option<Value> {
    let (_, _, body) = extract_yaml_top_block(yaml, "stt")?;
    let mut direct: HashMap<String, String> = HashMap::new();
    let mut openai: HashMap<String, String> = HashMap::new();
    let mut current_sub: Option<String> = None;

    for line in body.lines() {
        if line.starts_with("  ") && !line.starts_with("   ") {
            let trimmed = line[2..].trim_end();
            current_sub = None;
            if let Some(colon) = trimmed.find(':') {
                let k = trimmed[..colon].trim();
                let v = trimmed[colon + 1..].trim();
                if v.is_empty() {
                    current_sub = Some(k.to_string());
                } else {
                    direct.insert(k.to_string(), strip_yaml_quotes(v));
                }
            }
        } else if current_sub.as_deref() == Some("openai") && line.starts_with("    ") {
            let trimmed = line.trim();
            if let Some(colon) = trimmed.find(':') {
                let k = trimmed[..colon].trim();
                let v = trimmed[colon + 1..].trim();
                if !v.is_empty() {
                    openai.insert(k.to_string(), strip_yaml_quotes(v));
                }
            }
        }
    }

    Some(json!({
        "enabled": direct.get("enabled"),
        "provider": direct.get("provider"),
        "model": openai.get("model"),
        "base_url": openai.get("base_url"),
        "api_key": openai.get("api_key"),
    }))
}

fn hermes_has_9router(cfg: Option<&Value>) -> bool {
    let Some(cfg) = cfg else { return false };
    let provider = cfg.get("provider").and_then(|v| v.as_str());
    let base_url = cfg.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
    provider == Some("custom")
        && (base_url.contains("localhost")
            || base_url.contains("127.0.0.1")
            || base_url.contains("0.0.0.0"))
}

fn hermes_is_custom_block(cfg: Option<&Value>) -> bool {
    cfg.and_then(|v| v.get("provider")).and_then(|v| v.as_str()) == Some("custom")
}

fn hermes_is_9router_stt(cfg: Option<&Value>) -> bool {
    let Some(cfg) = cfg else { return false };
    cfg.get("provider").and_then(|v| v.as_str()) == Some("openai")
        && cfg
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        && cfg.get("api_key").and_then(|v| v.as_str()) == Some(HERMES_STT_API_KEY_REF)
}

fn build_hermes_model_block(model: &str, base_url: &str) -> String {
    format!(
        "model:\n  default: \"{}\"\n  provider: \"custom\"\n  base_url: \"{}\"\n  api_key: {}\n",
        model, base_url, HERMES_STT_API_KEY_REF
    )
}

fn build_hermes_delegation_block(model: &str, base_url: &str) -> String {
    format!(
        "delegation:\n  model: \"{}\"\n  provider: \"custom\"\n  base_url: \"{}\"\n  api_key: {}\n",
        model, base_url, HERMES_STT_API_KEY_REF
    )
}

fn build_hermes_aux_role_block(role: &str, model: &str, base_url: &str) -> String {
    format!(
        "  {}:\n    provider: \"custom\"\n    model: \"{}\"\n    base_url: \"{}\"\n    api_key: {}\n",
        role, model, base_url, HERMES_STT_API_KEY_REF
    )
}

fn upsert_hermes_model_block(yaml: &str, new_block: &str) -> String {
    if let Some((s, e, _)) = extract_yaml_top_block(yaml, "model") {
        return format!("{}{}{}", &yaml[..s], new_block, &yaml[e..]);
    }
    if let Some((s, e)) = extract_yaml_scalar_line(yaml, "model") {
        return format!("{}{}{}", &yaml[..s], new_block, &yaml[e..]);
    }
    if yaml.is_empty() {
        new_block.to_string()
    } else {
        format!("{}\n{}", new_block, yaml)
    }
}

fn remove_hermes_model_block(yaml: &str) -> String {
    if let Some((s, e, _)) = extract_yaml_top_block(yaml, "model") {
        return format!("{}{}", &yaml[..s], &yaml[e..])
            .trim_start_matches('\n')
            .to_string();
    }
    if let Some((s, e)) = extract_yaml_scalar_line(yaml, "model") {
        return format!("{}{}", &yaml[..s], &yaml[e..])
            .trim_start_matches('\n')
            .to_string();
    }
    yaml.to_string()
}

fn upsert_hermes_delegation_block(yaml: &str, new_block: &str) -> String {
    if let Some((s, e, _)) = extract_yaml_top_block(yaml, "delegation") {
        return format!("{}{}{}", &yaml[..s], new_block, &yaml[e..]);
    }
    if yaml.is_empty() || yaml.ends_with('\n') {
        format!("{}{}", yaml, new_block)
    } else {
        format!("{}\n{}", yaml, new_block)
    }
}

fn remove_hermes_delegation_block(yaml: &str) -> String {
    if let Some((s, e, _)) = extract_yaml_top_block(yaml, "delegation") {
        format!("{}{}", &yaml[..s], &yaml[e..])
    } else {
        yaml.to_string()
    }
}

fn extract_aux_sub_bounds(aux_body: &str, role: &str) -> Option<(usize, usize)> {
    let target = format!("  {}:", role);
    let mut offset = 0usize;
    let mut start: Option<usize> = None;

    for line in aux_body.split_inclusive('\n') {
        let no_nl = line.trim_end_matches(['\r', '\n']);
        if let Some(s) = start {
            if !no_nl.trim().is_empty() && !no_nl.starts_with("    ") {
                return Some((s, offset));
            }
        } else if no_nl.starts_with(&target) && no_nl[target.len()..].trim().is_empty() {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.map(|s| (s, aux_body.len()))
}

fn upsert_hermes_aux_role(yaml: &str, role: &str, role_block: &str) -> String {
    let Some((s, e, aux_body)) = extract_yaml_top_block(yaml, "auxiliary") else {
        let block = format!("auxiliary:\n{}", role_block);
        return if yaml.is_empty() || yaml.ends_with('\n') {
            format!("{}{}", yaml, block)
        } else {
            format!("{}\n{}", yaml, block)
        };
    };
    let next_body = if let Some((rs, re)) = extract_aux_sub_bounds(&aux_body, role) {
        format!("{}{}{}", &aux_body[..rs], role_block, &aux_body[re..])
    } else if aux_body.is_empty() || aux_body.ends_with('\n') {
        format!("{}{}", aux_body, role_block)
    } else {
        format!("{}\n{}", aux_body, role_block)
    };
    format!("{}auxiliary:\n{}{}", &yaml[..s], next_body, &yaml[e..])
}

fn remove_hermes_aux_role(yaml: &str, role: &str) -> String {
    let Some((s, e, aux_body)) = extract_yaml_top_block(yaml, "auxiliary") else {
        return yaml.to_string();
    };
    let Some((rs, re)) = extract_aux_sub_bounds(&aux_body, role) else {
        return yaml.to_string();
    };
    let next_body = format!("{}{}", &aux_body[..rs], &aux_body[re..]);
    if next_body.trim().is_empty() {
        format!("{}{}", &yaml[..s], &yaml[e..])
    } else {
        format!("{}auxiliary:\n{}{}", &yaml[..s], next_body, &yaml[e..])
    }
}

fn upsert_hermes_stt_9router(yaml: &str, model: &str, base_url: &str) -> String {
    let Some((s, e, mut body)) = extract_yaml_top_block(yaml, "stt") else {
        let block = format!(
            "stt:\n  enabled: true\n  provider: \"openai\"\n  openai:\n    model: \"{}\"\n    base_url: \"{}\"\n    api_key: {}\n",
            model, base_url, HERMES_STT_API_KEY_REF
        );
        return if yaml.is_empty() {
            block
        } else if yaml.ends_with('\n') {
            format!("{}{}", yaml, block)
        } else {
            format!("{}\n{}", yaml, block)
        };
    };

    body = upsert_stt_direct_key(&body, "enabled", "true");
    body = upsert_stt_direct_key(&body, "provider", "\"openai\"");
    body = upsert_stt_openai_key(&body, "model", &format!("\"{}\"", model));
    body = upsert_stt_openai_key(&body, "base_url", &format!("\"{}\"", base_url));
    body = upsert_stt_openai_key(&body, "api_key", HERMES_STT_API_KEY_REF);

    format!("{}stt:\n{}{}", &yaml[..s], body, &yaml[e..])
}

fn upsert_stt_direct_key(body: &str, key: &str, value: &str) -> String {
    let prefix = format!("  {}:", key);
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in body.lines() {
        if line.starts_with(&prefix) && !replaced {
            lines.push(format!("  {}: {}", key, value));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if replaced {
        let mut out = lines.join("\n");
        if body.ends_with('\n') || !out.is_empty() {
            out.push('\n');
        }
        out
    } else {
        let head = format!("  {}: {}\n", key, value);
        if body.is_empty() || body.ends_with('\n') {
            format!("{}{}", body, head)
        } else {
            format!("{}\n{}", body, head)
        }
    }
}

fn upsert_stt_openai_key(body: &str, key: &str, value: &str) -> String {
    let Some((os, oe)) = extract_aux_sub_bounds(body, "openai") else {
        let block = format!("  openai:\n    {}: {}\n", key, value);
        return if body.is_empty() || body.ends_with('\n') {
            format!("{}{}", body, block)
        } else {
            format!("{}\n{}", body, block)
        };
    };
    let sub = &body[os..oe];
    let sub_lines: Vec<&str> = sub.lines().collect();
    let header = sub_lines.first().copied().unwrap_or("  openai:");
    let key_prefix = format!("    {}:", key);
    let mut replaced = false;
    let mut new_sub_lines = vec![header.to_string()];
    for line in sub_lines.iter().skip(1) {
        if line.starts_with(&key_prefix) && !replaced {
            new_sub_lines.push(format!("    {}: {}", key, value));
            replaced = true;
        } else {
            new_sub_lines.push((*line).to_string());
        }
    }
    if !replaced {
        new_sub_lines.push(format!("    {}: {}", key, value));
    }
    let mut new_sub = new_sub_lines.join("\n");
    new_sub.push('\n');
    format!("{}{}{}", &body[..os], new_sub, &body[oe..])
}

fn remove_hermes_stt_9router(yaml: &str) -> String {
    let Some((s, e, body)) = extract_yaml_top_block(yaml, "stt") else {
        return yaml.to_string();
    };
    let mut lines = Vec::new();
    let mut in_openai = false;
    let mut openai_header_idx: Option<usize> = None;
    let mut openai_remaining = 0usize;

    for line in body.lines() {
        let trimmed = line.trim();
        if line.starts_with("  ") && !line.starts_with("    ") {
            in_openai = false;
            if let Some(rest) = trimmed.strip_prefix("enabled:") {
                if strip_yaml_quotes(rest) == "true" {
                    continue;
                }
            }
            if let Some(rest) = trimmed.strip_prefix("provider:") {
                if strip_yaml_quotes(rest) == "openai" {
                    continue;
                }
            }
            if trimmed == "openai:" {
                in_openai = true;
                openai_header_idx = Some(lines.len());
                lines.push(line.to_string());
                continue;
            }
            lines.push(line.to_string());
        } else if in_openai && line.starts_with("    ") {
            let key = trimmed.split(':').next().unwrap_or("").trim();
            if matches!(key, "model" | "base_url" | "api_key") {
                continue;
            }
            openai_remaining += 1;
            lines.push(line.to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    if openai_remaining == 0 {
        if let Some(idx) = openai_header_idx {
            lines.remove(idx);
        }
    }
    let next_body = lines.join("\n");
    if next_body.trim().is_empty() {
        format!("{}{}", &yaml[..s], &yaml[e..])
            .trim_start_matches('\n')
            .to_string()
    } else {
        format!("{}stt:\n{}\n{}", &yaml[..s], next_body, &yaml[e..])
    }
}

fn upsert_env_var(env_text: &str, key: &str, value: &str) -> String {
    let line = format!("{}={}", key, value);
    let prefix = format!("{}=", key);
    let mut replaced = false;
    let mut lines = Vec::new();
    for l in env_text.lines() {
        if l.starts_with(&prefix) && !replaced {
            lines.push(line.clone());
            replaced = true;
        } else {
            lines.push(l.to_string());
        }
    }
    if replaced {
        let mut out = lines.join("\n");
        out.push('\n');
        out
    } else if env_text.is_empty() {
        format!("{}\n", line)
    } else if env_text.ends_with('\n') {
        format!("{}{}\n", env_text, line)
    } else {
        format!("{}\n{}\n", env_text, line)
    }
}

fn parse_hermes_display_name(profile_yaml: &str) -> Option<String> {
    for line in profile_yaml.lines() {
        if let Some(rest) = line.strip_prefix("display_name:") {
            let v = strip_yaml_quotes(rest);
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn build_hermes_profile_entry(home: &HermesHome) -> Value {
    let yaml = std::fs::read_to_string(&home.config_path).unwrap_or_default();
    let model = parse_hermes_model_block(&yaml);
    let delegation = parse_hermes_delegation_block(&yaml);
    let auxiliary = parse_hermes_aux_roles(&yaml);
    let display_name = std::fs::read_to_string(home.dir.join("profile.yaml"))
        .ok()
        .and_then(|s| parse_hermes_display_name(&s));
    let has_9router = hermes_has_9router(model.as_ref())
        || hermes_has_9router(delegation.as_ref())
        || auxiliary.values().any(|v| hermes_has_9router(Some(v)));

    json!({
        "name": home.name,
        "dir": home.dir.to_string_lossy(),
        "isDefault": home.is_default,
        "displayName": display_name,
        "command": if home.is_default { "hermes".to_string() } else { format!("hermes -p {}", home.name) },
        "alias": if home.is_default { Value::Null } else { Value::String(home.name.clone()) },
        "model": model.as_ref().and_then(|m| m.get("default")).cloned().unwrap_or(Value::Null),
        "baseUrl": model.as_ref().and_then(|m| m.get("base_url")).cloned().unwrap_or(Value::Null),
        "has9Router": has_9router,
    })
}

fn list_hermes_profiles() -> Vec<Value> {
    let mut entries = Vec::new();
    if let Ok(def) = resolve_hermes_home(None) {
        entries.push(build_hermes_profile_entry(&def));
    }
    let profiles_dir = hermes_root().join("profiles");
    let Ok(rd) = std::fs::read_dir(&profiles_dir) else {
        return entries;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_hermes_profile_name(n))
        .collect();
    names.sort();
    for name in names {
        if let Ok(home) = resolve_hermes_home(Some(&name)) {
            if is_hermes_profile_home(&home.dir) {
                entries.push(build_hermes_profile_entry(&home));
            }
        }
    }
    entries
}

async fn hermes_profiles_get() -> Response {
    Json(json!({ "profiles": list_hermes_profiles() })).into_response()
}

fn write_hermes_home_config(home: &HermesHome, yaml: &str) -> Result<(), String> {
    if home.is_default {
        write_file_with_backup(&home.config_path, yaml).map_err(|e| e.to_string())
    } else {
        write_file_with_backup(&home.config_path, yaml).map_err(|e| e.to_string())
    }
}

fn write_hermes_home_env(home: &HermesHome, api_key: Option<&str>) -> Result<(), String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let existing = std::fs::read_to_string(&home.env_path).unwrap_or_default();
    let updated = upsert_env_var(&existing, "OPENAI_API_KEY", key);
    rewrite_file(&home.env_path, &updated).map_err(|e| e.to_string())
}

async fn hermes_get(Query(params): Query<HashMap<String, String>>) -> Response {
    let default_cfg = hermes_root().join("config.yaml");
    if !is_tool_installed("hermes", &[default_cfg]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Hermes Agent is not installed"
        }))
        .into_response();
    }

    let profile_param = params.get("profile").map(String::as_str);
    let home = match resolve_hermes_home(profile_param) {
        Ok(h) => h,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    if !home.is_default && !home.dir.is_dir() {
        return json_err(
            StatusCode::NOT_FOUND,
            format!("Hermes profile \"{}\" not found", home.name),
        );
    }

    let yaml = std::fs::read_to_string(&home.config_path).unwrap_or_default();
    let model = parse_hermes_model_block(&yaml);
    let delegation = parse_hermes_delegation_block(&yaml);
    let auxiliary = parse_hermes_aux_roles(&yaml);
    let stt = parse_hermes_stt_block(&yaml);
    let has_9router = hermes_has_9router(model.as_ref())
        || hermes_has_9router(delegation.as_ref())
        || auxiliary.values().any(|v| hermes_has_9router(Some(v)));
    let has_9router_stt = hermes_is_9router_stt(stt.as_ref());

    Json(json!({
        "installed": true,
        "profile": {
            "name": home.name,
            "dir": home.dir.to_string_lossy(),
            "isDefault": home.is_default,
        },
        "settings": {
            "model": model,
            "delegation": delegation,
            "auxiliary": auxiliary,
            "stt": stt,
        },
        "has9Router": has_9router,
        "has9RouterStt": has_9router_stt,
        "configPath": home.config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn hermes_post(
    Query(params): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update hermes settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str());
    let model = raw.get("model").and_then(|v| v.as_str());
    let audio = raw.get("audio");

    let sel: Vec<(String, String)> = match raw.get("selections").and_then(|v| v.as_array()) {
        Some(arr) => {
            let valid: Vec<(String, String)> = arr
                .iter()
                .filter_map(|s| {
                    let r = s.get("role").and_then(|v| v.as_str())?;
                    let m = s.get("model").and_then(|v| v.as_str())?;
                    if !r.is_empty() && !m.is_empty() {
                        Some((r.to_string(), m.to_string()))
                    } else {
                        None
                    }
                })
                .collect();
            if valid.is_empty() {
                model
                    .filter(|s| !s.is_empty())
                    .map(|m| vec![("default".to_string(), m.to_string())])
                    .unwrap_or_default()
            } else {
                valid
            }
        }
        None => model
            .filter(|s| !s.is_empty())
            .map(|m| vec![("default".to_string(), m.to_string())])
            .unwrap_or_default(),
    };

    let default_sel = sel.iter().find(|(r, _)| r == "default").map(|(_, m)| m.clone());
    if base_url.is_empty() || (default_sel.is_none() && audio.is_none()) {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and model are required");
    }

    let audio_enabled = audio
        .and_then(|a| a.get("enabled"))
        .map(|v| truthy_value(Some(v)))
        .unwrap_or(false);
    let audio_model = audio
        .and_then(|a| a.get("model"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if audio_enabled && audio_model.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "audio.model is required when enabling audio transcription",
        );
    }

    let normalized_base = v1_url(base_url);
    let apply_to_all = truthy_value(raw.get("applyToAll"))
        || params
            .get("applyToAll")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);

    if apply_to_all {
        let profiles = list_hermes_profiles();
        let mut results = Vec::new();
        let mut updated = 0usize;
        for p in &profiles {
            let p_name = p.get("name").and_then(|v| v.as_str()).unwrap_or("default");
            let Ok(home) = resolve_hermes_home(Some(p_name)) else {
                continue;
            };
            let mut yaml = std::fs::read_to_string(&home.config_path).unwrap_or_default();
            let main = parse_hermes_model_block(&yaml);
            let main_default = main
                .as_ref()
                .and_then(|m| m.get("default"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string);

            if let Some(ref md) = main_default {
                if hermes_is_custom_block(main.as_ref()) {
                    yaml = upsert_hermes_model_block(
                        &yaml,
                        &build_hermes_model_block(md, &normalized_base),
                    );
                    let delegation = parse_hermes_delegation_block(&yaml);
                    if let Some(dm) = delegation
                        .as_ref()
                        .and_then(|d| d.get("model"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                    {
                        if hermes_is_custom_block(delegation.as_ref()) {
                            yaml = upsert_hermes_delegation_block(
                                &yaml,
                                &build_hermes_delegation_block(&dm, &normalized_base),
                            );
                        }
                    }
                    for (role, cfg) in parse_hermes_aux_roles(&yaml) {
                        if let Some(rm) = cfg.get("model").and_then(|v| v.as_str()) {
                            if hermes_is_custom_block(Some(&cfg)) {
                                yaml = upsert_hermes_aux_role(
                                    &yaml,
                                    &role,
                                    &build_hermes_aux_role_block(&role, rm, &normalized_base),
                                );
                            }
                        }
                    }
                    if write_hermes_home_config(&home, &yaml).is_ok()
                        && write_hermes_home_env(&home, api_key).is_ok()
                    {
                        updated += 1;
                        results.push(json!({ "profile": p_name, "status": "updated", "model": md }));
                    } else {
                        results.push(json!({ "profile": p_name, "status": "failed", "reason": "write failed" }));
                    }
                } else {
                    let prov = main
                        .as_ref()
                        .and_then(|m| m.get("provider"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    results.push(json!({
                        "profile": p_name,
                        "status": "skipped",
                        "reason": format!("main model uses provider \"{}\" — configure this profile individually", prov)
                    }));
                }
            } else if let Some(ref dm) = default_sel {
                yaml = upsert_hermes_model_block(
                    &yaml,
                    &build_hermes_model_block(dm, &normalized_base),
                );
                if write_hermes_home_config(&home, &yaml).is_ok()
                    && write_hermes_home_env(&home, api_key).is_ok()
                {
                    updated += 1;
                    results.push(json!({ "profile": p_name, "status": "updated", "model": dm }));
                } else {
                    results.push(json!({ "profile": p_name, "status": "failed", "reason": "write failed" }));
                }
            } else {
                results.push(json!({
                    "profile": p_name,
                    "status": "skipped",
                    "reason": "no main model configured"
                }));
            }
        }
        let skipped = results.len().saturating_sub(updated);
        return Json(json!({
            "success": updated > 0,
            "bulk": true,
            "results": results,
            "updated": updated,
            "skipped": skipped,
            "message": format!("Updated endpoint on {} profile(s)", updated),
        }))
        .into_response();
    }

    let profile_target = raw
        .get("profile")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("profile").map(String::as_str));
    let home = match resolve_hermes_home(profile_target) {
        Ok(h) => h,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    if !home.is_default && !home.dir.is_dir() {
        return json_err(
            StatusCode::NOT_FOUND,
            format!("Hermes profile \"{}\" not found", home.name),
        );
    }

    let mut new_yaml = std::fs::read_to_string(&home.config_path).unwrap_or_default();
    for (role, role_model) in &sel {
        if role == "default" {
            new_yaml = upsert_hermes_model_block(
                &new_yaml,
                &build_hermes_model_block(role_model, &normalized_base),
            );
        } else if role == "delegation" {
            new_yaml = upsert_hermes_delegation_block(
                &new_yaml,
                &build_hermes_delegation_block(role_model, &normalized_base),
            );
        } else {
            new_yaml = upsert_hermes_aux_role(
                &new_yaml,
                role,
                &build_hermes_aux_role_block(role, role_model, &normalized_base),
            );
        }
    }

    if audio.is_some() {
        if audio_enabled {
            new_yaml = upsert_hermes_stt_9router(&new_yaml, audio_model, &normalized_base);
        } else if hermes_is_9router_stt(parse_hermes_stt_block(&new_yaml).as_ref()) {
            new_yaml = remove_hermes_stt_9router(&new_yaml);
        }
    }

    if write_hermes_home_config(&home, &new_yaml).is_err()
        || write_hermes_home_env(&home, api_key).is_err()
    {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update hermes settings",
        );
    }

    let stt_9router = hermes_is_9router_stt(parse_hermes_stt_block(&new_yaml).as_ref());
    Json(json!({
        "success": true,
        "message": if home.is_default {
            "Hermes settings applied successfully!".to_string()
        } else {
            format!("Hermes settings applied to profile \"{}\"!", home.name)
        },
        "profile": {
            "name": home.name,
            "dir": home.dir.to_string_lossy(),
            "isDefault": home.is_default,
        },
        "stt9Router": stt_9router,
        "configPath": home.config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn hermes_delete(
    Query(params): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    let body_json = parse_json_body(&body).ok();
    let profile_target = body_json
        .as_ref()
        .and_then(|b| b.get("profile"))
        .and_then(|v| v.as_str())
        .or_else(|| params.get("profile").map(String::as_str));

    let home = match resolve_hermes_home(profile_target) {
        Ok(h) => h,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
    };
    if !home.is_default && !home.dir.is_dir() {
        return json_err(
            StatusCode::NOT_FOUND,
            format!("Hermes profile \"{}\" not found", home.name),
        );
    }

    let mut yaml = match std::fs::read_to_string(&home.config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({
                "success": true,
                "profile": home.name,
                "message": "No config file to reset"
            }))
            .into_response();
        }
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset hermes settings",
            )
        }
    };

    let mut removed_model = false;
    let mut removed_delegation = false;
    let mut removed_aux = Vec::new();
    let mut removed_stt = false;
    let mut kept = Vec::new();

    if let Some(main) = parse_hermes_model_block(&yaml) {
        if hermes_is_custom_block(Some(&main)) {
            yaml = remove_hermes_model_block(&yaml);
            removed_model = true;
        } else {
            kept.push("model");
        }
    }

    if let Some(delegation) = parse_hermes_delegation_block(&yaml) {
        if hermes_is_custom_block(Some(&delegation)) {
            yaml = remove_hermes_delegation_block(&yaml);
            removed_delegation = true;
        } else {
            kept.push("delegation");
        }
    }

    for (role, cfg) in parse_hermes_aux_roles(&yaml) {
        if hermes_is_custom_block(Some(&cfg)) {
            yaml = remove_hermes_aux_role(&yaml, &role);
            removed_aux.push(role);
        }
    }

    if let Some(stt_cfg) = parse_hermes_stt_block(&yaml) {
        if hermes_is_9router_stt(Some(&stt_cfg)) {
            yaml = remove_hermes_stt_9router(&yaml);
            removed_stt = true;
            if parse_hermes_stt_block(&yaml).is_some() {
                kept.push("stt");
            }
        } else {
            kept.push("stt");
        }
    }

    let yaml_trimmed = yaml.trim_start_matches('\n');
    if rewrite_file(&home.config_path, yaml_trimmed).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset hermes settings",
        );
    }

    let scope = if home.is_default {
        String::new()
    } else {
        format!(" in profile \"{}\"", home.name)
    };
    Json(json!({
        "success": true,
        "profile": home.name,
        "removed": {
            "model": removed_model,
            "delegation": removed_delegation,
            "auxiliary": removed_aux,
            "stt": removed_stt,
        },
        "kept": kept,
        "message": format!("9router model blocks removed{}", scope),
    }))
    .into_response()
}

// ============================================================================
// 18. /api/cli-tools/jcode-settings
// ============================================================================

fn jcode_dir() -> PathBuf {
    home_dir().join(".jcode")
}

fn jcode_config_path() -> PathBuf {
    jcode_dir().join("config.toml")
}

fn jcode_provider_env_path() -> PathBuf {
    xdg_config_home().join("jcode").join("provider-9router.env")
}

fn read_jcode_config() -> Map<String, Value> {
    if let Ok(content) = std::fs::read_to_string(jcode_config_path()) {
        let mut map = parse_toml_to_map(&content);
        if !map.contains_key("providers") {
            map.insert("providers".to_string(), Value::Object(Map::new()));
        }
        return map;
    }
    let mut map = Map::new();
    map.insert("providers".to_string(), Value::Object(Map::new()));
    map
}

fn jcode_has_9router(cfg: &Map<String, Value>, port: u16) -> bool {
    let Some(providers) = cfg.get("providers").and_then(|v| v.as_object()) else {
        return false;
    };
    if providers.contains_key("9router") {
        return true;
    }
    let port_needle = format!("localhost:{}", port);
    providers.values().any(|p| {
        p.get("base_url")
            .and_then(|u| u.as_str())
            .map(|s| s.contains(&port_needle))
            .unwrap_or(false)
    })
}

fn read_provider_env_map(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq) = trimmed.find('=') {
            let k = trimmed[..eq].trim().to_string();
            let mut v = trimmed[eq + 1..].trim().to_string();
            if ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
                && v.len() >= 2
            {
                v = v[1..v.len() - 1].to_string();
            }
            map.insert(k, v);
        }
    }
    map
}

fn write_provider_env_map(path: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    let mut out = String::from("# jcode provider environment variables\n");
    for (k, v) in env {
        out.push_str(&format!("{}=\"{}\"\n", k, v));
    }
    rewrite_file(path, &out).map_err(|e| e.to_string())
}

async fn jcode_get(State(state): State<AppState>) -> Response {
    if !is_tool_installed("jcode", &[jcode_dir()]) {
        return Json(json!({
            "installed": false,
            "message": "jcode not installed. Install via: curl -fsSL https://raw.githubusercontent.com/1jehuang/jcode/master/scripts/install.sh | bash",
        }))
        .into_response();
    }

    let config = read_jcode_config();
    let has_9router = jcode_has_9router(&config, state.config.port);
    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": jcode_config_path().to_string_lossy(),
    }))
    .into_response()
}

async fn jcode_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() || api_key.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and apiKey are required");
    }

    let default_model = raw
        .get("models")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("cc/claude-opus-4-7");

    let mut config = read_jcode_config();
    let mut providers = config
        .get("providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    providers.insert(
        "9router".to_string(),
        json!({
            "type": "openai-compatible",
            "base_url": v1_url(base_url),
            "auth": "bearer",
            "api_key_env": "JCODE_9ROUTER_API_KEY",
            "env_file": "provider-9router.env",
            "default_model": default_model,
            "requires_api_key": true,
        }),
    );
    config.insert("providers".to_string(), Value::Object(providers));

    let cfg_path = jcode_config_path();
    if let Err(e) = write_file_with_backup(&cfg_path, &serialize_toml_map(&config)) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    let env_path = jcode_provider_env_path();
    let mut env = read_provider_env_map(&env_path);
    env.insert("JCODE_9ROUTER_API_KEY".to_string(), api_key.to_string());
    if let Err(e) = write_provider_env_map(&env_path, &env) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "jcode configured successfully. Use: jcode --provider-profile 9router",
        "configPath": cfg_path.to_string_lossy(),
    }))
    .into_response()
}

async fn jcode_delete() -> Response {
    let mut config = read_jcode_config();
    if let Some(providers) = config.get_mut("providers").and_then(|v| v.as_object_mut()) {
        providers.remove("9router");
        if let Err(e) = rewrite_file(&jcode_config_path(), &serialize_toml_map(&config)) {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
    }

    let env_path = jcode_provider_env_path();
    let mut env = read_provider_env_map(&env_path);
    env.remove("JCODE_9ROUTER_API_KEY");
    if let Err(e) = write_provider_env_map(&env_path, &env) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "9router configuration removed from jcode"
    }))
    .into_response()
}

// ============================================================================
// 19. /api/cli-tools/kilo-settings
// ============================================================================

fn kilo_auth_path() -> PathBuf {
    home_dir()
        .join(".local")
        .join("share")
        .join("kilo")
        .join("auth.json")
}

fn kilo_vscode_settings_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("Code")
        .join("User")
        .join("settings.json")
}

fn kilo_has_9router(auth: Option<&Map<String, Value>>) -> bool {
    let Some(auth) = auth else { return false };
    let Some(entry) = auth
        .get("openai-compatible")
        .or_else(|| auth.get("9router"))
        .and_then(|v| v.as_object())
    else {
        return false;
    };
    let base_url = get_str_field(entry, "baseUrl")
        .or_else(|| get_str_field(entry, "baseURL"))
        .unwrap_or("");
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("9router")
}

async fn kilo_get() -> Response {
    let auth_path = kilo_auth_path();
    if !is_tool_installed("kilo", &[auth_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Kilo Code CLI is not installed"
        }))
        .into_response();
    }

    let auth = read_json_obj_optional(&auth_path);
    let keys: Vec<String> = auth
        .as_ref()
        .map(|a| a.keys().cloned().collect())
        .unwrap_or_default();
    let has_9router = kilo_has_9router(auth.as_ref());

    Json(json!({
        "installed": true,
        "settings": { "auth": keys },
        "has9Router": has_9router,
        "authPath": auth_path.to_string_lossy(),
    }))
    .into_response()
}

async fn kilo_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update kilo settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "baseUrl, apiKey and model are required",
        );
    }

    let normalized_base = v1_url(base_url);
    let auth_path = kilo_auth_path();
    let mut auth = read_json_obj_optional(&auth_path).unwrap_or_default();
    auth.insert(
        "openai-compatible".to_string(),
        json!({
            "type": "api-key",
            "apiKey": api_key,
            "baseUrl": normalized_base,
            "model": model,
        }),
    );

    if write_json_pretty_with_backup(&auth_path, &Value::Object(auth)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update kilo settings",
        );
    }

    // Best-effort VS Code extension settings
    let vscode_path = kilo_vscode_settings_path();
    let mut vscode = read_json_obj_optional(&vscode_path).unwrap_or_default();
    vscode.insert(
        "kilocode.customProvider".to_string(),
        json!({
            "name": "9Router",
            "baseURL": normalized_base,
            "apiKey": api_key,
        }),
    );
    vscode.insert(
        "kilocode.defaultModel".to_string(),
        Value::String(model.to_string()),
    );
    let _ = write_json_pretty_no_backup(&vscode_path, &Value::Object(vscode));

    Json(json!({
        "success": true,
        "message": "Kilo Code settings applied successfully!",
        "authPath": auth_path.to_string_lossy(),
    }))
    .into_response()
}

async fn kilo_delete() -> Response {
    let auth_path = kilo_auth_path();
    let Some(mut auth) = read_json_obj_optional(&auth_path) else {
        return Json(json!({
            "success": true,
            "message": "No settings file to reset"
        }))
        .into_response();
    };
    auth.remove("openai-compatible");
    auth.remove("9router");
    if write_json_pretty_no_backup(&auth_path, &Value::Object(auth)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset kilo settings",
        );
    }

    let vscode_path = kilo_vscode_settings_path();
    if let Some(mut vscode) = read_json_obj_optional(&vscode_path) {
        vscode.remove("kilocode.customProvider");
        vscode.remove("kilocode.defaultModel");
        let _ = write_json_pretty_no_backup(&vscode_path, &Value::Object(vscode));
    }

    Json(json!({
        "success": true,
        "message": "9Router settings removed from Kilo Code"
    }))
    .into_response()
}

// ============================================================================
// 20. /api/cli-tools/omp-settings
// ============================================================================

fn omp_dir() -> PathBuf {
    home_dir().join(".omp").join("agent")
}

fn omp_db_path() -> PathBuf {
    omp_dir().join("agent.db")
}

fn omp_models_yml_path() -> PathBuf {
    omp_dir().join("models.yml")
}

fn build_omp_provider_yaml(base_url: &str, api_key: &str) -> String {
    format!(
        "  9router:\n    baseUrl: {}\n    apiKey: {}\n    api: openai-completions\n    authHeader: true\n    disableStrictTools: true\n    discovery:\n      type: proxy",
        v1_url(base_url),
        api_key
    )
}

fn remove_omp_9router_block(yml: &str) -> String {
    let mut out_lines = Vec::new();
    let mut skipping = false;
    let mut indent_level = 0usize;

    for line in yml.lines() {
        let trimmed = line.trim();
        let leading = line.len() - line.trim_start().len();
        if skipping {
            if trimmed.is_empty() || leading > indent_level {
                continue;
            }
            skipping = false;
        }
        if trimmed == "9router:" {
            skipping = true;
            indent_level = leading;
            continue;
        }
        out_lines.push(line);
    }
    let mut res = out_lines.join("\n");
    if yml.ends_with('\n') && !res.is_empty() && !res.ends_with('\n') {
        res.push('\n');
    }
    res
}

async fn omp_get() -> Response {
    let yml_path = omp_models_yml_path();
    if !is_tool_installed("omp", &[omp_db_path(), yml_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Oh My Pi is not installed"
        }))
        .into_response();
    }

    let content = std::fs::read_to_string(&yml_path).unwrap_or_default();
    let has_9router = content.contains("9router:") || content.contains("localhost:20128");
    Json(json!({
        "installed": true,
        "has9Router": has_9router,
        "configPath": yml_path.to_string_lossy(),
    }))
    .into_response()
}

async fn omp_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }

    let resolved_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let yml_path = omp_models_yml_path();
    let existing = std::fs::read_to_string(&yml_path).unwrap_or_default();
    let cleaned = remove_omp_9router_block(&existing);
    let block = build_omp_provider_yaml(base_url, &resolved_key);

    let updated = if cleaned.trim().is_empty() {
        format!("providers:\n{}\n", block)
    } else if cleaned.contains("providers:") {
        cleaned.replacen("providers:", &format!("providers:\n{}", block), 1)
    } else {
        format!("{}\n\nproviders:\n{}\n", cleaned.trim(), block)
    };

    if let Err(e) = write_file_with_backup(&yml_path, &updated) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    // Best-effort update to agent.db if it already exists with auth_credentials table
    let db_path = omp_db_path();
    if db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            let now = chrono::Utc::now().timestamp();
            let _ = conn.execute("DELETE FROM auth_credentials WHERE provider = ?", ["9router"]);
            let _ = conn.execute(
                "INSERT INTO auth_credentials (provider, credential_type, data, disabled_cause, identity_key, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5)",
                rusqlite::params![
                    "9router",
                    "api_key",
                    json!({ "apiKey": resolved_key, "baseUrl": base_url }).to_string(),
                    now,
                    now
                ],
            );
        }
    }

    Json(json!({
        "success": true,
        "message": "Oh My Pi settings applied! Run 'omp' and all 9Router models appear under 9router in /model.",
        "configPath": yml_path.to_string_lossy(),
    }))
    .into_response()
}

async fn omp_delete() -> Response {
    let yml_path = omp_models_yml_path();
    let existing = std::fs::read_to_string(&yml_path).unwrap_or_default();
    let cleaned = remove_omp_9router_block(&existing);

    if cleaned.trim() == "providers:" || cleaned.trim().is_empty() {
        let _ = std::fs::remove_file(&yml_path);
    } else if let Err(e) = rewrite_file(&yml_path, &cleaned) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from Oh My Pi"
    }))
    .into_response()
}

// ============================================================================
// 21. /api/cli-tools/openclaw-settings
// ============================================================================

fn openclaw_settings_path() -> PathBuf {
    home_dir().join(".openclaw").join("openclaw.json")
}

fn resolve_openclaw_agent_model(m: Option<&Value>) -> String {
    match m {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(obj)) => obj
            .get("primary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn read_openclaw_agent_model(agent_dir: &str) -> Option<String> {
    let path = Path::new(agent_dir).join("models.json");
    let data = read_jsonc_value(&path)?;
    data.get("providers")?
        .get("9router")?
        .get("models")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(ToString::to_string)
}

fn write_openclaw_agent_models(
    agent_dir: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
) -> Result<(), String> {
    let models_path = Path::new(agent_dir).join("models.json");
    let mut existing = read_json_obj_optional(&models_path).unwrap_or_default();
    let mut providers = existing
        .get("providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let display_name = model.rsplit('/').next().unwrap_or(model);
    providers.insert(
        "9router".to_string(),
        json!({
            "baseUrl": base_url,
            "apiKey": if api_key.is_empty() { "your_api_key" } else { api_key },
            "api": "openai-completions",
            "models": [{ "id": model, "name": display_name }],
        }),
    );
    existing.insert("providers".to_string(), Value::Object(providers));
    write_json_pretty_with_backup(&models_path, &Value::Object(existing))
}

async fn openclaw_get() -> Response {
    let settings_path = openclaw_settings_path();
    if !is_tool_installed("openclaw", &[settings_path.clone()]) {
        return Json(json!({
            "installed": false,
            "settings": null,
            "message": "Open Claw CLI is not installed"
        }))
        .into_response();
    }

    let settings = read_jsonc_value(&settings_path);
    let agent_list = settings
        .as_ref()
        .and_then(|s| s.get("agents"))
        .and_then(|a| a.get("list"))
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();

    let enriched_agents: Vec<Value> = agent_list
        .into_iter()
        .map(|agent| {
            let mut obj = agent.as_object().cloned().unwrap_or_default();
            let current_model = obj
                .get("agentDir")
                .and_then(|v| v.as_str())
                .and_then(read_openclaw_agent_model);
            let resolved_model = resolve_openclaw_agent_model(obj.get("model"));
            obj.insert("model".to_string(), Value::String(resolved_model));
            obj.insert(
                "currentModel".to_string(),
                current_model
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            Value::Object(obj)
        })
        .collect();

    let has_9router = settings
        .as_ref()
        .and_then(|s| s.get("models"))
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.get("9router"))
        .map(|v| !v.is_null())
        .unwrap_or(false);

    Json(json!({
        "installed": true,
        "settings": settings,
        "agents": enriched_agents,
        "has9Router": has_9router,
        "settingsPath": settings_path.to_string_lossy(),
    }))
    .into_response()
}

async fn openclaw_post(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update openclaw settings",
        );
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let agent_models = raw
        .get("agentModels")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    if base_url.is_empty() || model.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "baseUrl and model are required");
    }

    let settings_path = openclaw_settings_path();
    let mut settings = read_json_obj_optional(&settings_path).unwrap_or_default();

    if !settings.get("agents").map(|v| v.is_object()).unwrap_or(false) {
        settings.insert("agents".to_string(), json!({}));
    }
    {
        let agents = settings.get_mut("agents").unwrap().as_object_mut().unwrap();
        if !agents.get("defaults").map(|v| v.is_object()).unwrap_or(false) {
            agents.insert("defaults".to_string(), json!({}));
        }
        let defaults = agents.get_mut("defaults").unwrap().as_object_mut().unwrap();
        if !defaults.get("model").map(|v| v.is_object()).unwrap_or(false) {
            defaults.insert("model".to_string(), json!({}));
        }
        if !defaults.get("models").map(|v| v.is_object()).unwrap_or(false) {
            defaults.insert("models".to_string(), json!({}));
        }
    }

    let normalized_base = v1_url(base_url);
    let full_model_id = format!("9router/{}", model);

    let mut all_model_ids = vec![model.to_string()];
    let mut seen_models: HashSet<String> = [model.to_string()].into_iter().collect();
    for val in agent_models.values() {
        if let Some(m) = val.as_str().filter(|s| !s.is_empty()) {
            if seen_models.insert(m.to_string()) {
                all_model_ids.push(m.to_string());
            }
        }
    }

    {
        let defaults = settings
            .get_mut("agents")
            .unwrap()
            .get_mut("defaults")
            .unwrap()
            .as_object_mut()
            .unwrap();
        defaults
            .get_mut("model")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("primary".to_string(), Value::String(full_model_id));

        let allow = defaults
            .get_mut("models")
            .unwrap()
            .as_object_mut()
            .unwrap();
        allow.retain(|k, _| !k.starts_with("9router/"));
        for m in &all_model_ids {
            allow.insert(format!("9router/{}", m), json!({}));
        }
    }

    if let Some(list) = settings
        .get_mut("agents")
        .and_then(|a| a.get_mut("list"))
        .and_then(|l| l.as_array_mut())
    {
        for item in list.iter_mut() {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            if resolve_openclaw_agent_model(obj.get("model")).starts_with("9router/") {
                obj.remove("model");
            }
            if let Some(agent_id) = obj.get("id").and_then(|v| v.as_str()).map(ToString::to_string)
            {
                if let Some(am) = agent_models
                    .get(&agent_id)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    obj.insert("model".to_string(), Value::String(format!("9router/{}", am)));
                }
            }
            if let Some(agent_dir) = obj
                .get("agentDir")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
            {
                let agent_id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let model_to_write = agent_models
                    .get(agent_id)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(model);
                let _ = write_openclaw_agent_models(
                    &agent_dir,
                    model_to_write,
                    &normalized_base,
                    api_key,
                );
            }
        }
    }

    if !settings.get("models").map(|v| v.is_object()).unwrap_or(false) {
        settings.insert("models".to_string(), json!({}));
    }
    let models_obj = settings.get_mut("models").unwrap().as_object_mut().unwrap();
    if !models_obj
        .get("providers")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        models_obj.insert("providers".to_string(), json!({}));
    }
    let providers_obj = models_obj
        .get_mut("providers")
        .unwrap()
        .as_object_mut()
        .unwrap();
    providers_obj.insert(
        "9router".to_string(),
        json!({
            "baseUrl": normalized_base,
            "apiKey": if api_key.is_empty() { "your_api_key" } else { api_key },
            "api": "openai-completions",
            "models": all_model_ids
                .iter()
                .map(|m| json!({ "id": m, "name": m.rsplit('/').next().unwrap_or(m) }))
                .collect::<Vec<_>>(),
        }),
    );

    if write_json_pretty_with_backup(&settings_path, &Value::Object(settings)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update openclaw settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "Open Claw settings applied successfully!",
        "settingsPath": settings_path.to_string_lossy(),
    }))
    .into_response()
}

async fn openclaw_delete() -> Response {
    let settings_path = openclaw_settings_path();
    if !settings_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No settings file to reset"
        }))
        .into_response();
    }

    let mut settings = match read_json_or_empty_obj(&settings_path) {
        Ok(obj) => obj,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset openclaw settings",
            )
        }
    };

    if let Some(models_obj) = settings.get_mut("models").and_then(|v| v.as_object_mut()) {
        if let Some(providers) = models_obj.get_mut("providers").and_then(|v| v.as_object_mut()) {
            providers.remove("9router");
            if providers.is_empty() {
                models_obj.remove("providers");
            }
        }
    }

    if let Some(defaults) = settings
        .get_mut("agents")
        .and_then(|a| a.get_mut("defaults"))
        .and_then(|d| d.as_object_mut())
    {
        if let Some(allow) = defaults.get_mut("models").and_then(|m| m.as_object_mut()) {
            allow.retain(|k, _| !k.starts_with("9router/"));
            if allow.is_empty() {
                defaults.remove("models");
            }
        }
        if let Some(model_obj) = defaults.get_mut("model").and_then(|m| m.as_object_mut()) {
            if model_obj
                .get("primary")
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with("9router/"))
                .unwrap_or(false)
            {
                model_obj.remove("primary");
            }
        }
    }

    if write_json_pretty_no_backup(&settings_path, &Value::Object(settings)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset openclaw settings",
        );
    }

    Json(json!({
        "success": true,
        "message": "9Router settings removed successfully"
    }))
    .into_response()
}

// ============================================================================
// 22. /api/cli-tools/opencode-settings
// ============================================================================

fn opencode_config_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("opencode")
        .join("opencode.json")
}

fn resolve_alias_to_provider_id(alias: &str) -> &str {
    match alias {
        "ag" => "antigravity",
        "cc" => "claude",
        "cl" => "cline",
        "cbcn" => "codebuddy-cn",
        "cx" => "codex",
        "cu" => "cursor",
        "gc" => "gemini-cli",
        "gh" => "github",
        "if" => "iflow",
        "kc" => "kilocode",
        "kr" => "kiro",
        "mmf" => "mimo-free",
        "qdcn" => "qoder-cn",
        "ocz" => "opencode-zen",
        "oc" => "opencode",
        "qd" => "qoder",
        "el" => "elevenlabs",
        "gpse" => "google-pse",
        "cbai" => "codebuddy-intl",
        "zd" => "zed",
        "af" => "api-airforce",
        "qianfan" => "baidu",
        "bzl" => "bazaarlink",
        "bm" => "bluesminds",
        "kgw" => "kilo-gateway",
        "samba" => "sambanova",
        "hunyuan" => "tencent",
        "mm" => "minimax-code",
        "mmg" => "minimax-code-global",
        "br" => "bedrock",
        "brx" => "bedrock-xai",
        "polly" => "aws-polly",
        "brave" => "brave-search",
        "jina" => "jina-ai",
        "fish" => "fish-audio",
        "veo-free" => "veoaifree-web",
        "cmc" => "commandcode",
        "dv" => "devin-cli",
        other => other,
    }
}

fn get_thinking_levels(provider: &str, model: &str) -> Option<Vec<&'static str>> {
    let caps = model_caps_for(Some(provider), model);
    if !caps.reasoning {
        return None;
    }

    let mut levels: Vec<&'static str> = if glob_match("*gemini-4*", model) {
        vec!["low", "medium", "high"]
    } else if glob_match("*claude*4.6*", model) || glob_match("*claude*4-6*", model) {
        vec!["none", "low", "medium", "high", "max"]
    } else if provider == "codex" && glob_match("*gpt-6*", model) {
        vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    } else if provider == "codex"
        && (glob_match("*gpt-5.6-sol*", model) || glob_match("*gpt-5.6-terra*", model))
    {
        vec![
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ]
    } else if provider == "codex" && glob_match("*gpt-5.6-luna*", model) {
        vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    } else if glob_match("*codex*", model) {
        vec!["low", "medium", "high", "xhigh"]
    } else if provider == "opencode" && glob_match("*mimo*v2.6*", model) {
        vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    } else if glob_match("*mimo*v2.6*", model) || glob_match("*mimo*v2.5-pro*", model) {
        vec!["none", "low", "medium", "high", "xhigh"]
    } else if provider == "codebuddy-cn" && model == "deepseek-v4-pro" {
        vec!["low", "high", "xhigh"]
    } else if (provider == "codebuddy-cn" && model == "deepseek-v4.1-flash")
        || glob_match("*deepseek-v4.1-flash-free*", model)
    {
        vec!["low", "high", "max"]
    } else if glob_match("*deepseek-v4.*", model) {
        vec!["none", "low", "medium", "high", "xhigh", "max"]
    } else if provider == "minimax-code" || provider == "minimax-code-global" {
        if glob_match("MiniMax-M3.1*", model) {
            vec!["low", "medium", "high", "xhigh", "max"]
        } else if model == "MiniMax-M3" {
            vec!["none", "high"]
        } else {
            vec!["none", "thinking"]
        }
    } else {
        match caps.thinking_format {
            Some("openai") => vec!["none", "minimal", "low", "medium", "high", "xhigh"],
            Some("claude-adaptive") | Some("claude-budget") => {
                vec!["none", "low", "medium", "high", "xhigh", "max"]
            }
            Some("gemini-level") => vec!["minimal", "low", "medium", "high"],
            Some("kimi") => vec!["none", "low", "medium", "high", "max"],
            Some("deepseek") => vec!["none", "high", "max"],
            Some("zai") | Some("minimax") => vec!["none", "thinking"],
            Some("commandcode") => vec!["none", "low", "medium", "high", "xhigh", "max"],
            _ => vec!["none", "low", "medium", "high"],
        }
    };

    if !caps.thinking_can_disable {
        levels.retain(|l| *l != "none");
    }
    Some(levels)
}

fn build_effort_variants(full_model_id: &str) -> Option<Value> {
    let slash = full_model_id.find('/')?;
    if slash == 0 {
        return None;
    }
    let alias = &full_model_id[..slash];
    let provider_id = resolve_alias_to_provider_id(alias);
    let levels = get_thinking_levels(provider_id, &full_model_id[slash + 1..])?;
    if levels.is_empty() {
        return None;
    }
    let mut map = Map::new();
    for level in levels {
        map.insert(level.to_string(), json!({ "reasoningEffort": level }));
    }
    Some(Value::Object(map))
}

async fn opencode_get() -> Response {
    let config_path = opencode_config_path();
    if !is_tool_installed("opencode", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "OpenCode CLI is not installed"
        }))
        .into_response();
    }

    let config = read_jsonc_value(&config_path);
    let provider_cfg = config
        .as_ref()
        .and_then(|c| c.get("provider"))
        .and_then(|p| p.get("9router"));
    let models: Vec<String> = provider_cfg
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let active_model = config
        .as_ref()
        .and_then(|c| c.get("model"))
        .and_then(|m| m.as_str())
        .and_then(|s| s.strip_prefix("9router/"))
        .map(ToString::to_string);
    let base_url = provider_cfg
        .and_then(|p| p.get("options"))
        .and_then(|o| o.get("baseURL"))
        .cloned()
        .unwrap_or(Value::Null);

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": provider_cfg.is_some(),
        "configPath": config_path.to_string_lossy(),
        "opencode": {
            "models": models,
            "activeModel": active_model,
            "baseURL": base_url,
        }
    }))
    .into_response()
}

async fn opencode_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to apply settings");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let models_array: Vec<String> = match raw.get("models").and_then(|v| v.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect(),
        None => raw
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };

    if base_url.is_empty() || models_array.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "baseUrl and at least one model are required",
        );
    }

    let config_path = opencode_config_path();
    let mut config = read_json_obj_optional(&config_path).unwrap_or_default();
    let normalized_base = v1_url(base_url);
    let key_to_use = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let effective_subagent = raw
        .get("subagentModel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&models_array[0])
        .to_string();

    let mut providers = config
        .get("provider")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut existing_provider = providers
        .get("9router")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(|| {
            let mut m = Map::new();
            m.insert(
                "npm".to_string(),
                Value::String("@ai-sdk/openai-compatible".to_string()),
            );
            m.insert("options".to_string(), json!({}));
            m.insert("models".to_string(), json!({}));
            m
        });

    let mut options = existing_provider
        .get("options")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    options.insert("baseURL".to_string(), Value::String(normalized_base));
    options.insert("apiKey".to_string(), Value::String(key_to_use));
    existing_provider.insert("options".to_string(), Value::Object(options));

    let mut models_map = existing_provider
        .get("models")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for m in &models_array {
        if m.is_empty() {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("name".to_string(), Value::String(m.clone()));
        entry.insert(
            "modalities".to_string(),
            json!({ "input": ["text", "image"], "output": ["text"] }),
        );
        let existing_variants = models_map
            .get(m)
            .and_then(|v| v.get("variants"))
            .cloned()
            .or_else(|| build_effort_variants(m));
        if let Some(variants) = existing_variants {
            entry.insert("variants".to_string(), variants);
        }
        models_map.insert(m.clone(), Value::Object(entry));
    }
    existing_provider.insert("models".to_string(), Value::Object(models_map));
    providers.insert("9router".to_string(), Value::Object(existing_provider));
    config.insert("provider".to_string(), Value::Object(providers));

    match raw.get("activeModel").and_then(|v| v.as_str()) {
        Some("") => {
            config.insert("model".to_string(), Value::String(String::new()));
        }
        Some(am) => {
            config.insert("model".to_string(), Value::String(format!("9router/{}", am)));
        }
        None => {
            config.insert(
                "model".to_string(),
                Value::String(format!("9router/{}", models_array[0])),
            );
        }
    }

    let mut agent_obj = config
        .get("agent")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    agent_obj.insert(
        "explorer".to_string(),
        json!({
            "description": "Fast explorer subagent for codebase exploration",
            "mode": "subagent",
            "model": format!("9router/{}", effective_subagent),
        }),
    );
    config.insert("agent".to_string(), Value::Object(agent_obj));

    if write_json_pretty_with_backup(&config_path, &Value::Object(config)).is_err() {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to apply settings");
    }

    Json(json!({
        "success": true,
        "message": "OpenCode settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn opencode_patch(body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to patch settings");
    };
    let config_path = opencode_config_path();
    if !config_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No config file found"
        }))
        .into_response();
    }

    let mut config = match read_json_or_empty_obj(&config_path) {
        Ok(obj) => obj,
        Err(_) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to patch settings"),
    };

    if raw.get("clearActiveModel").and_then(|v| v.as_bool()) == Some(true) {
        if config
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.starts_with("9router/"))
            .unwrap_or(false)
        {
            config.insert("model".to_string(), Value::String(String::new()));
        }
    }

    if write_json_pretty_no_backup(&config_path, &Value::Object(config)).is_err() {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to patch settings");
    }

    Json(json!({
        "success": true,
        "message": "Settings updated"
    }))
    .into_response()
}

async fn opencode_delete(Query(params): Query<HashMap<String, String>>) -> Response {
    let model_to_remove = params.get("model").filter(|s| !s.is_empty()).cloned();
    let config_path = opencode_config_path();
    if !config_path.exists() {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    }

    let mut config = match read_json_or_empty_obj(&config_path) {
        Ok(obj) => obj,
        Err(_) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to reset opencode settings",
            )
        }
    };

    if let Some(ref target) = model_to_remove {
        let mut remaining_first: Option<String> = None;
        let mut emptied = false;
        let mut had_models = false;
        if let Some(models_map) = config
            .get_mut("provider")
            .and_then(|p| p.get_mut("9router"))
            .and_then(|r| r.get_mut("models"))
            .and_then(|m| m.as_object_mut())
        {
            had_models = true;
            models_map.remove(target);
            if models_map.is_empty() {
                emptied = true;
            } else {
                remaining_first = models_map.keys().next().cloned();
            }
        }
        if had_models {
            if emptied {
                if let Some(prov) = config.get_mut("provider").and_then(|v| v.as_object_mut()) {
                    prov.remove("9router");
                }
                if config
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.starts_with("9router/"))
                    .unwrap_or(false)
                {
                    config.remove("model");
                }
            } else if config.get("model").and_then(|v| v.as_str())
                == Some(&format!("9router/{}", target))
            {
                if let Some(first) = remaining_first {
                    config.insert(
                        "model".to_string(),
                        Value::String(format!("9router/{}", first)),
                    );
                }
            }
        }
    } else {
        if let Some(prov) = config.get_mut("provider").and_then(|v| v.as_object_mut()) {
            prov.remove("9router");
        }
        if config
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.starts_with("9router/"))
            .unwrap_or(false)
        {
            config.remove("model");
        }
    }

    if let Some(agent) = config.get_mut("agent").and_then(|v| v.as_object_mut()) {
        if agent
            .get("explorer")
            .and_then(|e| e.get("model"))
            .and_then(|m| m.as_str())
            .map(|s| s.starts_with("9router/"))
            .unwrap_or(false)
        {
            agent.remove("explorer");
            if agent.is_empty() {
                config.remove("agent");
            }
        }
    }

    if write_json_pretty_no_backup(&config_path, &Value::Object(config)).is_err() {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to reset opencode settings",
        );
    }

    let message = match model_to_remove {
        Some(m) => format!("Model \"{}\" removed", m),
        None => "9Router settings removed from OpenCode".to_string(),
    };
    Json(json!({
        "success": true,
        "message": message,
    }))
    .into_response()
}

// ============================================================================
// 23. /api/cli-tools/pi-settings
// ============================================================================

fn resolve_pi_models_json_path() -> PathBuf {
    let agent_path = home_dir().join(".pi").join("agent").join("models.json");
    let root_path = home_dir().join(".pi").join("models.json");
    if agent_path.exists() {
        agent_path
    } else if root_path.exists() {
        root_path
    } else {
        agent_path
    }
}

fn pi_has_9router(cfg: Option<&Map<String, Value>>) -> bool {
    let Some(providers) = cfg.and_then(|c| c.get("providers")).and_then(|v| v.as_object()) else {
        return false;
    };
    if providers
        .get("9router")
        .and_then(|p| p.get("baseUrl"))
        .and_then(|u| u.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    providers.values().any(|prov| {
        prov.get("baseUrl")
            .and_then(|u| u.as_str())
            .map(|s| s.contains("20128"))
            .unwrap_or(false)
    })
}

fn cap_positive(v: Option<&Value>, fallback: u64, max: u64) -> u64 {
    match v.and_then(|val| val.as_u64().or_else(|| val.as_f64().map(|f| f as u64))) {
        Some(n) if n > 0 && n <= max => n,
        _ => fallback,
    }
}

async fn pi_get() -> Response {
    let agent_path = home_dir().join(".pi").join("agent").join("models.json");
    let root_path = home_dir().join(".pi").join("models.json");
    if !is_tool_installed("pi", &[agent_path, root_path]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Pi CLI is not installed"
        }))
        .into_response();
    }

    let config_path = resolve_pi_models_json_path();
    let config = read_jsonc_value(&config_path);
    let has_9router = pi_has_9router(config.as_ref().and_then(|v| v.as_object()));

    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn pi_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }

    let config_path = resolve_pi_models_json_path();
    let mut existing = read_json_obj_optional(&config_path).unwrap_or_default();
    let mut providers = existing
        .get("providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let existing_provider = providers
        .get("9router")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut prev_models: HashMap<String, Map<String, Value>> = HashMap::new();
    if let Some(arr) = existing_provider.get("models").and_then(|v| v.as_array()) {
        for m in arr {
            if let Some(s) = m.as_str() {
                let mut obj = Map::new();
                obj.insert("id".to_string(), Value::String(s.to_string()));
                prev_models.insert(s.to_string(), obj);
            } else if let Some(obj) = m.as_object() {
                if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                    prev_models.insert(id.to_string(), obj.clone());
                }
            }
        }
    }

    let with_defaults = |m_obj: Map<String, Value>| -> Value {
        let id = m_obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("provider/model-id")
            .to_string();
        let prev = prev_models.get(&id).cloned().unwrap_or_default();
        let mut merged = prev.clone();
        for (k, v) in &m_obj {
            merged.insert(k.clone(), v.clone());
        }
        let m_name = m_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let prev_name = prev.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let resolved_name = if !m_name.is_empty() && m_name != id {
            m_name.to_string()
        } else if !prev_name.is_empty() {
            prev_name.to_string()
        } else {
            id.clone()
        };
        let ctx_win = cap_positive(
            m_obj.get("contextWindow").or_else(|| prev.get("contextWindow")),
            200_000,
            2_000_000,
        );
        let max_tok = cap_positive(
            m_obj.get("maxTokens").or_else(|| prev.get("maxTokens")),
            64_000,
            200_000,
        );
        merged.insert("id".to_string(), Value::String(id));
        merged.insert("name".to_string(), Value::String(resolved_name));
        merged.insert("contextWindow".to_string(), json!(ctx_win));
        merged.insert("maxTokens".to_string(), json!(max_tok));
        Value::Object(merged)
    };

    let model_list: Vec<Value> = match raw.get("models").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .map(|m| {
                if let Some(s) = m.as_str() {
                    let mut map = Map::new();
                    map.insert("id".to_string(), Value::String(s.to_string()));
                    with_defaults(map)
                } else {
                    let mut map = m.as_object().cloned().unwrap_or_default();
                    if !map.get("id").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                        map.insert(
                            "id".to_string(),
                            Value::String("provider/model-id".to_string()),
                        );
                    }
                    with_defaults(map)
                }
            })
            .collect(),
        _ => {
            let model_id = raw
                .get("model")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("provider/model-id");
            let mut map = Map::new();
            map.insert("id".to_string(), Value::String(model_id.to_string()));
            vec![with_defaults(map)]
        }
    };

    let api_key = raw
        .get("apiKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            existing_provider
                .get("apiKey")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| resolve_cli_api_key(&state.db, None));

    let api_val = existing_provider
        .get("api")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("openai-completions")
        .to_string();

    let mut new_provider = existing_provider;
    new_provider.insert("baseUrl".to_string(), Value::String(v1_url(base_url)));
    new_provider.insert("apiKey".to_string(), Value::String(api_key));
    new_provider.insert("api".to_string(), Value::String(api_val));
    new_provider.insert("models".to_string(), Value::Array(model_list));
    providers.insert("9router".to_string(), Value::Object(new_provider));
    existing.insert("providers".to_string(), Value::Object(providers));

    if let Err(e) = write_json_pretty_with_backup(&config_path, &Value::Object(existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "Pi settings applied! Use /model in Pi to select the 9Router model.",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn pi_delete() -> Response {
    let config_path = resolve_pi_models_json_path();
    let Some(mut existing) = read_json_obj_optional(&config_path) else {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    };

    if let Some(providers) = existing.get_mut("providers").and_then(|v| v.as_object_mut()) {
        if providers.remove("9router").is_some() {
            if providers.is_empty() {
                existing.remove("providers");
            }
            if let Err(e) = write_json_pretty_no_backup(&config_path, &Value::Object(existing)) {
                return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
            }
        }
    }

    Json(json!({
        "success": true,
        "message": "9Router removed from Pi"
    }))
    .into_response()
}

// ============================================================================
// 24. /api/cli-tools/smelt-settings
// ============================================================================

fn smelt_config_path() -> PathBuf {
    home_dir().join(".smelt").join("config.json")
}

fn smelt_has_9router(cfg: Option<&Map<String, Value>>) -> bool {
    let Some(cfg) = cfg else { return false };
    get_str_field(cfg, "_managedBy") == Some("9router")
        || get_str_field(cfg, "baseUrl")
            .map(|u| u.contains("20128"))
            .unwrap_or(false)
}

async fn smelt_get() -> Response {
    let config_path = smelt_config_path();
    if !is_tool_installed("smelt", &[config_path.clone()]) {
        return Json(json!({
            "installed": false,
            "config": null,
            "message": "Smelt CLI is not installed"
        }))
        .into_response();
    }

    let config = read_jsonc_value(&config_path);
    let has_9router = smelt_has_9router(config.as_ref().and_then(|v| v.as_object()));
    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn smelt_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err_obj(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let base_url = raw.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() {
        return json_err_obj(StatusCode::BAD_REQUEST, "baseUrl is required");
    }

    let config_path = smelt_config_path();
    let mut existing = read_json_obj_optional(&config_path).unwrap_or_default();
    let api_key = resolve_cli_api_key(&state.db, raw.get("apiKey").and_then(|v| v.as_str()));
    let model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| get_str_field(&existing, "model").map(ToString::to_string))
        .unwrap_or_else(|| "provider/model-id".to_string());

    existing.insert("baseUrl".to_string(), Value::String(v1_url(base_url)));
    existing.insert("apiKey".to_string(), Value::String(api_key));
    existing.insert("model".to_string(), Value::String(model));
    existing.insert(
        "_managedBy".to_string(),
        Value::String("9router".to_string()),
    );

    if let Err(e) = write_json_pretty_with_backup(&config_path, &Value::Object(existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "Smelt settings applied successfully!",
        "configPath": config_path.to_string_lossy(),
    }))
    .into_response()
}

async fn smelt_delete() -> Response {
    let config_path = smelt_config_path();
    let Some(mut existing) = read_json_obj_optional(&config_path) else {
        return Json(json!({
            "success": true,
            "message": "No config file to reset"
        }))
        .into_response();
    };

    existing.remove("baseUrl");
    existing.remove("apiKey");
    existing.remove("model");
    existing.remove("_managedBy");

    if existing.is_empty() {
        let _ = std::fs::remove_file(&config_path);
    } else if let Err(e) = write_json_pretty_no_backup(&config_path, &Value::Object(existing)) {
        return json_err_obj(StatusCode::INTERNAL_SERVER_ERROR, e);
    }

    Json(json!({
        "success": true,
        "message": "Smelt 9Router settings removed"
    }))
    .into_response()
}

// ============================================================================
// 25. /api/cli-tools/antigravity-mitm
// ============================================================================

const MITM_PWD_SALT: &str = "9router-mitm-pwd";

static MITM_CACHED_PASSWORD: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn cached_mitm_password() -> &'static RwLock<Option<String>> {
    MITM_CACHED_PASSWORD.get_or_init(|| RwLock::new(None))
}

fn get_cached_mitm_password() -> Option<String> {
    cached_mitm_password().read().ok().and_then(|g| g.clone())
}

fn set_cached_mitm_password(pwd: Option<String>) {
    if let Ok(mut g) = cached_mitm_password().write() {
        *g = pwd;
    }
}

fn derive_mitm_pwd_key(data_dir: &Path) -> [u8; 32] {
    let raw = load_or_create_raw_machine_id(data_dir);
    let d = digest::digest(&digest::SHA256, format!("{}{}", raw, MITM_PWD_SALT).as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(d.as_ref());
    key
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn encrypt_mitm_password(data_dir: &Path, plaintext: &str) -> Option<String> {
    let key_bytes = derive_mitm_pwd_key(data_dir);
    let unbound = UnboundKey::new(&AES_256_GCM, &key_bytes).ok()?;
    let key = LessSafeKey::new(unbound);
    let rng = SystemRandom::new();
    let mut iv = [0u8; 12];
    rng.fill(&mut iv).ok()?;
    let nonce = Nonce::assume_unique_for_key(iv);
    let mut buf = plaintext.as_bytes().to_vec();
    let tag = key
        .seal_in_place_separate_tag(nonce, Aad::empty(), &mut buf)
        .ok()?;
    Some(format!(
        "{}:{}:{}",
        hex_encode(&iv),
        hex_encode(tag.as_ref()),
        hex_encode(&buf)
    ))
}

fn decrypt_mitm_password(data_dir: &Path, stored: &str) -> Option<String> {
    let mut parts = stored.split(':');
    let iv = hex_decode(parts.next()?)?;
    let tag = hex_decode(parts.next()?)?;
    let data = hex_decode(parts.next()?)?;
    if parts.next().is_some() || iv.len() != 12 {
        return None;
    }
    let key_bytes = derive_mitm_pwd_key(data_dir);
    let unbound = UnboundKey::new(&AES_256_GCM, &key_bytes).ok()?;
    let key = LessSafeKey::new(unbound);
    let mut iv_arr = [0u8; 12];
    iv_arr.copy_from_slice(&iv);
    let nonce = Nonce::assume_unique_for_key(iv_arr);
    let mut combined = Vec::with_capacity(data.len() + tag.len());
    combined.extend_from_slice(&data);
    combined.extend_from_slice(&tag);
    let plain = key.open_in_place(nonce, Aad::empty(), &mut combined).ok()?;
    String::from_utf8(plain.to_vec()).ok()
}

fn load_encrypted_mitm_password(state: &AppState) -> Option<String> {
    let settings = state.db.load_settings().ok()?;
    let enc = settings.get("mitmSudoEncrypted").and_then(|v| v.as_str())?;
    decrypt_mitm_password(&state.config.data_dir, enc)
}

fn update_settings_fields(db: &Database, updates: Map<String, Value>) -> Result<(), String> {
    let mut settings = db
        .load_settings()
        .unwrap_or_else(|_| Value::Object(Map::new()));
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let obj = settings.as_object_mut().unwrap();
    for (k, v) in updates {
        if v.is_null() {
            obj.remove(&k);
        } else {
            obj.insert(k, v);
        }
    }
    db.save_settings(&settings).map_err(|e| e.to_string())
}

fn normalize_mitm_router_base_url(input: Option<&Value>, default_port: u16) -> Result<String, String> {
    let default_base = format!("http://localhost:{}", default_port);
    let Some(val) = input else {
        return Ok(default_base);
    };
    if val.is_null() {
        return Ok(default_base);
    }
    let raw = match val {
        Value::String(s) => s.trim(),
        _ => return Err("Invalid MITM router URL".to_string()),
    };
    if raw.is_empty() {
        return Ok(default_base);
    }
    let trimmed = raw.trim_end_matches('/');
    let parsed = url::Url::parse(trimmed).map_err(|_| "Invalid MITM router URL".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("MITM router URL must use http or https".to_string());
    }
    Ok(trimmed.to_string())
}

fn check_is_admin() -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "net session >nul 2>&1"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
    }
}

fn is_sudo_available() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        find_in_path("sudo").is_some()
    }
}

fn can_run_sudo_without_password() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        if !is_sudo_available() {
            return true;
        }
        Command::new("sudo")
            .args(["-n", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn is_sudo_password_required() -> bool {
    !cfg!(windows) && is_sudo_available() && !can_run_sudo_without_password()
}

fn check_mitm_privilege(pwd: &str) -> bool {
    if check_is_admin() {
        return true;
    }
    if cfg!(windows) {
        return false;
    }
    if !is_sudo_password_required() {
        return true;
    }
    !pwd.is_empty()
}

fn read_mitm_running_state(data_dir: &Path) -> (bool, Option<u32>) {
    let mgr = MitmDaemonManager::new();
    if let Some(rec) = mgr.read_pid_record() {
        if MitmDaemonManager::is_pid_alive(rec.pid) {
            return (true, Some(rec.pid));
        }
    }
    let legacy_pid = data_dir.join("mitm").join(".mitm.pid");
    if let Ok(s) = std::fs::read_to_string(&legacy_pid) {
        if let Ok(pid) = s.trim().parse::<u32>() {
            if MitmDaemonManager::is_pid_alive(pid) {
                return (true, Some(pid));
            }
        }
    }
    (false, None)
}

fn read_dns_status_from_file(hosts_path: &Path) -> HashMap<String, bool> {
    let content = std::fs::read_to_string(hosts_path).unwrap_or_default();
    check_tool_dns_status(&content)
}

fn check_port_443_free() -> bool {
    TcpListener::bind("127.0.0.1:443").is_ok()
}

fn get_port_443_owner() -> Option<(u32, String)> {
    #[cfg(not(windows))]
    {
        let out = Command::new("lsof")
            .args(["-nP", "-iTCP:443", "-sTCP:LISTEN", "-t"])
            .output()
            .ok()?;
        let pid_str = String::from_utf8_lossy(&out.stdout);
        let pid: u32 = pid_str.lines().next()?.trim().parse().ok()?;
        let ps = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok();
        let name = ps
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let short_name = name
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&name)
            .to_string();
        Some((pid, short_name))
    }
    #[cfg(windows)]
    {
        None
    }
}

fn exec_with_sudo(cmd_str: &str, sudo_pwd: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = (cmd_str, sudo_pwd);
        Err("Use elevated PowerShell on Windows".to_string())
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        if is_sudo_available() && !check_is_admin() {
            let mut child = Command::new("sudo")
                .args(["-S", "sh", "-c", cmd_str])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = writeln!(stdin, "{}", sudo_pwd);
            }
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        } else {
            let out = Command::new("sh")
                .args(["-c", cmd_str])
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        }
    }
}

fn enable_or_disable_tool_dns(tool: &str, enable: bool, sudo_pwd: &str) -> Result<(), String> {
    let hosts = TOOL_HOSTS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, h)| *h)
        .ok_or_else(|| format!("Unknown tool: {}", tool))?;

    let hosts_path = get_hosts_file_path();
    let current = std::fs::read_to_string(&hosts_path).unwrap_or_default();
    let updated = if enable {
        crate::mitm::dns::compute_added_hosts_content(&current, hosts, "127.0.0.1")
    } else {
        crate::mitm::dns::compute_removed_hosts_content(&current, hosts)
    };
    if current == updated {
        return Ok(());
    }
    if enable {
        if enable_dns_for_hosts(hosts).is_ok() {
            return Ok(());
        }
    } else if disable_dns_for_hosts(hosts).is_ok() {
        return Ok(());
    }

    // Fallback: write via sudo cp if direct write/default sudo lacked password
    let tmp = std::env::temp_dir().join(format!("9r_hosts_api_{}", std::process::id()));
    std::fs::write(&tmp, &updated).map_err(|e| e.to_string())?;
    let cmd = format!(
        "cp '{}' '{}'",
        tmp.to_string_lossy().replace('\'', "'\\''"),
        hosts_path.to_string_lossy().replace('\'', "'\\''")
    );
    let res = exec_with_sudo(&cmd, sudo_pwd);
    let _ = std::fs::remove_file(&tmp);
    res?;
    let _ = crate::mitm::dns::flush_dns_cache();
    Ok(())
}

async fn mitm_get(State(state): State<AppState>) -> Response {
    let (running, pid) = read_mitm_running_state(&state.config.data_dir);
    let cert_path = state.config.data_dir.join("mitm").join("rootCA.crt");
    let cert_exists = cert_path.exists();
    let cert_trusted = if cert_exists {
        check_ca_installed(&cert_path)
    } else {
        false
    };
    let dns_status = read_dns_status_from_file(&get_hosts_file_path());
    let settings = state.db.load_settings().ok();
    let enc_pwd = settings
        .as_ref()
        .and_then(|s| s.get("mitmSudoEncrypted"))
        .and_then(|v| v.as_str())
        .and_then(|enc| decrypt_mitm_password(&state.config.data_dir, enc));
    let has_cached_password = get_cached_mitm_password().is_some() || enc_pwd.is_some();
    let is_win = cfg!(windows);
    let mitm_router_base_url = settings
        .as_ref()
        .and_then(|s| s.get("mitmRouterBaseUrl"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("http://localhost:{}", state.config.port));

    Json(json!({
        "running": running,
        "pid": pid,
        "certExists": cert_exists,
        "certTrusted": cert_trusted,
        "dnsStatus": dns_status,
        "hasCachedPassword": has_cached_password,
        "isWin": is_win,
        "needsSudoPassword": !is_win && !has_cached_password && is_sudo_password_required(),
        "isAdmin": check_is_admin(),
        "mitmRouterBaseUrl": mitm_router_base_url,
    }))
    .into_response()
}

async fn mitm_post(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let api_key = raw.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    let sudo_pwd_arg = raw
        .get("sudoPassword")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let pwd = sudo_pwd_arg
        .clone()
        .or_else(get_cached_mitm_password)
        .or_else(|| load_encrypted_mitm_password(&state))
        .unwrap_or_default();

    if api_key.is_empty() || (!cfg!(windows) && pwd.is_empty() && is_sudo_password_required()) {
        return json_err(
            StatusCode::BAD_REQUEST,
            if api_key.is_empty() {
                "Missing apiKey"
            } else {
                "Missing sudoPassword"
            },
        );
    }

    if !check_mitm_privilege(&pwd) {
        return json_err(
            StatusCode::FORBIDDEN,
            if cfg!(windows) {
                "Administrator required — restart 9Router as Administrator"
            } else {
                "Root or sudo password required to start MITM"
            },
        );
    }

    if let Some(url_val) = raw.get("mitmRouterBaseUrl").filter(|v| !v.is_null()) {
        match normalize_mitm_router_base_url(Some(url_val), state.config.port) {
            Ok(normalized) => {
                let mut u = Map::new();
                u.insert("mitmRouterBaseUrl".to_string(), Value::String(normalized));
                let _ = update_settings_fields(&state.db, u);
            }
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e),
        }
    }

    let (already_running, existing_pid) = read_mitm_running_state(&state.config.data_dir);
    if already_running {
        if !cfg!(windows) && !pwd.is_empty() {
            set_cached_mitm_password(Some(pwd.clone()));
        }
        return Json(json!({
            "success": true,
            "running": true,
            "pid": existing_pid,
        }))
        .into_response();
    }

    let force_kill = truthy_value(raw.get("forceKillPort443"));
    if !check_port_443_free() {
        if let Some((owner_pid, owner_name)) = get_port_443_owner() {
            if force_kill {
                let _ = exec_with_sudo(&format!("kill -9 {}", owner_pid), &pwd);
                tokio::time::sleep(Duration::from_millis(400)).await;
            } else {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": format!("Port 443 is already in use by \"{}\" (PID {}).", owner_name, owner_pid),
                        "code": "PORT_443_BUSY",
                        "portOwner": { "pid": owner_pid, "name": owner_name },
                    })),
                )
                    .into_response();
            }
        }
    }

    let mitm_dir = state.config.data_dir.join("mitm");
    if let Err(e) = CertificateAuthority::load_or_generate(&mitm_dir) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    let cert_path = mitm_dir.join("rootCA.crt");
    if !check_ca_installed(&cert_path) && (check_is_admin() || is_sudo_available()) {
        let _ = install_ca_to_system(&cert_path);
    }

    let router_base = state
        .db
        .load_settings()
        .ok()
        .and_then(|s| s.get("mitmRouterBaseUrl").and_then(|v| v.as_str()).map(ToString::to_string))
        .unwrap_or_else(|| format!("http://localhost:{}", state.config.port));

    let Ok(exe) = std::env::current_exe() else {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to locate current executable",
        );
    };
    let mitm_port: u16 = std::env::var("MITM_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(443);

    let child_pid: u32 = if !cfg!(windows) && !check_is_admin() && is_sudo_available() {
        use std::io::Write;
        let cmd_line = format!(
            "HOME='{}' DATA_DIR='{}' ROUTER_API_KEY='{}' MITM_ROUTER_BASE='{}' '{}' mitm run --port {}",
            home_dir().to_string_lossy().replace('\'', "'\\''"),
            state.config.data_dir.to_string_lossy().replace('\'', "'\\''"),
            api_key.replace('\'', "'\\''"),
            router_base.replace('\'', "'\\''"),
            exe.to_string_lossy().replace('\'', "'\\''"),
            mitm_port
        );
        let mut child = match Command::new("sudo")
            .args(["-S", "-E", "sh", "-c", &cmd_line])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", pwd);
        }
        child.id()
    } else {
        let child = match Command::new(&exe)
            .args(["mitm", "run", "--port", &mitm_port.to_string()])
            .env("ROUTER_API_KEY", api_key)
            .env("MITM_ROUTER_BASE", &router_base)
            .env("DATA_DIR", &state.config.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        child.id()
    };

    let mgr = MitmDaemonManager::new();
    let _ = mgr.write_pid_record(&MitmPidRecord {
        pid: child_pid,
        port: mitm_port,
        started_at: chrono::Utc::now().to_rfc3339(),
        exe: exe.to_string_lossy().to_string(),
    });

    if !cfg!(windows) && !pwd.is_empty() {
        set_cached_mitm_password(Some(pwd.clone()));
    }
    let mut updates = Map::new();
    updates.insert("mitmEnabled".to_string(), Value::Bool(true));
    updates.insert("mitmCertInstalled".to_string(), Value::Bool(true));
    if !pwd.is_empty() {
        if let Some(enc) = encrypt_mitm_password(&state.config.data_dir, &pwd) {
            updates.insert("mitmSudoEncrypted".to_string(), Value::String(enc));
        }
    }
    let _ = update_settings_fields(&state.db, updates);

    Json(json!({
        "success": true,
        "running": true,
        "pid": child_pid,
    }))
    .into_response()
}

async fn mitm_delete(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let body_json = parse_json_body(&body).ok();
    let sudo_pwd_arg = body_json
        .as_ref()
        .and_then(|b| b.get("sudoPassword"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let pwd = sudo_pwd_arg
        .clone()
        .or_else(get_cached_mitm_password)
        .or_else(|| load_encrypted_mitm_password(&state))
        .unwrap_or_default();

    if !cfg!(windows) && pwd.is_empty() && is_sudo_password_required() {
        return json_err(StatusCode::BAD_REQUEST, "Missing sudoPassword");
    }

    let (running, pid_opt) = read_mitm_running_state(&state.config.data_dir);
    if running {
        if let Some(pid) = pid_opt {
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/PID", &pid.to_string()])
                    .status();
            }
            #[cfg(not(windows))]
            {
                let _ = exec_with_sudo(&format!("kill {}", pid), &pwd);
            }
        }
    }
    MitmDaemonManager::new().remove_pid_file();
    let _ = std::fs::remove_file(state.config.data_dir.join("mitm").join(".mitm.pid"));
    let _ = clean_all_mitm_dns();

    if !cfg!(windows) {
        if let Some(p) = sudo_pwd_arg {
            set_cached_mitm_password(Some(p));
        }
    }
    let mut updates = Map::new();
    updates.insert("mitmEnabled".to_string(), Value::Bool(false));
    let _ = update_settings_fields(&state.db, updates);

    Json(json!({
        "success": true,
        "running": false,
    }))
    .into_response()
}

async fn mitm_patch(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let Ok(raw) = parse_json_body(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    let tool = raw.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let action = raw.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let sudo_pwd_arg = raw
        .get("sudoPassword")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let pwd = sudo_pwd_arg
        .clone()
        .or_else(get_cached_mitm_password)
        .or_else(|| load_encrypted_mitm_password(&state))
        .unwrap_or_default();

    if tool.is_empty() || action.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "tool and action required");
    }
    if !cfg!(windows) && pwd.is_empty() && is_sudo_password_required() {
        return json_err(StatusCode::BAD_REQUEST, "Missing sudoPassword");
    }
    if !check_mitm_privilege(&pwd) {
        return json_err(
            StatusCode::FORBIDDEN,
            if cfg!(windows) {
                "Administrator required — restart 9Router as Administrator"
            } else {
                "Root or sudo password required to modify DNS"
            },
        );
    }

    match action {
        "enable" | "disable" => {
            let enable = action == "enable";
            if let Err(e) = enable_or_disable_tool_dns(tool, enable, &pwd) {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, e);
            }
            let mut dns_enabled = state
                .db
                .load_settings()
                .ok()
                .and_then(|s| s.get("dnsToolEnabled").and_then(|v| v.as_object()).cloned())
                .unwrap_or_default();
            dns_enabled.insert(tool.to_string(), Value::Bool(enable));
            let mut updates = Map::new();
            updates.insert("dnsToolEnabled".to_string(), Value::Object(dns_enabled));
            let _ = update_settings_fields(&state.db, updates);

            if !cfg!(windows) {
                if let Some(p) = sudo_pwd_arg {
                    set_cached_mitm_password(Some(p));
                }
            }
            let dns_status = read_dns_status_from_file(&get_hosts_file_path());
            Json(json!({
                "success": true,
                "dnsStatus": dns_status,
            }))
            .into_response()
        }
        "trust-cert" => {
            let mitm_dir = state.config.data_dir.join("mitm");
            let _ = CertificateAuthority::load_or_generate(&mitm_dir);
            let cert_path = mitm_dir.join("rootCA.crt");
            if let Err(e) = install_ca_to_system(&cert_path) {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
            if !cfg!(windows) {
                if let Some(p) = sudo_pwd_arg {
                    set_cached_mitm_password(Some(p));
                }
            }
            Json(json!({
                "success": true,
                "certTrusted": check_ca_installed(&cert_path),
            }))
            .into_response()
        }
        _ => json_err(
            StatusCode::BAD_REQUEST,
            "action must be enable, disable, or trust-cert",
        ),
    }
}

// ============================================================================
// 26. /api/cli-tools/antigravity-mitm/alias
// ============================================================================

fn load_mitm_aliases_from_db(db: &Database, tool_name: Option<&str>) -> Result<Value, String> {
    if db.is_postgres() {
        let exported = db.export_database().map_err(|e| e.to_string())?;
        let all = exported
            .get("mitmAlias")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if let Some(tool) = tool_name {
            return Ok(all.get(tool).cloned().unwrap_or_else(|| json!({})));
        }
        return Ok(Value::Object(all));
    }

    let guard = db.reader().map_err(|e| e.to_string())?;
    if let Some(tool) = tool_name {
        let mut stmt = guard
            .prepare_cached("SELECT value FROM kv WHERE scope = 'mitmAlias' AND key = ?")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([tool]).map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let val_str: String = row.get(0).map_err(|e| e.to_string())?;
            return Ok(serde_json::from_str(&val_str).unwrap_or_else(|_| json!({})));
        }
        return Ok(json!({}));
    }

    let mut stmt = guard
        .prepare_cached("SELECT key, value FROM kv WHERE scope = 'mitmAlias'")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let k: String = row.get(0)?;
            let v_str: String = row.get(1)?;
            let v: Value = serde_json::from_str(&v_str).unwrap_or_else(|_| json!({}));
            Ok((k, v))
        })
        .map_err(|e| e.to_string())?;
    let mut map = Map::new();
    for r in rows.flatten() {
        map.insert(r.0, r.1);
    }
    Ok(Value::Object(map))
}

fn save_mitm_aliases_to_db(db: &Database, tool: &str, mappings: &Value) -> Result<(), String> {
    if db.is_postgres() {
        // Persist in settings fallback + aliases.json cache without touching db_pg.rs
        let mut updates = Map::new();
        let mut cur = db
            .load_settings()
            .ok()
            .and_then(|s| s.get("mitmAlias").and_then(|v| v.as_object()).cloned())
            .unwrap_or_default();
        cur.insert(tool.to_string(), mappings.clone());
        updates.insert("mitmAlias".to_string(), Value::Object(cur));
        return update_settings_fields(db, updates);
    }

    let val_str = serde_json::to_string(mappings).map_err(|e| e.to_string())?;
    let guard = db.writer();
    guard
        .execute(
            "INSERT INTO kv(scope, key, value) VALUES('mitmAlias', ?, ?) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
            rusqlite::params![tool, val_str],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn write_mitm_alias_cache(data_dir: &Path, tool: &str, mappings: &Map<String, Value>) {
    let cache_file = data_dir.join("mitm").join("aliases.json");
    if let Some(parent) = cache_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut current = read_json_obj_optional(&cache_file).unwrap_or_default();
    current.insert(tool.to_string(), Value::Object(mappings.clone()));
    let tmp = data_dir.join("mitm").join("aliases.json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(&Value::Object(current)) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &cache_file);
        }
    }
}

async fn mitm_alias_get(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let tool = params.get("tool").filter(|s| !s.is_empty()).map(String::as_str);
    match load_mitm_aliases_from_db(&state.db, tool) {
        Ok(aliases) => Json(json!({ "aliases": aliases })).into_response(),
        Err(_) => json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch aliases"),
    }
}

fn handle_mitm_alias_put_with_hosts(
    state: &AppState,
    hosts_path: &Path,
    body: &[u8],
) -> Response {
    let Ok(raw) = parse_json_body(body) else {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save aliases");
    };
    let tool = raw.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let Some(mappings) = raw.get("mappings").and_then(|v| v.as_object()) else {
        return json_err(StatusCode::BAD_REQUEST, "tool and mappings required");
    };
    if tool.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "tool and mappings required");
    }

    let dns_status = read_dns_status_from_file(hosts_path);
    if !dns_status.get(tool).copied().unwrap_or(false) {
        return json_err(
            StatusCode::FORBIDDEN,
            format!("DNS must be enabled for {} before editing model mappings", tool),
        );
    }

    let mut filtered = Map::new();
    for (alias, model_val) in mappings {
        if let Some(m) = model_val.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            filtered.insert(alias.clone(), Value::String(m.to_string()));
        }
    }

    let val = Value::Object(filtered.clone());
    if save_mitm_aliases_to_db(&state.db, tool, &val).is_err() {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to save aliases");
    }
    write_mitm_alias_cache(&state.config.data_dir, tool, &filtered);

    Json(json!({
        "success": true,
        "aliases": val,
    }))
    .into_response()
}

async fn mitm_alias_put(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    handle_mitm_alias_put_with_hosts(&state, &get_hosts_file_path(), &body)
}

// ============================================================================
// Tests (isolated with temp HOME / DATA_DIR — never mutates user dotfiles)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use tower::ServiceExt;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    fn env_mutex() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct TempEnvGuard {
        _lock: MutexGuard<'static, ()>,
        root: PathBuf,
        home_dir: PathBuf,
        xdg_dir: PathBuf,
        data_dir: PathBuf,
        prev_home: Option<String>,
        prev_xdg: Option<String>,
        prev_data: Option<String>,
        prev_path: Option<String>,
    }

    impl TempEnvGuard {
        fn new(name: &str) -> Self {
            let lock = env_mutex().lock().unwrap_or_else(|e| e.into_inner());
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "9r_cli_tools_test_{}_{}_{}",
                name,
                std::process::id(),
                seq
            ));
            let home = root.join("home");
            let xdg = root.join("xdg");
            let data = root.join("data");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&xdg).unwrap();
            std::fs::create_dir_all(&data).unwrap();

            let prev_home = std::env::var("HOME").ok();
            let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
            let prev_data = std::env::var("DATA_DIR").ok();
            let prev_path = std::env::var("PATH").ok();

            unsafe {
                std::env::set_var("HOME", &home);
                std::env::set_var("XDG_CONFIG_HOME", &xdg);
                std::env::set_var("DATA_DIR", &data);
                std::env::set_var("PATH", root.join("empty_bin"));
            }

            Self {
                _lock: lock,
                root,
                home_dir: home,
                xdg_dir: xdg,
                data_dir: data,
                prev_home,
                prev_xdg,
                prev_data,
                prev_path,
            }
        }
    }

    impl Drop for TempEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_xdg {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
                match &self.prev_data {
                    Some(v) => std::env::set_var("DATA_DIR", v),
                    None => std::env::remove_var("DATA_DIR"),
                }
                match &self.prev_path {
                    Some(v) => std::env::set_var("PATH", v),
                    None => std::env::remove_var("PATH"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn make_test_state(data_dir: &Path) -> AppState {
        let mut cfg = crate::config::Config::default();
        cfg.data_dir = data_dir.to_path_buf();
        cfg.db_path = data_dir.join("db.sqlite");
        cfg.port = 20128;
        let config = Arc::new(cfg);
        let db = Database::open_in_memory().unwrap();
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let circuit_breaker = Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::default());
        let combo_tracker =
            crate::combo_router::ComboTracker::with_circuit_breakers(Arc::clone(&circuit_breaker));
        let token_cache = crate::cache::TokenCache::default();
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

    async fn call_router(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let req_builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:20128");
        let req = match body {
            Some(v) => req_builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&v).unwrap()))
                .unwrap(),
            None => req_builder.body(Body::empty()).unwrap(),
        };
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let val = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, val)
    }

    fn seed_active_api_key(db: &Database, key_str: &str) {
        let mut k = crate::db::ApiKey::default();
        k.id = "k1".to_string();
        k.key = key_str.to_string();
        k.name = Some("Default".to_string());
        k.is_active = true;
        db.save_api_key(&k).unwrap();
    }

    #[tokio::test]
    async fn test_claude_settings_lifecycle() {
        let env = TempEnvGuard::new("claude");
        let state = make_test_state(&env.data_dir);
        let app = cli_tools_router().with_state(state);

        // Initially not installed
        let (st, val) = call_router(app.clone(), "GET", "/api/cli-tools/claude-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["installed"], json!(false));

        // Bad payload
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/claude-settings",
            Some(json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // POST apply + Exa + autoCompactWindow
        let (st, val) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/claude-settings",
            Some(json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "http://localhost:20128",
                    "ANTHROPIC_AUTH_TOKEN": "sk_test_1234567890",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "cc/claude-sonnet-4-6"
                },
                "exaMcpEnabled": true,
                "autoCompactWindow": 150000
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["success"], json!(true));

        let (st, val) = call_router(app.clone(), "GET", "/api/cli-tools/claude-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["installed"], json!(true));
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["exaMcpEnabled"], json!(true));
        assert_eq!(
            val["settings"]["env"]["ANTHROPIC_BASE_URL"],
            json!("http://localhost:20128/v1")
        );
        assert_eq!(
            val["settings"]["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"],
            json!("150000")
        );

        // DELETE reset
        let (st, val) =
            call_router(app.clone(), "DELETE", "/api/cli-tools/claude-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["success"], json!(true));

        let (st, val) = call_router(app, "GET", "/api/cli-tools/claude-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["has9Router"], json!(false));
        assert_eq!(val["exaMcpEnabled"], json!(false));
    }

    #[tokio::test]
    async fn test_cline_kilo_droid_smelt_crush_pi_openclaw_copilot() {
        let env = TempEnvGuard::new("json_tools");
        let state = make_test_state(&env.data_dir);
        seed_active_api_key(&state.db, "nr_active_db_key");
        let app = cli_tools_router().with_state(state);

        // Cline
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/cline-settings",
            Some(json!({ "baseUrl": "http://localhost:20128/v1", "apiKey": "k1", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/cline-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["settings"]["openAiBaseUrl"], json!("http://localhost:20128"));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/cline-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Kilo
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/kilo-settings",
            Some(json!({ "baseUrl": "http://127.0.0.1:20128", "apiKey": "k1", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/kilo-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/kilo-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Droid (multi-model + activeModel reorder)
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/droid-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128",
                "models": ["m-a", "m-b"],
                "activeModel": "m-b"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/droid-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["settings"]["customModels"][0]["model"], json!("m-b"));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/droid-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Crush (uses resolve_cli_api_key when apiKey omitted)
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/crush-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "apiKey": "sk_9router", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/crush-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(
            val["config"]["providers"]["9router"]["api_key"],
            json!("nr_active_db_key")
        );
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/crush-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Smelt
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/smelt-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/smelt-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["config"]["apiKey"], json!("nr_active_db_key"));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/smelt-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Pi
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/pi-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "models": ["cc/claude-sonnet-4-6"] })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/pi-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(
            val["config"]["providers"]["9router"]["apiKey"],
            json!("nr_active_db_key")
        );
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/pi-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // OpenClaw
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/openclaw-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128",
                "apiKey": "k1",
                "model": "cx/gpt-5",
                "agentModels": { "main": "cc/claude-opus-4-6" }
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/openclaw-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/openclaw-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Copilot
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/copilot-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128/v1",
                "models": ["cc/claude-sonnet-4-6"]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/copilot-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["currentModel"], json!("cc/claude-sonnet-4-6"));
        let (st, _) = call_router(app, "DELETE", "/api/cli-tools/copilot-settings", None).await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn test_opencode_patch_and_model_delete() {
        let env = TempEnvGuard::new("opencode");
        let state = make_test_state(&env.data_dir);
        seed_active_api_key(&state.db, "nr_oc_key");
        let app = cli_tools_router().with_state(state);

        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/opencode-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128",
                "models": ["cc/claude-opus-4-6", "cx/gpt-5.4"],
                "activeModel": "cc/claude-opus-4-6"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/opencode-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["opencode"]["activeModel"], json!("cc/claude-opus-4-6"));
        // Verify thinking variants were generated
        assert!(
            val["config"]["provider"]["9router"]["models"]["cc/claude-opus-4-6"]["variants"]["high"]
                .is_object()
        );

        // PATCH clearActiveModel
        let (st, _) = call_router(
            app.clone(),
            "PATCH",
            "/api/cli-tools/opencode-settings",
            Some(json!({ "clearActiveModel": true })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/opencode-settings", None).await;
        assert_eq!(val["opencode"]["activeModel"], Value::Null);

        // DELETE single model
        let (st, _) = call_router(
            app.clone(),
            "DELETE",
            "/api/cli-tools/opencode-settings?model=cc/claude-opus-4-6",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/opencode-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["opencode"]["models"], json!(["cx/gpt-5.4"]));

        // Full DELETE
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/opencode-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app, "GET", "/api/cli-tools/opencode-settings", None).await;
        assert_eq!(val["has9Router"], json!(false));
    }

    #[tokio::test]
    async fn test_toml_tools_codex_codewhale_forge_deepseek_jcode_grok() {
        let env = TempEnvGuard::new("toml_tools");
        let state = make_test_state(&env.data_dir);
        seed_active_api_key(&state.db, "nr_toml_key");
        let app = cli_tools_router().with_state(state);

        // Codex settings
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/codex-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "apiKey": "ck", "model": "gpt-5.4" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/codex-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/codex-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Codex profiles
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/codex-profiles",
            Some(json!({ "name": "work-fast", "model": "cx/gpt-5.4-mini" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/codex-profiles",
            Some(json!({ "name": "config", "model": "m" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/codex-profiles", None).await;
        assert_eq!(val["profiles"][0]["name"], json!("work-fast"));
        let (st, _) = call_router(
            app.clone(),
            "DELETE",
            "/api/cli-tools/codex-profiles",
            Some(json!({ "name": "work-fast" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        // CodeWhale
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/codewhale-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/codewhale-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["config"]["openai"]["api_key"], json!("nr_toml_key"));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/codewhale-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Forge
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/forge-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "model": "m1" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/forge-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/forge-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // DeepSeek TUI
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/deepseek-tui-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "model": "deepseek-chat" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/deepseek-tui-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/deepseek-tui-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // jcode
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/jcode-settings",
            Some(json!({ "baseUrl": "http://localhost:20128", "apiKey": "jk_1", "models": ["cc/claude-opus-4-7"] })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/jcode-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let env_text = std::fs::read_to_string(env.xdg_dir.join("jcode/provider-9router.env")).unwrap();
        assert!(env_text.contains("JCODE_9ROUTER_API_KEY=\"jk_1\""));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/jcode-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // grok-build
        let grok_cfg = env.home_dir.join(".grok/config.toml");
        std::fs::create_dir_all(grok_cfg.parent().unwrap()).unwrap();
        std::fs::write(&grok_cfg, "[models]\ndefault = \"grok-code-fast\"\n").unwrap();
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/grok-build-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128",
                "model": "xai/grok-4.6",
                "subagentModels": { "explore": "cc/claude-haiku-4-5" }
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/grok-build-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["settings"]["model"]["context_window"], json!(500000));
        assert_eq!(
            val["settings"]["subagentMappings"]["explore"],
            json!("9router-explore")
        );
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/grok-build-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        let restored = std::fs::read_to_string(&grok_cfg).unwrap();
        assert!(restored.contains("default = \"grok-code-fast\""));
        assert!(!restored.contains("[model.9router]"));
    }

    #[tokio::test]
    async fn test_hermes_profiles_settings_omp_cowork_devin() {
        let env = TempEnvGuard::new("yaml_cowork");
        let state = make_test_state(&env.data_dir);
        seed_active_api_key(&state.db, "nr_yaml_key");
        let app = cli_tools_router().with_state(state);

        // Create a named Hermes profile directory with identity file + default config.yaml
        let work_profile_dir = env.home_dir.join(".hermes/profiles/work");
        std::fs::create_dir_all(&work_profile_dir).unwrap();
        std::fs::write(work_profile_dir.join("profile.yaml"), "display_name: \"Work\"\n").unwrap();
        std::fs::write(env.home_dir.join(".hermes/config.yaml"), "model: \"\"\n").unwrap();

        let (st, val) = call_router(app.clone(), "GET", "/api/cli-tools/hermes-profiles", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["profiles"].as_array().unwrap().len(), 2);

        // Apply Hermes settings + STT audio
        let (st, val) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/hermes-settings",
            Some(json!({
                "profile": "work",
                "baseUrl": "http://127.0.0.1:20128",
                "apiKey": "hk_secret",
                "selections": [
                    { "role": "default", "model": "cc/claude-opus-4-6" },
                    { "role": "delegation", "model": "cc/claude-haiku-4-5" }
                ],
                "audio": { "enabled": true, "model": "groq/whisper-large-v3" }
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["stt9Router"], json!(true));

        let (_, val) = call_router(
            app.clone(),
            "GET",
            "/api/cli-tools/hermes-settings?profile=work",
            None,
        )
        .await;
        assert_eq!(val["installed"], json!(true));
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["has9RouterStt"], json!(true));

        let (st, val) = call_router(
            app.clone(),
            "DELETE",
            "/api/cli-tools/hermes-settings?profile=work",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["removed"]["model"], json!(true));
        assert_eq!(val["removed"]["stt"], json!(true));

        // OMP
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/omp-settings",
            Some(json!({ "baseUrl": "http://localhost:20128" })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/omp-settings", None).await;
        assert_eq!(val["has9Router"], json!(true));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/omp-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Cowork
        let (st, val) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/cowork-settings",
            Some(json!({
                "baseUrl": "http://localhost:20128/v1",
                "apiKey": "cw_key",
                "models": ["cc/claude-opus-4-6"],
                "localPlugins": ["browsermcp"],
                "customPlugins": [{ "name": "my-mcp", "url": "http://localhost:20128/api/mcp/my-mcp/sse" }]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["bootstrapped"], json!(true));
        let (_, val) = call_router(app.clone(), "GET", "/api/cli-tools/cowork-settings", None).await;
        assert_eq!(val["installed"], json!(true));
        assert_eq!(val["has9Router"], json!(true));
        assert_eq!(val["cowork"]["localPlugins"], json!(["browsermcp"]));
        let (st, _) = call_router(app.clone(), "DELETE", "/api/cli-tools/cowork-settings", None).await;
        assert_eq!(st, StatusCode::OK);

        // Devin (not installed in clean empty_bin)
        let (st, val) = call_router(app, "GET", "/api/cli-tools/devin-settings", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["installed"], json!(false));
    }

    #[tokio::test]
    async fn test_cowork_mcp_tools_ssrf_and_registry_filters() {
        let env = TempEnvGuard::new("mcp");
        let state = make_test_state(&env.data_dir);
        let app = cli_tools_router().with_state(state);

        // Missing url -> 400
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/cowork-mcp-tools",
            Some(json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // Remote caller targeting localhost -> SSRF blocked (400)
        let req = Request::builder()
            .method("POST")
            .uri("/api/cli-tools/cowork-mcp-tools")
            .header(header::HOST, "example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "url": "http://127.0.0.1:9999/mcp" })).unwrap(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // URL filter checks
        assert!(is_direct_connect_mcp_url("https://mcp.exa.ai/mcp"));
        assert!(!is_direct_connect_mcp_url("https://mcp.claude.com/v1"));
        assert!(!is_direct_connect_mcp_url("https://api.anthropic.com/mcp/x"));
        assert!(!is_direct_connect_mcp_url("http://insecure.example.com"));
        assert!(!is_direct_connect_mcp_url("https://example.com/{tenant}/mcp"));
    }

    #[tokio::test]
    async fn test_antigravity_mitm_and_alias_routes() {
        let env = TempEnvGuard::new("mitm");
        let state = make_test_state(&env.data_dir);
        let app = cli_tools_router().with_state(state.clone());

        // GET status
        let (st, val) = call_router(app.clone(), "GET", "/api/cli-tools/antigravity-mitm", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["mitmRouterBaseUrl"], json!("http://localhost:20128"));

        // POST missing apiKey -> 400
        let (st, _) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/antigravity-mitm",
            Some(json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // PATCH missing tool/action -> 400
        let (st, _) = call_router(
            app.clone(),
            "PATCH",
            "/api/cli-tools/antigravity-mitm",
            Some(json!({ "tool": "antigravity" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // Password encryption roundtrip
        let enc = encrypt_mitm_password(&env.data_dir, "s3cr3t!").unwrap();
        assert_eq!(
            decrypt_mitm_password(&env.data_dir, &enc).as_deref(),
            Some("s3cr3t!")
        );

        // Alias PUT fails with 403 when DNS not enabled in temp hosts file
        let temp_hosts = env.root.join("hosts");
        std::fs::write(&temp_hosts, "127.0.0.1 localhost\n").unwrap();
        let resp = handle_mitm_alias_put_with_hosts(
            &state,
            &temp_hosts,
            br#"{"tool":"antigravity","mappings":{"gemini-pro":"cc/claude-opus-4-6"}}"#,
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Enable DNS in temp hosts file -> PUT succeeds & updates DB + aliases.json
        std::fs::write(
            &temp_hosts,
            "127.0.0.1 daily-cloudcode-pa.googleapis.com\n127.0.0.1 cloudcode-pa.googleapis.com\n",
        )
        .unwrap();
        let resp = handle_mitm_alias_put_with_hosts(
            &state,
            &temp_hosts,
            br#"{"tool":"antigravity","mappings":{"gemini-pro":" cc/claude-opus-4-6 ","empty":"  "}}"#,
        );
        assert_eq!(resp.status(), StatusCode::OK);

        // GET /api/cli-tools/antigravity-mitm/alias?tool=antigravity reads from DB
        let (st, val) = call_router(
            app.clone(),
            "GET",
            "/api/cli-tools/antigravity-mitm/alias?tool=antigravity",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            val["aliases"]["gemini-pro"],
            json!("cc/claude-opus-4-6")
        );
        assert!(val["aliases"].get("empty").is_none());

        // Verify aliases.json cache was written
        let cache_val = read_jsonc_value(&env.data_dir.join("mitm/aliases.json")).unwrap();
        assert_eq!(
            cache_val["antigravity"]["gemini-pro"],
            json!("cc/claude-opus-4-6")
        );
    }

    #[tokio::test]
    async fn test_hermes_apply_to_all_and_options_preflight() {
        let env = TempEnvGuard::new("hermes_bulk");
        let state = make_test_state(&env.data_dir);
        let app = cli_tools_router().with_state(state);

        // Default profile: unconfigured
        std::fs::create_dir_all(env.home_dir.join(".hermes")).unwrap();
        std::fs::write(env.home_dir.join(".hermes/config.yaml"), "model: \"\"\n").unwrap();

        // Profile "customp": uses custom provider
        let custom_dir = env.home_dir.join(".hermes/profiles/customp");
        std::fs::create_dir_all(&custom_dir).unwrap();
        std::fs::write(
            custom_dir.join("config.yaml"),
            "model:\n  default: \"cx/gpt-5\"\n  provider: \"custom\"\n  base_url: \"http://localhost:1111/v1\"\n",
        )
        .unwrap();

        // Profile "other": uses openrouter provider -> must be skipped
        let other_dir = env.home_dir.join(".hermes/profiles/other");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(
            other_dir.join("config.yaml"),
            "model:\n  default: \"or/model\"\n  provider: \"openrouter\"\n",
        )
        .unwrap();

        let (st, val) = call_router(
            app.clone(),
            "POST",
            "/api/cli-tools/hermes-settings",
            Some(json!({
                "applyToAll": true,
                "baseUrl": "http://127.0.0.1:20128",
                "apiKey": "bulk_key",
                "model": "cc/claude-sonnet-4-6"
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(val["bulk"], json!(true));
        assert_eq!(val["updated"], json!(2));
        assert_eq!(val["skipped"], json!(1));

        // Verify customp kept its own model while updating base_url
        let custom_yaml = std::fs::read_to_string(custom_dir.join("config.yaml")).unwrap();
        assert!(custom_yaml.contains("cx/gpt-5"));
        assert!(custom_yaml.contains("http://127.0.0.1:20128/v1"));

        // Verify OPTIONS preflight returns 204
        for path in [
            "/api/cli-tools/claude-settings",
            "/api/cli-tools/codex-profiles",
            "/api/cli-tools/cowork-mcp-registry",
            "/api/cli-tools/antigravity-mitm",
            "/api/cli-tools/antigravity-mitm/alias",
        ] {
            let (opt_st, _) = call_router(app.clone(), "OPTIONS", path, None).await;
            assert_eq!(opt_st, StatusCode::NO_CONTENT);
        }
    }

    #[test]
    fn test_mcp_registry_parser_and_sse_tools_parser() {
        let page = json!({
            "servers": [
                {
                    "server": {
                        "name": "exa",
                        "title": "Exa",
                        "description": "Search",
                        "remotes": [{ "type": "http", "url": "https://mcp.exa.ai/mcp" }]
                    },
                    "_meta": {
                        "com.anthropic.api/mcp-registry": {
                            "slug": "exa",
                            "isAuthless": true,
                            "toolNames": ["web_search_exa"]
                        }
                    }
                },
                {
                    "server": {
                        "name": "mediated",
                        "remotes": [{ "type": "http", "url": "https://foo.mcp.claude.com/mcp" }]
                    }
                },
                {
                    "server": {
                        "name": "needs-tenant",
                        "remotes": [{ "type": "sse", "url": "https://example.com/sse" }]
                    },
                    "_meta": {
                        "com.anthropic.api/mcp-registry": {
                            "requiredFields": ["tenantId"]
                        }
                    }
                }
            ],
            "metadata": { "nextCursor": "cur_2" }
        });
        let mut out = Vec::new();
        let next = parse_mcp_registry_page(&page, &mut out);
        assert_eq!(next.as_deref(), Some("cur_2"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["slug"], json!("exa"));
        assert_eq!(out[0]["oauth"], json!(false));
        assert_eq!(out[0]["toolCount"], json!(1));

        // SSE response parsing
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"search\",\"description\":\"Web search\"}]}}\n\n";
        let tools = parse_mcp_tools_from_response("text/event-stream", sse);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!("search"));

        // JSON response parsing
        let raw_json = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":""}]}}"#;
        let tools_json = parse_mcp_tools_from_response("application/json", raw_json);
        assert_eq!(tools_json.len(), 1);
        assert_eq!(tools_json[0]["name"], json!("ping"));
    }
}
