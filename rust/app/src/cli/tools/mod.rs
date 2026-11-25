use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

pub struct ConnectContext {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub claude_models: HashMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ToolStatus {
    pub configured: bool,
    pub base_url: Option<String>,
    pub api_key_masked: Option<String>,
    pub models: HashMap<String, String>,
    pub active: Option<bool>,
    pub error: Option<String>,
}

pub trait CliTool: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn config_paths(&self) -> Vec<PathBuf>;
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>>;
    fn inspect(&self) -> ToolStatus;
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>>;
}

// ── Helpers ─────────────────────────────────────────────────────────────────

pub fn home_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into()),
    )
}

pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.len() >= 12 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else if !trimmed.is_empty() {
        "****".to_string()
    } else {
        "(none)".to_string()
    }
}

pub fn v1_url(base: &str) -> String {
    let clean = base.trim().trim_end_matches('/');
    if clean.ends_with("/v1") {
        clean.to_string()
    } else {
        format!("{}/v1", clean)
    }
}

pub fn write_file_with_backup(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup_path = PathBuf::from(format!("{}.bak-9router", path.display()));
    if path.exists() && !backup_path.exists() {
        let _ = fs::copy(path, &backup_path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600));
        }
    }

    fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn rewrite_file(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Strip trailing commas before `}` or `]` outside JSON string literals (JSONC tolerance).
pub fn strip_jsonc_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_str = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if b == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn read_json_value(path: &Path) -> Option<serde_json::Value> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn read_jsonc_value(path: &Path) -> Option<serde_json::Value> {
    let s = fs::read_to_string(path).ok()?;
    let stripped = strip_jsonc_trailing_commas(&s);
    serde_json::from_str(&stripped).ok()
}

// ── Claude Code ─────────────────────────────────────────────────────────────

pub struct ClaudeTool;
impl CliTool for ClaudeTool {
    fn id(&self) -> &'static str { "claude" }
    fn display_name(&self) -> &'static str { "Claude Code" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".claude/settings.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() {
            cur = serde_json::json!({});
        }
        if !cur["env"].is_object() {
            cur["env"] = serde_json::json!({});
        }

        cur["hasCompletedOnboarding"] = serde_json::json!(true);
        cur["env"]["ANTHROPIC_BASE_URL"] = serde_json::json!(v1_url(&ctx.base_url));
        cur["env"]["ANTHROPIC_AUTH_TOKEN"] = serde_json::json!(&ctx.api_key);

        for (k, v) in &ctx.claude_models {
            cur["env"][k] = serde_json::json!(v);
        }

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(env) = cur.get("env").and_then(|e| e.as_object()) {
                if let Some(base) = env.get("ANTHROPIC_BASE_URL").and_then(|b| b.as_str()) {
                    status.base_url = Some(base.to_string());
                    status.configured = true;
                }
                if let Some(token) = env.get("ANTHROPIC_AUTH_TOKEN").and_then(|t| t.as_str()) {
                    status.api_key_masked = Some(mask_key(token));
                    status.configured = true;
                }
                for (k, v) in env {
                    if k.starts_with("ANTHROPIC_DEFAULT_") || k == "ANTHROPIC_MODEL" {
                        if let Some(s) = v.as_str() {
                            status.models.insert(k.clone(), s.to_string());
                        }
                    }
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(env) = cur.get_mut("env").and_then(|e| e.as_object_mut()) {
                env.remove("ANTHROPIC_BASE_URL");
                env.remove("ANTHROPIC_AUTH_TOKEN");
                env.remove("ANTHROPIC_DEFAULT_FABLE_MODEL");
                env.remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
                env.remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
                env.remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Codex CLI ───────────────────────────────────────────────────────────────

pub struct CodexTool;
impl CliTool for CodexTool {
    fn id(&self) -> &'static str { "codex" }
    fn display_name(&self) -> &'static str { "OpenAI Codex CLI" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".codex/config.toml")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let toml_content = format!(
            r#"model = "{model}"
model_provider = "9router"

[model_providers.9router]
name = "9Router"
base_url = "{base_url}"
wire_api = "responses"

[model_providers.9router.http_headers]
Authorization = "Bearer {api_key}"

[agents]
default_subagent_model = "{model}"
"#,
            model = ctx.model,
            base_url = v1_url(&ctx.base_url),
            api_key = ctx.api_key
        );

        write_file_with_backup(&file, &toml_content)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Ok(content) = fs::read_to_string(file) {
            if content.contains("model_provider = \"9router\"") {
                status.configured = true;
                status.active = Some(true);
            }
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("base_url =") {
                    status.base_url = trimmed.split('=').nth(1).map(|s| s.trim().trim_matches('"').to_string());
                } else if trimmed.starts_with("Authorization =") {
                    if let Some(auth) = trimmed.split('=').nth(1) {
                        let token = auth.trim().trim_matches('"').replace("Bearer ", "");
                        status.api_key_masked = Some(mask_key(&token));
                    }
                } else if trimmed.starts_with("model =") {
                    if let Some(m) = trimmed.split('=').nth(1) {
                        status.models.insert("main".into(), m.trim().trim_matches('"').to_string());
                    }
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if file.exists() {
            let _ = fs::remove_file(&file);
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── OpenCode ────────────────────────────────────────────────────────────────

pub struct OpenCodeTool;
impl CliTool for OpenCodeTool {
    fn id(&self) -> &'static str { "opencode" }
    fn display_name(&self) -> &'static str { "OpenCode" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".config/opencode/opencode.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() { cur = serde_json::json!({}); }
        if !cur["provider"].is_object() { cur["provider"] = serde_json::json!({}); }
        if !cur["agent"].is_object() { cur["agent"] = serde_json::json!({}); }

        cur["model"] = serde_json::json!(format!("9router/{}", ctx.model));

        let p_9router = serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": {
                "baseURL": v1_url(&ctx.base_url),
                "apiKey": ctx.api_key
            },
            "models": {
                &ctx.model: {
                    "name": &ctx.model,
                    "modalities": { "input": ["text", "image"], "output": ["text"] }
                }
            }
        });
        cur["provider"]["9router"] = p_9router;

        cur["agent"]["explorer"] = serde_json::json!({
            "description": "Fast explorer subagent for codebase exploration",
            "mode": "subagent",
            "model": format!("9router/{}", ctx.model)
        });

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(p) = cur.pointer("/provider/9router/options") {
                status.base_url = p.get("baseURL").and_then(|b| b.as_str()).map(ToString::to_string);
                status.api_key_masked = p.get("apiKey").and_then(|k| k.as_str()).map(mask_key);
                status.configured = true;
            }
            if let Some(m) = cur.get("model").and_then(|m| m.as_str()) {
                status.models.insert("main".into(), m.to_string());
                status.active = Some(m.starts_with("9router/"));
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(obj) = cur.as_object_mut() {
                if let Some(prov) = obj.get_mut("provider").and_then(|p| p.as_object_mut()) {
                    prov.remove("9router");
                }
                if let Some(m) = obj.get("model").and_then(|m| m.as_str()) {
                    if m.starts_with("9router/") {
                        obj.remove("model");
                    }
                }
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Factory Droid ───────────────────────────────────────────────────────────

pub struct DroidTool;
impl CliTool for DroidTool {
    fn id(&self) -> &'static str { "droid" }
    fn display_name(&self) -> &'static str { "Factory Droid" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".factory/settings.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() { cur = serde_json::json!({}); }

        let new_model = serde_json::json!({
            "model": &ctx.model,
            "id": "custom:9Router-0",
            "index": 0,
            "baseUrl": v1_url(&ctx.base_url),
            "apiKey": &ctx.api_key,
            "displayName": &ctx.model,
            "maxOutputTokens": 131072,
            "noImageSupport": false,
            "provider": "openai"
        });

        let mut models = vec![new_model];
        if let Some(existing) = cur.get("customModels").and_then(|c| c.as_array()) {
            for m in existing {
                if m.get("id").and_then(|i| i.as_str()).map(|s| s.starts_with("custom:9Router")) != Some(true) {
                    models.push(m.clone());
                }
            }
        }
        cur["customModels"] = serde_json::Value::Array(models);

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(models) = cur.get("customModels").and_then(|c| c.as_array()) {
                for m in models {
                    if m.get("id").and_then(|i| i.as_str()).map(|s| s.starts_with("custom:9Router")) == Some(true) {
                        status.configured = true;
                        status.base_url = m.get("baseUrl").and_then(|b| b.as_str()).map(ToString::to_string);
                        status.api_key_masked = m.get("apiKey").and_then(|k| k.as_str()).map(mask_key);
                        if let Some(name) = m.get("model").and_then(|n| n.as_str()) {
                            status.models.insert("main".into(), name.to_string());
                        }
                        break;
                    }
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(models) = cur.get_mut("customModels").and_then(|c| c.as_array_mut()) {
                models.retain(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.starts_with("custom:9Router")) != Some(true));
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Crush ───────────────────────────────────────────────────────────────────

pub struct CrushTool;
impl CliTool for CrushTool {
    fn id(&self) -> &'static str { "crush" }
    fn display_name(&self) -> &'static str { "Crush" }
    fn config_paths(&self) -> Vec<PathBuf> {
        let config_home = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".config"));
        vec![config_home.join("crush/crush.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() { cur = serde_json::json!({}); }
        if !cur["providers"].is_object() { cur["providers"] = serde_json::json!({}); }

        cur["providers"]["9router"] = serde_json::json!({
            "type": "openai-compat",
            "base_url": v1_url(&ctx.base_url),
            "api_key": &ctx.api_key,
            "models": [{ "id": &ctx.model, "name": &ctx.model, "context_window": 128000 }]
        });

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(p) = cur.pointer("/providers/9router") {
                status.configured = true;
                status.base_url = p.get("base_url").and_then(|b| b.as_str()).map(ToString::to_string);
                status.api_key_masked = p.get("api_key").and_then(|k| k.as_str()).map(mask_key);
                if let Some(arr) = p.get("models").and_then(|m| m.as_array()) {
                    if let Some(first) = arr.first().and_then(|f| f.get("id")).and_then(|id| id.as_str()) {
                        status.models.insert("main".into(), first.to_string());
                    }
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(providers) = cur.get_mut("providers").and_then(|p| p.as_object_mut()) {
                providers.remove("9router");
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Kilo Code CLI ───────────────────────────────────────────────────────────

pub struct KiloTool;
impl CliTool for KiloTool {
    fn id(&self) -> &'static str { "kilo" }
    fn display_name(&self) -> &'static str { "Kilo Code CLI" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".local/share/kilo/auth.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() { cur = serde_json::json!({}); }

        cur["openai-compatible"] = serde_json::json!({
            "type": "api-key",
            "apiKey": &ctx.api_key,
            "baseUrl": v1_url(&ctx.base_url),
            "model": &ctx.model
        });

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(p) = cur.get("openai-compatible") {
                status.configured = true;
                status.base_url = p.get("baseUrl").and_then(|b| b.as_str()).map(ToString::to_string);
                status.api_key_masked = p.get("apiKey").and_then(|k| k.as_str()).map(mask_key);
                if let Some(m) = p.get("model").and_then(|m| m.as_str()) {
                    status.models.insert("main".into(), m.to_string());
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(obj) = cur.as_object_mut() {
                obj.remove("openai-compatible");
                obj.remove("9router");
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Cline CLI ───────────────────────────────────────────────────────────────

pub struct ClineTool;
impl CliTool for ClineTool {
    fn id(&self) -> &'static str { "cline" }
    fn display_name(&self) -> &'static str { "Cline CLI" }
    fn config_paths(&self) -> Vec<PathBuf> {
        let dir = home_dir().join(".cline/data");
        vec![dir.join("globalState.json"), dir.join("secrets.json")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let state_file = self.config_paths()[0].clone();
        let secrets_file = self.config_paths()[1].clone();

        let mut state = read_json_value(&state_file).unwrap_or_else(|| serde_json::json!({}));
        if !state.is_object() { state = serde_json::json!({}); }
        state["actModeApiProvider"] = serde_json::json!("openai");
        state["planModeApiProvider"] = serde_json::json!("openai");
        state["openAiBaseUrl"] = serde_json::json!(ctx.base_url.trim_end_matches('/'));
        state["openAiModelId"] = serde_json::json!(&ctx.model);
        state["planModeOpenAiModelId"] = serde_json::json!(&ctx.model);
        write_file_with_backup(&state_file, &serde_json::to_string_pretty(&state)?)?;

        let mut secrets = read_json_value(&secrets_file).unwrap_or_else(|| serde_json::json!({}));
        if !secrets.is_object() { secrets = serde_json::json!({}); }
        secrets["openAiApiKey"] = serde_json::json!(&ctx.api_key);
        write_file_with_backup(&secrets_file, &serde_json::to_string_pretty(&secrets)?)?;

        Ok(vec![state_file, secrets_file])
    }
    fn inspect(&self) -> ToolStatus {
        let state_file = &self.config_paths()[0];
        let secrets_file = &self.config_paths()[1];
        let mut status = ToolStatus::default();

        if let Some(state) = read_json_value(state_file) {
            status.base_url = state.get("openAiBaseUrl").and_then(|b| b.as_str()).map(ToString::to_string);
            if let Some(m) = state.get("openAiModelId").and_then(|m| m.as_str()) {
                status.models.insert("act".into(), m.to_string());
            }
            if let Some(p) = state.get("actModeApiProvider").and_then(|p| p.as_str()) {
                status.active = Some(p == "openai");
                if p == "openai" {
                    status.configured = true;
                }
            }
        }
        if let Some(sec) = read_json_value(secrets_file) {
            if let Some(k) = sec.get("openAiApiKey").and_then(|k| k.as_str()) {
                status.api_key_masked = Some(mask_key(k));
                status.configured = true;
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let state_file = self.config_paths()[0].clone();
        let secrets_file = self.config_paths()[1].clone();
        let mut touched = Vec::new();

        if let Some(mut state) = read_json_value(&state_file) {
            if state.get("actModeApiProvider").and_then(|p| p.as_str()) == Some("openai") {
                if let Some(obj) = state.as_object_mut() {
                    obj.remove("openAiBaseUrl");
                    obj.remove("openAiModelId");
                    obj.remove("planModeOpenAiModelId");
                    obj.insert("actModeApiProvider".into(), serde_json::json!("cline"));
                    obj.insert("planModeApiProvider".into(), serde_json::json!("cline"));
                }
            }
            rewrite_file(&state_file, &serde_json::to_string_pretty(&state)?)?;
            touched.push(state_file);
        }

        if let Some(mut sec) = read_json_value(&secrets_file) {
            if let Some(obj) = sec.as_object_mut() {
                obj.remove("openAiApiKey");
            }
            rewrite_file(&secrets_file, &serde_json::to_string_pretty(&sec)?)?;
            touched.push(secrets_file);
        }

        Ok(touched)
    }
}

// ── Pi (pi-coding-agent) ────────────────────────────────────────────────────

pub struct PiTool;
impl CliTool for PiTool {
    fn id(&self) -> &'static str { "pi" }
    fn display_name(&self) -> &'static str { "Pi" }
    fn config_paths(&self) -> Vec<PathBuf> {
        let agent = home_dir().join(".pi/agent/models.json");
        let root = home_dir().join(".pi/models.json");
        if !agent.exists() && root.exists() {
            vec![root]
        } else {
            vec![agent]
        }
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let mut cur = read_json_value(&file).unwrap_or_else(|| serde_json::json!({}));
        if !cur.is_object() { cur = serde_json::json!({}); }
        if !cur["providers"].is_object() { cur["providers"] = serde_json::json!({}); }

        cur["providers"]["9router"] = serde_json::json!({
            "baseUrl": v1_url(&ctx.base_url),
            "apiKey": &ctx.api_key,
            "api": "openai-completions",
            "models": [{
                "id": &ctx.model,
                "name": &ctx.model,
                "contextWindow": 128000,
                "maxTokens": 16384
            }]
        });

        write_file_with_backup(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Some(cur) = read_json_value(file) {
            if let Some(p) = cur.pointer("/providers/9router") {
                status.configured = true;
                status.base_url = p.get("baseUrl").and_then(|b| b.as_str()).map(ToString::to_string);
                status.api_key_masked = p.get("apiKey").and_then(|k| k.as_str()).map(mask_key);
                if let Some(models) = p.get("models").and_then(|m| m.as_array()) {
                    if let Some(first) = models.first().and_then(|f| f.get("id")).and_then(|i| i.as_str()) {
                        status.models.insert("main".into(), first.to_string());
                    }
                }
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if let Some(mut cur) = read_json_value(&file) {
            if let Some(providers) = cur.get_mut("providers").and_then(|p| p.as_object_mut()) {
                providers.remove("9router");
            }
            rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Oh My Pi ────────────────────────────────────────────────────────────────

pub struct OmpTool;
impl CliTool for OmpTool {
    fn id(&self) -> &'static str { "omp" }
    fn display_name(&self) -> &'static str { "Oh My Pi" }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![home_dir().join(".omp/agent/models.yml")]
    }
    fn apply(&self, ctx: &ConnectContext) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        let yml_content = format!(
            r#"providers:
  9router:
    baseUrl: "{base_url}"
    apiKey: "{api_key}"
    api: "openai-completions"
    authHeader: true
    disableStrictTools: true
    discovery:
      type: "proxy"
"#,
            base_url = v1_url(&ctx.base_url),
            api_key = ctx.api_key
        );

        write_file_with_backup(&file, &yml_content)?;
        Ok(vec![file])
    }
    fn inspect(&self) -> ToolStatus {
        let file = &self.config_paths()[0];
        let mut status = ToolStatus::default();
        if let Ok(content) = fs::read_to_string(file) {
            if content.contains("9router:") {
                status.configured = true;
            }
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("baseUrl:") {
                    status.base_url = trimmed.split(':').nth(1).map(|s| s.trim().trim_matches('"').to_string());
                } else if trimmed.starts_with("apiKey:") {
                    if let Some(k) = trimmed.split(':').nth(1) {
                        status.api_key_masked = Some(mask_key(k.trim().trim_matches('"')));
                    }
                }
            }
            if status.configured {
                status.models.insert("main".into(), "(all server models via proxy discovery)".into());
            }
        }
        status
    }
    fn reset(&self) -> anyhow::Result<Vec<PathBuf>> {
        let file = self.config_paths()[0].clone();
        if file.exists() {
            let _ = fs::remove_file(&file);
            return Ok(vec![file]);
        }
        Ok(vec![])
    }
}

// ── Registry ────────────────────────────────────────────────────────────────

pub fn all_tools() -> Vec<Box<dyn CliTool>> {
    vec![
        Box::new(ClaudeTool),
        Box::new(CodexTool),
        Box::new(OpenCodeTool),
        Box::new(DroidTool),
        Box::new(CrushTool),
        Box::new(KiloTool),
        Box::new(ClineTool),
        Box::new(PiTool),
        Box::new(OmpTool),
    ]
}

pub fn resolve_tools(names: &[String]) -> Vec<Box<dyn CliTool>> {
    let all = all_tools();
    if names.is_empty() || names.contains(&"all".to_string()) {
        return all;
    }

    let mut selected: Vec<Box<dyn CliTool>> = Vec::new();
    for tool in all {
        for n in names {
            let norm = n.trim().to_lowercase();
            if norm == tool.id() || norm == tool.display_name().to_lowercase() {
                selected.push(tool);
                break;
            }
        }
    }
    selected
}

pub fn run_connect(
    target_url: Option<String>,
    tool_filter: Option<String>,
    api_key_arg: Option<String>,
    model_arg: Option<String>,
    reset: bool,
) -> anyhow::Result<()> {
    let tools = match tool_filter {
        Some(ref f) => {
            let parts: Vec<String> = f.split(',').map(|s| s.trim().to_string()).collect();
            resolve_tools(&parts)
        }
        None => all_tools(),
    };

    if reset {
        println!("\x1b[36m>\x1b[0m Resetting 9Router configuration in selected CLI tools...");
        for tool in tools {
            let touched = tool.reset()?;
            if !touched.is_empty() {
                println!("  \x1b[32m✓\x1b[0m Reset {}: {}", tool.display_name(), touched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
            }
        }
        println!("\x1b[32m\x1b[1m✓\x1b[0m Reset complete.");
        return Ok(());
    }

    let base_url = target_url.unwrap_or_else(|| "http://localhost:20127".to_string());
    let api_key = api_key_arg.unwrap_or_else(|| {
        std::env::var("NINE_ROUTER_API_KEY").unwrap_or_else(|_| "nr_default_key".to_string())
    });
    let model = model_arg.unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string());

    let mut claude_models = HashMap::new();
    claude_models.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), model.clone());

    let ctx = ConnectContext {
        base_url: base_url.clone(),
        api_key: api_key.clone(),
        model: model.clone(),
        claude_models,
    };

    println!("\x1b[36m>\x1b[0m Connecting CLI tools to 9Router at {}...", base_url);
    println!("  Model:   {}", model);
    println!("  API Key: {}", mask_key(&api_key));

    for tool in tools {
        match tool.apply(&ctx) {
            Ok(paths) => {
                println!("  \x1b[32m\x1b[1m✓\x1b[0m {}: configured ({})", tool.display_name(), paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
            }
            Err(e) => {
                eprintln!("  \x1b[31mx\x1b[0m {}: failed ({})", tool.display_name(), e);
            }
        }
    }

    println!("\n\x1b[32m\x1b[1m✓\x1b[0m All specified tools connected to 9Router!");
    println!("  Run '9r show' to inspect the active configuration.");
    Ok(())
}

pub fn run_show(tool_filter: Option<String>, json_output: bool) -> anyhow::Result<()> {
    let tools = match tool_filter {
        Some(ref f) => {
            let parts: Vec<String> = f.split(',').map(|s| s.trim().to_string()).collect();
            resolve_tools(&parts)
        }
        None => all_tools(),
    };

    if json_output {
        let mut list = Vec::new();
        for tool in &tools {
            let st = tool.inspect();
            list.push(serde_json::json!({
                "id": tool.id(),
                "name": tool.display_name(),
                "paths": tool.config_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "status": st
            }));
        }
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }

    println!("\x1b[1m9Router CLI Tool Configurations\x1b[0m\n");
    for tool in &tools {
        let st = tool.inspect();
        let paths = tool.config_paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");

        if let Some(err) = st.error {
            println!("\x1b[31mx\x1b[0m {}: {}", tool.display_name(), err);
            continue;
        }

        if !st.configured {
            println!("• {}: \x1b[2mnot configured for 9Router ({})\x1b[0m", tool.display_name(), paths);
            continue;
        }

        let active_indicator = match st.active {
            Some(false) => " \x1b[33m(present but inactive)\x1b[0m",
            _ => "",
        };

        println!("\x1b[32m\x1b[1m✓\x1b[0m \x1b[1m{}\x1b[0m{}", tool.display_name(), active_indicator);
        println!("   File:     {}", paths);
        println!("   Base URL: {}", st.base_url.unwrap_or_else(|| "(none)".into()));
        println!("   API Key:  {}", st.api_key_masked.unwrap_or_else(|| "(none)".into()));

        if st.models.is_empty() {
            println!("   Models:   (default)");
        } else {
            let models_str = st.models.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(", ");
            println!("   Models:   {}", models_str);
        }
        println!();
    }

    Ok(())
}
